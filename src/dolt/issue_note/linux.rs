use mysql::prelude::Queryable;
use time::OffsetDateTime;

use super::{CommonOptions, DoltSuccess, ErrorCode, HiveError, IssueNoteArgs};
use crate::dolt::{
    DoltTarget, Snapshot,
    context_evidence::read_evidence_bytes,
    context_workflow as workflow, is_uuid, is_uuid_v7,
    issue_note_fingerprint::{KIND, MAX_BODY_BYTES, NoteFingerprint},
    issue_note_receipt::{NoteEvidence, NoteResult},
    issue_writer::{
        self, Decision, Kind, Mutation, StagedIssue, WriteIdentity, digest, invalid, new_uuid_v7,
    },
    read_error,
    scope::require_active_area,
};

struct Request<'a> {
    args: &'a IssueNoteArgs,
    workspace: &'a str,
    expected: &'a str,
    body: String,
    fingerprint: String,
}

pub(super) fn write(
    common: &CommonOptions,
    args: &IssueNoteArgs,
) -> Result<DoltSuccess, HiveError> {
    let target = DoltTarget::parse(&args.target)?;
    let workspace =
        crate::config_v2::read_explicit(common.workspace.as_deref().expect("validated workspace"))?;
    let request = Request::read(args, workspace.workspace_id())?;
    let decision: Decision<NoteEvidence> = issue_writer::read_decision(&args.evidence_file)?;
    issue_writer::execute(&workspace, target, &Progress { request, decision })
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

    fn verify_decision(
        &self,
        decision: &Decision<NoteEvidence>,
        now: OffsetDateTime,
    ) -> Result<(), HiveError> {
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
    require_active_area(
        db,
        &args.area,
        "A progress note requires active Area ancestry.",
    )
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

struct Progress<'a> {
    request: Request<'a>,
    decision: Decision<NoteEvidence>,
}

impl Mutation for Progress<'_> {
    type Guard = ();

    fn identity(&self) -> WriteIdentity<'_> {
        let args = self.request.args;
        WriteIdentity {
            kind: Kind::Note,
            request_id: &args.request_id,
            area_id: &args.area,
            issue_id: Some(&args.issue),
            actor: &args.actor,
            fingerprint: &self.request.fingerprint,
            authorization_ref: &self.decision.evidence.authorization_ref,
        }
    }

    fn verify_decision(&self, now: OffsetDateTime) -> Result<(), HiveError> {
        self.request.verify_decision(&self.decision, now)
    }

    fn evidence(&self) -> Result<String, HiveError> {
        issue_writer::encode(&self.decision.evidence, Kind::Note)
    }

    fn check_scope(&self, snapshot: &mut Snapshot) -> Result<(), HiveError> {
        scope_at(snapshot, &self.request)
    }

    fn guard(&self, db: &mut mysql::Transaction<'_>, _: &mut Snapshot) -> Result<(), HiveError> {
        guard_issue(db, &self.request)
    }

    fn stage(
        &self,
        tx: &mut mysql::Transaction<'_>,
        _: (),
        now: OffsetDateTime,
        sql_time: &str,
    ) -> Result<StagedIssue, HiveError> {
        let request = &self.request;
        let args = request.args;
        let result = NoteResult {
            version: 1,
            area_id: args.area.clone(),
            issue_id: args.issue.clone(),
            note_id: new_uuid_v7(now)?,
            issue_revision: new_uuid_v7(now)?,
        };
        tx.exec_drop(
            "UPDATE issues SET revision = ?, updated_at = ?
         WHERE CAST(id AS BINARY) = ? AND CAST(area_id AS BINARY) = ?
           AND CAST(revision AS BINARY) = ? AND status = 'in-progress'",
            (
                &result.issue_revision,
                sql_time,
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
                sql_time,
            ),
        )
        .map_err(read_error)?;

        Ok(StagedIssue {
            issue_id: args.issue.clone(),
            result: issue_writer::encode(&result, Kind::Note)?,
        })
    }
}
