use mysql::{Transaction, prelude::Queryable};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use time::OffsetDateTime;

use crate::cli::{AreaCreateArgs, CommonOptions, is_slug};
use crate::dolt::{
    DoltSuccess, DoltTarget, Snapshot, context_workflow as workflow, is_uuid, is_uuid_v7,
    issue_writer::{
        self, Decision, Kind, Mutation, StagedMutation, WriteIdentity, digest, invalid,
        new_uuid_v7, valid_revision, valid_title,
    },
    read_error,
    receipt_history::HistoricalReceipt,
    scope::{require_active_area, require_active_area_at},
};
use crate::error::{ErrorCode, HiveError};

pub(in crate::dolt) const KIND: &str = "area.create";
const ARCHIVED: &str = "Area creation requires active parent ancestry.";
const SLUG_SCOPE: &str = "parent_id <=> CAST(? AS CHAR) AND slug = CAST(? AS CHAR)";

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
    // Both keys must be present, including explicit nulls for a workspace-level decision.
    #[serde(deserialize_with = "Option::deserialize")]
    parent_id: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    expected_parent_revision: Option<String>,
    authorization_ref: String,
    evidence_id: String,
    revision: String,
    source_ref: String,
    issuer_ref: String,
    valid_from: String,
    expires_at: String,
    withdrawn: bool,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CreateResult {
    version: u8,
    area_id: String,
    area_revision: String,
    #[serde(deserialize_with = "Option::deserialize")]
    parent_id: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    parent_revision: Option<String>,
}

#[derive(Clone, Copy)]
struct Scope<'a> {
    workspace: &'a str,
    actor: &'a str,
    parent: Option<(&'a str, &'a str)>,
}

impl Scope<'_> {
    fn fingerprint(self, slug: &str, name: &str) -> String {
        let (parent, expected) = self.parent.unzip();
        digest(
            json!([
                1,
                KIND,
                self.workspace,
                parent,
                self.actor,
                expected,
                slug,
                name
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
        let (parent, expected) = self.parent.unzip();
        evidence.version == 1
            && evidence.operation == KIND
            && evidence.workspace_id == self.workspace
            && evidence.request_id == request
            && evidence.fingerprint == fingerprint
            && evidence.actor_ref == self.actor
            && evidence.parent_id.as_deref() == parent
            && evidence.expected_parent_revision.as_deref() == expected
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
    args: &'a AreaCreateArgs,
    scope: Scope<'a>,
    fingerprint: String,
    decision: Decision<CreateEvidence>,
}

pub(super) fn write(
    common: &CommonOptions,
    args: &AreaCreateArgs,
) -> Result<DoltSuccess, HiveError> {
    let target = DoltTarget::parse(&args.target)?;
    let workspace =
        crate::config_v2::read_explicit(common.workspace.as_deref().expect("validated workspace"))?;
    let parent = match (args.parent.as_deref(), args.if_match.as_deref()) {
        (None, None) => None,
        (Some(parent), Some(expected)) if is_uuid_v7(parent) => Some((
            parent,
            expected
                .split_once('=')
                .filter(|(id, rev)| *id == parent && valid_revision(rev))
                .map(|(_, rev)| rev)
                .ok_or_else(invalid_parent)?,
        )),
        _ => return Err(invalid_parent()),
    };
    if !is_uuid(&args.request_id) || !workflow::valid_ref(&args.actor) {
        return Err(invalid(
            "Supply a canonical request UUID and an explicit actor reference.",
        ));
    }
    if !valid_slug(&args.slug) || !valid_title(&args.name) {
        return Err(invalid(
            "Supply a lowercase kebab-case Area slug of up to 100 bytes and a nonblank name of up to 255 characters without control characters.",
        ));
    }
    let scope = Scope {
        workspace: workspace.workspace_id(),
        actor: &args.actor,
        parent,
    };
    let create = Create {
        args,
        scope,
        fingerprint: scope.fingerprint(&args.slug, &args.name),
        decision: issue_writer::read_decision(&args.evidence_file)?,
    };
    issue_writer::execute(&workspace, target, &create)
}

impl Mutation for Create<'_> {
    type Guard = ();

    fn identity(&self) -> WriteIdentity<'_> {
        WriteIdentity {
            kind: Kind::AreaCreate,
            request_id: &self.args.request_id,
            area_id: self.scope.parent.map(|(parent, _)| parent),
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
        issue_writer::encode(&self.decision.evidence, Kind::AreaCreate)
    }

    fn check_scope(&self, snapshot: &mut Snapshot) -> Result<(), HiveError> {
        if let Some((parent, _)) = self.scope.parent {
            super::select_area::<(String,)>(snapshot, "id", parent)?;
        }
        Ok(())
    }

    fn guard(&self, db: &mut Transaction<'_>, _: &mut Snapshot) -> Result<(), HiveError> {
        if let Some((parent, expected)) = self.scope.parent {
            require_active_area(db, parent, ARCHIVED)?;
            let revision: Option<String> = db
                .exec_first(
                    "SELECT revision FROM areas WHERE CAST(id AS BINARY) = ?",
                    (parent,),
                )
                .map_err(read_error)?;
            if revision.as_deref() != Some(expected) {
                return Err(stale());
            }
        }
        let exists: Option<String> = db
            .exec_first(
                format!("SELECT id FROM areas WHERE {SLUG_SCOPE} LIMIT 1"),
                (self.scope.parent.map(|(parent, _)| parent), &self.args.slug),
            )
            .map_err(read_error)?;
        if exists.is_some() {
            return Err(HiveError::new(
                ErrorCode::AlreadyExists,
                "The Area slug already exists in the selected root or sibling scope.",
            ));
        }
        Ok(())
    }

    fn stage(
        &self,
        db: &mut Transaction<'_>,
        (): (),
        now: OffsetDateTime,
        _: &str,
    ) -> Result<StagedMutation, HiveError> {
        let area = new_uuid_v7(now)?;
        let revision = new_uuid_v7(now)?;
        let parent_id = self.scope.parent.map(|(parent, _)| parent);
        db.exec_drop(
            "INSERT INTO areas (id, parent_id, slug, name, revision) VALUES (?, ?, ?, ?, ?)",
            (
                &area,
                parent_id,
                &self.args.slug,
                &self.args.name,
                &revision,
            ),
        )
        .map_err(read_error)?;
        let parent_revision = if let Some((parent, expected)) = self.scope.parent {
            let new_revision = new_uuid_v7(now)?;
            db.exec_drop(
                "UPDATE areas SET revision = ? WHERE CAST(id AS BINARY) = ? AND CAST(revision AS BINARY) = ?",
                (&new_revision, parent, expected),
            ).map_err(read_error)?;
            if db.affected_rows() != 1 {
                return Err(stale());
            }
            Some(new_revision)
        } else {
            None
        };
        let result = CreateResult {
            version: 1,
            area_id: area.clone(),
            area_revision: revision,
            parent_id: parent_id.map(str::to_owned),
            parent_revision,
        };
        Ok(StagedMutation {
            record_id: area,
            result: issue_writer::encode(&result, Kind::AreaCreate)?,
        })
    }
}

fn valid_slug(slug: &str) -> bool {
    slug.len() <= 100 && is_slug(slug)
}

fn invalid_parent() -> HiveError {
    invalid("Supply an exact --parent UUIDv7 and --if-match PARENT=REV together.")
}

fn stale() -> HiveError {
    HiveError::new(
        ErrorCode::StaleRevision,
        "The selected parent revision changed.",
    )
}

pub(in crate::dolt) struct ScopedCreate {
    result: CreateResult,
    expected: Option<String>,
}

pub(in crate::dolt) fn check_scope(
    snapshot: &Snapshot,
    request: &str,
    parent: Option<&str>,
    area: Option<&str>,
    actor: &str,
    receipt: &HistoricalReceipt,
) -> Result<ScopedCreate, HiveError> {
    let result: CreateResult =
        serde_json::from_str(&receipt.result).map_err(|_| workflow::unverified())?;
    let evidence: CreateEvidence =
        serde_json::from_str(&receipt.workflow_evidence).map_err(|_| workflow::unverified())?;
    let expected = evidence.expected_parent_revision.as_deref();
    let valid_parent = match (parent, expected, result.parent_revision.as_deref()) {
        (None, None, None) => true,
        (Some(id), Some(before), Some(after)) => {
            is_uuid_v7(id)
                && id != result.area_id
                && valid_revision(before)
                && is_uuid_v7(after)
                && before != after
        }
        _ => false,
    };
    let scope = Scope {
        workspace: &snapshot.workspace_id,
        actor,
        parent: parent.zip(expected),
    };
    if receipt.kind != KIND
        || !workflow::valid_ref(actor)
        || receipt.actor_ref != actor
        || result.version != 1
        || result.parent_id.as_deref() != parent
        || area.is_some_and(|id| id != result.area_id)
        || !is_uuid_v7(&result.area_id)
        || !is_uuid_v7(&result.area_revision)
        || !valid_parent
        || evidence.authorization_ref != receipt.authorization_ref
        || !workflow::parse_time(&receipt.created_at).is_some_and(|created| {
            scope.verifies(&evidence, request, &receipt.fingerprint, created)
        })
    {
        return Err(workflow::unverified());
    }
    Ok(ScopedCreate {
        result,
        expected: evidence.expected_parent_revision,
    })
}

pub(in crate::dolt) fn check_current_scope(
    snapshot: &mut Snapshot,
    scoped: &ScopedCreate,
) -> Result<(), HiveError> {
    let result = &scoped.result;
    let rows: Vec<(Option<String>,)> = snapshot.exec_rows(
        |commit| {
            format!(
                "SELECT parent_id FROM areas AS OF '{commit}' WHERE CAST(id AS BINARY) = ? LIMIT 2"
            )
        },
        (&result.area_id,),
    )?;
    if rows == [(result.parent_id.clone(),)] {
        Ok(())
    } else {
        Err(workflow::unverified())
    }
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
    let parent = result.parent_id.as_deref();
    let previous: Vec<String> = snapshot.exec_rows(
        |_| format!("SELECT id FROM areas AS OF '{base}' WHERE id = CAST(? AS CHAR) LIMIT 1"),
        (&result.area_id,),
    )?;
    let rows: Vec<(Option<String>, String, String, String, bool)> = snapshot.exec_rows(
        |_| {
            format!(
            "SELECT parent_id, slug, name, revision, (description IS NULL AND archived_at IS NULL)
             FROM areas AS OF '{introduced}' WHERE CAST(id AS BINARY) = ? LIMIT 2"
        )
        },
        (&result.area_id,),
    )?;
    let [(stored_parent, slug, name, revision, true)] = rows.as_slice() else {
        return Err(workflow::unverified());
    };
    let bound_parent = parent.zip(scoped.expected.as_deref());
    let scope = Scope {
        workspace: &snapshot.workspace_id,
        actor: &receipt.actor_ref,
        parent: bound_parent,
    };
    if !previous.is_empty()
        || stored_parent.as_deref() != parent
        || revision != &result.area_revision
        || !valid_slug(slug)
        || !valid_title(name)
        || scope.fingerprint(slug, name) != receipt.fingerprint
    {
        return Err(workflow::unverified());
    }
    for (commit, expected) in [
        (base.as_str(), &[][..]),
        (introduced, &[result.area_id.as_str()][..]),
    ] {
        let matched: Vec<String> = snapshot.exec_rows(
            |_| format!("SELECT id FROM areas AS OF '{commit}' WHERE {SLUG_SCOPE} LIMIT 2"),
            (parent, slug),
        )?;
        if matched != expected {
            return Err(workflow::unverified());
        }
    }
    if let (Some((parent, expected)), Some(revision)) =
        (bound_parent, result.parent_revision.as_deref())
    {
        require_active_area_at(&mut snapshot.connection, parent, base, ARCHIVED)?;
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
            (expected, revision, parent),
        )?;
        if unchanged != [true] {
            return Err(workflow::unverified());
        }
    }
    verify_delta(&mut snapshot.connection, base, introduced, parent.is_some())?;
    Ok(json!(result))
}

fn verify_delta(
    db: &mut impl Queryable,
    base: &str,
    introduced: &str,
    child: bool,
) -> Result<(), HiveError> {
    let tables: Vec<(String, String, String, bool, bool)> = db
        .exec(
            "SELECT from_table_name, to_table_name, diff_type, data_change, schema_change
         FROM DOLT_DIFF_SUMMARY(?, ?) ORDER BY from_table_name LIMIT 3",
            (base, introduced),
        )
        .map_err(read_error)?;
    if !tables
        .iter()
        .map(|(from, to, kind, data, schema)| {
            (from.as_str(), to.as_str(), kind.as_str(), *data, *schema)
        })
        .eq([
            ("areas", "areas", "modified", true, false),
            (
                "operation_receipts",
                "operation_receipts",
                "modified",
                true,
                false,
            ),
        ])
        || db
            .query_first::<mysql::Row, _>("SHOW WARNINGS LIMIT 1")
            .map_err(read_error)?
            .is_some()
    {
        return Err(workflow::unverified());
    }
    let rows: Vec<(String, u64, u64, u64)> = db
        .exec(
            "SELECT table_name, rows_added, rows_deleted, rows_modified
         FROM DOLT_DIFF_STAT(?, ?) ORDER BY table_name LIMIT 3",
            (base, introduced),
        )
        .map_err(read_error)?;
    if !rows
        .iter()
        .map(|(name, added, deleted, modified)| (name.as_str(), *added, *deleted, *modified))
        .eq([
            ("areas", 1, 0, u64::from(child)),
            ("operation_receipts", 1, 0, 0),
        ])
        || db
            .query_first::<mysql::Row, _>("SHOW WARNINGS LIMIT 1")
            .map_err(read_error)?
            .is_some()
    {
        return Err(workflow::unverified());
    }
    Ok(())
}
