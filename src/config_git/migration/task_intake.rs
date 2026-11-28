use std::collections::{BTreeMap, HashMap, HashSet, hash_map::Entry};
use std::process::Command;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::scope_plan::{ScopeOutcome, is_portable_scope};
use super::task_graph::{Node, held_nodes};
use super::task_provenance::{Key, Prepared, Relations, Reviews, SourceBody, TaskReview};
use super::{Inventory, Reason, snapshot_object};
use crate::cli::is_slug;
use crate::config_file::parse_v2;
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
    classification: Result<CandidateStatus, Reason>,
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
            classification: Err(Reason::MissingTaskEntrypoint),
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
        let (outcome, relations) = record.assess(
            scopes[record.scope.as_str()],
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
        nodes.push(Node {
            key: record.key(),
            relations,
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

    fn assess<'a, 'review, 'record>(
        &'record self,
        scope: &ScopeOutcome<'a>,
        unique: bool,
        reviews: &Reviews<'review>,
        source_bodies: &mut HashMap<&'record str, SourceBody>,
        git: &impl Fn() -> Result<Command, HiveError>,
    ) -> Result<(Outcome<'a>, Option<&'review Relations>), HiveError> {
        let hold = |reason| {
            let mut reason_codes = vec![reason];
            if !matches!(reason, Reason::ExecutionNeedsReconciliation)
                && matches!(
                    self.classification,
                    Err(Reason::ExecutionNeedsReconciliation)
                )
            {
                reason_codes.push(Reason::ExecutionNeedsReconciliation);
            }
            (
                Outcome::Held {
                    reason_codes,
                    candidate_status: None,
                    area_id: None,
                },
                None,
            )
        };
        let area = match scope {
            ScopeOutcome::Excluded { reason } => {
                return Ok((Outcome::Excluded { reason: *reason }, None));
            }
            _ if !unique => return Ok(hold(Reason::DuplicateLegacyWorkId)),
            _ if !is_slug(&self.work_id) => return Ok(hold(Reason::InvalidLegacyState)),
            ScopeOutcome::Held { reason } => return Ok(hold(*reason)),
            ScopeOutcome::Mapped { area } => area,
        };
        let binding = self
            .blob_oid
            .as_deref()
            .map_or(Ok(None), |oid| reviews.get(self.key(), oid, &area.id));
        let relations = binding
            .as_ref()
            .ok()
            .and_then(|review| review.and_then(|review| review.relations.as_ref()));
        let outcome = match self.classification {
            Err(reason) => hold(reason).0,
            Ok(status) => self.candidate_outcome(status, &area.id, binding, source_bodies, git)?,
        };
        Ok((outcome, relations))
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
    pub(super) title: Option<String>,
    #[serde(default, deserialize_with = "present_value")]
    status: Option<Value>,
    #[serde(default, deserialize_with = "present_value")]
    claim: Option<Value>,
    #[serde(default, deserialize_with = "present_value")]
    pub(super) priority: Option<Value>,
    #[serde(default, deserialize_with = "present_value")]
    pub(super) tags: Option<Vec<String>>,
}

fn present_value<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    deserializer: D,
) -> Result<Option<T>, D::Error> {
    // Present fields are decoded as their declared type, including explicit null.
    T::deserialize(deserializer).map(Some)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Claim {
    id: String,
    holder: String,
}

fn classify(
    bytes: &[u8],
    lifecycle: Lifecycle,
    claims_supported: bool,
) -> Result<CandidateStatus, Reason> {
    let text = std::str::from_utf8(bytes).map_err(|_| Reason::UnsupportedTaskMetadata)?;
    let (metadata, body) = parse_record(text).ok_or(Reason::UnsupportedTaskMetadata)?;
    let claimed = metadata.claim.is_some();
    if let Some(claim) = metadata.claim {
        let claim: Claim = serde_json::from_value(claim).map_err(|_| Reason::InvalidLegacyState)?;
        if !claims_supported || claim.id.trim().is_empty() || claim.holder.trim().is_empty() {
            return Err(Reason::InvalidLegacyState);
        }
    }
    let status = metadata.status.as_ref().and_then(Value::as_str);
    let candidate = match (lifecycle, status, claimed) {
        (Lifecycle::Backlog, None, false) if metadata.status.is_none() => CandidateStatus::Backlog,
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
    Ok(candidate)
}

pub(super) fn parse_record(text: &str) -> Option<(Frontmatter, &str)> {
    let mut lines = text.split_inclusive('\n');
    let delimiter = |line: &str| matches!(line, "---\n" | "---\r\n" | "---");
    let start = lines.next().filter(|line| delimiter(line))?.len();
    let mut end = start;
    for line in lines {
        if delimiter(line) {
            let metadata: Frontmatter =
                parse_v2(&text.as_bytes()[start..end], "v1 Task frontmatter").ok()?;
            return (!metadata.description.trim().is_empty())
                .then_some((metadata, &text[end + line.len()..]));
        }
        end += line.len();
        if end - start > crate::config_file::MAX_FILE_BYTES {
            return None;
        }
    }
    None
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
    // Recognize a leading ATX H1. Defer other plausible headings until full
    // Markdown parsing can distinguish headings from nested blocks and examples.
    let mut lines = body.lines().filter(|line| !line.trim().is_empty());
    let first = lines.next().unwrap_or_default();
    let h1 = |line: &str| line == "#" || line.starts_with("# ") || line.starts_with("#\t");
    if (std::iter::once(first).filter(|line| !h1(line)))
        .chain(lines)
        .any(|line| {
            let line = line.trim();
            h1(line) || (!line.is_empty() && line.bytes().all(|byte| byte == b'='))
        })
    {
        return None;
    }
    let heading = first.get(1..).filter(|_| h1(first)).map(str::trim);
    let heading = heading.map(|value| {
        let unclosed = value.trim_end_matches('#');
        if unclosed.is_empty() || unclosed.ends_with([' ', '\t']) {
            unclosed.trim_end()
        } else {
            value
        }
    });
    heading
        .filter(|heading| valid(heading))
        .map(|heading| (heading, TitleSource::Heading))
        .or_else(|| valid(description).then_some((description, TitleSource::Description)))
}
