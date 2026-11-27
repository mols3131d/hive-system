//! Explicit v2 context evidence. A receipt locator only selects a historical
//! record. Current declarations carry their own operation and invocation binding.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Component, Path};

use serde::{Deserialize, Deserializer};
use serde_json::value::RawValue;

use super::context_workflow::{
    self as workflow, CurrentBinding, Declaration, Operation, RecordKind,
};
use super::{is_uuid, is_uuid_v7};
use crate::config::{absolute_env, current_dir, resolve_path};
use crate::config_file::ScopedDirectory;
use crate::error::{ErrorCode, HiveError};

const MAX_EVIDENCE_BYTES: usize = 256 * 1024;
pub(super) const MAX_DIRECTIVE_RECEIPTS: usize = 100;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct EvidenceFile {
    version: u8,
    #[serde(default, deserialize_with = "deserialize_receipts")]
    directive_receipts: Vec<DirectiveReceipt>,
    handoff: Option<Box<RawValue>>,
    #[serde(default)]
    artifacts: Vec<Box<RawValue>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DirectiveReceipt {
    directive_id: String,
    request_id: String,
}

fn deserialize_receipts<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<DirectiveReceipt>, D::Error> {
    Vec::<&RawValue>::deserialize(deserializer)?
        .into_iter()
        .map(workflow::parse_json_object)
        .collect()
}

pub(super) struct Selection<'a> {
    pub(super) workspace_id: &'a str,
    pub(super) actor_ref: &'a str,
    pub(super) bot_id: Option<&'a str>,
    pub(super) invocation_id: Option<&'a str>,
    pub(super) issue: Option<&'a str>,
    pub(super) area: Option<&'a str>,
    pub(super) artifacts: &'a [String],
}

#[derive(Default)]
#[cfg_attr(
    target_os = "macos",
    allow(dead_code, reason = "Runtime pairing is Linux-only")
)]
pub(super) struct CheckedEvidence<'a> {
    pub(super) activation_receipts: BTreeMap<&'a str, &'a str>,
    pub(super) handoff: Option<&'a [u8]>,
    pub(super) artifacts: BTreeMap<String, &'a [u8]>,
}

#[cfg(target_os = "linux")]
impl CheckedEvidence<'_> {
    pub(super) fn fingerprint(&self) -> String {
        use sha2::{Digest, Sha256};

        // Include the exact current declarations, including their revisions
        // and validity bounds. Only the digest enters the cursor binding.
        let evidence = serde_json::json!([self.activation_receipts, self.handoff, self.artifacts]);
        format!("{:x}", Sha256::digest(evidence.to_string()))
    }
}

impl EvidenceFile {
    pub(super) fn read(path: &Path) -> Result<Self, HiveError> {
        let path = resolve_path(path, &current_dir()?, absolute_env("HOME").as_deref())?;
        // Scope the descriptor open to every named component and refuse traversal.
        if path.components().any(|part| part == Component::ParentDir) {
            return Err(invalid(
                "The context evidence path cannot traverse parent directories.",
            ));
        }
        let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
            return Err(invalid("Select an explicit context evidence file."));
        };
        let directory = ScopedDirectory::open_absolute(parent).map_err(evidence_io)?;
        let file = directory
            .open_regular(name)
            .map_err(evidence_io)?
            .ok_or_else(file_unavailable)?;
        let mut bytes = Vec::new();
        file.take(MAX_EVIDENCE_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| {
                HiveError::new(
                    ErrorCode::IoError,
                    "Could not read the context evidence file.",
                )
            })?;
        if bytes.len() > MAX_EVIDENCE_BYTES {
            return Err(limit("The context evidence file exceeds 256 KiB."));
        }
        if bytes
            .iter()
            .copied()
            .find(|byte| !byte.is_ascii_whitespace())
            != Some(b'{')
        {
            return Err(invalid(
                "The context evidence file must contain a JSON object.",
            ));
        }
        serde_json::from_slice(&bytes)
            .map_err(|_| invalid("The context evidence file is not a valid versioned JSON object."))
    }

    pub(super) fn preflight(&self, selection: &Selection<'_>) -> Result<(), HiveError> {
        self.validate_inner(selection, None).map(|_| ())
    }

    #[cfg(any(test, target_os = "linux"))]
    pub(super) fn validate(
        &self,
        selection: &Selection<'_>,
        observed_at: time::OffsetDateTime,
    ) -> Result<CheckedEvidence<'_>, HiveError> {
        self.validate_inner(selection, Some(observed_at))
    }

    fn validate_inner(
        &self,
        selection: &Selection<'_>,
        observed_at: Option<time::OffsetDateTime>,
    ) -> Result<CheckedEvidence<'_>, HiveError> {
        if self.version != 1 {
            return Err(invalid("The context evidence file version must be 1."));
        }
        if self.directive_receipts.len() > MAX_DIRECTIVE_RECEIPTS
            || self.artifacts.len() > crate::cli::MAX_CONTEXT_ARTIFACTS
        {
            return Err(limit(
                "The context evidence file exceeds its record count limits.",
            ));
        }

        let mut checked = CheckedEvidence::default();
        let mut requests = BTreeSet::new();
        for receipt in &self.directive_receipts {
            if !is_uuid_v7(&receipt.directive_id)
                || !is_uuid(&receipt.request_id)
                || checked
                    .activation_receipts
                    .insert(receipt.directive_id.as_str(), receipt.request_id.as_str())
                    .is_some()
                || !requests.insert(&receipt.request_id)
            {
                return Err(invalid(
                    "Supply one exact activation receipt per distinct Directive.",
                ));
            }
        }

        let selected: BTreeSet<_> = selection.artifacts.iter().map(String::as_str).collect();
        if self.handoff.is_none() && self.artifacts.is_empty() && selected.is_empty() {
            return Ok(checked);
        }
        let issue = selection.issue.ok_or_else(|| {
            unauthorized("Current body declarations require an explicitly selected Issue.")
        })?;
        let invocation_id = selection.invocation_id.ok_or_else(workflow::unverified)?;
        let binding = CurrentBinding {
            workspace_id: selection.workspace_id,
            actor_ref: selection.actor_ref,
            bot_id: selection.bot_id,
            invocation_id,
        };
        let mut exact_scope = None;
        for (body, handoff) in self
            .handoff
            .iter()
            .map(|body| (body, true))
            .chain(self.artifacts.iter().map(|body| (body, false)))
        {
            let declaration = Declaration::parse(body.get().as_bytes())?;
            if let Some(observed_at) = observed_at {
                declaration.check_valid_at(observed_at)?;
            }
            declaration.check_binding(&binding)?;
            let area = declaration.area_id.as_str();
            let owner = declaration.issue_id.as_str();
            if selection
                .area
                .is_some_and(|selected_area| selected_area != area)
                || (is_uuid_v7(issue) && issue != owner)
                || exact_scope
                    .as_ref()
                    .is_some_and(|(previous_area, previous_owner)| {
                        previous_area != area || previous_owner != owner
                    })
            {
                return Err(unauthorized(
                    "Current body declarations must match the exact selected Area and Issue.",
                ));
            }
            exact_scope.get_or_insert((declaration.area_id.clone(), declaration.issue_id.clone()));
            let artifact =
                declaration.covers(RecordKind::Artifact, Operation::DiscloseArtifactBody);
            if handoff {
                if !artifact
                    && !declaration.covers(RecordKind::IssueUpdate, Operation::ReadHandoffBody)
                {
                    return Err(unauthorized(
                        "The handoff declaration must authorize the exact handoff body.",
                    ));
                }
                checked.handoff = Some(body.get().as_bytes());
            } else {
                if !artifact || !selected.contains(declaration.record.id.as_str()) {
                    return Err(unauthorized(
                        "Artifact evidence must match an explicitly selected Artifact body.",
                    ));
                }
                if checked
                    .artifacts
                    .insert(declaration.record.id, body.get().as_bytes())
                    .is_some()
                {
                    return Err(invalid(
                        "Supply one current declaration per selected Artifact.",
                    ));
                }
            }
        }
        if checked.artifacts.len() != selected.len() {
            return Err(workflow::unverified());
        }
        Ok(checked)
    }
}

fn evidence_io(error: HiveError) -> HiveError {
    match error.code {
        ErrorCode::NotFound => file_unavailable(),
        ErrorCode::InvalidConfig => invalid(
            "The context evidence source must be a local regular file without symlink traversal.",
        ),
        _ => HiveError::new(error.code, "Could not open the context evidence file."),
    }
}

fn file_unavailable() -> HiveError {
    HiveError::new(
        ErrorCode::AuthorityUnverified,
        "The selected context evidence file is unavailable.",
    )
}

fn invalid(message: &str) -> HiveError {
    HiveError::new(ErrorCode::InvalidArgument, message)
}

fn unauthorized(message: &str) -> HiveError {
    HiveError::new(ErrorCode::Unauthorized, message)
}

fn limit(message: &str) -> HiveError {
    HiveError::new(ErrorCode::ContextLimit, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_a_declaration_expiring_at_snapshot_acquisition() {
        let workspace = "01900000-0000-7000-8000-000000000001";
        let area = "01900000-0000-7000-8000-000000000002";
        let issue = "01900000-0000-7000-8000-000000000003";
        let invocation = "01900000-0000-7000-8000-000000000004";
        let start = "2026-01-01T00:00:00Z";
        let expires = "2026-01-01T00:00:01Z";
        let file: EvidenceFile = serde_json::from_str(
            &serde_json::json!({
                "version": 1,
                "handoff": {
                    "version": 1,
                    "evidence_id": "fixture-evidence",
                    "revision": "r1",
                    "source_ref": "fixture-source",
                    "issuer_ref": "fixture-issuer",
                    "workspace_id": workspace,
                    "actor_ref": "fixture-actor",
                    "area_id": area,
                    "issue_id": issue,
                    "invocation_id": invocation,
                    "db_commit": "a".repeat(32),
                    "operations": ["read_handoff_body"],
                    "record": {
                        "kind": "issue_update",
                        "id": "01900000-0000-7000-8000-000000000005",
                        "revision": "a".repeat(32)
                    },
                    "valid_from": start,
                    "expires_at": expires,
                    "withdrawn": false
                }
            })
            .to_string(),
        )
        .unwrap();
        let selection = Selection {
            workspace_id: workspace,
            actor_ref: "fixture-actor",
            bot_id: None,
            invocation_id: Some(invocation),
            issue: Some(issue),
            area: Some(area),
            artifacts: &[],
        };
        assert!(file.preflight(&selection).is_ok());
        assert!(
            file.validate(&selection, workflow::parse_time(start).unwrap())
                .is_ok()
        );
        assert!(matches!(
            file.validate(&selection, workflow::parse_time(expires).unwrap()),
            Err(error) if matches!(error.code, ErrorCode::AuthorityUnverified)
        ));
    }
}
