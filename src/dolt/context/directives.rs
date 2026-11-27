//! Structural applicability and historical provenance candidates.
//!
//! Active status and a matching approval reference do not prove an approved
//! direction. The activation receipt result and reviewed decision must bind
//! the exact Directive revision and its scope before any body can be included.

use time::OffsetDateTime;

use super::{
    Scope,
    activation_receipt::{self, ActivatedDirective, DirectiveScope as ReceiptScope},
    invalid_record, limit, workflow,
};
use crate::dolt::{
    Snapshot,
    directive::{METADATA, MetadataRow, Scope as DirectiveScope},
    is_commit_hash, is_uuid,
    receipt_history::{HistoricalReceipt, receipt_at, receipt_introduction},
};
use crate::error::{ErrorCode, HiveError};

pub(super) const MAX_DIRECTIVES: usize = 100;

pub(super) struct Candidate {
    db_commit: String,
    scope: ReceiptScope,
    pub(super) valid_from: Option<String>,
    pub(super) expires_at: Option<String>,
    pub(super) id: String,
    pub(super) revision: String,
    pub(super) issuer_ref: String,
    pub(super) source_ref: String,
    pub(super) approval_ref: String,
}

pub(super) fn collect(
    snapshot: &mut Snapshot,
    scope: &Scope,
    observed_at: OffsetDateTime,
) -> Result<Vec<Candidate>, HiveError> {
    if snapshot.commit != scope.db_commit {
        return Err(HiveError::new(
            ErrorCode::StaleRevision,
            "Resolve Directives from the same committed context.",
        ));
    }
    let mut selections = vec![DirectiveScope::Workspace];
    selections.extend(
        scope
            .ancestry
            .iter()
            .rev()
            .map(|area| DirectiveScope::Area(&area.id)),
    );
    if let Some(project) = &scope.project {
        selections.push(DirectiveScope::Project(&project.id));
    }
    let mut candidates = Vec::new();
    let mut scanned = 0;
    for selection in selections {
        let (filter, value, descendants_only) = match selection {
            DirectiveScope::Workspace => (
                "(scope_kind = ? OR LOWER(scope_kind) = ?)",
                "workspace",
                false,
            ),
            DirectiveScope::Area(id) => (
                "(area_id = ? OR CAST(LOWER(area_id) AS BINARY) = CAST(? AS BINARY))",
                id,
                scope.ancestry.first().is_some_and(|area| area.id != id),
            ),
            DirectiveScope::Project(id) => (
                "(project_id = ? OR CAST(LOWER(project_id) AS BINARY) = CAST(? AS BINARY))",
                id,
                false,
            ),
        };
        let rows: Vec<MetadataRow> = snapshot.exec_rows(
            |commit| {
                format!(
                    "SELECT {METADATA} FROM directives AS OF '{commit}'
                 WHERE {filter} AND LOWER(status) = 'active'
                   AND (NOT ? OR include_descendants <> 0)
                 ORDER BY CAST(id AS BINARY) LIMIT ?"
                )
            },
            (value, value, descendants_only, MAX_DIRECTIVES - scanned + 1),
        )?;
        scanned += rows.len();
        if scanned > MAX_DIRECTIVES {
            return Err(limit(
                "The context exceeds the applicable Directive scan limit.",
            ));
        }
        for row in rows {
            if row.6 != "active" {
                return Err(invalid_record(
                    "An applicable Directive has noncanonical active status.",
                ));
            }
            let metadata = selection.metadata(row)?;
            if !currently_applicable(
                metadata.valid_from.as_deref(),
                metadata.expires_at.as_deref(),
                observed_at,
            )? {
                continue;
            }
            if !workflow::valid_ref(&metadata.revision) {
                return Err(invalid_record(
                    "An applicable Directive has invalid revision provenance.",
                ));
            }
            let rows: Vec<(String, Option<String>, Option<String>)> = snapshot.exec_rows(
                |commit| {
                    format!(
                        "SELECT issuer_ref,
                       CASE WHEN OCTET_LENGTH(source_ref) <= 255 THEN source_ref END,
                       CASE WHEN OCTET_LENGTH(approval_ref) <= 255 THEN approval_ref END
                     FROM directives AS OF '{commit}' WHERE CAST(id AS BINARY) = ? LIMIT 2"
                    )
                },
                (&metadata.id,),
            )?;
            let [(issuer, Some(source), Some(approval))] = rows.as_slice() else {
                return Err(invalid_record(
                    "An applicable Directive has missing or oversized provenance.",
                ));
            };
            if ![issuer, source, approval]
                .into_iter()
                .all(|value| workflow::valid_ref(value))
            {
                return Err(invalid_record(
                    "An applicable Directive has invalid provenance references.",
                ));
            }
            candidates.push(Candidate {
                db_commit: snapshot.commit.clone(),
                scope: match selection {
                    DirectiveScope::Workspace => ReceiptScope::Workspace,
                    DirectiveScope::Area(id) => ReceiptScope::Area {
                        id: id.to_owned(),
                        include_descendants: metadata.include_descendants,
                    },
                    DirectiveScope::Project(id) => ReceiptScope::Project { id: id.to_owned() },
                },
                valid_from: metadata.valid_from,
                expires_at: metadata.expires_at,
                id: metadata.id,
                revision: metadata.revision,
                issuer_ref: issuer.clone(),
                source_ref: source.clone(),
                approval_ref: approval.clone(),
            });
        }
    }
    Ok(candidates)
}

fn currently_applicable(
    start: Option<&str>,
    end: Option<&str>,
    observed_at: OffsetDateTime,
) -> Result<bool, HiveError> {
    let parse = |value| {
        workflow::parse_time(value).ok_or_else(|| {
            invalid_record("An applicable Directive has an invalid validity timestamp.")
        })
    };
    let start = start.map(parse).transpose()?;
    let end = end.map(parse).transpose()?;
    if matches!((start, end), (Some(start), Some(end)) if end <= start) {
        return Err(invalid_record(
            "An applicable Directive has an invalid validity interval.",
        ));
    }
    Ok(start.is_none_or(|value| observed_at >= value)
        && end.is_none_or(|value| observed_at < value))
}

/// This is a historical lookup only. The caller supplies an exact request ID
/// from reviewed provenance; approval_ref is an opaque locator, not that ID.
pub(super) fn lookup_activation_receipt(
    snapshot: &mut Snapshot,
    candidate: &Candidate,
    request_id: &str,
) -> Result<HistoricalReceipt, HiveError> {
    if snapshot.commit != candidate.db_commit {
        return Err(HiveError::new(
            ErrorCode::StaleRevision,
            "Resolve historical Directive provenance from the same committed context.",
        ));
    }
    if !is_uuid(request_id) {
        return Err(HiveError::new(
            ErrorCode::InvalidArgument,
            "Select an exact activation receipt request ID.",
        ));
    }
    let observed = snapshot.commit.clone();
    let receipt = receipt_at(snapshot, request_id, &observed)?.ok_or_else(workflow::unverified)?;
    if receipt.kind != activation_receipt::KIND
        || receipt.authorization_ref != candidate.approval_ref
    {
        return Err(workflow::unverified());
    }
    Ok(receipt)
}

type DirectiveVersion = (
    String,
    Option<String>,
    Option<String>,
    i8,
    String,
    String,
    String,
);

fn directive_version(
    snapshot: &mut Snapshot,
    id: &str,
    commit: &str,
) -> Result<DirectiveVersion, HiveError> {
    if !is_commit_hash(commit) {
        return Err(invalid_record("A Directive history commit is malformed."));
    }
    let rows = snapshot.exec_rows(
        |_| {
            format!(
                "SELECT scope_kind, area_id, project_id, include_descendants, status, revision,
                SHA2(CAST(JSON_ARRAY(title, SHA2(body, 256), issuer_ref, SHA2(source_ref, 256),
                    SHA2(approval_ref, 256), valid_from, expires_at, supersedes_id) AS CHAR), 256)
             FROM directives AS OF '{commit}' WHERE CAST(id AS BINARY) = ? LIMIT 2"
            )
        },
        (id,),
    )?;
    let Ok([version]) = <[_; 1]>::try_from(rows) else {
        return Err(workflow::unverified());
    };
    Ok(version)
}

fn verify_activation(
    before: &DirectiveVersion,
    introduced: &DirectiveVersion,
    observed: &DirectiveVersion,
    revision: &str,
) -> Result<(), HiveError> {
    let (
        before_kind,
        before_area,
        before_project,
        before_descendants,
        before_status,
        before_revision,
        _,
    ) = before;
    let (kind, area, project, descendants, status, activated_revision, _) = introduced;
    if before_status != "proposed"
        || before_revision.trim().is_empty()
        || before_revision == activated_revision
        || status != "active"
        || activated_revision != revision
        || (before_kind, before_area, before_project, before_descendants)
            != (kind, area, project, descendants)
        || introduced != observed
    {
        return Err(workflow::unverified());
    }
    Ok(())
}

pub(super) fn read_approved(
    snapshot: &mut Snapshot,
    candidate: &Candidate,
    request_id: &str,
    remaining: usize,
) -> Result<String, HiveError> {
    let receipt = lookup_activation_receipt(snapshot, candidate, request_id)?;
    verify_receipt(candidate, &receipt, &snapshot.workspace_id, request_id)?;
    let introduced_at = receipt_introduction(snapshot, request_id, &receipt)?;
    let observed_at = snapshot.commit.clone();
    let before = directive_version(snapshot, &candidate.id, &receipt.base_commit)?;
    let introduced = directive_version(snapshot, &candidate.id, &introduced_at)?;
    let observed = directive_version(snapshot, &candidate.id, &observed_at)?;
    verify_activation(&before, &introduced, &observed, &candidate.revision)?;
    let rows: Vec<Option<String>> = snapshot.exec_rows(
        |commit| {
            format!(
                "SELECT CASE WHEN OCTET_LENGTH(body) <= ? THEN body END
             FROM directives AS OF '{commit}' WHERE CAST(id AS BINARY) = ?
               AND CAST(revision AS BINARY) = ? AND CAST(status AS BINARY) = 'active' LIMIT 2"
            )
        },
        (remaining, &candidate.id, &candidate.revision),
    )?;
    let Ok([body]) = <[_; 1]>::try_from(rows) else {
        return Err(workflow::unverified());
    };
    let body = body.filter(|body| body.len() <= remaining).ok_or_else(|| {
        limit("A required Directive exceeds the remaining inline context budget.")
    })?;
    if body.trim().is_empty() || body.contains('\0') {
        return Err(invalid_record("An applicable Directive body is invalid."));
    }
    Ok(body)
}

fn verify_receipt(
    candidate: &Candidate,
    receipt: &HistoricalReceipt,
    workspace_id: &str,
    request_id: &str,
) -> Result<(), HiveError> {
    let evidence: activation_receipt::Approval =
        serde_json::from_str(&receipt.workflow_evidence).map_err(|_| workflow::unverified())?;
    let result: activation_receipt::ResultRecord =
        serde_json::from_str(&receipt.result).map_err(|_| workflow::unverified())?;
    let expected = ActivatedDirective {
        id: candidate.id.clone(),
        revision: candidate.revision.clone(),
        scope: candidate.scope.clone(),
    };
    if evidence.version != 1
        || result.version != 1
        || evidence.workspace_id != workspace_id
        || evidence.request_id != request_id
        || evidence.fingerprint != receipt.fingerprint
        || evidence.actor_ref != receipt.actor_ref
        || !workflow::valid_ref(&evidence.reviewer_ref)
        || evidence.approval_ref != candidate.approval_ref
        || evidence.directive != expected
        || result.directive != expected
    {
        return Err(workflow::unverified());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requires_an_activation_delta_and_an_unchanged_observed_revision() {
        let before = (
            "workspace".to_owned(),
            None,
            None,
            0,
            "proposed".to_owned(),
            "draft-r1".to_owned(),
            "draft-fingerprint".to_owned(),
        );
        let mut activated = before.clone();
        activated.4 = "active".to_owned();
        activated.5 = "r1".to_owned();
        activated.6 = "activated-fingerprint".to_owned();
        assert!(verify_activation(&before, &activated, &activated, "r1").is_ok());
        assert!(verify_activation(&activated, &activated, &activated, "r1").is_err());
        assert!(verify_activation(&before, &activated, &activated, "r2").is_err());
        let mut edited = activated.clone();
        edited.6 = "edited-fingerprint".to_owned();
        assert!(verify_activation(&before, &activated, &edited, "r1").is_err());
    }

    #[test]
    fn requires_the_same_directive_revision_and_scope_in_both_historical_payloads() {
        let candidate = Candidate {
            db_commit: "db".to_owned(),
            scope: ReceiptScope::Area {
                id: "area".to_owned(),
                include_descendants: false,
            },
            valid_from: None,
            expires_at: None,
            id: "directive".to_owned(),
            revision: "r1".to_owned(),
            issuer_ref: "issuer".to_owned(),
            source_ref: "source".to_owned(),
            approval_ref: "approval".to_owned(),
        };
        let target = serde_json::json!({
            "id": "directive", "revision": "r1",
            "scope": {"kind": "area", "id": "area", "include_descendants": false}
        });
        let approval = serde_json::json!({
            "version": 1, "workspace_id": "workspace", "request_id": "request",
            "fingerprint": "fingerprint", "actor_ref": "actor", "reviewer_ref": "reviewer",
            "approval_ref": "approval", "decision": "approved", "directive": target
        });
        let result = serde_json::json!({"version": 1, "directive": target});
        let mut receipt = HistoricalReceipt {
            kind: activation_receipt::KIND.to_owned(),
            authorization_ref: candidate.approval_ref.clone(),
            fingerprint: "fingerprint".to_owned(),
            base_commit: "base".to_owned(),
            created_at: "fixture-date".to_owned(),
            actor_ref: "actor".to_owned(),
            workflow_evidence: approval.to_string(),
            result: result.to_string(),
        };
        assert!(verify_receipt(&candidate, &receipt, "workspace", "request").is_ok());

        let mut wrong = result.clone();
        wrong["directive"]["scope"]["include_descendants"] = serde_json::json!(true);
        receipt.result = wrong.to_string();
        assert!(verify_receipt(&candidate, &receipt, "workspace", "request").is_err());

        receipt.result = result.to_string();
        let mut wrong = approval;
        wrong["directive"]["revision"] = serde_json::json!("other");
        receipt.workflow_evidence = wrong.to_string();
        assert!(verify_receipt(&candidate, &receipt, "workspace", "request").is_err());
    }

    #[test]
    fn checks_directive_validity_at_one_observed_time() {
        let start = "2026-01-01T00:00:00.000000Z";
        let end = "2026-01-02T00:00:00.000000Z";
        assert!(
            currently_applicable(Some(start), Some(end), workflow::parse_time(start).unwrap())
                .unwrap()
        );
        assert!(
            !currently_applicable(Some(start), Some(end), workflow::parse_time(end).unwrap())
                .unwrap()
        );
        assert!(
            currently_applicable(
                Some("fixture-invalid"),
                None,
                workflow::parse_time(start).unwrap()
            )
            .is_err()
        );
    }
}
