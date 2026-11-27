use mysql::{Transaction, prelude::Queryable};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use time::OffsetDateTime;

use crate::cli::{BotRetireArgs, CommonOptions};
use crate::dolt::{
    DoltSuccess, DoltTarget, Snapshot, context_workflow as workflow, is_commit_hash, is_uuid,
    is_uuid_v7,
    issue_writer::{
        self, Decision, Kind, Mutation, StagedMutation, WriteIdentity, digest, invalid, new_uuid_v7,
    },
    read_error,
    receipt_history::{HistoricalReceipt, verify_delta},
};
use crate::error::{ErrorCode, HiveError};

use super::{BotMetadata, BotRow, COLUMNS};

pub(in crate::dolt) const KIND: &str = "bot.retire";

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ExecutionReconciliation {
    bot_id: String,
    db_commit: String,
    reconciled: bool,
    source_ref: String,
    observed_at: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RetireEvidence {
    version: u8,
    operation: String,
    workspace_id: String,
    request_id: String,
    fingerprint: String,
    actor_ref: String,
    expected_bot_revision: String,
    execution: ExecutionReconciliation,
    authorization_ref: String,
    evidence_id: String,
    revision: String,
    source_ref: String,
    issuer_ref: String,
    reviewer_ref: String,
    reviewed_at: String,
    valid_from: String,
    expires_at: String,
    withdrawn: bool,
}

impl RetireEvidence {
    fn fingerprint(&self, workspace: &str, actor: &str, bot: &str, revision: &str) -> String {
        digest(
            json!([
                1,
                KIND,
                workspace,
                actor,
                bot,
                revision,
                self.execution.db_commit,
                self.execution.reconciled,
                self.execution.source_ref,
                self.execution.observed_at,
            ])
            .to_string()
            .as_bytes(),
        )
    }

    fn verifies(
        &self,
        workspace: &str,
        request: &str,
        actor: &str,
        bot: &str,
        fingerprint: &str,
        now: OffsetDateTime,
    ) -> bool {
        self.version == 1
            && self.operation == KIND
            && self.workspace_id == workspace
            && self.request_id == request
            && self.actor_ref == actor
            && self.execution.bot_id == bot
            && self.execution.reconciled
            && is_commit_hash(&self.execution.db_commit)
            && is_uuid_v7(&self.expected_bot_revision)
            && self.fingerprint == fingerprint
            && self.fingerprint
                == self.fingerprint(workspace, actor, bot, &self.expected_bot_revision)
            && !self.withdrawn
            && [
                &self.authorization_ref,
                &self.evidence_id,
                &self.revision,
                &self.source_ref,
                &self.issuer_ref,
                &self.reviewer_ref,
                &self.execution.source_ref,
            ]
            .into_iter()
            .all(|value| workflow::valid_ref(value))
            && matches!(
                (
                    workflow::parse_time(&self.valid_from),
                    workflow::parse_time(&self.execution.observed_at),
                    workflow::parse_time(&self.reviewed_at),
                    workflow::parse_time(&self.expires_at),
                ),
                (Some(start), Some(observed), Some(reviewed), Some(end))
                    if start <= observed && observed <= reviewed && reviewed <= now && now < end
            )
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(in crate::dolt) struct RetireResult {
    version: u8,
    bot_id: String,
    bot_revision: String,
}

pub(super) fn write(
    common: &CommonOptions,
    args: &BotRetireArgs,
) -> Result<DoltSuccess, HiveError> {
    let target = DoltTarget::parse(&args.target)?;
    if !is_uuid_v7(&args.bot_id) || !is_uuid_v7(&args.if_match) {
        return Err(invalid(
            "Supply the exact Queen UUIDv7 and expected revision UUIDv7.",
        ));
    }
    if !is_uuid(&args.request_id) || !workflow::valid_ref(&args.actor) {
        return Err(invalid("Supply a request UUID and an actor reference."));
    }
    let workspace =
        crate::config_v2::read_explicit(common.workspace.as_deref().expect("validated workspace"))?;
    let decision: Decision<RetireEvidence> = issue_writer::read_decision(&args.evidence_file)?;
    let retire = Retire {
        args,
        workspace_id: workspace.workspace_id(),
        fingerprint: decision.evidence.fingerprint(
            workspace.workspace_id(),
            &args.actor,
            &args.bot_id,
            &args.if_match,
        ),
        decision,
    };
    issue_writer::execute(&workspace, target, &retire)
}

struct Retire<'a> {
    args: &'a BotRetireArgs,
    workspace_id: &'a str,
    fingerprint: String,
    decision: Decision<RetireEvidence>,
}

impl Mutation for Retire<'_> {
    type Guard = ();

    fn identity(&self) -> WriteIdentity<'_> {
        WriteIdentity {
            kind: Kind::BotRetire,
            request_id: &self.args.request_id,
            area_id: None,
            record_id: Some(&self.args.bot_id),
            actor: &self.args.actor,
            fingerprint: &self.fingerprint,
            authorization_ref: &self.decision.evidence.authorization_ref,
        }
    }

    fn verify_decision(&self, now: OffsetDateTime) -> Result<(), HiveError> {
        if self.decision.decision == "allow"
            && self.decision.evidence.expected_bot_revision == self.args.if_match
            && self.decision.evidence.verifies(
                self.workspace_id,
                &self.args.request_id,
                &self.args.actor,
                &self.args.bot_id,
                &self.fingerprint,
                now,
            )
        {
            Ok(())
        } else {
            Err(workflow::unverified())
        }
    }

    fn evidence(&self) -> Result<String, HiveError> {
        issue_writer::encode(&self.decision.evidence, Kind::BotRetire)
    }

    fn check_scope(&self, _: &mut Snapshot) -> Result<(), HiveError> {
        Ok(())
    }

    fn guard(&self, db: &mut Transaction<'_>, snapshot: &mut Snapshot) -> Result<(), HiveError> {
        if snapshot.commit != self.decision.evidence.execution.db_commit {
            return Err(HiveError::new(
                ErrorCode::StaleRevision,
                "The execution reconciliation must observe the current committed workspace.",
            ));
        }
        let base = snapshot.commit.clone();
        let bot_id = &self.args.bot_id;
        let rows = bot_at(snapshot, &base, bot_id)?;
        let [(id, .., revision, None)] = rows.as_slice() else {
            return Err(HiveError::new(
                ErrorCode::NotFound,
                "The Queen is absent from the active committed registry.",
            ));
        };
        BotMetadata::try_from(rows[0].clone())?;
        if id != bot_id || revision != &self.args.if_match {
            return Err(HiveError::new(
                ErrorCode::StaleRevision,
                "The active Queen revision changed.",
            ));
        }
        let current: Vec<BotRow> = db
            .exec(
                format!("SELECT {COLUMNS} FROM bots WHERE CAST(id AS BINARY) = ? LIMIT 2"),
                (bot_id,),
            )
            .map_err(read_error)?;
        if rows != current {
            return Err(HiveError::new(
                ErrorCode::DirtyConflict,
                "The Queen transaction does not match the committed identity.",
            ));
        }
        let committed_empty = claims_empty_at(snapshot, &base)?;
        let transactional: Option<bool> = db
            .query_first("SELECT EXISTS(SELECT 1 FROM issue_claims)")
            .map_err(read_error)?;
        // Claim holders are opaque execution references. Any row can leave
        // execution related to the selected Queen uncertain.
        if !committed_empty || transactional != Some(false) {
            return Err(HiveError::new(
                ErrorCode::AlreadyClaimed,
                "Retirement requires zero active Claims in the selected workspace.",
            ));
        }
        Ok(())
    }

    fn stage(
        &self,
        db: &mut Transaction<'_>,
        (): (),
        now: OffsetDateTime,
        sql_time: &str,
    ) -> Result<StagedMutation, HiveError> {
        let result = RetireResult {
            version: 1,
            bot_id: self.args.bot_id.clone(),
            bot_revision: new_uuid_v7(now)?,
        };
        db.exec_drop(
            "UPDATE bots SET revision = ?, retired_at = ?
             WHERE CAST(id AS BINARY) = ? AND CAST(revision AS BINARY) = ? AND retired_at IS NULL",
            (
                &result.bot_revision,
                sql_time,
                &result.bot_id,
                &self.args.if_match,
            ),
        )
        .map_err(read_error)?;
        if db.affected_rows() != 1 {
            return Err(HiveError::new(
                ErrorCode::StaleRevision,
                "The active Queen revision changed.",
            ));
        }
        Ok(StagedMutation {
            record_id: result.bot_id.clone(),
            result: issue_writer::encode(&result, Kind::BotRetire)?,
        })
    }
}

pub(in crate::dolt) fn check_scope(
    snapshot: &Snapshot,
    request: &str,
    bot: &str,
    actor: &str,
    receipt: &HistoricalReceipt,
) -> Result<RetireResult, HiveError> {
    let result: RetireResult =
        serde_json::from_str(&receipt.result).map_err(|_| workflow::unverified())?;
    let evidence: RetireEvidence =
        serde_json::from_str(&receipt.workflow_evidence).map_err(|_| workflow::unverified())?;
    if receipt.kind != KIND
        || receipt.actor_ref != actor
        || result.version != 1
        || result.bot_id != bot
        || !is_uuid_v7(bot)
        || !is_uuid_v7(&result.bot_revision)
        || result.bot_revision == evidence.expected_bot_revision
        || evidence.execution.db_commit != receipt.base_commit
        || evidence.authorization_ref != receipt.authorization_ref
        || !workflow::parse_time(&receipt.created_at).is_some_and(|created| {
            evidence.verifies(
                &snapshot.workspace_id,
                request,
                actor,
                bot,
                &receipt.fingerprint,
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
    result: &RetireResult,
) -> Result<Value, HiveError> {
    let evidence: RetireEvidence =
        serde_json::from_str(&receipt.workflow_evidence).map_err(|_| workflow::unverified())?;
    let base = &receipt.base_commit;
    let prior = bot_at(snapshot, base, &result.bot_id)?;
    let [(id, slug, name, revision, None)] = prior.as_slice() else {
        return Err(workflow::unverified());
    };
    if id != &result.bot_id
        || revision != &evidence.expected_bot_revision
        || !claims_empty_at(snapshot, base)?
    {
        return Err(workflow::unverified());
    }
    let retired = (
        id.clone(),
        slug.clone(),
        name.clone(),
        result.bot_revision.clone(),
        Some(receipt.created_at.clone()),
    );
    if bot_at(snapshot, introduced, id)? != [retired.clone()] {
        return Err(workflow::unverified());
    }
    verify_delta(
        &mut snapshot.connection,
        base,
        introduced,
        &[("bots", 0, 0, 1), ("operation_receipts", 1, 0, 0)],
    )?;
    let mut response = json!(result);
    response["bot"] = json!(BotMetadata::try_from(retired)?);
    Ok(response)
}

fn bot_at(snapshot: &mut Snapshot, commit: &str, bot: &str) -> Result<Vec<BotRow>, HiveError> {
    // Commit literals come from the verified snapshot or receipt history.
    snapshot.exec_rows(
        |_| {
            format!(
                "SELECT {COLUMNS} FROM bots AS OF '{commit}' WHERE CAST(id AS BINARY) = ? LIMIT 2"
            )
        },
        (bot,),
    )
}

fn claims_empty_at(snapshot: &mut Snapshot, commit: &str) -> Result<bool, HiveError> {
    let rows: Vec<bool> = snapshot.exec_rows(
        |_| format!("SELECT EXISTS(SELECT 1 FROM issue_claims AS OF '{commit}')"),
        (),
    )?;
    Ok(rows.as_slice() == [false])
}
