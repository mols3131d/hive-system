//! Inspect one request in the committed, authenticated workspace snapshot.

use crate::cli::{CommonOptions, OperationShowArgs};
use crate::error::{ErrorCode, HiveError};

use super::{DoltFailure, DoltSuccess, DoltTarget, is_uuid, is_uuid_v7};

pub(crate) fn show(
    common: &CommonOptions,
    args: &OperationShowArgs,
) -> Result<DoltSuccess, Box<DoltFailure>> {
    let request = exact_id(
        args.request_id.as_deref(),
        is_uuid,
        "Select a canonical request UUID.",
    )?;
    let area = exact_id(
        args.area.as_deref(),
        is_uuid_v7,
        "Select an exact Area UUIDv7.",
    )?;
    let issue = exact_id(
        args.issue.as_deref(),
        is_uuid_v7,
        "Select an exact Issue UUIDv7.",
    )?;
    let invocation = exact_id(
        args.invocation_id.as_deref(),
        is_uuid,
        "Select a canonical invocation UUID.",
    )?;
    let (Some(actor), Some(path)) = (args.actor.as_deref(), args.evidence_file.as_deref()) else {
        return Err(DoltFailure::without_context(HiveError::new(
            ErrorCode::AuthorityUnverified,
            "Operation inspection requires an explicit actor and current workflow evidence.",
        )));
    };
    let target = DoltTarget::parse(&args.target).map_err(DoltFailure::without_context)?;

    #[cfg(target_os = "linux")]
    {
        use super::context_evidence::read_evidence_bytes;
        use super::context_workflow as workflow;

        if !workflow::valid_ref(actor) {
            return Err(DoltFailure::without_context(HiveError::new(
                ErrorCode::InvalidArgument,
                "Supply a bounded nonempty actor reference.",
            )));
        }
        let workspace = crate::config_v2::read_explicit(
            common.workspace.as_deref().expect("validated workspace"),
        )
        .map_err(DoltFailure::without_context)?;
        let evidence = read_evidence_bytes(path, workflow::MAX_DECLARATION_BYTES)
            .map_err(DoltFailure::without_context)?;
        let selected = Selection {
            request,
            area,
            issue,
            actor,
            invocation,
            evidence: &evidence,
        };
        crate::workspace_status::with_operation_snapshot(&workspace, target, |snapshot, pending| {
            inspect(snapshot, workspace.policy_revision(), &selected, pending)
        })
        .map_err(DoltFailure::without_context)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (
            common, target, request, area, issue, actor, invocation, path,
        );
        Err(DoltFailure::without_context(HiveError::new(
            ErrorCode::NotImplemented,
            "Operation inspection requires a verified Linux workspace runtime.",
        )))
    }
}

fn exact_id<'a>(
    value: Option<&'a str>,
    valid: fn(&str) -> bool,
    message: &'static str,
) -> Result<&'a str, Box<DoltFailure>> {
    value.filter(|id| valid(id)).ok_or_else(|| {
        DoltFailure::without_context(HiveError::new(ErrorCode::InvalidArgument, message))
    })
}

#[cfg(target_os = "linux")]
use super::{Snapshot, context_workflow as workflow, receipt_history};
#[cfg(target_os = "linux")]
use serde_json::{Value, json};

#[cfg(target_os = "linux")]
struct Selection<'a> {
    request: &'a str,
    area: &'a str,
    issue: &'a str,
    actor: &'a str,
    invocation: &'a str,
    evidence: &'a [u8],
}

#[cfg(target_os = "linux")]
fn inspect(
    snapshot: &mut Snapshot,
    policy_revision: &str,
    selected: &Selection<'_>,
    pending: bool,
) -> Result<DoltSuccess, HiveError> {
    let observed = super::context_resolve::observe_time()?;
    let commit = snapshot.commit.clone();
    workflow::verify(
        &workflow::Request {
            actor_ref: selected.actor,
            bot_id: None,
            invocation_id: selected.invocation,
            observed_at: observed,
            input: Some(workflow::CurrentInput::OperatorRequest(selected.evidence)),
        },
        &workflow::Expected {
            workspace_id: &snapshot.workspace_id,
            db_commit: &commit,
            area_id: selected.area,
            issue_id: selected.issue,
            operation: workflow::Operation::InspectOperationReceipt,
            record_kind: workflow::RecordKind::OperationReceipt,
            record_id: selected.request,
            record_revision: &commit,
        },
    )?;
    let issues: Vec<(String, String)> = snapshot.exec_rows(
        |commit| {
            format!(
                "SELECT i.area_id, i.status FROM issues AS OF '{commit}' AS i
            JOIN areas AS OF '{commit}' AS a ON CAST(a.id AS BINARY) = CAST(i.area_id AS BINARY)
            WHERE CAST(i.id AS BINARY) = ? LIMIT 2"
            )
        },
        (selected.issue,),
    )?;
    let [(owner, status)] = issues.as_slice() else {
        return Err(workflow::unverified());
    };
    if owner != selected.area {
        return Err(workflow::unverified());
    }

    let Some(receipt) = receipt_history::receipt_at(snapshot, selected.request, &commit, None)
        .map_err(|_| uncertain())?
    else {
        return Err(if pending {
            uncertain()
        } else {
            HiveError::new(
                ErrorCode::NotFound,
                "The request is absent from the verified committed snapshot.",
            )
        });
    };
    let (introduced, result) = if receipt.kind == super::issue_create::KIND {
        let result = super::issue_create::check_scope(
            snapshot,
            selected.request,
            selected.area,
            Some(selected.issue),
            selected.actor,
            &receipt,
        )?;
        let introduced =
            receipt_history::receipt_introduction(snapshot, selected.request, &receipt)
                .map_err(|_| uncertain())?;
        super::issue_create::verify_effect(snapshot, &introduced, &receipt, &result)
            .map_err(|_| uncertain())?;
        (introduced, json!(result))
    } else {
        let result = super::issue_note_receipt::check_scope(
            snapshot,
            selected.request,
            selected.area,
            selected.issue,
            selected.actor,
            &receipt,
        )?;
        let introduced =
            receipt_history::receipt_introduction(snapshot, selected.request, &receipt)
                .map_err(|_| uncertain())?;
        super::issue_note_receipt::verify_effect(snapshot, &introduced, &receipt, &result, None)
            .map_err(|_| uncertain())?;
        (introduced, json!(result.result))
    };
    let claim = current_claim(snapshot, selected.issue, status)?;
    let observed_at = workflow::format_observed_at(observed);
    Ok(DoltSuccess {
        context: json!({
            "workspace_id": snapshot.workspace_id, "db_commit": commit,
            "policy_revision": policy_revision, "observed_at": observed_at,
            "scope": {"area_id": selected.area, "issue_id": selected.issue},
        }),
        data: json!({
            "request_id": selected.request, "kind": receipt.kind, "outcome": "confirmed",
            "base_commit": receipt.base_commit, "confirmed_commit": introduced,
            "result": result, "current_claim": claim,
        }),
    })
}

#[cfg(target_os = "linux")]
fn current_claim(snapshot: &mut Snapshot, issue: &str, status: &str) -> Result<Value, HiveError> {
    let claims: Vec<(String, String)> = snapshot.exec_rows(
        |commit| format!("SELECT claim_id, holder_ref FROM issue_claims AS OF '{commit}' WHERE CAST(issue_id AS BINARY) = ? LIMIT 2"),
        (issue,),
    )?;
    match claims.as_slice() {
        [] if matches!(status, "backlog" | "todo" | "review" | "done" | "cancelled") => {
            Ok(Value::Null)
        }
        [(claim, holder)]
            if status == "in-progress" && is_uuid_v7(claim) && workflow::valid_ref(holder) =>
        {
            Ok(json!({"claim_id": claim, "holder_ref": holder}))
        }
        _ => Err(workflow::unverified()),
    }
}

#[cfg(target_os = "linux")]
fn uncertain() -> HiveError {
    HiveError::new(
        ErrorCode::CommitUncertain,
        "The durable outcome could not be established.",
    )
}
