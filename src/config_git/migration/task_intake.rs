use std::collections::{BTreeMap, HashMap, HashSet, hash_map::Entry};
use std::process::Command;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::scope_plan::ScopeOutcome;
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
            let hold = |reason| Outcome::Held {
                reason_codes: vec![reason],
                candidate_status: None,
                area_id: None,
            };
            let outcome = match scope {
                ScopeOutcome::Excluded { reason } => {
                    excluded += 1;
                    Outcome::Excluded { reason: *reason }
                }
                _ if counts[&(&record.scope, &record.work_id)] > 1 => {
                    hold(Reason::DuplicateLegacyWorkId)
                }
                _ if !is_slug(&record.work_id) => hold(Reason::InvalidLegacyState),
                ScopeOutcome::Held { reason } => hold(*reason),
                ScopeOutcome::Mapped { area } => match record.classification {
                    Err(reason) => hold(reason),
                    Ok(status) => {
                        // The v1 policies keep dates, acceptance, closure and relations
                        // in free-form records. A scope mapping cannot verify that evidence.
                        let mut reasons = vec![Reason::IncompleteHistoricalProvenance];
                        reasons.push(match status {
                            CandidateStatus::Done => Reason::UnresolvedCompletionObligation,
                            _ => Reason::UnresolvedTaskRelations,
                        });
                        if matches!(status, CandidateStatus::Todo) {
                            reasons.push(Reason::UnverifiedAcceptanceCriteria);
                        }
                        Outcome::Held {
                            reason_codes: reasons,
                            candidate_status: Some(status),
                            area_id: Some(&area.id),
                        }
                    }
                },
            };
            json!({
                "legacy_key": {"scope": record.scope, "work_id": record.work_id},
                "lifecycle": record.lifecycle,
                "source_path": record.path,
                "source_git_blob_sha": record.blob_oid,
                "source_raw_sha256": record.raw_sha256,
                "outcome": outcome,
            })
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Frontmatter {
    description: String,
    title: Option<String>,
    #[serde(default, deserialize_with = "present_value")]
    status: Option<Value>,
    #[serde(default, deserialize_with = "present_value")]
    claim: Option<Value>,
    #[serde(default, deserialize_with = "present_value")]
    priority: Option<Value>,
    tags: Option<Vec<String>>,
}

fn present_value<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Value>, D::Error> {
    // Explicit null still counts as a present field in the lifecycle rules.
    Value::deserialize(deserializer).map(Some)
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
    let (metadata, body) = parse_record(bytes).ok_or(Reason::UnsupportedTaskMetadata)?;
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
    if !has_title(metadata.title.as_deref(), &metadata.description, body) {
        return Err(Reason::MissingOrConflictingTitle);
    }
    Ok(candidate)
}

fn parse_record(bytes: &[u8]) -> Option<(Frontmatter, &str)> {
    let text = std::str::from_utf8(bytes).ok()?;
    let mut lines = text.split_inclusive('\n');
    let delimiter = |line: &str| matches!(line, "---\n" | "---\r\n" | "---");
    let start = lines.next().filter(|line| delimiter(line))?.len();
    let mut end = start;
    for line in lines {
        if delimiter(line) {
            let metadata: Frontmatter = parse_v2(&bytes[start..end], "v1 Task frontmatter").ok()?;
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
