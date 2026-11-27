use serde_json::{Value, json};

use crate::dolt::{
    receipt_history::{HistoricalReceipt, verify_delta},
    scope::require_active_area_at,
};

use super::*;

pub(in crate::dolt) struct ScopedProject {
    result: ResultRow,
    membership: Option<Membership>,
}

pub(in crate::dolt) fn check_scope(
    snapshot: &Snapshot,
    kind: Kind,
    request: &str,
    area: Option<&str>,
    project: Option<&str>,
    actor: &str,
    receipt: &HistoricalReceipt,
) -> Result<ScopedProject, HiveError> {
    let result: ResultRow =
        serde_json::from_str(&receipt.result).map_err(|_| workflow::unverified())?;
    let evidence: Evidence =
        serde_json::from_str(&receipt.workflow_evidence).map_err(|_| workflow::unverified())?;
    if result.version != 1
        || !is_uuid_v7(&result.project_id)
        || !is_uuid_v7(&result.project_revision)
        || project.is_some_and(|id| id != result.project_id)
        || receipt.kind != kind.name()
        || evidence.operation != receipt.kind
        || receipt.actor_ref != actor
        || evidence.authorization_ref != receipt.authorization_ref
        || !workflow::parse_time(&receipt.created_at).is_some_and(|created| {
            evidence.verifies(
                &snapshot.workspace_id,
                actor,
                request,
                &receipt.fingerprint,
                created,
            )
        })
    {
        return Err(workflow::unverified());
    }
    match (kind, &evidence.membership, &result.membership) {
        (Kind::ProjectCreate, None, None) if area.is_none() => {}
        (Kind::ProjectAddIssue, Some(scope), Some(member))
            if scope.valid()
                && area == Some(scope.area_id.as_str())
                && scope.project_id == result.project_id
                && scope.area_id == member.area_id
                && scope.issue_id == member.issue_id
                && scope.expected_project_revision != result.project_revision
                && scope.expected_issue_revision != member.issue_revision
                && is_uuid_v7(&member.issue_revision) => {}
        _ => return Err(workflow::unverified()),
    }
    Ok(ScopedProject {
        result,
        membership: evidence.membership,
    })
}

pub(in crate::dolt) fn verify_effect(
    snapshot: &mut Snapshot,
    introduced: &str,
    receipt: &HistoricalReceipt,
    scoped: ScopedProject,
) -> Result<Value, HiveError> {
    // The receipt introduction verifier validated both commit hashes.
    let base = &receipt.base_commit;
    let result = &scoped.result;
    let expected = if let (Some(scope), Some(member)) = (scoped.membership, &result.membership) {
        verify_membership(snapshot, base, introduced, result, &scope, member)?;
        if Intent::AddIssue(scope).fingerprint(&snapshot.workspace_id, &receipt.actor_ref)
            != receipt.fingerprint
        {
            return Err(workflow::unverified());
        }
        &[
            ("issues", 0, 0, 1),
            ("operation_receipts", 1, 0, 0),
            ("project_issues", 1, 0, 0),
            ("projects", 0, 0, 1),
        ][..]
    } else {
        let prior: Vec<String> = snapshot.exec_rows(
            |_| format!("SELECT id FROM projects AS OF '{base}' WHERE id = ? LIMIT 1"),
            (&result.project_id,),
        )?;
        let rows: Vec<String> = snapshot.exec_rows(
            |_| {
                format!(
                    "SELECT name FROM projects AS OF '{introduced}'
                 WHERE CAST(id AS BINARY) = ? AND CAST(revision AS BINARY) = ?
                 AND CAST(status AS BINARY) = 'active' AND description IS NULL LIMIT 2"
                )
            },
            (&result.project_id, &result.project_revision),
        )?;
        let [name] = rows.as_slice() else {
            return Err(workflow::unverified());
        };
        if !prior.is_empty()
            || !valid_title(name)
            || Intent::Create(name).fingerprint(&snapshot.workspace_id, &receipt.actor_ref)
                != receipt.fingerprint
        {
            return Err(workflow::unverified());
        }
        &[("operation_receipts", 1, 0, 0), ("projects", 1, 0, 0)][..]
    };
    verify_delta(&mut snapshot.connection, base, introduced, expected)?;
    Ok(json!(result))
}

fn verify_membership(
    snapshot: &mut Snapshot,
    base: &str,
    introduced: &str,
    result: &ResultRow,
    scope: &Membership,
    member: &MemberResult,
) -> Result<(), HiveError> {
    require_active_area_at(
        &mut snapshot.connection,
        &scope.area_id,
        base,
        ARCHIVED_AREA,
    )?;
    let project: Vec<bool> = snapshot.exec_rows(
        |_| {
            format!(
                "SELECT CAST(b.revision AS BINARY) = ? AND CAST(a.revision AS BINARY) = ?
                AND CAST(b.name AS BINARY) = CAST(a.name AS BINARY)
                AND (CAST(b.description AS BINARY) <=> CAST(a.description AS BINARY))
                AND CAST(b.status AS BINARY) = 'active' AND CAST(a.status AS BINARY) = 'active'
             FROM projects AS OF '{base}' b JOIN projects AS OF '{introduced}' a
                ON CAST(b.id AS BINARY) = CAST(a.id AS BINARY)
             WHERE CAST(b.id AS BINARY) = ? LIMIT 2"
            )
        },
        (
            &scope.expected_project_revision,
            &result.project_revision,
            &scope.project_id,
        ),
    )?;
    let issue: Vec<bool> = snapshot.exec_rows(
        |_| format!(
            "SELECT CAST(b.revision AS BINARY) = ? AND CAST(a.revision AS BINARY) = ?
                AND CAST(b.area_id AS BINARY) = ? AND CAST(a.area_id AS BINARY) = ?
                AND b.number = a.number
                AND (CAST(b.parent_id AS BINARY) <=> CAST(a.parent_id AS BINARY))
                AND CAST(b.title AS BINARY) = CAST(a.title AS BINARY)
                AND (CAST(b.body AS BINARY) <=> CAST(a.body AS BINARY))
                AND (CAST(b.acceptance_criteria AS BINARY) <=> CAST(a.acceptance_criteria AS BINARY))
                AND CAST(b.type AS BINARY) = CAST(a.type AS BINARY)
                AND CAST(b.status AS BINARY) = CAST(a.status AS BINARY)
                AND (b.priority <=> a.priority)
                AND (CAST(b.hold_reason AS BINARY) <=> CAST(a.hold_reason AS BINARY))
                AND b.created_at = a.created_at AND b.updated_at = a.updated_at
                AND (b.closed_at <=> a.closed_at)
                AND (CAST(b.closed_by_ref AS BINARY) <=> CAST(a.closed_by_ref AS BINARY))
                AND (CAST(b.resolution_note AS BINARY) <=> CAST(a.resolution_note AS BINARY))
             FROM issues AS OF '{base}' b JOIN issues AS OF '{introduced}' a
                ON CAST(b.id AS BINARY) = CAST(a.id AS BINARY)
             WHERE CAST(b.id AS BINARY) = ? LIMIT 2"
        ),
        (&scope.expected_issue_revision, &member.issue_revision, &scope.area_id, &scope.area_id, &scope.issue_id),
    )?;
    let links: Vec<(bool, bool)> = snapshot.exec_rows(
        |_| format!(
            "SELECT EXISTS(SELECT 1 FROM project_issues AS OF '{base}' WHERE project_id = ? AND issue_id = ?),
                    EXISTS(SELECT 1 FROM project_issues AS OF '{introduced}'
                           WHERE CAST(project_id AS BINARY) = ? AND CAST(issue_id AS BINARY) = ?)"
        ),
        (&scope.project_id, &scope.issue_id, &scope.project_id, &scope.issue_id),
    )?;
    if project != [true] || issue != [true] || links != [(false, true)] {
        return Err(workflow::unverified());
    }
    Ok(())
}
