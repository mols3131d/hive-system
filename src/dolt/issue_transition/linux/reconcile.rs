//! Issue closure and reopening rules shared by the writer and historical verification.

use mysql::prelude::Queryable;

use super::{
    Effect, HistoricalReceipt, Operation, Record, Request, as_of, digest, invalid, invalid_record,
    read_error, workflow,
};
use crate::dolt::{
    issue_readiness::NON_WHITESPACE,
    records::{self, IssueStatus, RecordReader},
};
use crate::error::HiveError;

pub(super) const MAX_REASON_BYTES: usize = 4096;

pub(super) fn reason_digest(reason: &str) -> Result<String, HiveError> {
    if reason.len() > MAX_REASON_BYTES || reason.trim().is_empty() {
        return Err(invalid(
            "Supply a nonblank reason of at most 4096 UTF-8 bytes.",
        ));
    }
    Ok(digest(reason.as_bytes()))
}

pub(super) fn next_status(operation: Operation, status: IssueStatus) -> Option<&'static str> {
    match (operation, status) {
        (Operation::Cancel, IssueStatus::Backlog | IssueStatus::Todo | IssueStatus::Review) => {
            Some("cancelled")
        }
        (Operation::Reopen, IssueStatus::Done | IssueStatus::Cancelled) => Some("todo"),
        _ => None,
    }
}

pub(super) fn check_closure(
    db: &mut impl Queryable,
    request: &Request<'_>,
    commit: Option<&str>,
) -> Result<(), HiveError> {
    if request.operation == Operation::Reopen {
        let at = as_of(commit)?;
        let closure: Option<(Option<String>, bool)> = db
            .exec_first(
                format!(
                    "SELECT closed_by_ref,
                         COALESCE(YEAR(closed_at) > 0 AND resolution_note REGEXP ?, FALSE)
                         FROM issues{at} WHERE CAST(id AS BINARY) = ?"
                ),
                (NON_WHITESPACE, request.issue),
            )
            .map_err(read_error)?;
        if !matches!(closure, Some((Some(actor), true)) if workflow::valid_ref(&actor)) {
            return Err(invalid_record(
                "The terminal Issue has invalid closure fields.",
            ));
        }
    }
    Ok(())
}

pub(super) fn check_graph(
    reader: &mut RecordReader<'_>,
    request: &Request<'_>,
) -> Result<(), HiveError> {
    records::check_issue_reconciliation(
        reader,
        request.issue,
        request.operation == Operation::Reopen,
    )
}

pub(super) fn effect(request: &Request<'_>) -> Result<Effect, HiveError> {
    let reason_sha256 = request
        .reason_sha256
        .ok_or_else(workflow::unverified)?
        .to_owned();
    match request.operation {
        Operation::Cancel => Ok(Effect::Cancel { reason_sha256 }),
        Operation::Reopen => Ok(Effect::Reopen { reason_sha256 }),
        _ => Err(workflow::unverified()),
    }
}

pub(super) fn stage_issue(
    db: &mut impl Queryable,
    request: &Request<'_>,
    record: &Record,
    sql_time: &str,
    reason: &str,
) -> Result<(), HiveError> {
    let cancel = request.operation == Operation::Cancel;
    db.exec_drop(
        "UPDATE issues SET status = ?, revision = ?, updated_at = ?,
         closed_at = ?, closed_by_ref = ?, resolution_note = ?
         WHERE CAST(id AS BINARY) = ? AND CAST(area_id AS BINARY) = ?
           AND CAST(revision AS BINARY) = ? AND CAST(status AS BINARY) = ?",
        (
            &record.status,
            &record.issue_revision,
            sql_time,
            cancel.then_some(sql_time),
            cancel.then_some(request.actor),
            cancel.then_some(reason),
            request.issue,
            request.area,
            request.expected,
            &record.previous_status,
        ),
    )
    .map_err(read_error)
}

pub(super) fn verify_issue(
    db: &mut mysql::Conn,
    request: &Request<'_>,
    introduced: &str,
    receipt: &HistoricalReceipt,
) -> Result<(), HiveError> {
    check_closure(db, request, Some(&receipt.base_commit))?;
    check_graph(
        &mut RecordReader::Historical {
            connection: db,
            commit: &receipt.base_commit,
        },
        request,
    )?;
    let at = as_of(Some(introduced))?;
    let closure: Option<(Option<String>, Option<String>, Option<String>)> = db
        .exec_first(
            format!(
                "SELECT DATE_FORMAT(closed_at, '%Y-%m-%dT%H:%i:%s.%fZ'), closed_by_ref,
                     LOWER(SHA2(CAST(resolution_note AS BINARY), 256))
                     FROM issues{at} WHERE CAST(id AS BINARY) = ?"
            ),
            (request.issue,),
        )
        .map_err(read_error)?;
    let Some((closed_at, actor, digest)) = closure else {
        return Err(workflow::unverified());
    };
    let cancel = request.operation == Operation::Cancel;
    if closed_at.as_deref() != cancel.then_some(receipt.created_at.as_str())
        || actor.as_deref() != cancel.then_some(request.actor)
        || digest.as_deref() != request.reason_sha256.filter(|_| cancel)
    {
        return Err(workflow::unverified());
    }
    Ok(())
}
