use mysql::{Transaction, prelude::Queryable};
use serde::{Deserialize, Serialize};
use serde_json::json;
use time::OffsetDateTime;

use super::{CommonOptions, DoltSuccess, ErrorCode, HiveError, IssueCreateArgs};
use crate::dolt::{
    DoltTarget, Snapshot,
    context_evidence::read_evidence_bytes,
    context_workflow as workflow, is_uuid, is_uuid_v7,
    issue_writer::{
        self, Decision, Kind, Mutation, StagedMutation, WriteIdentity, digest, invalid,
        new_uuid_v7, valid_title,
    },
    read_error,
    receipt_history::{HistoricalReceipt, valid_sha256},
    records::is_issue_type,
    scope::require_active_area,
};

pub(in crate::dolt) const KIND: &str = "issue.create";
const MAX_BODY_BYTES: usize = 256 * 1024;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(in crate::dolt) struct CreateResult {
    version: u8,
    area_id: String,
    pub(in crate::dolt) issue_id: String,
    number: u64,
    issue_revision: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CreateEvidence {
    version: u8,
    operation: String,
    workspace_id: String,
    request_id: String,
    fingerprint: String,
    actor_ref: String,
    bot_id: Option<String>,
    area_id: String,
    authorization_ref: String,
    evidence_id: String,
    revision: String,
    source_ref: String,
    issuer_ref: String,
    valid_from: String,
    expires_at: String,
    withdrawn: bool,
}

impl CreateEvidence {
    fn verifies(
        &self,
        workspace: &str,
        request: &str,
        fingerprint: &str,
        actor: &str,
        area: &str,
        now: OffsetDateTime,
    ) -> bool {
        self.version == 1
            && self.operation == KIND
            && self.workspace_id == workspace
            && self.request_id == request
            && self.fingerprint == fingerprint
            && self.actor_ref == actor
            && self.area_id == area
            && self.bot_id.is_none()
            && !self.withdrawn
            && [
                &self.authorization_ref,
                &self.evidence_id,
                &self.revision,
                &self.source_ref,
                &self.issuer_ref,
            ]
            .into_iter()
            .all(|value| workflow::valid_ref(value))
            && matches!((workflow::parse_time(&self.valid_from), workflow::parse_time(&self.expires_at)),
                (Some(start), Some(end)) if start <= now && now < end)
    }
}

struct Create<'a> {
    args: &'a IssueCreateArgs,
    workspace: &'a str,
    body: Option<String>,
    fingerprint: String,
    decision: Decision<CreateEvidence>,
}

pub(super) fn write(
    common: &CommonOptions,
    args: &IssueCreateArgs,
) -> Result<DoltSuccess, HiveError> {
    let target = DoltTarget::parse(&args.target)?;
    let workspace =
        crate::config_v2::read_explicit(common.workspace.as_deref().expect("validated workspace"))?;
    if !is_uuid_v7(&args.area) || !is_uuid(&args.request_id) || !workflow::valid_ref(&args.actor) {
        return Err(invalid(
            "Select an exact Area UUIDv7, a request UUID and an actor reference.",
        ));
    }
    if !valid_title(&args.title) {
        return Err(invalid(
            "Supply a nonblank Issue title of at most 255 characters without control characters.",
        ));
    }
    let body = args
        .file
        .as_ref()
        .map(|path| {
            let bytes = read_evidence_bytes(path, MAX_BODY_BYTES).map_err(|error| {
                let message = if matches!(error.code, ErrorCode::ContextLimit) {
                    "The Issue body exceeds the supported byte limit."
                } else {
                    "Could not read the explicit regular Issue body file."
                };
                HiveError::new(error.code, message)
            })?;
            String::from_utf8(bytes).map_err(|_| invalid("The Issue body must be UTF-8."))
        })
        .transpose()?;
    let fingerprint = fingerprint(
        workspace.workspace_id(),
        &args.area,
        &args.actor,
        &args.kind,
        &args.title,
        body.as_deref()
            .map(|text| digest(text.as_bytes()))
            .as_deref(),
    );
    let decision = issue_writer::read_decision(&args.evidence_file)?;
    let create = Create {
        args,
        workspace: workspace.workspace_id(),
        body,
        fingerprint,
        decision,
    };
    issue_writer::execute(&workspace, target, &create)
}

fn fingerprint(
    workspace: &str,
    area: &str,
    actor: &str,
    kind: &str,
    title: &str,
    body_sha256: Option<&str>,
) -> String {
    digest(
        json!([1, KIND, workspace, area, actor, kind, title, body_sha256])
            .to_string()
            .as_bytes(),
    )
}

impl Mutation for Create<'_> {
    type Guard = u64;

    fn identity(&self) -> WriteIdentity<'_> {
        WriteIdentity {
            kind: Kind::Create,
            request_id: &self.args.request_id,
            area_id: Some(&self.args.area),
            record_id: None,
            actor: &self.args.actor,
            fingerprint: &self.fingerprint,
            authorization_ref: &self.decision.evidence.authorization_ref,
        }
    }

    fn verify_decision(&self, now: OffsetDateTime) -> Result<(), HiveError> {
        let args = self.args;
        if self.decision.decision == "allow"
            && self.decision.evidence.verifies(
                self.workspace,
                &args.request_id,
                &self.fingerprint,
                &args.actor,
                &args.area,
                now,
            )
        {
            Ok(())
        } else {
            Err(workflow::unverified())
        }
    }

    fn evidence(&self) -> Result<String, HiveError> {
        issue_writer::encode(&self.decision.evidence, Kind::Create)
    }

    fn check_scope(&self, snapshot: &mut Snapshot) -> Result<(), HiveError> {
        let areas: Vec<String> = snapshot.exec_rows(
            |commit| {
                format!(
                    "SELECT id FROM areas AS OF '{commit}' WHERE CAST(id AS BINARY) = ? LIMIT 2"
                )
            },
            (&self.args.area,),
        )?;
        if matches!(areas.as_slice(), [area] if area == &self.args.area) {
            Ok(())
        } else {
            Err(workflow::unverified())
        }
    }

    fn guard(&self, db: &mut Transaction<'_>, snapshot: &mut Snapshot) -> Result<u64, HiveError> {
        require_active_area(
            db,
            &self.args.area,
            "Issue creation requires active Area ancestry.",
        )?;
        let next: Option<u64> = db.query_first(
            "SELECT next_value FROM workspace_counters WHERE CAST(name AS BINARY) = 'issue_number'",
        ).map_err(read_error)?;
        let number = next
            .filter(|number| *number > 0)
            .ok_or_else(invalid_counter)?;
        if number.checked_add(1).is_none() {
            return Err(HiveError::new(
                ErrorCode::InvalidRecord,
                "The Issue number counter is exhausted.",
            ));
        }

        let live: Option<u64> = db
            .exec_first(
                "SELECT number FROM issues WHERE number >= ? ORDER BY number LIMIT 1",
                (number,),
            )
            .map_err(read_error)?;
        if live.is_some() {
            return Err(invalid_counter());
        }
        let retained: Vec<u64> = snapshot.exec_rows(
            |commit| {
                format!(
                    "SELECT 1 FROM dolt_diff_issues AS OF '{commit}'
                 WHERE from_number >= ? OR to_number >= ? LIMIT 1"
                )
            },
            (number, number),
        )?;
        if !retained.is_empty() {
            return Err(invalid_counter());
        }
        // Dolt 2.7.6 can stop this iterator at a schema boundary and return
        // an empty result with a warning. Read diagnostics on the same
        // session before any other query replaces them.
        if snapshot
            .connection
            .query_first::<mysql::Row, _>("SHOW WARNINGS LIMIT 1")
            .map_err(read_error)?
            .is_some()
        {
            return Err(HiveError::new(
                ErrorCode::InvalidRecord,
                "Retained Issue number history could not be verified.",
            ));
        }
        Ok(number)
    }

    fn stage(
        &self,
        db: &mut Transaction<'_>,
        number: u64,
        now: OffsetDateTime,
        sql_time: &str,
    ) -> Result<StagedMutation, HiveError> {
        let args = self.args;
        let result = CreateResult {
            version: 1,
            area_id: args.area.clone(),
            issue_id: new_uuid_v7(now)?,
            number,
            issue_revision: new_uuid_v7(now)?,
        };
        db.exec_drop(
            "UPDATE workspace_counters SET next_value = ?
             WHERE CAST(name AS BINARY) = 'issue_number' AND next_value = ?",
            (number.checked_add(1).ok_or_else(invalid_counter)?, number),
        )
        .map_err(read_error)?;
        if db.affected_rows() != 1 {
            return Err(invalid_counter());
        }
        db.exec_drop(
            "INSERT INTO issues (id, number, area_id, title, body, type, status, revision, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, 'backlog', ?, ?, ?)",
            (&result.issue_id, number, &args.area, &args.title, &self.body, &args.kind, &result.issue_revision, sql_time, sql_time),
        ).map_err(read_error)?;
        let encoded = issue_writer::encode(&result, Kind::Create)?;
        Ok(StagedMutation {
            record_id: result.issue_id,
            result: encoded,
        })
    }
}

fn invalid_counter() -> HiveError {
    HiveError::new(
        ErrorCode::InvalidRecord,
        "The exact Issue number counter is missing, invalid or stale.",
    )
}

pub(in crate::dolt) fn check_scope(
    snapshot: &Snapshot,
    request: &str,
    area: &str,
    issue: Option<&str>,
    actor: &str,
    receipt: &HistoricalReceipt,
) -> Result<CreateResult, HiveError> {
    if receipt.kind != KIND || receipt.actor_ref != actor {
        return Err(workflow::unverified());
    }
    let result: CreateResult =
        serde_json::from_str(&receipt.result).map_err(|_| workflow::unverified())?;
    let evidence: CreateEvidence =
        serde_json::from_str(&receipt.workflow_evidence).map_err(|_| workflow::unverified())?;
    if result.version != 1
        || result.area_id != area
        || issue.is_some_and(|id| id != result.issue_id)
        || !is_uuid_v7(&result.issue_id)
        || !is_uuid_v7(&result.issue_revision)
        || result.number == 0
        || evidence.authorization_ref != receipt.authorization_ref
        || !workflow::parse_time(&receipt.created_at).is_some_and(|created| {
            evidence.verifies(
                &snapshot.workspace_id,
                request,
                &receipt.fingerprint,
                actor,
                area,
                created,
            )
        })
    {
        return Err(workflow::unverified());
    }
    Ok(result)
}

pub(in crate::dolt) fn verify_effect(
    snapshot: &mut Snapshot,
    introduced: &str,
    receipt: &HistoricalReceipt,
    result: &CreateResult,
) -> Result<(), HiveError> {
    // Both literals were validated by receipt_introduction.
    let counter = |snapshot: &mut Snapshot, commit: &str| {
        snapshot.exec_rows::<u64, _>(
            |_| {
                format!(
                    "SELECT next_value FROM workspace_counters AS OF '{commit}'
                         WHERE CAST(name AS BINARY) = 'issue_number' LIMIT 2"
                )
            },
            (),
        )
    };
    let base = &receipt.base_commit;
    let before = counter(snapshot, base)?;
    let after = counter(snapshot, introduced)?;
    let base_issues: Vec<String> = snapshot.exec_rows(
        |_| format!("SELECT id FROM issues AS OF '{base}' WHERE CAST(id AS BINARY) = ? OR number = ? LIMIT 1"),
        (&result.issue_id, result.number),
    )?;
    type IssueRow = (
        String,
        u64,
        String,
        String,
        String,
        String,
        bool,
        Option<String>,
    );
    let rows: Vec<IssueRow> = snapshot.exec_rows(
        |_| {
            format!(
                "SELECT area_id, number, revision, type, status, title,
                (created_at = updated_at AND DATE_FORMAT(created_at, '%Y-%m-%dT%H:%i:%s.%fZ') = ?
                 AND parent_id IS NULL AND acceptance_criteria IS NULL AND priority IS NULL
                 AND hold_reason IS NULL AND closed_at IS NULL AND closed_by_ref IS NULL
                 AND resolution_note IS NULL AND NOT EXISTS (
                   SELECT 1 FROM issue_claims AS OF '{introduced}' WHERE issue_id = i.id)),
                CASE WHEN body IS NULL THEN NULL
                     WHEN OCTET_LENGTH(body) <= ? THEN LOWER(SHA2(CAST(body AS BINARY), 256))
                     ELSE '' END
             FROM issues AS OF '{introduced}' i WHERE CAST(id AS BINARY) = ? LIMIT 2"
            )
        },
        (&receipt.created_at, MAX_BODY_BYTES, &result.issue_id),
    )?;
    let [(area, number, revision, kind, status, title, true, body_sha256)] = rows.as_slice() else {
        return Err(workflow::unverified());
    };
    if before != [result.number]
        || !matches!(after.as_slice(), [next] if result.number.checked_add(1) == Some(*next))
        || !base_issues.is_empty()
        || area != &result.area_id
        || number != &result.number
        || revision != &result.issue_revision
        || status != "backlog"
        || !is_issue_type(kind)
        || !valid_title(title)
        || body_sha256
            .as_deref()
            .is_some_and(|hash| !valid_sha256(hash))
        || fingerprint(
            &snapshot.workspace_id,
            area,
            &receipt.actor_ref,
            kind,
            title,
            body_sha256.as_deref(),
        ) != receipt.fingerprint
    {
        return Err(workflow::unverified());
    }
    Ok(())
}
