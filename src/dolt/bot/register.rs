use std::path::Path;

use mysql::{Transaction, prelude::Queryable};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use time::OffsetDateTime;

use crate::cli::{BotRegisterArgs, CommonOptions, is_slug};
use crate::config_git::{PinnedAssets, is_full_lowercase_oid};
use crate::config_v2::Candidate;
use crate::dolt::{
    DoltSuccess, DoltTarget, Snapshot, context_workflow as workflow, is_uuid, is_uuid_v7,
    issue_writer::{
        self, Decision, Kind, Mutation, StagedMutation, WriteIdentity, digest, invalid,
        new_uuid_v7, valid_title,
    },
    read_error,
    receipt_history::{HistoricalReceipt, verify_delta},
};
use crate::error::{ErrorCode, HiveError};

use super::{BotMetadata, BotRow, COLUMNS, soul_body};

pub(in crate::dolt) const KIND: &str = "bot.register";

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RegisterEvidence {
    version: u8,
    operation: String,
    workspace_id: String,
    request_id: String,
    fingerprint: String,
    actor_ref: String,
    asset_revision: String,
    authorization_ref: String,
    evidence_id: String,
    revision: String,
    source_ref: String,
    issuer_ref: String,
    valid_from: String,
    expires_at: String,
    withdrawn: bool,
}

impl RegisterEvidence {
    fn verifies(
        &self,
        workspace: &str,
        request: &str,
        actor: &str,
        fingerprint: &str,
        now: OffsetDateTime,
    ) -> bool {
        self.version == 1
            && self.operation == KIND
            && self.workspace_id == workspace
            && self.request_id == request
            && self.actor_ref == actor
            && self.fingerprint == fingerprint
            && is_full_lowercase_oid(&self.asset_revision)
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

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(in crate::dolt) struct RegisterResult {
    version: u8,
    bot_id: String,
    bot_revision: String,
    asset_revision: String,
}

struct Register<'a> {
    args: &'a BotRegisterArgs,
    workspace: &'a Candidate,
    fingerprint: String,
    decision: Decision<RegisterEvidence>,
}

pub(super) fn write(
    common: &CommonOptions,
    args: &BotRegisterArgs,
) -> Result<DoltSuccess, HiveError> {
    let target = DoltTarget::parse(&args.target)?;
    if !valid_slug(&args.slug) || !valid_title(&args.name) {
        return Err(invalid(
            "Supply a lowercase kebab-case slug of up to 100 characters and a nonblank name of up to 255 characters without controls.",
        ));
    }
    if !is_full_lowercase_oid(&args.asset_revision) {
        return Err(invalid(
            "Supply a full lowercase Git commit ID as --asset-revision.",
        ));
    }
    if !is_uuid(&args.request_id) || !workflow::valid_ref(&args.actor) {
        return Err(invalid("Supply a request UUID and an actor reference."));
    }
    let workspace =
        crate::config_v2::read_explicit(common.workspace.as_deref().expect("validated workspace"))?;
    let register = Register {
        args,
        workspace: &workspace,
        fingerprint: fingerprint(
            workspace.workspace_id(),
            &args.actor,
            &args.slug,
            &args.name,
            &args.asset_revision,
        ),
        decision: issue_writer::read_decision(&args.evidence_file)?,
    };
    issue_writer::execute(&workspace, target, &register)
}

fn valid_slug(slug: &str) -> bool {
    slug.len() <= 100 && is_slug(slug)
}

fn fingerprint(workspace: &str, actor: &str, slug: &str, name: &str, asset: &str) -> String {
    digest(
        json!([1, KIND, workspace, actor, slug, name, asset])
            .to_string()
            .as_bytes(),
    )
}

impl<'a> Mutation for Register<'a> {
    type Guard = PinnedAssets<'a>;

    fn identity(&self) -> WriteIdentity<'_> {
        WriteIdentity {
            kind: Kind::BotRegister,
            request_id: &self.args.request_id,
            area_id: None,
            record_id: None,
            actor: &self.args.actor,
            fingerprint: &self.fingerprint,
            authorization_ref: &self.decision.evidence.authorization_ref,
        }
    }

    fn verify_decision(&self, now: OffsetDateTime) -> Result<(), HiveError> {
        if self.decision.decision == "allow"
            && self.decision.evidence.asset_revision == self.args.asset_revision
            && self.decision.evidence.verifies(
                self.workspace.workspace_id(),
                &self.args.request_id,
                &self.args.actor,
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
        issue_writer::encode(&self.decision.evidence, Kind::BotRegister)
    }

    fn check_scope(&self, _: &mut Snapshot) -> Result<(), HiveError> {
        Ok(())
    }

    fn guard(&self, db: &mut Transaction<'_>, _: &mut Snapshot) -> Result<Self::Guard, HiveError> {
        let existing: Option<String> = db
            .exec_first(
                "SELECT id FROM bots WHERE slug = ? LIMIT 1",
                (&self.args.slug,),
            )
            .map_err(read_error)?;
        if existing.is_some() {
            return Err(HiveError::new(
                ErrorCode::AlreadyExists,
                "The selected Queen slug is already registered, including retired Queens.",
            ));
        }
        PinnedAssets::open(&self.workspace.root, &self.args.asset_revision)
    }

    fn stage(
        &self,
        db: &mut Transaction<'_>,
        assets: Self::Guard,
        now: OffsetDateTime,
        _: &str,
    ) -> Result<StagedMutation, HiveError> {
        let bot = BotMetadata::try_from((
            new_uuid_v7(now)?,
            self.args.slug.clone(),
            self.args.name.clone(),
            new_uuid_v7(now)?,
            None,
        ))?;
        let relative = format!("bots/{}/SOUL.md", bot.slug);
        let bytes = assets.read_blob(Path::new(&relative))?.ok_or_else(|| {
            HiveError::new(
                ErrorCode::NotFound,
                "The selected Queen's Soul is missing from the asset commit.",
            )
        })?;
        soul_body(&bytes, self.workspace.workspace_id(), &bot)?;
        db.exec_drop(
            "INSERT INTO bots (id, slug, display_name, revision) VALUES (?, ?, ?, ?)",
            (&bot.id, &bot.slug, &bot.display_name, &bot.revision),
        )
        .map_err(read_error)?;
        assets.recheck()?;
        let result = RegisterResult {
            version: 1,
            bot_id: bot.id.clone(),
            bot_revision: bot.revision,
            asset_revision: self.args.asset_revision.clone(),
        };
        Ok(StagedMutation {
            record_id: bot.id,
            result: issue_writer::encode(&result, Kind::BotRegister)?,
        })
    }
}

pub(in crate::dolt) fn check_scope(
    snapshot: &Snapshot,
    request: &str,
    bot: Option<&str>,
    actor: &str,
    receipt: &HistoricalReceipt,
) -> Result<RegisterResult, HiveError> {
    let result: RegisterResult =
        serde_json::from_str(&receipt.result).map_err(|_| workflow::unverified())?;
    let evidence: RegisterEvidence =
        serde_json::from_str(&receipt.workflow_evidence).map_err(|_| workflow::unverified())?;
    if receipt.kind != KIND
        || receipt.actor_ref != actor
        || result.version != 1
        || bot.is_some_and(|id| id != result.bot_id)
        || !is_uuid_v7(&result.bot_id)
        || !is_uuid_v7(&result.bot_revision)
        || result.asset_revision != evidence.asset_revision
        || evidence.authorization_ref != receipt.authorization_ref
        || !workflow::parse_time(&receipt.created_at).is_some_and(|created| {
            evidence.verifies(
                &snapshot.workspace_id,
                request,
                actor,
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
    result: &RegisterResult,
) -> Result<Value, HiveError> {
    // Receipt history validates both commit literals.
    let base = &receipt.base_commit;
    let rows: Vec<BotRow> = snapshot.exec_rows(
        |_| format!("SELECT {COLUMNS} FROM bots AS OF '{introduced}' WHERE CAST(id AS BINARY) = ? LIMIT 2"),
        (&result.bot_id,),
    )?;
    let [(id, slug, name, revision, None)] = rows.as_slice() else {
        return Err(workflow::unverified());
    };
    if id != &result.bot_id
        || revision != &result.bot_revision
        || !valid_slug(slug)
        || !valid_title(name)
        || fingerprint(
            &snapshot.workspace_id,
            &receipt.actor_ref,
            slug,
            name,
            &result.asset_revision,
        ) != receipt.fingerprint
    {
        return Err(workflow::unverified());
    }
    let prior: Vec<String> = snapshot.exec_rows(
        |_| format!("SELECT id FROM bots AS OF '{base}' WHERE id = ? OR slug = ? LIMIT 1"),
        (id, slug),
    )?;
    if !prior.is_empty() {
        return Err(workflow::unverified());
    }
    verify_delta(
        &mut snapshot.connection,
        base,
        introduced,
        &[("bots", 1, 0, 0), ("operation_receipts", 1, 0, 0)],
    )?;
    let mut response = json!(result);
    response["bot"] = json!(BotMetadata::try_from((
        id.clone(),
        slug.clone(),
        name.clone(),
        revision.clone(),
        None
    ))?);
    Ok(response)
}
