use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap, HashSet, hash_map::Entry};
use std::rc::Rc;

use pulldown_cmark::{Event, HeadingLevel, Parser, Tag};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

use super::directive_history::{LinkDecision, ReviewedDirective, resolve_links};
use super::prepared_records::{Content, Fields, Intake, PreparedRecord};
use super::reviewed_records::{self, Review, TitleProblem, valid_title};
use super::scope_plan::{ScopeOutcome, is_portable_scope};
use super::standalone_artifact::JoinContext;
use super::{AssetObjectReader, Inventory, Reason, snapshot_object};
use crate::cli::is_slug;
use crate::config_file::{MAX_FILE_BYTES, parse_v2, present};
use crate::error::HiveError;

const MAX_MARKDOWN_BYTES: usize = 256 * 1024;

#[derive(Clone, Copy, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
enum Surface {
    Directives,
    ArtifactsStandalone,
    ArtifactsWorkScoped,
}

#[derive(Clone, Copy, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum DirectiveStatus {
    Proposed,
    Active,
    Superseded,
    Withdrawn,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum ArtifactStatus {
    Active,
    Parked,
    Closed,
}

#[derive(Clone, Copy, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum ArtifactKind {
    Report,
    Research,
    Review,
    Handoff,
    Message,
    Checkpoint,
    WorkNote,
    Manifest,
    Design,
    Decision,
    Evidence,
}

impl ArtifactKind {
    fn is_v1(self) -> bool {
        !matches!(self, Self::Design | Self::Decision | Self::Evidence)
    }
}

#[derive(Clone, Copy)]
enum Metadata {
    Directive(DirectiveStatus),
    Artifact {
        kind: ArtifactKind,
        legacy_status: Option<ArtifactStatus>,
    },
}

enum Shape {
    Record,
    Held(Reason),
    Excluded(Reason),
}

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum Outcome {
    Held {
        reason_codes: Vec<Reason>,
        #[serde(skip_serializing_if = "Option::is_none")]
        candidate: Option<Candidate>,
    },
    Excluded {
        reason: Reason,
    },
    Prepared {
        candidate: Candidate,
        target_id: String,
        target_title_sha256: String,
        target_body_sha256: String,
        provenance: Box<Fields>,
    },
}

#[derive(Serialize)]
#[serde(tag = "record_type", rename_all = "snake_case")]
pub(super) enum Candidate {
    Directive {
        status: DirectiveStatus,
        scope_kind: &'static str,
        area_id: String,
        include_descendants: bool,
    },
    Artifact {
        kind: ArtifactKind,
        legacy_status: Option<ArtifactStatus>,
    },
}

impl Outcome {
    fn held(reason: Reason) -> Self {
        Self::Held {
            reason_codes: vec![reason],
            candidate: None,
        }
    }

    fn with_review(
        self,
        review: &Review,
        selected: SelectedContent<'_>,
        supported_shape: bool,
        observed_at: Option<OffsetDateTime>,
        parent: Option<&str>,
        ctx: &JoinContext<'_>,
    ) -> Self {
        let Self::Held {
            candidate: Some(candidate),
            mut reason_codes,
        } = self
        else {
            return self;
        };
        let provenance = if review.is_work_scoped() {
            match (supported_shape, parent, selected.record.body.as_deref()) {
                (true, Some(parent), Some(body)) => {
                    review.prepare_work_scoped(&candidate, body, parent, &ctx.tasks)
                }
                _ => Err(Reason::UnsupportedReviewedRecord),
            }
        } else if supported_shape && parent.is_none() {
            review.prepare(
                &candidate,
                observed_at,
                ctx,
                selected.record.body.as_deref(),
            )
        } else {
            Err(Reason::UnsupportedReviewedRecord)
        };
        match provenance {
            Ok(provenance) => Self::Prepared {
                candidate,
                target_id: review.target_id.clone(),
                target_title_sha256: reviewed_records::sha256(selected.title),
                target_body_sha256: selected.record.body_sha256.clone(),
                provenance: Box::new(provenance),
            },
            Err(reason) => {
                if !matches!(reason, Reason::UnsupportedReviewedRecord) {
                    reason_codes.clear();
                }
                reason_codes.push(reason);
                Self::Held {
                    reason_codes,
                    candidate: Some(candidate),
                }
            }
        }
    }

    fn with_link(mut self, decision: LinkDecision<'_>) -> Self {
        match decision {
            LinkDecision::ReverseTo(supersedes_id) => {
                if let Self::Prepared { provenance, .. } = &mut self
                    && let Fields::Directive(fields) = provenance.as_mut()
                {
                    fields.supersedes_id = Some(supersedes_id.to_owned());
                }
            }
            LinkDecision::Hold => match self {
                Self::Prepared { candidate, .. } => {
                    return Self::Held {
                        candidate: Some(candidate),
                        reason_codes: vec![Reason::InvalidSupersessionChain],
                    };
                }
                Self::Held {
                    ref mut reason_codes,
                    ..
                } => {
                    if !reason_codes
                        .iter()
                        .any(|reason| matches!(reason, Reason::InvalidSupersessionChain))
                    {
                        reason_codes.push(Reason::InvalidSupersessionChain);
                    }
                }
                Self::Excluded { .. } => {}
            },
        }
        self
    }
}

#[derive(Default, Serialize)]
struct Counts {
    inventoried: usize,
    held: usize,
    excluded: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    prepared: Option<usize>,
}

struct Inspection {
    raw_sha256: String,
    record: Result<InspectedRecord, Reason>,
}

struct InspectedRecord {
    metadata: Metadata,
    title: Result<String, TitleProblem>,
    body_sha256: String,
    body: Option<Rc<str>>,
}

struct SelectedContent<'a> {
    record: &'a InspectedRecord,
    title: &'a str,
}

impl SelectedContent<'_> {
    fn for_storage(&self) -> Option<Content> {
        Some(Content {
            title: self.title.to_owned(),
            body: self.record.body.clone()?,
        })
    }
}

pub(super) fn scan(
    inventory: &Inventory,
    reader: &AssetObjectReader<'_>,
    scopes: &BTreeMap<&str, &ScopeOutcome<'_>>,
    reviews: Option<&[Review]>,
    observed_at: Option<OffsetDateTime>,
    prepare_storage: bool,
    ctx: &JoinContext<'_>,
) -> Result<Intake, HiveError> {
    let mut reviewed = reviewed_records::bind(reviews.unwrap_or_default(), inventory)?;
    let mut counts = BTreeMap::from([
        (Surface::Directives, Counts::default()),
        (Surface::ArtifactsStandalone, Counts::default()),
        (Surface::ArtifactsWorkScoped, Counts::default()),
    ]);
    if reviews.is_some() {
        for count in counts.values_mut() {
            count.prepared = Some(0);
        }
    }
    let mut inspected = HashMap::new();
    let mut records = Vec::new();
    let mut directives = Vec::new();
    for (path, entry) in &inventory.entries {
        let Some(size) = entry.size else { continue };
        let (surface, scope, parent, tail) =
            match path.splitn(6, '/').collect::<Vec<_>>().as_slice() {
                ["directives", tail] => (Surface::Directives, None, None, *tail),
                ["directives", scope, ..] => (
                    Surface::Directives,
                    Some(*scope),
                    None,
                    &path["directives/".len() + scope.len() + 1..],
                ),
                ["artifacts", _, ..] => (
                    Surface::ArtifactsStandalone,
                    None,
                    None,
                    &path["artifacts/".len()..],
                ),
                [
                    "tasks",
                    scope,
                    "backlog" | "active" | "archive",
                    work_id,
                    "artifacts",
                    tail,
                ] => {
                    let root_len = path.len() - tail.len() - "/artifacts/".len();
                    (
                        Surface::ArtifactsWorkScoped,
                        Some(*scope),
                        Some((&path[..root_len], *work_id)),
                        *tail,
                    )
                }
                _ => continue,
            };
        let root = &path[..path.len() - tail.len()];
        let review = reviewed.remove(path.as_str());
        let shape = if surface == Surface::Directives {
            directive_shape(inventory, root, tail, scope.is_some())
        } else {
            artifact_shape(inventory, root, tail)
        };

        let mut raw_sha256 = None;
        let mut content = None;
        let outcome = match (shape, scope.and_then(|scope| scopes.get(scope))) {
            (Shape::Excluded(reason), _) => Outcome::Excluded { reason },
            (_, Some(ScopeOutcome::Excluded { reason })) => Outcome::Excluded { reason: *reason },
            (Shape::Record, Some(ScopeOutcome::Held { reason }))
                if parent.is_some() && review.is_some_and(Review::is_work_scoped) =>
            {
                Outcome::Held {
                    reason_codes: vec![*reason, Reason::ParentTaskQuarantined],
                    candidate: None,
                }
            }
            (_, Some(ScopeOutcome::Held { reason })) => Outcome::held(*reason),
            (Shape::Held(reason), _) => Outcome::held(reason),
            (Shape::Record, mapped) => {
                if size > MAX_MARKDOWN_BYTES {
                    Outcome::held(Reason::RecordMarkdownTooLarge)
                } else {
                    let directive = surface == Surface::Directives;
                    let direct_markdown = tail.strip_suffix(".md").is_some_and(is_slug);
                    let reviewed_kind = review
                        .filter(|_| !directive && direct_markdown)
                        .and_then(|review| review.artifact_kind(parent.is_some()));
                    let capture = review.is_some() && (prepare_storage || reviewed_kind.is_some());
                    let key = (entry.oid.as_str(), directive, capture, reviewed_kind);
                    let inspection = match inspected.entry(key) {
                        Entry::Occupied(cached) => cached.into_mut(),
                        Entry::Vacant(cache) => {
                            let bytes =
                                snapshot_object(&|| reader.command(), "blob", &entry.oid, size)?;
                            cache.insert(Inspection {
                                raw_sha256: format!("{:x}", Sha256::digest(&bytes)),
                                record: inspect(&bytes, directive, capture, reviewed_kind),
                            })
                        }
                    };
                    raw_sha256 = Some(inspection.raw_sha256.as_str());
                    match &inspection.record {
                        Err(reason) => Outcome::held(*reason),
                        Ok(record) => {
                            let source_title = record.title.as_deref();
                            let title = match review {
                                Some(review) => review.selected_title(source_title),
                                None => source_title.ok(),
                            };
                            if let Some(title) = title {
                                let outcome = candidate(
                                    record.metadata,
                                    mapped.copied(),
                                    parent.map(|(path, _)| path),
                                    inventory,
                                );
                                match review {
                                    Some(review) => {
                                        let selected = SelectedContent { record, title };
                                        if prepare_storage {
                                            content = selected.for_storage();
                                        }
                                        outcome.with_review(
                                            review,
                                            selected,
                                            (surface == Surface::Directives
                                                && review.directive_edges().is_some())
                                                || direct_markdown,
                                            observed_at,
                                            parent.map(|(path, _)| path),
                                            ctx,
                                        )
                                    }
                                    None => outcome,
                                }
                            } else {
                                Outcome::held(Reason::MissingOrConflictingTitle)
                            }
                        }
                    }
                }
            }
        };
        let legacy_key = if surface == Surface::Directives {
            path.strip_suffix("/README.md")
                .or_else(|| path.strip_suffix(".md"))
                .unwrap_or(path)
        } else {
            path
        };
        if surface == Surface::Directives
            && let Some(review) = review
        {
            directives.push(ReviewedDirective {
                key: legacy_key,
                row: records.len(),
                review,
                prepared: matches!(outcome, Outcome::Prepared { .. }),
            });
        }
        let mut record = json!({
            "surface": surface,
            "source_git_blob_sha": entry.oid,
            "source_raw_sha256": raw_sha256,
            "source_size": size,
            "legacy_key_sha256": format!("{:x}", Sha256::digest(legacy_key)),
        });
        let portable_ancestry = scope.is_none_or(is_portable_scope)
            && parent.is_none_or(|(_, work_id)| is_slug(work_id));
        if portable_ancestry && portable_record_tail(tail) {
            record["source_path"] = json!(path);
        } else {
            record["source_path_sha256"] = json!(format!("{:x}", Sha256::digest(path)));
        }
        match parent {
            None => record["parent_task_path"] = Value::Null,
            Some((path, _)) if portable_ancestry => record["parent_task_path"] = json!(path),
            Some((path, _)) => {
                record["parent_task_path_sha256"] = json!(format!("{:x}", Sha256::digest(path)));
            }
        }
        records.push((surface, record, outcome, content));
    }
    if !reviewed.is_empty() {
        return Err(reviewed_records::invalid(
            "Reviewed records must belong to the Directive or Artifact intake surfaces.",
        ));
    }
    let mut links: HashMap<_, _> = resolve_links(&directives).into_iter().collect();
    let mut prepared = Vec::new();
    let records = records
        .into_iter()
        .enumerate()
        .map(|(index, (surface, mut record, outcome, content))| {
            let outcome = match links.remove(&index) {
                Some(decision) => outcome.with_link(decision),
                None => outcome,
            };
            let count = counts.entry(surface).or_default();
            count.inventoried += 1;
            match outcome {
                Outcome::Held { .. } => count.held += 1,
                Outcome::Excluded { .. } => count.excluded += 1,
                Outcome::Prepared { .. } => *count.prepared.get_or_insert(0) += 1,
            }
            record["outcome"] = json!(outcome);
            if prepare_storage
                && let Outcome::Prepared {
                    target_id,
                    provenance,
                    ..
                } = outcome
            {
                let content = content.ok_or_else(|| {
                    reviewed_records::invalid("Prepared source content is unavailable.")
                })?;
                prepared.push(PreparedRecord {
                    id: target_id,
                    fields: *provenance,
                    content,
                });
            }
            Ok(record)
        })
        .collect::<Result<Vec<_>, HiveError>>()?;
    Ok(Intake {
        records: prepared,
        report: json!({"coverage": "partial", "counts": counts, "records": records}),
    })
}

fn portable_record_tail(tail: &str) -> bool {
    let filename = match tail.rsplit_once('/') {
        Some((directories, filename)) if directories.split('/').all(is_slug) => filename,
        Some(_) => return false,
        None => tail,
    };
    if matches!(
        filename,
        "README.md" | "AGENTS.md" | "SOUL.md" | "MEMORY.md"
    ) {
        return true;
    }
    if let Some(stem) = filename.strip_suffix(".md") {
        return is_slug(stem);
    }
    let mut parts = filename.split('.');
    parts.next().is_some_and(is_slug)
        && parts.all(|extension| {
            !extension.is_empty()
                && extension
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        })
}

fn directive_shape(inventory: &Inventory, root: &str, tail: &str, scoped: bool) -> Shape {
    if tail == "README.md" {
        return Shape::Excluded(Reason::NavigationOnly);
    }
    if !scoped {
        return Shape::Held(Reason::UnsupportedDirectiveShape);
    }
    let (slug, bundle) = match tail.split_once('/') {
        Some((slug, name)) if is_slug(slug) && name == "README.md" => (slug, true),
        Some((slug, member))
            if is_slug(slug)
                && member.ends_with(".md")
                && has_file(inventory, &format!("{root}{slug}/README.md")) =>
        {
            return Shape::Excluded(Reason::DirectiveBundleSupport);
        }
        None => match tail.strip_suffix(".md").filter(|slug| is_slug(slug)) {
            Some(slug) => (slug, false),
            None => return Shape::Held(Reason::UnsupportedDirectiveShape),
        },
        _ => return Shape::Held(Reason::UnsupportedDirectiveShape),
    };
    let alternative = if bundle {
        format!("{root}{slug}.md")
    } else {
        format!("{root}{slug}/README.md")
    };
    if has_file(inventory, &alternative) {
        Shape::Held(Reason::DuplicateLegacyDirective)
    } else {
        Shape::Record
    }
}

fn artifact_shape(inventory: &Inventory, root: &str, tail: &str) -> Shape {
    if tail == "README.md" {
        return Shape::Excluded(Reason::NavigationOnly);
    }
    if !tail.ends_with(".md") {
        return Shape::Held(Reason::UnresolvedBinaryOrExternalOwner);
    }
    match tail.split_once('/') {
        None | Some((_, "README.md")) => Shape::Record,
        Some((bundle, _)) if has_file(inventory, &format!("{root}{bundle}/README.md")) => {
            Shape::Held(Reason::UnresolvedArtifactBundleMember)
        }
        Some(_) => Shape::Held(Reason::UnsupportedArtifactShape),
    }
}

fn has_file(inventory: &Inventory, path: &str) -> bool {
    inventory
        .entries
        .get(path)
        .is_some_and(|entry| !entry.is_directory())
}

fn candidate(
    metadata: Metadata,
    scope: Option<&ScopeOutcome<'_>>,
    parent: Option<&str>,
    inventory: &Inventory,
) -> Outcome {
    let mut reasons = vec![Reason::IncompleteHistoricalProvenance];
    let candidate = match metadata {
        Metadata::Directive(status) => {
            if matches!(status, DirectiveStatus::Active) {
                reasons.push(Reason::MissingDirectiveApproval);
                reasons.push(Reason::UnverifiedDirectiveValidity);
            }
            if matches!(status, DirectiveStatus::Superseded) {
                reasons.push(Reason::UnresolvedSupersessionGraph);
            }
            let area_id = match scope {
                Some(ScopeOutcome::Mapped { area, .. }) => &area.id,
                _ => return Outcome::held(Reason::AmbiguousScopeOrCollision),
            };
            Candidate::Directive {
                status,
                scope_kind: "area",
                area_id: area_id.clone(),
                include_descendants: false,
            }
        }
        Metadata::Artifact {
            kind,
            legacy_status,
        } => {
            reasons.push(Reason::UnverifiedArtifactAuthor);
            reasons.push(Reason::UnverifiedArtifactRetention);
            if let Some(parent) = parent {
                reasons.push(if has_file(inventory, &format!("{parent}/README.md")) {
                    Reason::UnresolvedParentTask
                } else {
                    Reason::MissingParentTaskEntrypoint
                });
            }
            Candidate::Artifact {
                kind,
                legacy_status,
            }
        }
    };
    Outcome::Held {
        reason_codes: reasons,
        candidate: Some(candidate),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Frontmatter {
    description: String,
    #[serde(default, deserialize_with = "present")]
    title: Option<String>,
    #[serde(default, deserialize_with = "present")]
    tags: Option<Vec<String>>,
    #[serde(default, deserialize_with = "present")]
    status: Option<Value>,
    #[serde(default, deserialize_with = "present")]
    kind: Option<Value>,
}

fn scalar_label<T: DeserializeOwned>(value: Value) -> Option<T> {
    // Serde enums also accept externally tagged objects. Only v1 scalar labels map.
    if value.is_string() {
        serde_json::from_value(value).ok()
    } else {
        None
    }
}

fn inspect(
    bytes: &[u8],
    directive: bool,
    capture: bool,
    reviewed_kind: Option<ArtifactKind>,
) -> Result<InspectedRecord, Reason> {
    let invalid = if directive {
        Reason::UnsupportedDirectiveMetadata
    } else {
        Reason::UnsupportedArtifactMetadata
    };
    let nontext = if directive {
        invalid
    } else {
        Reason::UnresolvedBinaryOrExternalOwner
    };
    let text = std::str::from_utf8(bytes).map_err(|_| nontext)?;
    if text.starts_with("version https://git-lfs.github.com/spec/v1")
        || text
            .bytes()
            .any(|byte| byte.is_ascii_control() && !matches!(byte, b'\n' | b'\r' | b'\t'))
    {
        return Err(nontext);
    }

    let (header, body) = split_frontmatter(text).ok_or(invalid)?;
    let fields: Frontmatter =
        parse_v2(header.as_bytes(), "v1 record frontmatter").map_err(|_| invalid)?;
    if fields.description.trim().is_empty() {
        return Err(invalid);
    }
    if let Some(tags) = fields.tags {
        let mut seen = HashSet::new();
        if tags.is_empty()
            || tags
                .iter()
                .any(|tag| tag.trim().is_empty() || !seen.insert(tag.trim()))
        {
            return Err(invalid);
        }
    }
    let metadata = if directive {
        if fields.kind.is_some() {
            return Err(invalid);
        }
        let status = fields
            .status
            .and_then(scalar_label)
            .ok_or(Reason::InvalidDirectiveStatus)?;
        Metadata::Directive(status)
    } else {
        let kind = match fields.kind {
            Some(value) => scalar_label::<ArtifactKind>(value).filter(|kind| kind.is_v1()),
            None => reviewed_kind,
        }
        .ok_or(Reason::UnclassifiedArtifactKind)?;
        let legacy_status = fields
            .status
            .map(|value| scalar_label(value).ok_or(Reason::InvalidArtifactStatus))
            .transpose()?;
        Metadata::Artifact {
            kind,
            legacy_status,
        }
    };
    Ok(InspectedRecord {
        metadata,
        title: select_title(fields.title.as_deref(), &fields.description, body)
            .map(Cow::into_owned),
        body_sha256: format!("{:x}", Sha256::digest(body)),
        body: capture.then(|| Rc::from(body)),
    })
}

fn split_frontmatter(text: &str) -> Option<(&str, &str)> {
    let delimiter = |line| matches!(line, "---\n" | "---\r\n" | "---");
    let mut lines = text.split_inclusive('\n');
    let opening = lines.next().filter(|line| delimiter(line))?;
    let mut offset = opening.len();
    for line in lines {
        if delimiter(line) {
            return Some((&text[opening.len()..offset], &text[offset + line.len()..]));
        }
        offset += line.len();
        if offset - opening.len() > MAX_FILE_BYTES {
            return None;
        }
    }
    None
}

fn select_title<'a>(
    title: Option<&'a str>,
    description: &'a str,
    body: &str,
) -> Result<Cow<'a, str>, TitleProblem> {
    if let Some(title) = title.filter(|value| valid_title(value)) {
        return Ok(Cow::Borrowed(title));
    }
    let mut depth = 0;
    let mut heading = None;
    let mut first_heading = None;
    for event in Parser::new(body) {
        match event {
            Event::Start(tag) => {
                if depth == 0
                    && matches!(
                        tag,
                        Tag::Heading {
                            level: HeadingLevel::H1,
                            ..
                        }
                    )
                {
                    heading = Some(String::new());
                }
                depth += 1;
            }
            Event::End(_) => {
                depth -= 1;
                if depth == 0
                    && let Some(text) = heading.take()
                {
                    let text = text.trim();
                    match first_heading.as_deref() {
                        None => first_heading = Some(text.to_owned()),
                        Some(first) if first != text => return Err(TitleProblem::Conflicting),
                        Some(_) => {}
                    }
                }
            }
            Event::Text(text) | Event::Code(text) => {
                if let Some(heading) = &mut heading {
                    heading.push_str(&text);
                }
            }
            Event::SoftBreak | Event::HardBreak => {
                if let Some(heading) = &mut heading {
                    heading.push(' ');
                }
            }
            _ => {}
        }
    }
    first_heading
        .filter(|text| valid_title(text))
        .map(Cow::Owned)
        .or_else(|| valid_title(description).then_some(Cow::Borrowed(description)))
        .ok_or(if title.is_some() {
            TitleProblem::Conflicting
        } else {
            TitleProblem::Unavailable
        })
}
