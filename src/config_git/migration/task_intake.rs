use std::collections::{BTreeMap, HashMap, HashSet, hash_map::Entry};
use std::process::Command;

use pulldown_cmark::{Event, HeadingLevel, Parser, Tag};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::scope_plan::{ScopeOutcome, is_portable_scope};
use super::task_graph::{Node, held_nodes};
use super::task_provenance::{Key, Prepared, Reviews, SourceBody, TaskReview};
use super::{Inventory, Reason, snapshot_object};
use crate::cli::is_slug;
use crate::config_file::{parse_v2, parse_v2_projection, present};
use crate::error::HiveError;

#[derive(Clone, Copy, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
enum Lifecycle {
    Backlog,
    Active,
    Archive,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum CandidateStatus {
    Backlog,
    Todo,
    Done,
    Cancelled,
}

impl CandidateStatus {
    pub(super) fn is_terminal(self) -> bool {
        matches!(self, Self::Done | Self::Cancelled)
    }

    pub(super) fn relation_hold(self) -> Reason {
        match self {
            Self::Done => Reason::UnresolvedCompletionObligation,
            _ => Reason::UnresolvedTaskRelations,
        }
    }
}

pub(super) struct Record {
    scope: String,
    work_id: String,
    lifecycle: Lifecycle,
    path: String,
    blob_oid: Option<String>,
    blob_size: usize,
    raw_sha256: Option<String>,
    classification: Classification,
}

#[derive(Clone, Copy)]
struct Classification {
    candidate: Result<CandidateStatus, Reason>,
    execution_needs_reconciliation: bool,
}

pub(super) fn read_records(
    inventory: &Inventory,
    git: &impl Fn() -> Result<Command, HiveError>,
    claims_supported: bool,
) -> Result<Vec<Record>, HiveError> {
    let mut records = Vec::new();
    let mut parsed = HashMap::new();
    for (path, entry) in &inventory.entries {
        if !entry.is_directory() {
            continue;
        }
        let mut parts = path.split('/');
        let (Some("tasks"), Some(scope), Some(area), Some(work_id), None) = (
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
        ) else {
            continue;
        };
        let lifecycle = match area {
            "backlog" => Lifecycle::Backlog,
            "active" => Lifecycle::Active,
            "archive" => Lifecycle::Archive,
            _ => continue,
        };
        let readme = format!("{path}/README.md");
        let mut record = Record {
            scope: scope.to_owned(),
            work_id: work_id.to_owned(),
            lifecycle,
            path: path.clone(),
            blob_oid: None,
            blob_size: 0,
            raw_sha256: None,
            classification: Classification {
                candidate: Err(Reason::MissingTaskEntrypoint),
                execution_needs_reconciliation: false,
            },
        };
        if let Some(blob) = inventory.entries.get(&readme)
            && let Some(size) = blob.size
        {
            let (digest, classification) = match parsed.entry((blob.oid.as_str(), lifecycle)) {
                Entry::Occupied(entry) => entry.into_mut(),
                Entry::Vacant(entry) => {
                    let bytes = snapshot_object(git, "blob", &blob.oid, size)?;
                    entry.insert((
                        format!("{:x}", Sha256::digest(&bytes)),
                        classify(&bytes, lifecycle, claims_supported),
                    ))
                }
            };
            record.path = readme;
            record.blob_oid = Some(blob.oid.clone());
            record.blob_size = size;
            record.raw_sha256 = Some(digest.clone());
            record.classification = *classification;
        }
        records.push(record);
    }
    records.sort_unstable_by(|left, right| {
        (&left.scope, &left.work_id, &left.path).cmp(&(&right.scope, &right.work_id, &right.path))
    });
    Ok(records)
}

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum Outcome<'a> {
    Prepared {
        area_id: &'a str,
        candidate_status: CandidateStatus,
        provenance: Box<Prepared>,
    },
    Held {
        reason_codes: Vec<Reason>,
        #[serde(skip_serializing_if = "Option::is_none")]
        candidate_status: Option<CandidateStatus>,
        #[serde(skip_serializing_if = "Option::is_none")]
        area_id: Option<&'a str>,
    },
    Excluded {
        reason: Reason,
    },
}

pub(super) struct TaskPlan<'a> {
    records: Vec<Record>,
    outcomes: Vec<Outcome<'a>>,
}

pub(super) fn prepare<'a>(
    records: Vec<Record>,
    scopes: &BTreeMap<&str, &ScopeOutcome<'a>>,
    task_reviews: &[TaskReview],
    git: &impl Fn() -> Result<Command, HiveError>,
) -> Result<TaskPlan<'a>, HiveError> {
    let mut counts = HashMap::new();
    for record in &records {
        *counts.entry(record.key()).or_insert(0) += 1;
    }
    let reviews = Reviews::new(task_reviews, &counts)?;
    let mut source_bodies = HashMap::new();
    let mut outcomes = Vec::with_capacity(records.len());
    let mut nodes = Vec::with_capacity(records.len());
    for record in &records {
        let scope = scopes[record.scope.as_str()];
        let outcome = record.assess(
            scope,
            counts[&record.key()] == 1,
            &reviews,
            &mut source_bodies,
            git,
        )?;
        let status = match outcome {
            Outcome::Prepared {
                candidate_status, ..
            } => Some(candidate_status),
            _ => None,
        };
        // A verified declaration can veto preparation of another Task even
        // when this Task has no usable Area, metadata, or provenance.
        let source_reviews = match (scope, record.blob_oid.as_deref()) {
            (ScopeOutcome::Excluded { .. }, _) | (_, None) => &[],
            (_, Some(oid)) => reviews.source_bound(record.key(), oid),
        };
        nodes.push(Node {
            key: record.key(),
            source_reviews,
            status,
        });
        outcomes.push(outcome);
    }

    for (outcome, held) in outcomes.iter_mut().zip(held_nodes(&nodes)) {
        if held
            && let Outcome::Prepared {
                area_id,
                candidate_status,
                ..
            } = *outcome
        {
            *outcome = Outcome::Held {
                reason_codes: vec![candidate_status.relation_hold()],
                candidate_status: Some(candidate_status),
                area_id: Some(area_id),
            };
        }
    }
    Ok(TaskPlan { records, outcomes })
}

impl TaskPlan<'_> {
    pub(super) fn report(self) -> Value {
        let mut excluded = 0;
        let mut prepared = 0;
        let tasks: Vec<_> = self
            .records
            .iter()
            .zip(self.outcomes)
            .map(|(record, outcome)| {
                match outcome {
                    Outcome::Prepared { .. } => prepared += 1,
                    Outcome::Excluded { .. } => excluded += 1,
                    Outcome::Held { .. } => {}
                }
                record.report(outcome)
            })
            .collect();
        json!({
            "coverage": "partial",
            "counts": {
                "inventoried": tasks.len(),
                "held": tasks.len() - excluded - prepared,
                "excluded": excluded,
                "prepared": prepared,
            },
            "tasks": tasks,
        })
    }
}

impl Record {
    fn key(&self) -> Key<'_> {
        (&self.scope, &self.work_id)
    }

    fn hold_reasons(&self, scope: &ScopeOutcome<'_>, duplicate: bool) -> Vec<Reason> {
        let mut reasons = Vec::new();
        if duplicate {
            reasons.push(Reason::DuplicateLegacyWorkId);
        }
        if !is_slug(&self.work_id) {
            reasons.push(Reason::InvalidLegacyState);
        }
        if let ScopeOutcome::Held { reason } = scope {
            reasons.push(*reason);
        }
        if let Err(reason) = self.classification.candidate
            && !reasons.contains(&reason)
        {
            reasons.push(reason);
        }
        if self.classification.execution_needs_reconciliation
            && !reasons.contains(&Reason::ExecutionNeedsReconciliation)
        {
            reasons.push(Reason::ExecutionNeedsReconciliation);
        }
        reasons
    }

    fn assess<'a, 'record>(
        &'record self,
        scope: &ScopeOutcome<'a>,
        unique: bool,
        reviews: &Reviews<'_>,
        source_bodies: &mut HashMap<&'record str, SourceBody>,
        git: &impl Fn() -> Result<Command, HiveError>,
    ) -> Result<Outcome<'a>, HiveError> {
        if let ScopeOutcome::Excluded { reason } = scope {
            return Ok(Outcome::Excluded { reason: *reason });
        }
        let reason_codes = self.hold_reasons(scope, !unique);
        let hold = |reason_codes| Outcome::Held {
            reason_codes,
            candidate_status: None,
            area_id: None,
        };
        let area = match scope {
            ScopeOutcome::Mapped { area } if unique && is_slug(&self.work_id) => area,
            _ => return Ok(hold(reason_codes)),
        };
        let binding = self.blob_oid.as_deref().map_or(Ok(None), |oid| {
            reviews.for_preparation(self.key(), oid, &area.id)
        });
        match self.classification.candidate {
            Ok(status) if reason_codes.is_empty() => {
                self.candidate_outcome(status, &area.id, binding, source_bodies, git)
            }
            _ => Ok(hold(reason_codes)),
        }
    }

    fn candidate_outcome<'a, 'record>(
        &'record self,
        status: CandidateStatus,
        area_id: &'a str,
        review: Result<Option<&TaskReview>, Reason>,
        source_bodies: &mut HashMap<&'record str, SourceBody>,
        git: &impl Fn() -> Result<Command, HiveError>,
    ) -> Result<Outcome<'a>, HiveError> {
        let held = |reason_codes| Outcome::Held {
            reason_codes,
            candidate_status: Some(status),
            area_id: Some(area_id),
        };
        match review {
            Ok(Some(review)) => {
                let oid = self.blob_oid.as_deref().expect("classified Task blob");
                let body = match source_bodies.entry(oid) {
                    Entry::Occupied(entry) => entry.into_mut(),
                    Entry::Vacant(entry) => {
                        let bytes = snapshot_object(git, "blob", oid, self.blob_size)?;
                        entry.insert(SourceBody::from_verified_blob(bytes)?)
                    }
                };
                Ok(match review.prepare(body, status) {
                    Ok(provenance) => Outcome::Prepared {
                        area_id,
                        candidate_status: status,
                        provenance: Box::new(provenance),
                    },
                    Err(reasons) => held(reasons),
                })
            }
            Ok(None) => {
                let mut reasons = vec![
                    Reason::IncompleteHistoricalProvenance,
                    status.relation_hold(),
                ];
                if status == CandidateStatus::Todo {
                    reasons.push(Reason::UnverifiedAcceptanceCriteria);
                }
                Ok(held(reasons))
            }
            Err(reason) => Ok(held(vec![reason])),
        }
    }

    fn report(&self, outcome: Outcome<'_>) -> Value {
        let mut task = json!({
            "lifecycle": self.lifecycle,
            "source_git_blob_sha": self.blob_oid,
            "source_raw_sha256": self.raw_sha256,
            "outcome": outcome,
        });
        if is_portable_scope(&self.scope) && is_slug(&self.work_id) {
            task["legacy_key"] = json!({"scope": self.scope, "work_id": self.work_id});
            task["source_path"] = json!(self.path);
        } else {
            task["source_path_sha256"] = json!(format!("{:x}", Sha256::digest(&self.path)));
        }
        task
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Frontmatter {
    pub(super) description: String,
    #[serde(default, deserialize_with = "present")]
    pub(super) title: Option<String>,
    #[serde(default, deserialize_with = "present")]
    status: Option<Value>,
    #[serde(default, deserialize_with = "present")]
    claim: Option<Value>,
    #[serde(default, deserialize_with = "present")]
    pub(super) priority: Option<Value>,
    #[serde(default, deserialize_with = "present")]
    pub(super) tags: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct ExecutionFields {
    #[serde(default, deserialize_with = "present")]
    status: Option<String>,
    #[serde(default, deserialize_with = "present")]
    claim: Option<Claim>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Claim {
    id: String,
    holder: String,
}

fn classify(bytes: &[u8], lifecycle: Lifecycle, claims_supported: bool) -> Classification {
    let mut classification = Classification {
        candidate: Err(Reason::UnsupportedTaskMetadata),
        execution_needs_reconciliation: false,
    };
    let Some((header, body)) = split_record(bytes) else {
        return classification;
    };
    let Ok(metadata) = parse_v2::<Frontmatter>(header, "v1 Task frontmatter") else {
        // A malformed unrelated field must not erase independently readable
        // execution evidence. The typed projection rejects duplicate execution fields.
        classification.execution_needs_reconciliation = lifecycle == Lifecycle::Active
            && parse_v2_projection::<ExecutionFields>(header, "v1 Task execution metadata")
                .is_ok_and(|fields| {
                    classify_state(
                        Ok(fields.status.as_deref()),
                        Ok(fields.claim),
                        lifecycle,
                        claims_supported,
                    )
                    .is_err_and(|reason| reason == Reason::ExecutionNeedsReconciliation)
                });
        return classification;
    };
    let status = metadata
        .status
        .as_ref()
        .map(|status| status.as_str().ok_or(Reason::InvalidLegacyState))
        .transpose();
    let claim = metadata
        .claim
        .as_ref()
        .map(Claim::deserialize)
        .transpose()
        .map_err(|_| Reason::InvalidLegacyState);
    let state = classify_state(status, claim, lifecycle, claims_supported);
    classification.execution_needs_reconciliation =
        state.is_err_and(|reason| reason == Reason::ExecutionNeedsReconciliation);
    if !metadata.description.trim().is_empty() {
        classification.candidate = if state.is_ok() || classification.execution_needs_reconciliation
        {
            validate_metadata(&metadata, body, lifecycle).and(state)
        } else {
            state
        };
    }
    classification
}

fn classify_state(
    status: Result<Option<&str>, Reason>,
    claim: Result<Option<Claim>, Reason>,
    lifecycle: Lifecycle,
    claims_supported: bool,
) -> Result<CandidateStatus, Reason> {
    let status = status?;
    let claim = claim?;
    let claimed = claim.is_some();
    if let Some(claim) = claim
        && (!claims_supported || claim.id.trim().is_empty() || claim.holder.trim().is_empty())
    {
        return Err(Reason::InvalidLegacyState);
    }
    let candidate = match (lifecycle, status, claimed) {
        (Lifecycle::Backlog, None, false) => CandidateStatus::Backlog,
        (Lifecycle::Active, Some("todo"), false) => CandidateStatus::Todo,
        (Lifecycle::Active | Lifecycle::Archive, Some("done"), false) => CandidateStatus::Done,
        (Lifecycle::Archive, Some("cancelled"), false) => CandidateStatus::Cancelled,
        (Lifecycle::Active, Some("in-progress"), false) if claims_supported => {
            return Err(Reason::InvalidLegacyState);
        }
        (Lifecycle::Active, Some("in-progress"), _)
        | (Lifecycle::Active, Some("todo" | "review"), true) => {
            return Err(Reason::ExecutionNeedsReconciliation);
        }
        (Lifecycle::Active, Some("review"), false) => {
            return Err(Reason::MissingTypedSubmissionEvidence);
        }
        _ => return Err(Reason::InvalidLegacyState),
    };
    Ok(candidate)
}

fn validate_metadata(
    metadata: &Frontmatter,
    body: &str,
    lifecycle: Lifecycle,
) -> Result<(), Reason> {
    if metadata.priority.as_ref().is_some_and(|priority| {
        lifecycle == Lifecycle::Archive
            || !matches!(priority.as_str(), Some("high" | "normal" | "low"))
    }) {
        return Err(Reason::InvalidLegacyState);
    }
    if let Some(tags) = &metadata.tags {
        let mut trimmed = HashSet::new();
        if tags.is_empty()
            || tags.iter().any(|tag| {
                let tag = tag.trim();
                tag.is_empty()
                    || tag.chars().count() > 100
                    || tag.chars().any(char::is_control)
                    || !trimmed.insert(tag)
            })
        {
            return Err(Reason::UnsupportedTaskMetadata);
        }
    }
    if resolve_title(metadata.title.as_deref(), &metadata.description, body).is_none() {
        return Err(Reason::MissingOrConflictingTitle);
    }
    Ok(())
}

fn split_record(bytes: &[u8]) -> Option<(&[u8], &str)> {
    let text = std::str::from_utf8(bytes).ok()?;
    let mut lines = text.split_inclusive('\n');
    let delimiter = |line: &str| matches!(line, "---\n" | "---\r\n" | "---");
    let start = lines.next().filter(|line| delimiter(line))?.len();
    let mut end = start;
    for line in lines {
        if delimiter(line) {
            return Some((&bytes[start..end], &text[end + line.len()..]));
        }
        end += line.len();
        if end - start > crate::config_file::MAX_FILE_BYTES {
            return None;
        }
    }
    None
}

pub(super) fn parse_record(text: &str) -> Option<(Frontmatter, &str)> {
    let (header, body) = split_record(text.as_bytes())?;
    let metadata: Frontmatter = parse_v2(header, "v1 Task frontmatter").ok()?;
    (!metadata.description.trim().is_empty()).then_some((metadata, body))
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum TitleSource {
    FrontmatterTitle,
    Heading,
    Description,
}

pub(super) fn resolve_title<'a>(
    title: Option<&'a str>,
    description: &'a str,
    body: &'a str,
) -> Option<(&'a str, TitleSource)> {
    let valid = |value: &str| {
        !value.trim().is_empty()
            && value.chars().count() <= 255
            && !value.chars().any(char::is_control)
    };
    if let Some(title) = title.filter(|title| valid(title)) {
        return Some((title, TitleSource::FrontmatterTitle));
    }
    let is_h1 = |event: &Event<'_>| {
        matches!(
            event,
            Event::Start(Tag::Heading {
                level: HeadingLevel::H1,
                ..
            })
        )
    };
    let mut events = Parser::new(body).into_offset_iter();
    let first_heading = events.next().filter(|(event, _)| is_h1(event));
    if events.any(|(event, _)| is_h1(&event)) {
        return None;
    }
    let heading = if let Some((_, range)) = first_heading {
        // Only a single leading unindented ATX H1 can supply the title.
        // Reference definitions count as blocks but emit no parser events.
        if (range.start > 0 && !matches!(body.as_bytes()[range.start - 1], b'\r' | b'\n'))
            || events
                .reference_definitions()
                .iter()
                .any(|(_, definition)| definition.span.start < range.start)
        {
            return None;
        }
        let line = body[range].split(['\r', '\n']).next()?.strip_prefix('#')?;
        if !line.is_empty() && !line.starts_with([' ', '\t']) {
            return None;
        }
        let value = line.trim();
        let unclosed = value.trim_end_matches('#');
        Some(if unclosed.is_empty() || unclosed.ends_with([' ', '\t']) {
            unclosed.trim_end()
        } else {
            value
        })
    } else {
        None
    };
    heading
        .filter(|heading| valid(heading))
        .map(|heading| (heading, TitleSource::Heading))
        .or_else(|| valid(description).then_some((description, TitleSource::Description)))
}
