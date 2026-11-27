use mysql::{Transaction, prelude::Queryable};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use time::OffsetDateTime;

use crate::cli::{ArtifactCreateArgs, CommonOptions};
use crate::dolt::{
    DoltSuccess, DoltTarget, Snapshot, context_workflow as workflow, is_uuid, is_uuid_v7,
    issue_writer::{
        self, Decision, Kind, Mutation, StagedMutation, WriteIdentity, digest, invalid,
        new_uuid_v7, valid_revision, valid_title,
    },
    read_error,
    receipt_history::{HistoricalReceipt, valid_sha256, verify_delta},
    records::is_artifact_kind,
    scope::{require_active_area, require_active_area_at},
};
use crate::error::{ErrorCode, HiveError};

use super::{
    MAX_BODY_BYTES, ScopeKind,
    writes::{read_body, verify_links},
};

pub(in crate::dolt) const KIND: &str = "artifact.create";
const ARCHIVED: &str = "Artifact linking requires active Area ancestry.";

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CreateResult {
    version: u8,
    artifact_id: String,
    artifact_revision: String,
    area_id: Option<String>,
    area_revision: Option<String>,
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
    area_id: Option<String>,
    expected_area_revision: Option<String>,
    authorization_ref: String,
    evidence_id: String,
    revision: String,
    source_ref: String,
    issuer_ref: String,
    valid_from: String,
    expires_at: String,
    withdrawn: bool,
}

#[derive(Clone, Copy)]
struct Scope<'a> {
    workspace: &'a str,
    actor: &'a str,
    area: Option<&'a str>,
    expected: Option<&'a str>,
}

impl Scope<'_> {
    fn fingerprint(self, kind: &str, title: &str, body_sha256: &str) -> String {
        digest(
            json!([
                1,
                KIND,
                self.workspace,
                self.area,
                self.actor,
                self.expected,
                kind,
                title,
                body_sha256,
            ])
            .to_string()
            .as_bytes(),
        )
    }

    fn verifies(
        self,
        evidence: &CreateEvidence,
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
            && evidence.expected_area_revision.as_deref() == self.expected
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

struct Create<'a> {
    args: &'a ArtifactCreateArgs,
    scope: Scope<'a>,
    body: String,
    fingerprint: String,
    decision: Decision<CreateEvidence>,
}

pub(super) fn write(
    common: &CommonOptions,
    args: &ArtifactCreateArgs,
) -> Result<DoltSuccess, HiveError> {
    let target = DoltTarget::parse(&args.target)?;
    let workspace =
        crate::config_v2::read_explicit(common.workspace.as_deref().expect("validated workspace"))?;
    let area = args.area.as_deref();
    let expected = match (
        area,
        args.if_match
            .as_deref()
            .and_then(|value| value.split_once('=')),
    ) {
        (None, None) if args.if_match.is_none() => None,
        (Some(area), Some((id, rev))) if is_uuid_v7(area) && id == area && valid_revision(rev) => {
            Some(rev)
        }
        _ => {
            return Err(invalid(
                "For a direct link, supply an exact --area UUIDv7 and one --if-match AREA=REV. Standalone creation takes neither option.",
            ));
        }
    };
    if !is_uuid(&args.request_id) || !workflow::valid_ref(&args.actor) {
        return Err(invalid("Supply a request UUID and an actor reference."));
    }
    if !is_artifact_kind(&args.kind) || !valid_title(&args.title) {
        return Err(invalid(
            "Supply a documented lowercase Artifact kind and a nonblank title of up to 255 characters without control characters.",
        ));
    }
    let body = read_body(&args.file)?;
    let scope = Scope {
        workspace: workspace.workspace_id(),
        actor: &args.actor,
        area,
        expected,
    };
    let create = Create {
        args,
        scope,
        fingerprint: scope.fingerprint(&args.kind, &args.title, &digest(body.as_bytes())),
        body,
        decision: issue_writer::read_decision(&args.evidence_file)?,
    };
    issue_writer::execute(&workspace, target, &create)
}

impl Mutation for Create<'_> {
    type Guard = ();

    fn identity(&self) -> WriteIdentity<'_> {
        WriteIdentity {
            kind: Kind::ArtifactCreate,
            request_id: &self.args.request_id,
            area_id: self.scope.area,
            record_id: None,
            actor: self.scope.actor,
            fingerprint: &self.fingerprint,
            authorization_ref: &self.decision.evidence.authorization_ref,
        }
    }

    fn verify_decision(&self, now: OffsetDateTime) -> Result<(), HiveError> {
        if self.decision.decision == "allow"
            && self.scope.verifies(
                &self.decision.evidence,
                &self.args.request_id,
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
        issue_writer::encode(&self.decision.evidence, Kind::ArtifactCreate)
    }

    fn check_scope(&self, snapshot: &mut Snapshot) -> Result<(), HiveError> {
        if let Some(area) = self.scope.area {
            super::Scope {
                kind: ScopeKind::Area,
                id: area,
            }
            .verify(snapshot)?;
        }
        Ok(())
    }

    fn guard(&self, db: &mut Transaction<'_>, _: &mut Snapshot) -> Result<(), HiveError> {
        if let (Some(area), Some(expected)) = (self.scope.area, self.scope.expected) {
            require_active_area(db, area, ARCHIVED)?;
            let revision: Option<String> = db
                .exec_first(
                    "SELECT revision FROM areas WHERE CAST(id AS BINARY) = ?",
                    (area,),
                )
                .map_err(read_error)?;
            if revision.as_deref() != Some(expected) {
                return Err(stale());
            }
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
        let artifact = new_uuid_v7(now)?;
        let revision = new_uuid_v7(now)?;
        db.exec_drop(
            "INSERT INTO artifacts (id, kind, title, body, author_ref, revision, created_at, substantive_updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            (&artifact, &self.args.kind, &self.args.title, &self.body, self.scope.actor, &revision, sql_time, sql_time),
        ).map_err(read_error)?;
        let area_revision = if let (Some(area), Some(expected)) =
            (self.scope.area, self.scope.expected)
        {
            let new_revision = new_uuid_v7(now)?;
            db.exec_drop(
                "UPDATE areas SET revision = ? WHERE CAST(id AS BINARY) = ? AND CAST(revision AS BINARY) = ?",
                (&new_revision, area, expected),
            ).map_err(read_error)?;
            if db.affected_rows() != 1 {
                return Err(stale());
            }
            db.exec_drop(
                "INSERT INTO artifact_areas (artifact_id, area_id) VALUES (?, ?)",
                (&artifact, area),
            )
            .map_err(read_error)?;
            Some(new_revision)
        } else {
            None
        };
        let result = CreateResult {
            version: 1,
            artifact_id: artifact.clone(),
            artifact_revision: revision,
            area_id: self.scope.area.map(str::to_owned),
            area_revision,
        };
        Ok(StagedMutation {
            record_id: artifact,
            result: issue_writer::encode(&result, Kind::ArtifactCreate)?,
        })
    }
}

fn stale() -> HiveError {
    HiveError::new(
        ErrorCode::StaleRevision,
        "The selected Area revision changed.",
    )
}

pub(in crate::dolt) struct ScopedCreate {
    result: CreateResult,
    expected: Option<String>,
}

pub(in crate::dolt) fn check_scope(
    snapshot: &Snapshot,
    request: &str,
    area: Option<&str>,
    artifact: Option<&str>,
    actor: &str,
    receipt: &HistoricalReceipt,
) -> Result<ScopedCreate, HiveError> {
    let result: CreateResult =
        serde_json::from_str(&receipt.result).map_err(|_| workflow::unverified())?;
    let evidence: CreateEvidence =
        serde_json::from_str(&receipt.workflow_evidence).map_err(|_| workflow::unverified())?;
    let scope = Scope {
        workspace: &snapshot.workspace_id,
        actor,
        area,
        expected: evidence.expected_area_revision.as_deref(),
    };
    let valid_area = match (area, scope.expected, result.area_revision.as_deref()) {
        (None, None, None) => true,
        (Some(id), Some(before), Some(after)) => {
            is_uuid_v7(id) && valid_revision(before) && is_uuid_v7(after) && before != after
        }
        _ => false,
    };
    if receipt.kind != KIND
        || receipt.actor_ref != actor
        || result.version != 1
        || result.area_id.as_deref() != area
        || artifact.is_some_and(|id| id != result.artifact_id)
        || !is_uuid_v7(&result.artifact_id)
        || !is_uuid_v7(&result.artifact_revision)
        || !valid_area
        || evidence.authorization_ref != receipt.authorization_ref
        || !workflow::parse_time(&receipt.created_at).is_some_and(|created| {
            scope.verifies(&evidence, request, &receipt.fingerprint, created)
        })
    {
        return Err(workflow::unverified());
    }
    Ok(ScopedCreate {
        result,
        expected: evidence.expected_area_revision,
    })
}

pub(in crate::dolt) fn check_current_scope(
    snapshot: &mut Snapshot,
    scoped: &ScopedCreate,
) -> Result<(), HiveError> {
    let result = &scoped.result;
    let ids: Vec<String> = snapshot.exec_rows(
        |commit| {
            format!(
                "SELECT id FROM artifacts AS OF '{commit}' WHERE CAST(id AS BINARY) = ? LIMIT 1"
            )
        },
        (&result.artifact_id,),
    )?;
    if ids.is_empty() {
        return Err(workflow::unverified());
    }
    let head = snapshot.commit.clone();
    verify_links(
        snapshot,
        &head,
        &result.artifact_id,
        result.area_id.as_deref(),
    )
}

pub(in crate::dolt) fn verify_effect(
    snapshot: &mut Snapshot,
    introduced: &str,
    receipt: &HistoricalReceipt,
    scoped: &ScopedCreate,
) -> Result<Value, HiveError> {
    // Receipt history validates both commit literals before this verifier runs.
    let base = &receipt.base_commit;
    let result = &scoped.result;
    let prior: Vec<String> = snapshot.exec_rows(
        |_| format!("SELECT id FROM artifacts AS OF '{base}' WHERE id = CAST(? AS CHAR) LIMIT 1"),
        (&result.artifact_id,),
    )?;
    let rows: Vec<(String, String, String, String, bool, String)> = snapshot.exec_rows(
        |_| format!(
            "SELECT kind, title, revision, author_ref,
                (created_at = substantive_updated_at AND DATE_FORMAT(created_at, '%Y-%m-%dT%H:%i:%s.%fZ') = ?
                 AND source_ref IS NULL AND archived_at IS NULL AND archive_reason IS NULL),
                CASE WHEN OCTET_LENGTH(body) <= ? THEN LOWER(SHA2(CAST(body AS BINARY), 256)) ELSE '' END
             FROM artifacts AS OF '{introduced}' WHERE CAST(id AS BINARY) = ? LIMIT 2"
        ),
        (&receipt.created_at, MAX_BODY_BYTES, &result.artifact_id),
    )?;
    let [(kind, title, revision, author, true, body_sha256)] = rows.as_slice() else {
        return Err(workflow::unverified());
    };
    let scope = Scope {
        workspace: &snapshot.workspace_id,
        actor: &receipt.actor_ref,
        area: result.area_id.as_deref(),
        expected: scoped.expected.as_deref(),
    };
    if !prior.is_empty()
        || revision != &result.artifact_revision
        || author != &receipt.actor_ref
        || !is_artifact_kind(kind)
        || !valid_title(title)
        || !valid_sha256(body_sha256)
        || scope.fingerprint(kind, title, body_sha256) != receipt.fingerprint
    {
        return Err(workflow::unverified());
    }
    verify_links(snapshot, base, &result.artifact_id, None)?;
    verify_links(
        snapshot,
        introduced,
        &result.artifact_id,
        result.area_id.as_deref(),
    )?;
    if let (Some(area), Some(expected), Some(revision)) = (
        result.area_id.as_deref(),
        scoped.expected.as_deref(),
        result.area_revision.as_deref(),
    ) {
        require_active_area_at(&mut snapshot.connection, area, base, ARCHIVED)?;
        verify_area_revision(snapshot, base, introduced, area, expected, revision)?;
    }
    let mut expected = vec![("artifacts", 1, 0, 0), ("operation_receipts", 1, 0, 0)];
    if result.area_id.is_some() {
        expected.splice(0..0, [("areas", 0, 0, 1), ("artifact_areas", 1, 0, 0)]);
    }
    verify_delta(&mut snapshot.connection, base, introduced, &expected)?;
    Ok(json!(result))
}

fn verify_area_revision(
    snapshot: &mut Snapshot,
    base: &str,
    introduced: &str,
    area: &str,
    expected: &str,
    revision: &str,
) -> Result<(), HiveError> {
    let unchanged: Vec<bool> = snapshot.exec_rows(
        |_| {
            format!(
                "SELECT (CAST(b.revision AS BINARY) = ? AND CAST(a.revision AS BINARY) = ?
                AND (CAST(b.parent_id AS BINARY) <=> CAST(a.parent_id AS BINARY))
                AND CAST(b.slug AS BINARY) = CAST(a.slug AS BINARY)
                AND CAST(b.name AS BINARY) = CAST(a.name AS BINARY)
                AND (CAST(b.description AS BINARY) <=> CAST(a.description AS BINARY))
                AND b.archived_at IS NULL AND a.archived_at IS NULL)
             FROM areas AS OF '{base}' b JOIN areas AS OF '{introduced}' a
               ON CAST(b.id AS BINARY) = CAST(a.id AS BINARY)
             WHERE CAST(b.id AS BINARY) = ? LIMIT 2"
            )
        },
        (expected, revision, area),
    )?;
    if unchanged != [true] {
        return Err(workflow::unverified());
    }
    Ok(())
}
