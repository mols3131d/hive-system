use std::collections::{BTreeMap, HashMap, HashSet, hash_map::Entry};
use std::process::Command;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::scope_plan::{ScopeOutcome, is_portable_scope};
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

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
enum CandidateStatus {
    Backlog,
    Todo,
    Done,
    Cancelled,
}

pub(super) struct Record {
    scope: String,
    work_id: String,
    lifecycle: Lifecycle,
    path: String,
    blob_oid: Option<String>,
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

pub(super) fn report(records: Vec<Record>, scopes: &BTreeMap<&str, &ScopeOutcome<'_>>) -> Value {
    let mut counts = HashMap::new();
    for record in &records {
        *counts.entry((&record.scope, &record.work_id)).or_insert(0) += 1;
    }
    let mut excluded = 0;
    let tasks: Vec<_> = records
        .iter()
        .map(|record| {
            let scope = scopes[record.scope.as_str()];
            let outcome = match scope {
                ScopeOutcome::Excluded { reason } => {
                    excluded += 1;
                    Outcome::Excluded { reason: *reason }
                }
                _ => held_outcome(record, scope, counts[&(&record.scope, &record.work_id)] > 1),
            };
            let mut task = json!({
                "lifecycle": record.lifecycle,
                "source_git_blob_sha": record.blob_oid,
                "source_raw_sha256": record.raw_sha256,
                "outcome": outcome,
            });
            if is_portable_scope(&record.scope) && is_slug(&record.work_id) {
                task["legacy_key"] = json!({"scope": record.scope, "work_id": record.work_id});
                task["source_path"] = json!(record.path);
            } else {
                task["source_path_sha256"] = json!(format!("{:x}", Sha256::digest(&record.path)));
            }
            task
        })
        .collect();
    json!({
        "coverage": "partial",
        "counts": {
            "inventoried": tasks.len(),
            "held": tasks.len() - excluded,
            "excluded": excluded,
        },
        "tasks": tasks,
    })
}

fn held_outcome<'a>(record: &Record, scope: &ScopeOutcome<'a>, duplicate: bool) -> Outcome<'a> {
    let mut reasons = Vec::new();
    if duplicate {
        reasons.push(Reason::DuplicateLegacyWorkId);
    }
    if !is_slug(&record.work_id) {
        reasons.push(Reason::InvalidLegacyState);
    }
    if let ScopeOutcome::Held { reason } = scope {
        reasons.push(*reason);
    }

    let mut candidate_status = None;
    let mut area_id = None;
    match record.classification.candidate {
        Ok(status)
            if reasons.is_empty()
                && let ScopeOutcome::Mapped { area } = scope =>
        {
            candidate_status = Some(status);
            area_id = Some(area.id.as_str());
            // The v1 policies keep these fields in free-form records.
            reasons.push(Reason::IncompleteHistoricalProvenance);
            reasons.push(match status {
                CandidateStatus::Done => Reason::UnresolvedCompletionObligation,
                _ => Reason::UnresolvedTaskRelations,
            });
            if matches!(status, CandidateStatus::Todo) {
                reasons.push(Reason::UnverifiedAcceptanceCriteria);
            }
        }
        Err(reason) if !reasons.contains(&reason) => reasons.push(reason),
        _ => {}
    }
    if record.classification.execution_needs_reconciliation
        && !reasons.contains(&Reason::ExecutionNeedsReconciliation)
    {
        reasons.push(Reason::ExecutionNeedsReconciliation);
    }
    Outcome::Held {
        reason_codes: reasons,
        candidate_status,
        area_id,
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Frontmatter {
    description: String,
    #[serde(default, deserialize_with = "present")]
    title: Option<String>,
    #[serde(default, deserialize_with = "present")]
    status: Option<Value>,
    #[serde(default, deserialize_with = "present")]
    claim: Option<Value>,
    #[serde(default, deserialize_with = "present")]
    priority: Option<Value>,
    #[serde(default, deserialize_with = "present")]
    tags: Option<Vec<String>>,
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
    if !has_title(metadata.title.as_deref(), &metadata.description, body) {
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

fn has_title(title: Option<&str>, description: &str, body: &str) -> bool {
    let valid = |value: &str| {
        !value.trim().is_empty()
            && value.chars().count() <= 255
            && !value.chars().any(char::is_control)
    };
    if title.is_some_and(valid) {
        return true;
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
        return false;
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
    heading.is_some_and(valid) || valid(description)
}
