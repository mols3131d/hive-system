//! Exclusive publication and same-launch reconciliation for typed Dolt writes.

use std::path::Path;

use mysql::{Transaction, TxOpts, prelude::Queryable};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

use super::{
    DoltSuccess, DoltTarget, Snapshot,
    area::create as area_create,
    artifact::{create as artifact_create, update as artifact_update},
    context_evidence::read_evidence_bytes,
    context_resolve::observe_time,
    context_workflow as workflow, is_commit_hash, is_uuid, is_uuid_v7, issue_create,
    issue_note_receipt, issue_update,
    project::planning,
    read_error,
    receipt_history::{
        HistoricalReceipt, has_receipt_history, receipt_at, receipt_introduction, valid_sha256,
    },
};
use crate::config_v2::Candidate;
use crate::error::{ErrorCode, HiveError};
use crate::workspace_status::{WriterRuntime, token, with_writer_runtime};

#[derive(Clone, Copy, Deserialize, Serialize)]
pub(super) enum Kind {
    #[serde(rename = "issue.note")]
    Note,
    #[serde(rename = "issue.create")]
    Create,
    #[serde(rename = "issue.update")]
    Update,
    #[serde(rename = "artifact.create")]
    ArtifactCreate,
    #[serde(rename = "artifact.update")]
    ArtifactUpdate,
    #[serde(rename = "area.create")]
    AreaCreate,
    #[serde(rename = "project.create")]
    ProjectCreate,
    #[serde(rename = "project.add-issue")]
    ProjectAddIssue,
}

impl Kind {
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Note => super::issue_note_fingerprint::KIND,
            Self::Create => issue_create::KIND,
            Self::Update => issue_update::KIND,
            Self::ArtifactCreate => artifact_create::KIND,
            Self::ArtifactUpdate => artifact_update::KIND,
            Self::AreaCreate => area_create::KIND,
            Self::ProjectCreate => planning::CREATE,
            Self::ProjectAddIssue => planning::ADD_ISSUE,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Note => "Issue note",
            Self::Create => "Issue create",
            Self::Update => "Issue update",
            Self::ArtifactCreate => "Artifact create",
            Self::ArtifactUpdate => "Artifact update",
            Self::AreaCreate => "Area create",
            Self::ProjectCreate => "Project create",
            Self::ProjectAddIssue => "Project add-issue",
        }
    }

    fn grants(self, area: Option<&str>) -> &'static [(&'static str, &'static str)] {
        match self {
            Self::Note => &[
                ("issues", "UPDATE"),
                ("issue_updates", "INSERT"),
                ("operation_receipts", "INSERT"),
            ],
            Self::Update => &[("issues", "UPDATE"), ("operation_receipts", "INSERT")],
            Self::Create => &[
                ("issues", "INSERT"),
                ("workspace_counters", "UPDATE"),
                ("operation_receipts", "INSERT"),
            ],
            Self::ArtifactCreate if area.is_some() => &[
                ("artifacts", "INSERT"),
                ("artifact_areas", "INSERT"),
                ("areas", "UPDATE"),
                ("operation_receipts", "INSERT"),
            ],
            Self::ArtifactCreate => &[("artifacts", "INSERT"), ("operation_receipts", "INSERT")],
            Self::ArtifactUpdate => &[("artifacts", "UPDATE"), ("operation_receipts", "INSERT")],
            Self::AreaCreate if area.is_some() => &[
                ("areas", "INSERT"),
                ("areas", "UPDATE"),
                ("operation_receipts", "INSERT"),
            ],
            Self::AreaCreate => &[("areas", "INSERT"), ("operation_receipts", "INSERT")],
            Self::ProjectCreate => &[("projects", "INSERT"), ("operation_receipts", "INSERT")],
            Self::ProjectAddIssue => &[
                ("issues", "UPDATE"),
                ("projects", "UPDATE"),
                ("project_issues", "INSERT"),
                ("operation_receipts", "INSERT"),
            ],
        }
    }

    fn is_project(self) -> bool {
        matches!(self, Self::ProjectCreate | Self::ProjectAddIssue)
    }
}

pub(super) struct WriteIdentity<'a> {
    pub(super) kind: Kind,
    pub(super) request_id: &'a str,
    pub(super) area_id: Option<&'a str>,
    pub(super) record_id: Option<&'a str>,
    pub(super) actor: &'a str,
    pub(super) fingerprint: &'a str,
    pub(super) authorization_ref: &'a str,
}

pub(super) struct StagedMutation {
    pub(super) record_id: String,
    pub(super) result: String,
}

pub(super) trait Mutation {
    type Guard;

    fn identity(&self) -> WriteIdentity<'_>;
    fn verify_decision(&self, now: OffsetDateTime) -> Result<(), HiveError>;
    fn evidence(&self) -> Result<String, HiveError>;
    fn check_scope(&self, snapshot: &mut Snapshot) -> Result<(), HiveError>;
    fn guard(
        &self,
        db: &mut Transaction<'_>,
        snapshot: &mut Snapshot,
    ) -> Result<Self::Guard, HiveError>;
    fn stage(
        &self,
        db: &mut Transaction<'_>,
        guard: Self::Guard,
        now: OffsetDateTime,
        sql_time: &str,
    ) -> Result<StagedMutation, HiveError>;
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Decision<E> {
    pub(super) decision: String,
    #[serde(
        bound(deserialize = "E: Deserialize<'de>"),
        deserialize_with = "deserialize_object"
    )]
    pub(super) evidence: E,
}

fn deserialize_object<'de, D: serde::Deserializer<'de>, E: Deserialize<'de>>(
    deserializer: D,
) -> Result<E, D::Error> {
    workflow::parse_json_object(<&serde_json::value::RawValue>::deserialize(deserializer)?)
}

pub(super) fn read_decision<E: serde::de::DeserializeOwned>(
    path: &Path,
) -> Result<Decision<E>, HiveError> {
    let bytes = read_evidence_bytes(path, workflow::MAX_DECLARATION_BYTES)?;
    workflow::parse_json_object::<_, serde_json::Error>(
        serde_json::from_slice(&bytes).map_err(|_| workflow::unverified())?,
    )
    .map_err(|_| workflow::unverified())
}

// Version 1 has no kind field and always denotes a progress note. Version 2
// carries an Issue kind. Version 3 carries an Artifact ID and optional Area.
// Version 4 carries either a new Area ID and optional parent or a Project ID and optional Area.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Pending {
    version: u8,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_kind"
    )]
    kind: Option<Kind>,
    workspace_id: String,
    launch_nonce: String,
    session_id: u64,
    request_id: String,
    base_commit: String,
    fingerprint: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_id"
    )]
    area_id: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_id"
    )]
    issue_id: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_id"
    )]
    artifact_id: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_id"
    )]
    parent_id: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_id"
    )]
    project_id: Option<String>,
    receipt_digest: String,
}

fn present_kind<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Kind>, D::Error> {
    Kind::deserialize(deserializer).map(Some)
}

fn present_id<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<String>, D::Error> {
    String::deserialize(deserializer).map(Some)
}

impl Pending {
    pub(crate) fn read(bytes: &[u8], workspace: &str, nonce: &str) -> Result<Self, HiveError> {
        let marker: Self = workflow::parse_json_object::<_, serde_json::Error>(
            serde_json::from_slice(bytes).map_err(|_| uncertain())?,
        )
        .map_err(|_| uncertain())?;
        marker.validate(workspace, nonce)?;
        Ok(marker)
    }

    /// The caller must establish prior-session termination and open a fresh,
    /// verified snapshot before resolving the marker.
    pub(crate) fn resolve(&self, snapshot: &mut Snapshot) -> Result<&'static str, HiveError> {
        if confirm(snapshot, self)?.is_some() {
            return Ok("confirmed");
        }
        if snapshot.commit != self.base_commit || has_receipt_history(snapshot, &self.request_id)? {
            return Err(uncertain());
        }
        require_clean(&mut snapshot.connection, &self.base_commit)?;
        Ok("rolled_back")
    }

    fn operation(&self) -> Result<Kind, HiveError> {
        match (
            self.version,
            self.kind,
            &self.area_id,
            &self.issue_id,
            &self.artifact_id,
            &self.parent_id,
            &self.project_id,
        ) {
            (1, None, Some(_), Some(_), None, None, None) => Ok(Kind::Note),
            (2, Some(kind @ (Kind::Create | Kind::Update)), Some(_), Some(_), None, None, None) => {
                Ok(kind)
            }
            (
                3,
                Some(kind @ (Kind::ArtifactCreate | Kind::ArtifactUpdate)),
                _,
                None,
                Some(_),
                None,
                None,
            ) => Ok(kind),
            (4, Some(Kind::AreaCreate), Some(area), None, None, parent, None)
                if parent.as_ref() != Some(area) =>
            {
                Ok(Kind::AreaCreate)
            }
            (4, Some(Kind::ProjectCreate), None, None, None, None, Some(_)) => {
                Ok(Kind::ProjectCreate)
            }
            (4, Some(Kind::ProjectAddIssue), Some(_), None, None, None, Some(_)) => {
                Ok(Kind::ProjectAddIssue)
            }
            _ => Err(uncertain()),
        }
    }

    fn area_scope(&self, kind: Kind) -> Option<&str> {
        match kind {
            Kind::AreaCreate => self.parent_id.as_deref(),
            _ => self.area_id.as_deref(),
        }
    }

    fn record_id(&self, kind: Kind) -> Option<&str> {
        match kind {
            Kind::AreaCreate => self.area_id.as_deref(),
            Kind::ArtifactCreate | Kind::ArtifactUpdate => self.artifact_id.as_deref(),
            Kind::ProjectCreate | Kind::ProjectAddIssue => self.project_id.as_deref(),
            _ => self.issue_id.as_deref(),
        }
    }

    fn validate(&self, workspace: &str, nonce: &str) -> Result<(), HiveError> {
        self.operation()?;
        if self.workspace_id != workspace
            || self.launch_nonce != nonce
            || self.session_id == 0
            || !is_uuid(&self.request_id)
            || !is_commit_hash(&self.base_commit)
            || [
                &self.area_id,
                &self.issue_id,
                &self.artifact_id,
                &self.parent_id,
                &self.project_id,
            ]
            .into_iter()
            .flatten()
            .any(|id| !is_uuid_v7(id))
            || !valid_sha256(&self.fingerprint)
            || !valid_sha256(&self.receipt_digest)
        {
            return Err(uncertain());
        }
        Ok(())
    }
}

pub(super) fn execute(
    workspace: &Candidate,
    target: DoltTarget<'_>,
    mutation: &impl Mutation,
) -> Result<DoltSuccess, HiveError> {
    let request = mutation.identity();
    mutation.verify_decision(observe_time()?)?;
    with_writer_runtime(workspace, target, |runtime, snapshot| {
        mutation.verify_decision(observe_time()?)?;
        mutation.check_scope(snapshot)?;
        if let Some(recovered) = reconcile(runtime, workspace.workspace_id())? {
            *snapshot = recovered;
            mutation.check_scope(snapshot)?;
        }
        require_clean(&mut snapshot.connection, &snapshot.commit)?;
        let commit = snapshot.commit.clone();
        let existing =
            receipt_at(snapshot, request.request_id, &commit, None).map_err(|_| uncertain())?;
        mutation.verify_decision(observe_time()?)?;
        let (outcome, base, introduced, result) = if let Some(receipt) = existing {
            if matches!(request.kind, Kind::ArtifactCreate | Kind::ArtifactUpdate | Kind::AreaCreate) || request.kind.is_project() {
                check_fingerprint(&receipt.fingerprint, request.fingerprint, request.kind)?;
            }
            let scoped = ScopedReceipt::read(
                snapshot,
                request.kind,
                request.request_id,
                request.area_id,
                request.record_id,
                request.actor,
                &receipt,
            )?;
            check_fingerprint(&receipt.fingerprint, request.fingerprint, request.kind)?;
            if let ScopedReceipt::Create(result) = &scoped {
                let owners: Vec<String> = snapshot.exec_rows(
                    |commit| format!("SELECT area_id FROM issues AS OF '{commit}' WHERE CAST(id AS BINARY) = ? LIMIT 2"),
                    (&result.issue_id,),
                )?;
                if !matches!(owners.as_slice(), [owner] if Some(owner.as_str()) == request.area_id) {
                    return Err(workflow::unverified());
                }
            }
            if let ScopedReceipt::ArtifactCreate(scoped) = &scoped {
                artifact_create::check_current_scope(snapshot, scoped)?;
            }
            if let ScopedReceipt::AreaCreate(scoped) = &scoped {
                area_create::check_current_scope(snapshot, scoped)?;
            }
            let introduced = receipt_introduction(snapshot, request.request_id, &receipt)
                .map_err(|_| uncertain())?;
            let result = scoped
                .verify(snapshot, &introduced, &receipt)
                .map_err(|_| uncertain())?;
            require_clean(&mut snapshot.connection, &commit)?;
            runtime.check()?;
            mutation.verify_decision(observe_time()?)?;
            ("replayed", receipt.base_commit, introduced, result)
        } else {
            if has_receipt_history(snapshot, request.request_id).map_err(|_| uncertain())? {
                return Err(uncertain());
            }
            let (introduced, result) = apply(runtime, mutation, snapshot, &commit)?;
            ("confirmed", commit.clone(), introduced, result)
        };
        Ok(DoltSuccess {
            context: json!({
                "workspace_id": workspace.workspace_id(),
                "db_commit": if outcome == "confirmed" { &introduced } else { &commit },
                "policy_revision": workspace.policy_revision(),
                "scope": match request.kind {
                    Kind::AreaCreate => json!({"area_id": result["area_id"], "parent_id": request.area_id}),
                    Kind::ArtifactCreate | Kind::ArtifactUpdate => json!({"area_id": request.area_id, "artifact_id": result["artifact_id"]}),
                    Kind::ProjectCreate | Kind::ProjectAddIssue => json!({"project_id": result["project_id"], "area_id": request.area_id,
                           "issue_id": result["membership"]["issue_id"]}),
                    _ => json!({"area_id": request.area_id, "issue_id": result["issue_id"]}),
                },
            }),
            data: json!({
                "request_id": request.request_id, "kind": request.kind.name(), "outcome": outcome,
                "base_commit": base, "confirmed_commit": introduced, "result": result,
            }),
        })
    })
    .map_err(|error| {
        if matches!(request.kind, Kind::Note) && matches!(error.code, ErrorCode::CommitUncertain) {
            HiveError::new(
                ErrorCode::CommitUncertain,
                "The pending Issue note outcome could not be established.",
            )
        } else {
            error
        }
    })
}

fn apply(
    runtime: &WriterRuntime<'_>,
    mutation: &impl Mutation,
    snapshot: &mut Snapshot,
    base: &str,
) -> Result<(String, Value), HiveError> {
    let request = mutation.identity();
    let mut writer = open_verified_writer(runtime, request.kind, request.area_id, false)?;
    require_clean(&mut writer.connection, base)?;
    let db = &mut writer.connection;
    let session_id = visible_session(db, None)?;
    db.query_drop("SET SESSION dolt_transaction_commit=1")
        .map_err(read_error)?;
    db.query_drop("SET SESSION dolt_force_transaction_commit=0")
        .map_err(read_error)?;
    let mut tx = db
        .start_transaction(TxOpts::default())
        .map_err(read_error)?;
    require_clean(&mut tx, base)?;
    let guard = mutation.guard(&mut tx, snapshot)?;

    let observed = observe_time()?;
    let now = observed
        .replace_nanosecond(observed.nanosecond() / 1000 * 1000)
        .expect("valid microsecond adjustment");
    mutation.verify_decision(now)?;
    let timestamp = workflow::format_observed_at(now);
    let sql_time = timestamp[..26].replace('T', " ");
    let staged = mutation.stage(&mut tx, guard, now, &sql_time)?;
    let receipt = HistoricalReceipt {
        fingerprint: request.fingerprint.to_owned(),
        base_commit: base.to_owned(),
        created_at: format!("{}Z", &timestamp[..26]),
        kind: request.kind.name().to_owned(),
        authorization_ref: request.authorization_ref.to_owned(),
        actor_ref: request.actor.to_owned(),
        workflow_evidence: mutation.evidence()?,
        result: staged.result,
    };
    tx.exec_drop(
        "INSERT INTO operation_receipts (request_id, kind, fingerprint, actor_ref,
         authorization_ref, workflow_evidence_json, base_commit, result_json, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        (
            request.request_id,
            &receipt.kind,
            &receipt.fingerprint,
            &receipt.actor_ref,
            &receipt.authorization_ref,
            &receipt.workflow_evidence,
            base,
            &receipt.result,
            &sql_time,
        ),
    )
    .map_err(read_error)?;

    let (version, kind) = match request.kind {
        Kind::Note => (1, None),
        kind @ (Kind::Create | Kind::Update) => (2, Some(kind)),
        kind @ (Kind::ArtifactCreate | Kind::ArtifactUpdate) => (3, Some(kind)),
        kind @ (Kind::AreaCreate | Kind::ProjectCreate | Kind::ProjectAddIssue) => (4, Some(kind)),
    };
    let area_scope = request.area_id.map(str::to_owned);
    let (area_id, parent_id, issue_id, artifact_id, project_id) = match request.kind {
        Kind::AreaCreate => (Some(staged.record_id), area_scope, None, None, None),
        Kind::ArtifactCreate | Kind::ArtifactUpdate => {
            (area_scope, None, None, Some(staged.record_id), None)
        }
        Kind::ProjectCreate | Kind::ProjectAddIssue => {
            (area_scope, None, None, None, Some(staged.record_id))
        }
        _ => (area_scope, None, Some(staged.record_id), None, None),
    };
    let marker = Pending {
        version,
        kind,
        workspace_id: snapshot.workspace_id.clone(),
        launch_nonce: runtime.launch_nonce().to_owned(),
        session_id,
        request_id: request.request_id.to_owned(),
        base_commit: base.to_owned(),
        fingerprint: request.fingerprint.to_owned(),
        area_id,
        parent_id,
        issue_id,
        artifact_id,
        project_id,
        receipt_digest: receipt_digest(&receipt),
    };
    let entry = runtime
        .publish_pending(encode(&marker, request.kind)?.as_bytes())
        .map_err(|_| uncertain())?;
    mutation.verify_decision(observe_time()?)?;
    runtime.check().map_err(|_| uncertain())?;
    tx.commit().map_err(|_| uncertain())?;
    drop(writer);

    let mut confirmed = runtime.open(false).map_err(|_| uncertain())?;
    let result = confirm(&mut confirmed, &marker)
        .map_err(|_| uncertain())?
        .ok_or_else(uncertain)?;
    runtime.clear_pending(entry).map_err(|_| uncertain())?;
    Ok(result)
}

fn reconcile(runtime: &WriterRuntime<'_>, workspace: &str) -> Result<Option<Snapshot>, HiveError> {
    if !runtime.has_pending()? {
        return Ok(None);
    }
    let entry = runtime
        .pending()
        .map_err(|_| uncertain())?
        .ok_or_else(uncertain)?;
    let marker = Pending::read(&entry.bytes, workspace, runtime.launch_nonce())?;
    let kind = marker.operation()?;
    let mut observer = open_verified_writer(runtime, kind, marker.area_scope(kind), true)?;
    visible_session(&mut observer.connection, Some(marker.session_id)).map_err(|_| uncertain())?;

    // Take a fresh committed snapshot after positive same-launch session evidence.
    let mut snapshot = runtime.open(false).map_err(|_| uncertain())?;
    marker.resolve(&mut snapshot).map_err(|_| uncertain())?;
    runtime.clear_pending(entry).map_err(|_| uncertain())?;
    Ok(Some(snapshot))
}

fn open_verified_writer(
    runtime: &WriterRuntime<'_>,
    kind: Kind,
    area: Option<&str>,
    pending: bool,
) -> Result<Snapshot, HiveError> {
    let mut writer = runtime.open(true)?;
    let grantee = runtime.writer_grantee();
    let grants: Vec<(String, String, String)> = writer
        .connection
        .exec(
            "SELECT GRANTEE, TABLE_NAME, PRIVILEGE_TYPE FROM information_schema.TABLE_PRIVILEGES
         WHERE TABLE_SCHEMA = 'db' AND GRANTEE = ?
         AND TABLE_NAME IN ('issues', 'issue_updates', 'workspace_counters', 'operation_receipts',
                            'artifacts', 'artifact_areas', 'areas', 'projects', 'project_issues')",
            (&grantee,),
        )
        .map_err(read_error)?;
    if kind.grants(area).iter().all(|&(table, privilege)| {
        grants
            .iter()
            .any(|row| row.0 == grantee && row.1 == table && row.2 == privilege)
    }) {
        return Ok(writer);
    }
    let guidance = if pending {
        "Use explicit workspace stop to preserve the pending request, then complete reviewed recovery before workspace start."
    } else {
        "Use explicit workspace stop, then workspace start with the updated Hive binary."
    };
    Err(HiveError::new(
        ErrorCode::RuntimeUnavailable,
        format!(
            "The owned Dolt launch lacks the required {} writer grants. {guidance}",
            kind.label()
        ),
    ))
}

fn confirm(
    snapshot: &mut Snapshot,
    marker: &Pending,
) -> Result<Option<(String, Value)>, HiveError> {
    let head = snapshot.commit.clone();
    require_clean(&mut snapshot.connection, &head)?;
    let Some(receipt) = receipt_at(snapshot, &marker.request_id, &head, None)? else {
        return Ok(None);
    };
    if receipt.base_commit != marker.base_commit
        || receipt.fingerprint != marker.fingerprint
        || receipt_digest(&receipt) != marker.receipt_digest
    {
        return Err(uncertain());
    }
    let kind = marker.operation()?;
    let scoped = ScopedReceipt::read(
        snapshot,
        kind,
        &marker.request_id,
        marker.area_scope(kind),
        marker.record_id(kind),
        &receipt.actor_ref,
        &receipt,
    )?;
    let introduced = receipt_introduction(snapshot, &marker.request_id, &receipt)?;
    if introduced != head {
        return Err(uncertain());
    }
    let result = scoped.verify(snapshot, &introduced, &receipt)?;
    require_clean(&mut snapshot.connection, &head)?;
    Ok(Some((introduced, result)))
}

enum ScopedReceipt {
    Note(Box<issue_note_receipt::ScopedNote>),
    Create(issue_create::CreateResult),
    Update(Box<issue_update::ScopedUpdate>),
    ArtifactCreate(artifact_create::ScopedCreate),
    ArtifactUpdate(artifact_update::ScopedUpdate),
    AreaCreate(area_create::ScopedCreate),
    Project(Box<planning::ScopedProject>),
}

impl ScopedReceipt {
    fn read(
        snapshot: &Snapshot,
        kind: Kind,
        request: &str,
        area: Option<&str>,
        record: Option<&str>,
        actor: &str,
        receipt: &HistoricalReceipt,
    ) -> Result<Self, HiveError> {
        match kind {
            Kind::Note => issue_note_receipt::check_scope(
                snapshot,
                request,
                area.ok_or_else(uncertain)?,
                record.ok_or_else(uncertain)?,
                actor,
                receipt,
            )
            .map(|note| Self::Note(Box::new(note))),
            Kind::Update => {
                if receipt.kind != kind.name() {
                    return Err(conflict(kind));
                }
                issue_update::check_scope(
                    snapshot,
                    request,
                    area.ok_or_else(uncertain)?,
                    record.ok_or_else(uncertain)?,
                    actor,
                    receipt,
                )
                .map(|update| Self::Update(Box::new(update)))
            }
            Kind::Create => {
                if receipt.kind != kind.name() {
                    return Err(conflict(kind));
                }
                issue_create::check_scope(
                    snapshot,
                    request,
                    area.ok_or_else(uncertain)?,
                    record,
                    actor,
                    receipt,
                )
                .map(Self::Create)
            }
            Kind::ArtifactCreate => {
                if receipt.kind != kind.name() {
                    return Err(conflict(kind));
                }
                artifact_create::check_scope(snapshot, request, area, record, actor, receipt)
                    .map(Self::ArtifactCreate)
            }
            Kind::ArtifactUpdate => {
                if receipt.kind != kind.name() {
                    return Err(conflict(kind));
                }
                artifact_update::check_scope(
                    snapshot,
                    request,
                    area,
                    record.ok_or_else(uncertain)?,
                    actor,
                    receipt,
                )
                .map(Self::ArtifactUpdate)
            }
            Kind::AreaCreate => {
                if receipt.kind != kind.name() {
                    return Err(conflict(kind));
                }
                area_create::check_scope(snapshot, request, area, record, actor, receipt)
                    .map(Self::AreaCreate)
            }
            Kind::ProjectCreate | Kind::ProjectAddIssue => {
                if receipt.kind != kind.name() {
                    return Err(conflict(kind));
                }
                planning::check_scope(snapshot, kind, request, area, record, actor, receipt)
                    .map(|scoped| Self::Project(Box::new(scoped)))
            }
        }
    }

    fn verify(
        self,
        snapshot: &mut Snapshot,
        commit: &str,
        receipt: &HistoricalReceipt,
    ) -> Result<Value, HiveError> {
        match self {
            Self::Note(note) => {
                if note.evidence.claim_id.is_none() || note.evidence.bot_id.is_some() {
                    return Err(uncertain());
                }
                issue_note_receipt::verify_effect(
                    snapshot,
                    commit,
                    receipt,
                    &note,
                    Some("progress"),
                )?;
                Ok(json!(note.result))
            }
            Self::Update(update) => issue_update::verify_effect(snapshot, commit, receipt, &update),
            Self::ArtifactCreate(scoped) => {
                artifact_create::verify_effect(snapshot, commit, receipt, &scoped)
            }
            Self::ArtifactUpdate(scoped) => {
                artifact_update::verify_effect(snapshot, commit, receipt, &scoped)
            }
            Self::AreaCreate(scoped) => {
                area_create::verify_effect(snapshot, commit, receipt, &scoped)
            }
            Self::Project(scoped) => planning::verify_effect(snapshot, commit, receipt, *scoped),
            Self::Create(result) => {
                issue_create::verify_effect(snapshot, commit, receipt, &result)?;
                Ok(json!(result))
            }
        }
    }
}

fn check_fingerprint(actual: &str, expected: &str, kind: Kind) -> Result<(), HiveError> {
    if actual == expected {
        Ok(())
    } else {
        Err(conflict(kind))
    }
}

fn conflict(kind: Kind) -> HiveError {
    HiveError::new(
        ErrorCode::RequestConflict,
        format!("The request UUID is bound to a different {}.", kind.label()),
    )
}

pub(crate) fn uncertain() -> HiveError {
    HiveError::new(
        ErrorCode::CommitUncertain,
        "The pending Dolt write outcome could not be established.",
    )
}

pub(super) fn encode(value: &impl Serialize, kind: Kind) -> Result<String, HiveError> {
    serde_json::to_string(value).map_err(|_| {
        HiveError::new(
            ErrorCode::InternalError,
            format!("Could not encode the {} record.", kind.label()),
        )
    })
}
fn require_clean(db: &mut impl Queryable, base: &str) -> Result<(), HiveError> {
    let status: Option<(String, String, bool)> = db
        .query_first(
            "SELECT ACTIVE_BRANCH(), DOLT_HASHOF('HEAD'), EXISTS(SELECT 1 FROM dolt_status)",
        )
        .map_err(read_error)?;
    if matches!(status, Some((branch, head, false)) if branch == "main" && head == base) {
        Ok(())
    } else {
        Err(HiveError::new(
            ErrorCode::DirtyConflict,
            "The selected main HEAD or working set changed.",
        ))
    }
}

fn visible_session(db: &mut impl Queryable, prior: Option<u64>) -> Result<u64, HiveError> {
    let observation: Option<(u64, bool, bool)> = db
        .exec_first(
            "SELECT CONNECTION_ID(),
           EXISTS(SELECT 1 FROM information_schema.PROCESSLIST WHERE ID = CONNECTION_ID()),
           EXISTS(SELECT 1 FROM information_schema.PROCESSLIST WHERE ID = ?)",
            (prior.unwrap_or(0),),
        )
        .map_err(read_error)?;
    match observation {
        Some((id, true, false)) if id != 0 && Some(id) != prior => Ok(id),
        _ => Err(uncertain()),
    }
}

fn receipt_digest(receipt: &HistoricalReceipt) -> String {
    digest(
        json!([
            receipt.kind,
            receipt.actor_ref,
            receipt.fingerprint,
            receipt.authorization_ref,
            receipt.base_commit,
            receipt.created_at,
            receipt.workflow_evidence,
            receipt.result,
        ])
        .to_string()
        .as_bytes(),
    )
}

pub(super) fn new_uuid_v7(now: OffsetDateTime) -> Result<String, HiveError> {
    let random = token()?;
    let millis = (now.unix_timestamp_nanos() / 1_000_000) as u64;
    let time = format!("{millis:012x}");
    Ok(format!("{}-{}-7{}", &time[..8], &time[8..], &random[15..]))
}

pub(super) fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub(super) fn invalid(message: &'static str) -> HiveError {
    HiveError::new(ErrorCode::InvalidArgument, message)
}

pub(super) fn valid_revision(revision: &str) -> bool {
    revision.chars().count() <= 36 && workflow::valid_ref(revision)
}

pub(super) fn valid_title(title: &str) -> bool {
    !title.trim().is_empty() && title.chars().count() <= 255 && !title.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_marker_versions_keep_the_note_wire_format() {
        let mut value = json!({
            "version": 1,
            "workspace_id": "workspace",
            "launch_nonce": "launch",
            "session_id": 42,
            "request_id": "01900000-0000-7000-8000-000000000001",
            "base_commit": "a".repeat(32),
            "fingerprint": "b".repeat(64),
            "area_id": "01900000-0000-7000-8000-000000000002",
            "issue_id": "01900000-0000-7000-8000-000000000003",
            "receipt_digest": "c".repeat(64),
        });
        let read =
            |value: &Value| Pending::read(value.to_string().as_bytes(), "workspace", "launch");
        let legacy = read(&value).unwrap();
        assert!(matches!(legacy.operation(), Ok(Kind::Note)));
        assert_eq!(json!(legacy), value);

        value["version"] = json!(2);
        assert!(read(&value).is_err());
        value["kind"] = json!("issue.create");
        let create = read(&value).unwrap();
        assert!(matches!(create.operation(), Ok(Kind::Create)));
        assert_eq!(json!(create), value);
        value["kind"] = json!("issue.update");
        let update = read(&value).unwrap();
        assert!(matches!(update.operation(), Ok(Kind::Update)));
        assert_eq!(json!(update), value);
        value["extra"] = json!(true);
        assert!(read(&value).is_err());
        value.as_object_mut().unwrap().remove("extra");
        value["version"] = json!(3);
        assert!(read(&value).is_err());

        value["kind"] = json!("artifact.create");
        let issue_id = value.as_object_mut().unwrap().remove("issue_id").unwrap();
        value["artifact_id"] = issue_id;
        let linked = read(&value).unwrap();
        assert!(matches!(linked.operation(), Ok(Kind::ArtifactCreate)));
        assert_eq!(json!(linked), value);
        let area_id = value.as_object_mut().unwrap().remove("area_id").unwrap();
        let standalone = read(&value).unwrap();
        assert_eq!(json!(standalone), value);
        value["parent_id"] = area_id.clone();
        assert!(read(&value).is_err());
        value.as_object_mut().unwrap().remove("parent_id");
        value["kind"] = json!("artifact.update");
        let standalone = read(&value).unwrap();
        assert!(matches!(standalone.operation(), Ok(Kind::ArtifactUpdate)));
        assert_eq!(
            standalone.record_id(Kind::ArtifactUpdate),
            value["artifact_id"].as_str(),
        );
        assert_eq!(json!(standalone), value);
        value["area_id"] = area_id;
        let linked = read(&value).unwrap();
        assert!(matches!(linked.operation(), Ok(Kind::ArtifactUpdate)));
        assert_eq!(json!(linked), value);
        value["issue_id"] = value["artifact_id"].clone();
        assert!(read(&value).is_err());

        value["version"] = json!(1);
        assert!(read(&value).is_err());
        value["kind"] = Value::Null;
        assert!(read(&value).is_err());
        let legacy_bytes = serde_json::to_string(&legacy).unwrap();
        let duplicate = legacy_bytes.replacen(r#""version":1"#, r#""version":1,"version":1"#, 1);
        assert!(Pending::read(duplicate.as_bytes(), "workspace", "launch").is_err());

        let mut project = value;
        project["version"] = json!(4);
        project["project_id"] = project
            .as_object_mut()
            .unwrap()
            .remove("artifact_id")
            .unwrap();
        project.as_object_mut().unwrap().remove("issue_id");
        project["kind"] = json!("project.add-issue");
        let linked = read(&project).unwrap();
        assert!(matches!(linked.operation(), Ok(Kind::ProjectAddIssue)));
        assert_eq!(json!(linked), project);

        project["kind"] = json!("project.create");
        assert!(read(&project).is_err());
        project.as_object_mut().unwrap().remove("area_id");
        let created = read(&project).unwrap();
        assert!(matches!(created.operation(), Ok(Kind::ProjectCreate)));
        assert_eq!(json!(created), project);
        project["version"] = json!(3);
        assert!(read(&project).is_err());

        let mut value = json!(legacy);
        value.as_object_mut().unwrap().remove("issue_id");
        value["version"] = json!(4);
        value["kind"] = json!("area.create");
        let root = read(&value).unwrap();
        assert!(matches!(root.operation(), Ok(Kind::AreaCreate)));
        assert_eq!(json!(root), value);
        value["parent_id"] = json!("01900000-0000-7000-8000-000000000003");
        let child = read(&value).unwrap();
        assert_eq!(json!(child), value);
        value["parent_id"] = value["area_id"].clone();
        assert!(read(&value).is_err());
        value["parent_id"] = Value::Null;
        assert!(read(&value).is_err());
    }
}
