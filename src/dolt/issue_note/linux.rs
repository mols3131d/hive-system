use mysql::TxOpts;
use mysql::prelude::Queryable;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

use super::{CommonOptions, DoltSuccess, ErrorCode, HiveError, IssueNoteArgs, uncertain};
use crate::dolt::{
    DoltTarget, Snapshot,
    context_evidence::read_evidence_bytes,
    context_resolve::observe_time,
    context_workflow as workflow, is_commit_hash, is_uuid, is_uuid_v7,
    issue_note_fingerprint::{KIND, MAX_BODY_BYTES, NoteFingerprint},
    issue_note_receipt::{self, NoteEvidence, NoteResult, ScopedNote},
    read_error,
    receipt_history::{HistoricalReceipt, has_receipt_history, receipt_at, receipt_introduction},
    scope::require_active_area,
};
use crate::workspace_status::{WriterRuntime, token, with_writer_runtime};

struct Request<'a> {
    args: &'a IssueNoteArgs,
    workspace: &'a str,
    expected: &'a str,
    body: String,
    fingerprint: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Decision {
    decision: String,
    #[serde(deserialize_with = "deserialize_evidence")]
    evidence: NoteEvidence,
}

fn deserialize_evidence<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<NoteEvidence, D::Error> {
    workflow::parse_json_object(<&serde_json::value::RawValue>::deserialize(deserializer)?)
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Pending {
    version: u8,
    workspace_id: String,
    launch_nonce: String,
    session_id: u64,
    request_id: String,
    base_commit: String,
    fingerprint: String,
    area_id: String,
    issue_id: String,
    receipt_digest: String,
}

pub(super) fn write(
    common: &CommonOptions,
    args: &IssueNoteArgs,
) -> Result<DoltSuccess, HiveError> {
    let target = DoltTarget::parse(&args.target)?;
    let workspace =
        crate::config_v2::read_explicit(common.workspace.as_deref().expect("validated workspace"))?;
    let request = Request::read(args, workspace.workspace_id())?;
    let bytes = read_evidence_bytes(&args.evidence_file, workflow::MAX_DECLARATION_BYTES)?;
    let decision: Decision = workflow::parse_json_object::<_, serde_json::Error>(
        serde_json::from_slice(&bytes).map_err(|_| workflow::unverified())?,
    )
    .map_err(|_| workflow::unverified())?;
    request.verify_decision(&decision, observe_time()?)?;
    with_writer_runtime(&workspace, target, |runtime, snapshot| {
        request.verify_decision(&decision, observe_time()?)?;
        scope_at(snapshot, &request)?;
        if let Some(recovered) = reconcile(runtime, request.workspace)? {
            *snapshot = recovered;
            scope_at(snapshot, &request)?;
        }
        require_clean(&mut snapshot.connection, &snapshot.commit)?;
        let commit = snapshot.commit.clone();
        let existing =
            receipt_at(snapshot, &args.request_id, &commit, None).map_err(|_| uncertain())?;
        request.verify_decision(&decision, observe_time()?)?;
        let (outcome, base, introduced, result) = if let Some(receipt) = existing {
            let note = issue_note_receipt::check_scope(
                snapshot,
                &args.request_id,
                &args.area,
                &args.issue,
                &args.actor,
                &receipt,
            )?;
            check_fingerprint(&receipt.fingerprint, &request.fingerprint)?;
            let introduced = receipt_introduction(snapshot, &args.request_id, &receipt)
                .map_err(|_| uncertain())?;
            verify_progress(snapshot, &introduced, &receipt, &note).map_err(|_| uncertain())?;
            require_clean(&mut snapshot.connection, &commit)?;
            runtime.check()?;
            request.verify_decision(&decision, observe_time()?)?;
            ("replayed", receipt.base_commit, introduced, note.result)
        } else {
            if has_receipt_history(snapshot, &args.request_id).map_err(|_| uncertain())? {
                return Err(uncertain());
            }
            let (introduced, result) = apply(runtime, &request, &decision, &commit)?;
            ("confirmed", commit.clone(), introduced, result)
        };
        Ok(DoltSuccess {
            context: json!({
                "workspace_id": request.workspace, "db_commit": if outcome == "confirmed" { &introduced } else { &commit },
                "policy_revision": workspace.policy_revision(),
                "scope": {"area_id": args.area, "issue_id": args.issue},
            }),
            data: json!({
                "request_id": args.request_id, "kind": KIND, "outcome": outcome,
                "base_commit": base, "confirmed_commit": introduced, "result": result,
            }),
        })
    })
}

impl<'a> Request<'a> {
    fn read(args: &'a IssueNoteArgs, workspace: &'a str) -> Result<Self, HiveError> {
        let expected = args
            .if_match
            .split_once('=')
            .filter(|(id, revision)| {
                *id == args.issue && revision.chars().count() <= 36 && workflow::valid_ref(revision)
            })
            .map(|(_, revision)| revision)
            .ok_or_else(|| invalid("Supply one Issue revision as --if-match ISSUE=REV."))?;
        if ![&args.area, &args.issue, &args.claim]
            .into_iter()
            .all(|id| is_uuid_v7(id))
            || !is_uuid(&args.request_id)
            || !workflow::valid_ref(&args.actor)
        {
            return Err(invalid(
                "Select exact Area, Issue and Claim UUIDv7s, a request UUID and an actor reference.",
            ));
        }
        let body_bytes = read_evidence_bytes(&args.file, MAX_BODY_BYTES).map_err(|error| {
            let message = if matches!(error.code, ErrorCode::ContextLimit) {
                "The progress note exceeds the supported byte limit."
            } else {
                "Could not read the explicit regular progress note file."
            };
            HiveError::new(error.code, message)
        })?;
        let body = String::from_utf8(body_bytes)
            .map_err(|_| invalid("The progress note must be UTF-8."))?;
        if body.trim().is_empty() {
            return Err(invalid("The progress note must have a body."));
        }
        let fingerprint = NoteFingerprint {
            workspace_id: workspace,
            area_id: &args.area,
            issue_id: &args.issue,
            actor_ref: &args.actor,
            claim_id: Some(&args.claim),
            expected_issue_revision: expected,
            note_kind: &args.kind,
        }
        .with_body_digest(&digest(body.as_bytes()));
        Ok(Self {
            args,
            workspace,
            expected,
            body,
            fingerprint,
        })
    }

    fn verify_decision(&self, decision: &Decision, now: OffsetDateTime) -> Result<(), HiveError> {
        let e = &decision.evidence;
        if decision.decision != "allow"
            || e.version != 1
            || e.operation != KIND
            || e.workspace_id != self.workspace
            || e.request_id != self.args.request_id
            || e.fingerprint != self.fingerprint
            || e.actor_ref != self.args.actor
            || e.area_id != self.args.area
            || e.issue_id != self.args.issue
            || e.expected_issue_revision != self.expected
            || e.claim_id.as_deref() != Some(self.args.claim.as_str())
            || e.bot_id.is_some()
            || e.withdrawn
            || ![
                &e.authorization_ref,
                &e.evidence_id,
                &e.revision,
                &e.source_ref,
                &e.issuer_ref,
            ]
            .into_iter()
            .all(|value| workflow::valid_ref(value))
            || !matches!((workflow::parse_time(&e.valid_from), workflow::parse_time(&e.expires_at)),
                (Some(start), Some(end)) if start <= now && now < end)
        {
            return Err(workflow::unverified());
        }
        Ok(())
    }
}

fn scope_at(snapshot: &mut Snapshot, request: &Request<'_>) -> Result<(), HiveError> {
    let owners: Vec<String> = snapshot.exec_rows(
        |commit| {
            format!(
                "SELECT i.area_id FROM issues AS OF '{commit}' i
             JOIN areas AS OF '{commit}' a ON CAST(a.id AS BINARY) = CAST(i.area_id AS BINARY)
             WHERE CAST(i.id AS BINARY) = ? LIMIT 2"
            )
        },
        (&request.args.issue,),
    )?;
    if matches!(owners.as_slice(), [area] if area == &request.args.area) {
        Ok(())
    } else {
        Err(workflow::unverified())
    }
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

fn guard_issue(db: &mut impl Queryable, request: &Request<'_>) -> Result<(), HiveError> {
    let args = request.args;
    let issue: Option<(String, String, String, String)> = db
        .exec_first(
            "SELECT i.area_id, i.revision, i.status, i.type FROM issues i
         JOIN areas a ON CAST(a.id AS BINARY) = CAST(i.area_id AS BINARY)
         WHERE CAST(i.id AS BINARY) = ?",
            (&args.issue,),
        )
        .map_err(read_error)?;
    let claim: Option<(String, String)> = db
        .exec_first(
            "SELECT claim_id, holder_ref FROM issue_claims WHERE CAST(issue_id AS BINARY) = ?",
            (&args.issue,),
        )
        .map_err(read_error)?;
    verify_issue(issue, claim, request)?;
    require_active_area(db, &args.area)
}

fn verify_issue(
    issue: Option<(String, String, String, String)>,
    claim: Option<(String, String)>,
    request: &Request<'_>,
) -> Result<(), HiveError> {
    let Some((_, revision, status, kind)) = issue.filter(|row| row.0 == request.args.area) else {
        return Err(workflow::unverified());
    };
    if revision != request.expected {
        return Err(HiveError::new(
            ErrorCode::StaleRevision,
            "The Issue revision changed.",
        ));
    }
    if status != "in-progress" || !matches!(kind.as_str(), "feature" | "task" | "bug") {
        return Err(HiveError::new(
            ErrorCode::InvalidTransition,
            "A progress note requires an in-progress executable Issue.",
        ));
    }
    if !matches!(claim, Some((id, holder)) if id == request.args.claim && holder == request.args.actor)
    {
        return Err(HiveError::new(
            ErrorCode::ClaimMismatch,
            "The exact active Claim and holder are required.",
        ));
    }
    Ok(())
}

fn apply(
    runtime: &WriterRuntime<'_>,
    request: &Request<'_>,
    decision: &Decision,
    base: &str,
) -> Result<(String, NoteResult), HiveError> {
    let mut writer = open_verified_writer(runtime, false)?;
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
    guard_issue(&mut tx, request)?;

    let observed = observe_time()?;
    let now = observed
        .replace_nanosecond(observed.nanosecond() / 1000 * 1000)
        .expect("valid microsecond adjustment");
    request.verify_decision(decision, now)?;
    let timestamp = workflow::format_observed_at(now);
    let created_at = format!("{}Z", &timestamp[..26]);
    let sql_time = timestamp[..26].replace('T', " ");
    let args = request.args;
    let result = NoteResult {
        version: 1,
        area_id: args.area.clone(),
        issue_id: args.issue.clone(),
        note_id: new_uuid_v7(now)?,
        issue_revision: new_uuid_v7(now)?,
    };
    let receipt = HistoricalReceipt {
        fingerprint: request.fingerprint.clone(),
        base_commit: base.to_owned(),
        created_at,
        kind: KIND.to_owned(),
        authorization_ref: decision.evidence.authorization_ref.clone(),
        actor_ref: args.actor.clone(),
        workflow_evidence: encode(&decision.evidence)?,
        result: encode(&result)?,
    };
    tx.exec_drop(
        "UPDATE issues SET revision = ?, updated_at = ?
         WHERE CAST(id AS BINARY) = ? AND CAST(area_id AS BINARY) = ?
           AND CAST(revision AS BINARY) = ? AND status = 'in-progress'",
        (
            &result.issue_revision,
            &sql_time,
            &args.issue,
            &args.area,
            request.expected,
        ),
    )
    .map_err(read_error)?;
    if tx.affected_rows() != 1 {
        return Err(HiveError::new(
            ErrorCode::StaleRevision,
            "The Issue revision changed.",
        ));
    }
    tx.exec_drop(
        "INSERT INTO issue_updates (id, issue_id, kind, body, author_ref, created_at)
         VALUES (?, ?, 'progress', ?, ?, ?)",
        (
            &result.note_id,
            &args.issue,
            &request.body,
            &args.actor,
            &sql_time,
        ),
    )
    .map_err(read_error)?;
    tx.exec_drop(
        "INSERT INTO operation_receipts (request_id, kind, fingerprint, actor_ref,
         authorization_ref, workflow_evidence_json, base_commit, result_json, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        (
            &args.request_id,
            KIND,
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

    let marker = Pending {
        version: 1,
        workspace_id: request.workspace.to_owned(),
        launch_nonce: runtime.launch_nonce().to_owned(),
        session_id,
        request_id: args.request_id.clone(),
        base_commit: base.to_owned(),
        fingerprint: request.fingerprint.clone(),
        area_id: args.area.clone(),
        issue_id: args.issue.clone(),
        receipt_digest: receipt_digest(&receipt),
    };
    let entry = runtime
        .publish_pending(encode(&marker)?.as_bytes())
        .map_err(|_| uncertain())?;
    request.verify_decision(decision, observe_time()?)?;
    runtime.check().map_err(|_| uncertain())?;
    tx.commit().map_err(|_| uncertain())?;
    drop(writer);

    let mut confirmed = runtime.open(false).map_err(|_| uncertain())?;
    let (introduced, confirmed_result) = confirm(&mut confirmed, &marker)
        .map_err(|_| uncertain())?
        .ok_or_else(uncertain)?;
    runtime.clear_pending(entry).map_err(|_| uncertain())?;
    Ok((introduced, confirmed_result))
}

fn reconcile(runtime: &WriterRuntime<'_>, workspace: &str) -> Result<Option<Snapshot>, HiveError> {
    if !runtime.has_pending()? {
        return Ok(None);
    }
    let mut observer = open_verified_writer(runtime, true)?;
    let entry = runtime
        .pending()
        .map_err(|_| uncertain())?
        .ok_or_else(uncertain)?;
    let marker = Pending::read(&entry.bytes, workspace, runtime.launch_nonce())?;
    visible_session(&mut observer.connection, Some(marker.session_id)).map_err(|_| uncertain())?;

    // Take the committed snapshot only after positive same-launch session evidence.
    let mut snapshot = runtime.open(false).map_err(|_| uncertain())?;
    marker.resolve(&mut snapshot).map_err(|_| uncertain())?;
    runtime.clear_pending(entry).map_err(|_| uncertain())?;
    Ok(Some(snapshot))
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

    /// Requires positive prior-session termination and a fresh verified snapshot.
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

    fn validate(&self, workspace: &str, nonce: &str) -> Result<(), HiveError> {
        if self.version != 1
            || self.workspace_id != workspace
            || self.launch_nonce != nonce
            || self.session_id == 0
            || !is_uuid(&self.request_id)
            || !is_commit_hash(&self.base_commit)
            || !is_uuid_v7(&self.area_id)
            || !is_uuid_v7(&self.issue_id)
            || ![&self.fingerprint, &self.receipt_digest]
                .into_iter()
                .all(|hash| {
                    hash.len() == 64
                        && hash
                            .bytes()
                            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                })
        {
            return Err(uncertain());
        }
        Ok(())
    }
}

fn open_verified_writer(runtime: &WriterRuntime<'_>, pending: bool) -> Result<Snapshot, HiveError> {
    let mut writer = runtime.open(true)?;
    verify_writer_grants(&mut writer.connection, &runtime.writer_grantee(), pending)?;
    Ok(writer)
}

fn verify_writer_grants(
    db: &mut impl Queryable,
    grantee: &str,
    pending: bool,
) -> Result<(), HiveError> {
    let grants: Vec<(String, String, String)> = db
        .exec(
            "SELECT GRANTEE, TABLE_NAME, PRIVILEGE_TYPE FROM information_schema.TABLE_PRIVILEGES
         WHERE TABLE_SCHEMA = 'db' AND GRANTEE = ?
         AND TABLE_NAME IN ('issues', 'issue_updates', 'operation_receipts')",
            (grantee,),
        )
        .map_err(read_error)?;
    if [
        ("issues", "UPDATE"),
        ("issue_updates", "INSERT"),
        ("operation_receipts", "INSERT"),
    ]
    .into_iter()
    .all(|(table, privilege)| {
        grants
            .iter()
            .any(|row| row.0 == grantee && row.1 == table && row.2 == privilege)
    }) {
        return Ok(());
    }
    let guidance = if pending {
        "Use explicit workspace stop to preserve the pending request, then complete reviewed recovery before workspace start."
    } else {
        "Use explicit workspace stop, then workspace start with the updated Hive binary."
    };
    Err(HiveError::new(
        ErrorCode::RuntimeUnavailable,
        format!("The owned Dolt launch lacks the required Issue note writer grants. {guidance}"),
    ))
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

fn confirm(
    snapshot: &mut Snapshot,
    marker: &Pending,
) -> Result<Option<(String, NoteResult)>, HiveError> {
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
    let note = issue_note_receipt::check_scope(
        snapshot,
        &marker.request_id,
        &marker.area_id,
        &marker.issue_id,
        &receipt.actor_ref,
        &receipt,
    )?;
    let introduced = receipt_introduction(snapshot, &marker.request_id, &receipt)?;
    if introduced != head {
        return Err(uncertain());
    }
    verify_progress(snapshot, &introduced, &receipt, &note)?;
    require_clean(&mut snapshot.connection, &head)?;
    Ok(Some((introduced, note.result)))
}

fn verify_progress(
    snapshot: &mut Snapshot,
    commit: &str,
    receipt: &HistoricalReceipt,
    note: &ScopedNote,
) -> Result<(), HiveError> {
    if note.evidence.claim_id.is_none() || note.evidence.bot_id.is_some() {
        return Err(uncertain());
    }
    issue_note_receipt::verify_effect(snapshot, commit, receipt, note, Some("progress"))
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

fn check_fingerprint(actual: &str, expected: &str) -> Result<(), HiveError> {
    if actual == expected {
        Ok(())
    } else {
        Err(HiveError::new(
            ErrorCode::RequestConflict,
            "The request UUID is bound to a different Issue note.",
        ))
    }
}

fn new_uuid_v7(now: OffsetDateTime) -> Result<String, HiveError> {
    let random = token()?;
    let millis = (now.unix_timestamp_nanos() / 1_000_000) as u64;
    let time = format!("{millis:012x}");
    Ok(format!("{}-{}-7{}", &time[..8], &time[8..], &random[15..]))
}

fn encode(value: &impl Serialize) -> Result<String, HiveError> {
    serde_json::to_string(value).map_err(|_| {
        HiveError::new(
            ErrorCode::InternalError,
            "Could not encode the Issue note record.",
        )
    })
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn invalid(message: &'static str) -> HiveError {
    HiveError::new(ErrorCode::InvalidArgument, message)
}
