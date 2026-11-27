use mysql::{Transaction, prelude::Queryable};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use time::OffsetDateTime;

use crate::cli::{ArtifactUpdateArgs, CommonOptions};
use crate::dolt::{
    DoltSuccess, DoltTarget, Snapshot, context_workflow as workflow, is_uuid, is_uuid_v7,
    issue_writer::{
        self, Decision, Kind, Mutation, StagedMutation, WriteIdentity, digest, invalid,
        new_uuid_v7, valid_revision, valid_title,
    },
    read_error,
    receipt_history::{HistoricalReceipt, valid_sha256},
    records::is_artifact_kind,
    scope::{require_active_area, verify_area_ancestry},
};
use crate::error::{ErrorCode, HiveError};

use super::{
    MAX_BODY_BYTES, ScopeKind,
    writes::{read_body, verify_delta, verify_links},
};

pub(in crate::dolt) const KIND: &str = "artifact.update";
const ARCHIVED_AREA: &str = "Artifact updates require active Area ancestry.";

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct UpdateResult {
    version: u8,
    artifact_id: String,
    artifact_revision: String,
    area_id: Option<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct UpdateEvidence {
    version: u8,
    operation: String,
    workspace_id: String,
    request_id: String,
    fingerprint: String,
    actor_ref: String,
    bot_id: Option<String>,
    area_id: Option<String>,
    artifact_id: String,
    expected_artifact_revision: String,
    authorization_ref: String,
    evidence_id: String,
    revision: String,
    source_ref: String,
    issuer_ref: String,
    valid_from: String,
    expires_at: String,
    withdrawn: bool,
}

struct Scope<'a> {
    workspace: &'a str,
    area: Option<&'a str>,
    artifact: &'a str,
    actor: &'a str,
    expected: &'a str,
}

impl Scope<'_> {
    fn fingerprint(&self, body_sha256: &str) -> String {
        digest(
            json!([
                1,
                KIND,
                self.workspace,
                self.area,
                self.artifact,
                self.actor,
                self.expected,
                body_sha256,
            ])
            .to_string()
            .as_bytes(),
        )
    }

    fn verifies(
        &self,
        evidence: &UpdateEvidence,
        request: &str,
        fingerprint: &str,
        now: OffsetDateTime,
    ) -> bool {
        evidence.version == 1
            && evidence.operation == KIND
            && evidence.workspace_id == self.workspace
            && evidence.request_id == request
            && evidence.fingerprint == fingerprint
            && evidence.actor_ref == self.actor
            && evidence.area_id.as_deref() == self.area
            && evidence.artifact_id == self.artifact
            && evidence.expected_artifact_revision == self.expected
            && evidence.bot_id.is_none()
            && !evidence.withdrawn
            && [
                &evidence.authorization_ref,
                &evidence.evidence_id,
                &evidence.revision,
                &evidence.source_ref,
                &evidence.issuer_ref,
            ]
            .into_iter()
            .all(|value| workflow::valid_ref(value))
            && matches!((workflow::parse_time(&evidence.valid_from), workflow::parse_time(&evidence.expires_at)),
                (Some(start), Some(end)) if start <= now && now < end)
    }
}

struct Update<'a> {
    scope: Scope<'a>,
    request: &'a str,
    body: String,
    fingerprint: String,
    decision: Decision<UpdateEvidence>,
}

pub(super) fn write(
    common: &CommonOptions,
    args: &ArtifactUpdateArgs,
) -> Result<DoltSuccess, HiveError> {
    let target = DoltTarget::parse(&args.target)?;
    let workspace =
        crate::config_v2::read_explicit(common.workspace.as_deref().expect("validated workspace"))?;
    let area = args.area.as_deref();
    if !is_uuid_v7(&args.id) || area.is_some_and(|id| !is_uuid_v7(id)) {
        return Err(invalid(
            "Supply exact lowercase UUIDv7 Artifact and optional Area IDs.",
        ));
    }
    let expected = args
        .if_match
        .split_once('=')
        .filter(|(id, rev)| *id == args.id && valid_revision(rev))
        .map(|(_, rev)| rev)
        .ok_or_else(|| invalid("Supply one exact --if-match ARTIFACT=REV."))?;
    if !is_uuid(&args.request_id) || !workflow::valid_ref(&args.actor) {
        return Err(invalid("Supply a request UUID and an actor reference."));
    }

    let body = read_body(&args.file)?;
    let scope = Scope {
        workspace: workspace.workspace_id(),
        area,
        artifact: &args.id,
        actor: &args.actor,
        expected,
    };
    let update = Update {
        fingerprint: scope.fingerprint(&digest(body.as_bytes())),
        scope,
        request: &args.request_id,
        body,
        decision: issue_writer::read_decision(&args.evidence_file)?,
    };
    issue_writer::execute(&workspace, target, &update)
}

impl Mutation for Update<'_> {
    type Guard = ();

    fn identity(&self) -> WriteIdentity<'_> {
        WriteIdentity {
            kind: Kind::ArtifactUpdate,
            request_id: self.request,
            area_id: self.scope.area,
            record_id: Some(self.scope.artifact),
            actor: self.scope.actor,
            fingerprint: &self.fingerprint,
            authorization_ref: &self.decision.evidence.authorization_ref,
        }
    }

    fn verify_decision(&self, now: OffsetDateTime) -> Result<(), HiveError> {
        if self.decision.decision == "allow"
            && self.scope.verifies(
                &self.decision.evidence,
                self.request,
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
        issue_writer::encode(&self.decision.evidence, Kind::ArtifactUpdate)
    }

    fn check_scope(&self, snapshot: &mut Snapshot) -> Result<(), HiveError> {
        let head = snapshot.commit.clone();
        verify_links(snapshot, &head, self.scope.artifact, self.scope.area)?;
        if let Some(area) = self.scope.area {
            super::Scope {
                kind: ScopeKind::Area,
                id: area,
            }
            .verify(snapshot)?;
        }
        let rows: Vec<String> = snapshot.exec_rows(
            |commit| {
                format!(
                    "SELECT id FROM artifacts AS OF '{commit}' WHERE CAST(id AS BINARY) = ? LIMIT 2"
                )
            },
            (self.scope.artifact,),
        )?;
        if rows.len() != 1 {
            return Err(missing());
        }
        Ok(())
    }

    fn guard(&self, db: &mut Transaction<'_>, _: &mut Snapshot) -> Result<(), HiveError> {
        // The shared writer has verified this transaction starts at the same
        // clean base used by check_scope.
        if let Some(area) = self.scope.area {
            require_active_area(db, area, ARCHIVED_AREA)?;
        }
        let rows: Vec<(String, String, String, String, bool, bool, bool)> = db
            .exec(
                "SELECT kind, title, author_ref, revision, archived_at IS NOT NULL,
                        archive_reason IS NOT NULL, CAST(body AS BINARY) = CAST(? AS BINARY)
                 FROM artifacts WHERE CAST(id AS BINARY) = ? LIMIT 2",
                (&self.body, self.scope.artifact),
            )
            .map_err(read_error)?;
        let [(kind, title, author, revision, archived, has_reason, same_body)] = rows.as_slice()
        else {
            return Err(missing());
        };
        if !valid_metadata(kind, title, author, revision) || archived != has_reason {
            return Err(HiveError::new(
                ErrorCode::InvalidRecord,
                "The selected Artifact is invalid.",
            ));
        }
        if *archived {
            return Err(HiveError::new(
                ErrorCode::InvalidTransition,
                "An archived Artifact cannot be edited.",
            ));
        }
        if revision != self.scope.expected {
            return Err(stale());
        }
        if *same_body {
            return Err(HiveError::new(
                ErrorCode::InvalidTransition,
                "The supplied Markdown is identical to the current Artifact body.",
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
        let revision = new_uuid_v7(now)?;
        db.exec_drop(
            "UPDATE artifacts SET body = ?, revision = ?, substantive_updated_at = ?
             WHERE CAST(id AS BINARY) = ? AND CAST(revision AS BINARY) = ?",
            (
                &self.body,
                &revision,
                sql_time,
                self.scope.artifact,
                self.scope.expected,
            ),
        )
        .map_err(read_error)?;
        if db.affected_rows() != 1 {
            return Err(stale());
        }

        let result = UpdateResult {
            version: 1,
            artifact_id: self.scope.artifact.to_owned(),
            artifact_revision: revision,
            area_id: self.scope.area.map(str::to_owned),
        };
        Ok(StagedMutation {
            record_id: result.artifact_id.clone(),
            result: issue_writer::encode(&result, Kind::ArtifactUpdate)?,
        })
    }
}

fn valid_metadata(kind: &str, title: &str, author: &str, revision: &str) -> bool {
    is_artifact_kind(kind)
        && valid_title(title)
        && workflow::valid_ref(author)
        && valid_revision(revision)
}

fn missing() -> HiveError {
    HiveError::new(ErrorCode::NotFound, "The selected Artifact was not found.")
}

fn stale() -> HiveError {
    HiveError::new(
        ErrorCode::StaleRevision,
        "The selected Artifact revision changed.",
    )
}

pub(in crate::dolt) struct ScopedUpdate {
    result: UpdateResult,
    expected: String,
}

pub(in crate::dolt) fn check_scope(
    snapshot: &Snapshot,
    request: &str,
    area: Option<&str>,
    artifact: &str,
    actor: &str,
    receipt: &HistoricalReceipt,
) -> Result<ScopedUpdate, HiveError> {
    let result: UpdateResult =
        serde_json::from_str(&receipt.result).map_err(|_| workflow::unverified())?;
    let evidence: UpdateEvidence =
        serde_json::from_str(&receipt.workflow_evidence).map_err(|_| workflow::unverified())?;
    let scope = Scope {
        workspace: &snapshot.workspace_id,
        actor,
        area,
        artifact,
        expected: &evidence.expected_artifact_revision,
    };
    if receipt.kind != KIND
        || receipt.actor_ref != actor
        || result.version != 1
        || result.area_id.as_deref() != area
        || area.is_some_and(|area| !is_uuid_v7(area))
        || result.artifact_id != artifact
        || !is_uuid_v7(artifact)
        || !is_uuid_v7(&result.artifact_revision)
        || !valid_revision(scope.expected)
        || result.artifact_revision == scope.expected
        || evidence.authorization_ref != receipt.authorization_ref
        || !workflow::parse_time(&receipt.created_at).is_some_and(|created| {
            scope.verifies(&evidence, request, &receipt.fingerprint, created)
        })
    {
        return Err(workflow::unverified());
    }
    Ok(ScopedUpdate {
        result,
        expected: evidence.expected_artifact_revision,
    })
}

pub(in crate::dolt) fn verify_effect(
    snapshot: &mut Snapshot,
    introduced: &str,
    receipt: &HistoricalReceipt,
    scoped: &ScopedUpdate,
) -> Result<Value, HiveError> {
    // The shared receipt history verifies the commit hashes and direct parent.
    let base = &receipt.base_commit;
    let result = &scoped.result;
    verify_links(
        snapshot,
        base,
        &result.artifact_id,
        result.area_id.as_deref(),
    )?;
    if let Some(area) = result.area_id.as_deref() {
        verify_area_ancestry(area, ARCHIVED_AREA, |id| {
            let rows = snapshot.exec_rows(
                |_| {
                    format!(
                        "SELECT parent_id, slug, archived_at IS NOT NULL
                    FROM areas AS OF '{base}' WHERE CAST(id AS BINARY) = ? LIMIT 1"
                    )
                },
                (id,),
            )?;
            Ok(rows.into_iter().next())
        })?;
    }
    let rows: Vec<(String, String, String, bool, String)> = snapshot.exec_rows(
        |_| format!(
            "SELECT b.kind, b.title, b.author_ref,
                (CAST(b.revision AS BINARY) = ? AND CAST(a.revision AS BINARY) = ?
                 AND CAST(b.body AS BINARY) <> CAST(a.body AS BINARY)
                 AND CAST(b.kind AS BINARY) = CAST(a.kind AS BINARY)
                 AND CAST(b.title AS BINARY) = CAST(a.title AS BINARY)
                 AND CAST(b.author_ref AS BINARY) = CAST(a.author_ref AS BINARY)
                 AND (CAST(b.source_ref AS BINARY) <=> CAST(a.source_ref AS BINARY))
                 AND b.created_at = a.created_at
                 AND b.archived_at IS NULL AND a.archived_at IS NULL
                 AND b.archive_reason IS NULL AND a.archive_reason IS NULL
                 AND DATE_FORMAT(a.substantive_updated_at, '%Y-%m-%dT%H:%i:%s.%fZ') = ?),
                CASE WHEN OCTET_LENGTH(a.body) <= ? THEN LOWER(SHA2(CAST(a.body AS BINARY), 256)) ELSE '' END
             FROM artifacts AS OF '{base}' b JOIN artifacts AS OF '{introduced}' a
             ON CAST(b.id AS BINARY) = CAST(a.id AS BINARY)
             WHERE CAST(b.id AS BINARY) = ? LIMIT 2"
        ),
        (&scoped.expected, &result.artifact_revision, &receipt.created_at, MAX_BODY_BYTES, &result.artifact_id),
    )?;
    let [(kind, title, author, true, body_sha256)] = rows.as_slice() else {
        return Err(workflow::unverified());
    };
    let scope = Scope {
        workspace: &snapshot.workspace_id,
        area: result.area_id.as_deref(),
        artifact: &result.artifact_id,
        actor: &receipt.actor_ref,
        expected: &scoped.expected,
    };
    if !valid_metadata(kind, title, author, scope.expected)
        || !valid_sha256(body_sha256)
        || scope.fingerprint(body_sha256) != receipt.fingerprint
    {
        return Err(workflow::unverified());
    }
    verify_delta(
        &mut snapshot.connection,
        base,
        introduced,
        &[("artifacts", 0, 0, 1), ("operation_receipts", 1, 0, 0)],
    )?;
    Ok(json!(result))
}
