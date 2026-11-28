use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::ops::Range;
use std::path::Path;

use pulldown_cmark::{CowStr, Event, LinkType, Parser, Tag, TagEnd};
use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use url::{Host, Url};

use super::json_object::{self, Object};
use super::queen_assets::{self, SoulReview};
use super::queen_preparation::{Preparation, PreparedQueen, QueenMapping};
use super::reviewed_records::Review;
use super::task_provenance::TaskReview;
use super::{
    AssetObjectReader, Inventory, Reason, SourceSnapshot, directive_artifact_intake,
    is_portable_bot_memory_locator, snapshot_object, task_intake, verified_source,
};
use crate::cli::is_slug;
use crate::config_file::{MAX_FILE_BYTES, read_regular};
use crate::dolt::{is_uuid, is_uuid_v7};
use crate::error::{ErrorCode, HiveError};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Mapping {
    version: u32,
    source_commit: String,
    policy_pin: String,
    workspace_id: String,
    #[serde(deserialize_with = "json_object::deserialize_vec")]
    scopes: Vec<ScopeMapping>,
    #[serde(default, deserialize_with = "json_object::deserialize_vec")]
    task_reviews: Vec<TaskReview>,
    #[serde(default, deserialize_with = "optional_object_list")]
    reviewed_records: Option<Vec<Review>>,
    #[serde(default, deserialize_with = "optional_object_list")]
    queen_assets: Option<Vec<SoulReview>>,
    #[serde(default, deserialize_with = "optional_object_list")]
    queen_preparation: Option<Vec<QueenMapping>>,
}

fn optional_object_list<'de, D, T>(deserializer: D) -> Result<Option<Vec<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    json_object::deserialize_vec(deserializer).map(Some)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScopeMapping {
    scope: String,
    #[serde(deserialize_with = "json_object::deserialize")]
    decision: Decision,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Decision {
    Area {
        #[serde(deserialize_with = "json_object::deserialize")]
        area: Area,
        #[serde(default, deserialize_with = "json_object::deserialize_vec")]
        project_evidence: Vec<ProjectEvidence>,
    },
    External {},
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Area {
    pub(crate) id: String,
    pub(crate) slug: String,
    pub(crate) name: String,
    pub(crate) parent_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectEvidence {
    path: String,
    canonical_ref: String,
}

struct Scope<'a> {
    scope: &'a str,
    source_paths: Vec<&'a str>,
    outcome: ScopeOutcome<'a>,
}

pub(super) enum ScopeOutcome<'a> {
    Mapped { area: &'a Area },
    Held { reason: Reason },
    Excluded { reason: Reason },
}

impl Scope<'_> {
    fn report(&self) -> Value {
        let outcome = match self.outcome {
            ScopeOutcome::Mapped { area } => json!({
                "status": "mapped",
                "area": {"id": area.id, "slug": area.slug, "parent_id": area.parent_id},
            }),
            ScopeOutcome::Held { reason } => json!({"status": "held", "reason": reason}),
            ScopeOutcome::Excluded { reason } => json!({"status": "excluded", "reason": reason}),
        };
        let portable_scope = is_portable_scope(self.scope);
        let (paths, redacted): (Vec<_>, Vec<_>) = self
            .source_paths
            .iter()
            .copied()
            .partition(|path| portable_scope && has_portable_memory_locator(path));
        let mut report = json!({"source_paths": paths, "outcome": outcome});
        if portable_scope {
            report["scope"] = json!(self.scope);
        } else {
            report["scope_sha256"] = json!(format!("{:x}", Sha256::digest(self.scope)));
        }
        if !redacted.is_empty() {
            report["source_path_sha256s"] = json!(
                redacted
                    .into_iter()
                    .map(|path| format!("{:x}", Sha256::digest(path)))
                    .collect::<Vec<_>>()
            );
        }
        report
    }
}

pub(crate) fn plan(
    source: &Path,
    revision: &str,
    workspace_id: &str,
    mapping_file: &Path,
    include_tasks: bool,
    include_directives_artifacts: bool,
) -> Result<Value, HiveError> {
    Ok(prepare_plan(
        source,
        revision,
        workspace_id,
        mapping_file,
        include_tasks,
        include_directives_artifacts,
    )?
    .report)
}

struct MigrationPlan {
    report: Value,
    #[expect(dead_code, reason = "reserved for the later inactive staging step")]
    queen_souls: Vec<PreparedQueen>,
}

fn prepare_plan(
    source: &Path,
    revision: &str,
    workspace_id: &str,
    mapping_file: &Path,
    include_tasks: bool,
    include_directives_artifacts: bool,
) -> Result<MigrationPlan, HiveError> {
    with_scope_mapping(source, revision, workspace_id, mapping_file, |reviewed| {
        if reviewed.reviewed_records.is_some() && !include_directives_artifacts {
            return Err(invalid(
                "Reviewed records require --include-directives-artifacts.",
            ));
        }
        let mut report = json!({
            "mode": "scope_plan",
            "mapping_version": reviewed.mapping_version,
            "mapping_sha256": reviewed.mapping_sha256,
            "source": reviewed.source,
            "inventory": reviewed.inventory.summary,
            "target": {"workspace_id": workspace_id},
            "scopes": reviewed.scopes.iter().map(Scope::report).collect::<Vec<_>>(),
        });
        if include_tasks || include_directives_artifacts {
            let outcomes = reviewed
                .scopes
                .iter()
                .map(|scope| (scope.scope, &scope.outcome))
                .collect();
            if include_tasks {
                // Verified v1 coordination exists only under the pin that defines claims.
                let claims_supported = !reviewed.source.hive_yml.is_null();
                let records = task_intake::read_records(
                    reviewed.inventory,
                    &|| reviewed.reader.command(),
                    claims_supported,
                )?;
                report["task_intake"] =
                    task_intake::prepare(records, &outcomes, reviewed.task_reviews, &|| {
                        reviewed.reader.command()
                    })?
                    .report();
            }
            if include_directives_artifacts {
                report["directive_artifact_intake"] = directive_artifact_intake::report(
                    reviewed.inventory,
                    reviewed.reader,
                    &outcomes,
                    reviewed.reviewed_records,
                )?;
            }
        }
        let mut preparation = reviewed
            .queen_preparation
            .map(Preparation::new)
            .transpose()?;
        if reviewed.queen_assets.is_some() || preparation.is_some() {
            report["queen_assets"] = json!(queen_assets::classify(
                reviewed.inventory,
                reviewed.reader,
                reviewed.queen_assets.unwrap_or_default(),
                |slug, oid, digest, bytes| {
                    if let Some(preparation) = &mut preparation {
                        preparation.accept_candidate(slug, oid, digest, bytes);
                    }
                },
            )?);
        }
        let queen_souls = if let Some(preparation) = preparation {
            let (summary, souls) = preparation.finish();
            report["queen_preparation"] = summary;
            souls
        } else {
            Vec::new()
        };
        Ok(MigrationPlan {
            report,
            queen_souls,
        })
    })
}

pub(crate) struct ReviewedScopes<'a> {
    source: &'a SourceSnapshot<'a>,
    inventory: &'a Inventory,
    reader: &'a AssetObjectReader<'a>,
    mapping_version: u32,
    mapping_sha256: String,
    scopes: Vec<Scope<'a>>,
    queen_assets: Option<&'a [SoulReview]>,
    queen_preparation: Option<&'a [QueenMapping]>,
    task_reviews: &'a [TaskReview],
    reviewed_records: Option<&'a [Review]>,
}

impl ReviewedScopes<'_> {
    pub(crate) fn areas_in_parent_order(&self) -> Result<Vec<&Area>, HiveError> {
        if !self.task_reviews.is_empty()
            || self
                .reviewed_records
                .is_some_and(|records| !records.is_empty())
        {
            return Err(invalid(
                "Area staging requires a mapping without Task, Directive, or Artifact reviews.",
            ));
        }
        if self
            .queen_preparation
            .is_some_and(|reviews| !reviews.is_empty())
        {
            return Err(invalid(
                "Area staging requires a mapping without Queen preparation reviews.",
            ));
        }
        let mut ids = BTreeSet::new();
        let mut children = BTreeMap::<Option<&str>, Vec<&Area>>::new();
        for scope in &self.scopes {
            if let ScopeOutcome::Mapped { area } = scope.outcome {
                ids.insert(area.id.as_str());
                children
                    .entry(area.parent_id.as_deref())
                    .or_default()
                    .push(area);
            }
        }
        for parent in children.keys().flatten() {
            if !ids.contains(parent) {
                return Err(HiveError::new(
                    ErrorCode::InvalidRecord,
                    format!(
                        "Mapped Area parent {parent} is missing or held. Review the scope mapping."
                    ),
                ));
            }
        }

        let mut pending = VecDeque::from(children.remove(&None).unwrap_or_default());
        let mut ordered = Vec::with_capacity(ids.len());
        while let Some(area) = pending.pop_front() {
            ordered.push(area);
            if let Some(descendants) = children.remove(&Some(area.id.as_str())) {
                pending.extend(descendants);
            }
        }
        if !children.is_empty() {
            return Err(HiveError::new(
                ErrorCode::InvalidRecord,
                "Mapped Area ancestry contains a cycle. Review the scope mapping.",
            ));
        }
        Ok(ordered)
    }

    pub(crate) fn staging_evidence(&self) -> Value {
        let mut id_map = Vec::new();
        let mut holds = Vec::new();
        let mut exclusions = Vec::new();
        let hash = |value: &str| format!("{:x}", Sha256::digest(value));
        for scope in &self.scopes {
            let scope_sha256 = hash(scope.scope);
            let source_path_sha256s = scope
                .source_paths
                .iter()
                .map(|path| hash(path))
                .collect::<Vec<_>>();
            let mut entry =
                json!({"scope_sha256": scope_sha256, "source_path_sha256s": source_path_sha256s});
            match scope.outcome {
                ScopeOutcome::Mapped { area } => {
                    entry["area_id"] = json!(area.id);
                    entry["parent_area_id"] = json!(area.parent_id);
                    id_map.push(entry);
                }
                ScopeOutcome::Held { reason } => {
                    entry["reason_code"] = json!(reason);
                    holds.push(entry);
                }
                ScopeOutcome::Excluded { reason } => {
                    entry["reason_code"] = json!(reason);
                    exclusions.push(entry);
                }
            }
        }
        json!({
            "coverage": "scope_only",
            "mapping_version": self.mapping_version,
            "mapping_sha256": self.mapping_sha256,
            "source": self.source,
            "inventory": self.inventory.summary,
            "counts": {"scopes": {
                "inventoried": self.scopes.len(), "imported": id_map.len(), "held": holds.len(),
                "excluded_non_record": exclusions.len(),
            }},
            "id_map": id_map,
            "holds": holds,
            "exclusions": exclusions,
        })
    }
}

pub(crate) fn with_scope_mapping<T>(
    source: &Path,
    revision: &str,
    workspace_id: &str,
    mapping_file: &Path,
    inspect: impl FnOnce(ReviewedScopes<'_>) -> Result<T, HiveError>,
) -> Result<T, HiveError> {
    if !is_uuid(workspace_id) {
        return Err(HiveError::new(
            ErrorCode::InvalidArgument,
            "Select an exact lowercase --workspace-id UUID.",
        ));
    }
    let input = read_regular(mapping_file)
        .map_err(|error| HiveError::new(error.code, "Could not read the scope mapping file."))?
        .ok_or_else(|| invalid("The scope mapping file is missing."))?;
    let Object(mapping) = serde_json::from_slice::<Object<Mapping>>(&input)
        .map_err(|_| invalid("The scope mapping must match the versioned JSON format."))?;
    if mapping.version != 1 {
        return Err(invalid("The scope mapping version is unsupported."));
    }

    verified_source(source, revision, |source, inventory, reader| {
        if mapping.source_commit != source.source_commit
            || mapping.policy_pin != source.policy_pin
            || mapping.workspace_id != workspace_id
        {
            return Err(invalid(
                "The scope mapping must match the verified source commit, policy pin, and selected workspace ID.",
            ));
        }

        let scopes = classify_scopes(inventory, &mapping.scopes, reader)?;
        inspect(ReviewedScopes {
            source,
            inventory,
            reader,
            mapping_version: mapping.version,
            mapping_sha256: format!("{:x}", Sha256::digest(input)),
            scopes,
            queen_assets: mapping.queen_assets.as_deref(),
            queen_preparation: mapping.queen_preparation.as_deref(),
            task_reviews: &mapping.task_reviews,
            reviewed_records: mapping.reviewed_records.as_deref(),
        })
    })
}

fn classify_scopes<'a>(
    inventory: &'a Inventory,
    mappings: &'a [ScopeMapping],
    reader: &AssetObjectReader<'_>,
) -> Result<Vec<Scope<'a>>, HiveError> {
    let mut scopes = BTreeMap::<&str, Vec<(&str, &str)>>::new();
    for (path, entry) in &inventory.entries {
        if !entry.is_directory() {
            continue;
        }
        let components: Vec<_> = path.split('/').take(5).collect();
        let (scope, entrypoint) = match components.as_slice() {
            ["tasks" | "directives", scope] => (*scope, "README.md"),
            ["bots", _, "memory", scope] => (*scope, "MEMORY.md"),
            _ => continue,
        };
        scopes.entry(scope).or_default().push((path, entrypoint));
    }

    let mut decisions = HashMap::<&str, Vec<&Decision>>::new();
    let mut ids = HashMap::new();
    let mut siblings = HashMap::new();
    for mapping in mappings {
        let scope = mapping.scope.as_str();
        if !is_portable_scope(scope) || !scopes.contains_key(scope) {
            return Err(invalid(
                "A scope mapping key does not name a supported scope in the verified source.",
            ));
        }
        if let Decision::Area {
            area,
            project_evidence,
        } = &mapping.decision
        {
            if !is_uuid_v7(&area.id)
                || area
                    .parent_id
                    .as_ref()
                    .is_some_and(|id| !is_uuid_v7(id) || *id == area.id)
                || area.slug.len() > 100
                || !is_slug(&area.slug)
                || area.name.trim().is_empty()
                || area.name.chars().count() > 255
                || area.name.chars().any(char::is_control)
                || (reserved(scope) && !project_evidence.is_empty())
            {
                return Err(invalid(
                    "A scope mapping has invalid Area or project evidence fields.",
                ));
            }
            if scope != "_hive" {
                *ids.entry(area.id.as_str()).or_insert(0) += 1;
                *siblings
                    .entry((area.parent_id.as_deref(), area.slug.as_str()))
                    .or_insert(0) += 1;
            }
        }
        decisions.entry(scope).or_default().push(&mapping.decision);
    }

    let mut references = HashMap::new();
    scopes
        .into_iter()
        .map(|(scope, origins)| {
            let decision = decisions.get(scope).map(Vec::as_slice);
            let outcome = match (scope, decision) {
                ("_hive", _) => ScopeOutcome::Held {
                    reason: Reason::UnsplitLegacyScope,
                },
                (_, Some([Decision::External {}])) if !reserved(scope) => ScopeOutcome::Excluded {
                    reason: Reason::ExternalBackendOwned,
                },
                (
                    _,
                    Some(
                        [
                            Decision::Area {
                                area,
                                project_evidence,
                            },
                        ],
                    ),
                ) if ids[area.id.as_str()] == 1
                    && siblings[&(area.parent_id.as_deref(), area.slug.as_str())] == 1
                    && origins
                        .iter()
                        .all(|(path, _)| has_portable_memory_locator(path))
                    && (reserved(scope)
                        || project_agrees(
                            inventory,
                            &origins,
                            project_evidence,
                            reader,
                            &mut references,
                        )?) =>
                {
                    ScopeOutcome::Mapped { area }
                }
                _ => ScopeOutcome::Held {
                    reason: Reason::AmbiguousScopeOrCollision,
                },
            };
            Ok(Scope {
                scope,
                source_paths: origins.into_iter().map(|(path, _)| path).collect(),
                outcome,
            })
        })
        .collect()
}

fn project_agrees<'a>(
    inventory: &'a Inventory,
    origins: &[(&str, &str)],
    evidence: &[ProjectEvidence],
    reader: &AssetObjectReader<'_>,
    references: &mut HashMap<&'a str, Option<String>>,
) -> Result<bool, HiveError> {
    let Some(first) = evidence.first() else {
        return Ok(false);
    };
    let paths: BTreeSet<_> = evidence.iter().map(|item| item.path.as_str()).collect();
    if evidence.len() != origins.len()
        || paths.len() != origins.len()
        || !portable_reference(&first.canonical_ref)
        || evidence
            .iter()
            .any(|item| item.canonical_ref != first.canonical_ref)
    {
        return Ok(false);
    }
    for (directory, entrypoint) in origins {
        let path = format!("{directory}/{entrypoint}");
        if !paths.contains(path.as_str()) {
            return Ok(false);
        }
        let Some(entry) = inventory.entries.get(&path) else {
            return Ok(false);
        };
        let Some(size) = entry.size else {
            return Ok(false);
        };
        let reference = match references.entry(entry.oid.as_str()) {
            std::collections::hash_map::Entry::Occupied(cached) => cached.into_mut(),
            std::collections::hash_map::Entry::Vacant(cache) => {
                // Read by catalog object ID inside the same preflight checks.
                let blob = snapshot_object(&|| reader.command(), "blob", &entry.oid, size)?;
                cache.insert(entrypoint_reference(&blob).map(str::to_owned))
            }
        };
        if reference.as_deref() != Some(first.canonical_ref.as_str()) {
            return Ok(false);
        }
    }
    Ok(true)
}

fn entrypoint_reference(bytes: &[u8]) -> Option<&str> {
    if bytes.len() > MAX_FILE_BYTES {
        return None;
    }
    let mut body = std::str::from_utf8(bytes).ok()?;
    if let Some(header) = body
        .strip_prefix("---\n")
        .or_else(|| body.strip_prefix("---\r\n"))
    {
        let mut offset = 0;
        header.split_inclusive('\n').find(|line| {
            offset += line.len();
            line.trim_end_matches(['\r', '\n']) == "---"
        })?;
        body = &header[offset..];
    }

    let events: Vec<_> = Parser::new(body).into_offset_iter().collect();
    let html_blocks = closed_inline_html_blocks(&events)?;
    let mut skipped = 0;
    let mut reference = None;
    for (event, range) in events {
        if skipped > 0 {
            match event {
                Event::Start(_) => skipped += 1,
                Event::End(_) => skipped -= 1,
                _ => {}
            }
            continue;
        }
        match event {
            Event::Start(Tag::Paragraph | Tag::Heading { .. } | Tag::Item)
                if html_blocks.contains(&range) =>
            {
                skipped = 1;
            }
            Event::Start(Tag::CodeBlock(_) | Tag::Image { .. }) => skipped = 1,
            Event::Start(Tag::HtmlBlock) => {
                if !closed_raw_html_block(&body[range]) {
                    return None;
                }
                skipped = 1;
            }
            Event::Start(Tag::Link {
                link_type,
                dest_url,
                ..
            }) => {
                skipped = 1;
                if dest_url.contains("://") {
                    if !matches!(link_type, LinkType::Inline | LinkType::Autolink) {
                        return None;
                    }
                    if link_type == LinkType::Autolink
                        && unresolved_link_destination(&body[..range.start])
                    {
                        return None;
                    }
                    let CowStr::Borrowed(destination) = dest_url else {
                        return None;
                    };
                    agree(&mut reference, destination)?;
                }
            }
            Event::Text(_) => {
                let mut cursor = range.start;
                for token in body[range.clone()].split_whitespace() {
                    let start = cursor + body[cursor..range.end].find(token)?;
                    cursor = start + token.len();
                    if !token.contains("://") {
                        continue;
                    }
                    // Markdown can split malformed syntax or escapes across text events.
                    // Require a complete raw token outside unresolved link syntax.
                    let before = &body[..start];
                    if before
                        .chars()
                        .next_back()
                        .is_some_and(|ch| !ch.is_whitespace())
                        || body[cursor..]
                            .chars()
                            .next()
                            .is_some_and(|ch| !ch.is_whitespace())
                        || unresolved_link_destination(before)
                    {
                        return None;
                    }
                    agree(&mut reference, prose_url(token))?;
                }
            }
            _ => {}
        }
    }
    reference
}

fn unresolved_link_destination(before: &str) -> bool {
    before
        .trim_end()
        .strip_suffix('(')
        .is_some_and(|prefix| prefix.trim_end().ends_with(']'))
}

pub(super) fn closed_raw_html_block(raw: &str) -> bool {
    let raw = raw.trim();
    if raw.starts_with("<!--") {
        let mut remaining = raw;
        while let Some(comment) = remaining.strip_prefix("<!--") {
            let Some((body, rest)) = comment.split_once("-->") else {
                return false;
            };
            if comment.starts_with('>') || comment.starts_with("->") || body.contains("--!>") {
                return false;
            }
            remaining = rest.trim_start();
        }
        return remaining.is_empty();
    }

    // An unknown block can enclose subsequent Markdown even when the parser
    // emits that prose separately. Only skip a known self-contained wrapper.
    raw.strip_prefix("<div>")
        .or_else(|| raw.strip_prefix("<div hidden>"))
        .and_then(|contents| contents.strip_suffix("</div>"))
        .is_some_and(|contents| !contents.contains('<'))
}

fn closed_inline_html_blocks(
    events: &[(Event<'_>, Range<usize>)],
) -> Option<HashSet<Range<usize>>> {
    let mut prose = Vec::new();
    let mut html_blocks = HashSet::new();
    let mut image_depth = 0;
    let mut expected_close = None;
    for (event, range) in events {
        // A raw pair must close before crossing any Markdown element boundary.
        if expected_close.is_some() && matches!(event, Event::Start(_) | Event::End(_)) {
            return None;
        }
        match event {
            Event::Start(Tag::Image { .. }) => image_depth += 1,
            Event::End(TagEnd::Image) => image_depth -= 1,
            Event::Start(Tag::Paragraph | Tag::Heading { .. } | Tag::Item) => prose.push(range),
            Event::End(TagEnd::Paragraph | TagEnd::Heading(_) | TagEnd::Item) => {
                prose.pop();
            }
            Event::InlineHtml(html) if image_depth == 0 => {
                if html.starts_with("<!--") {
                    if !closed_raw_html_block(html) {
                        return None;
                    }
                } else if let Some(close) = expected_close.take() {
                    if html.as_ref() != close {
                        return None;
                    }
                } else {
                    let &block = prose.last()?;
                    expected_close = Some(match html.as_ref() {
                        "<code>" => "</code>",
                        "<span hidden>" => "</span>",
                        _ => return None,
                    });
                    html_blocks.insert(block.clone());
                }
            }
            _ => {}
        }
    }
    expected_close.is_none().then_some(html_blocks)
}

fn agree<'a>(reference: &mut Option<&'a str>, candidate: &'a str) -> Option<()> {
    match *reference {
        None if portable_reference(candidate) => *reference = Some(candidate),
        Some(previous) if previous == candidate => {}
        _ => return None,
    }
    Some(())
}

fn prose_url(token: &str) -> &str {
    let token = token.strip_suffix('.').unwrap_or(token);
    for (open, close) in [('(', ')'), ('"', '"'), ('\'', '\'')] {
        if let Some(target) = token
            .strip_prefix(open)
            .and_then(|target| target.strip_suffix(close))
        {
            return target;
        }
    }
    token
}

pub(super) fn portable_reference(reference: &str) -> bool {
    if reference.len() > MAX_FILE_BYTES
        || !reference.starts_with("https://")
        || !reference.is_ascii()
        || reference.contains('@')
        || reference.matches("://").count() != 1
    {
        return false;
    }
    let bytes = reference.as_bytes();
    if bytes.iter().enumerate().any(|(index, byte)| {
        *byte == b'%'
            && !bytes
                .get(index + 1..index + 3)
                .is_some_and(|escape| escape.iter().all(u8::is_ascii_hexdigit))
    }) {
        return false;
    }
    let Ok(url) = Url::parse(reference) else {
        return false;
    };
    let Some(Host::Domain(host)) = url.host() else {
        return false;
    };
    url.as_str() == reference
        && url.query().is_none()
        && url.fragment().is_none()
        && url.path() != "/"
        && host.len() <= 253
        && host.contains('.')
        && host.split('.').all(|label| {
            label.len() <= 63
                && label.starts_with(|c: char| c.is_ascii_alphanumeric())
                && label.ends_with(|c: char| c.is_ascii_alphanumeric())
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
        && ![".local", ".localhost", ".internal", ".lan", ".home.arpa"]
            .iter()
            .any(|suffix| host.ends_with(suffix))
}

fn reserved(scope: &str) -> bool {
    matches!(scope, "_general" | "_hive-userdata")
}

pub(super) fn is_portable_scope(scope: &str) -> bool {
    reserved(scope) || scope == "_hive" || is_slug(scope)
}

fn has_portable_memory_locator(path: &str) -> bool {
    path.strip_prefix("bots/").is_none_or(|relative| {
        relative
            .split_once("/memory/")
            .is_some_and(|(bot, scope)| is_portable_bot_memory_locator(bot, Some(scope)))
    })
}

fn invalid(message: &'static str) -> HiveError {
    HiveError::new(ErrorCode::InvalidConfig, message)
}

#[cfg(test)]
mod tests {
    use super::portable_reference;

    #[test]
    fn portable_reference_enforces_dns_length_limits() {
        let label = "a".repeat(63);
        let long_label = "b".repeat(64);
        let max_host = format!("{label}.{label}.{label}.{}", "c".repeat(61));
        assert_eq!(max_host.len(), 253);
        for (host, expected) in [
            (format!("{label}.example"), true),
            (format!("{long_label}.example"), false),
            (max_host.clone(), true),
            (format!("{max_host}d"), false),
            ("forge.example.".into(), false),
        ] {
            assert_eq!(
                portable_reference(&format!("https://{host}/app")),
                expected,
                "{host}"
            );
        }
    }
}
