mod reconcile;

use super::Input;
use mysql::{Transaction, prelude::Queryable};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use time::OffsetDateTime;

use crate::cli::{CommonOptions, IssueTransitionArgs};
use crate::error::{ErrorCode, HiveError};

use super::super::{
    DoltSuccess, DoltTarget, MAX_ROWS_PER_TABLE, Snapshot, context_workflow as workflow,
    is_commit_hash, is_uuid, is_uuid_v7,
    issue_readiness::{Blocker, NON_WHITESPACE},
    issue_writer::{
        self, Decision, Kind, Mutation, StagedMutation, WriteIdentity, digest, invalid,
        new_uuid_v7, valid_revision,
    },
    read_error,
    receipt_history::{HistoricalReceipt, verify_delta},
    records::{IssueStatus, is_issue_type},
    scope::{require_active_area, require_active_area_at},
};

#[derive(Clone, Copy, Deserialize, PartialEq, Eq, Serialize)]
pub(in crate::dolt) enum Operation {
    #[serde(rename = "issue.select")]
    Select,
    #[serde(rename = "issue.claim")]
    Claim,
    #[serde(rename = "issue.cancel")]
    Cancel,
    #[serde(rename = "issue.reopen")]
    Reopen,
}

impl Operation {
    pub(in crate::dolt) fn parse(kind: &str) -> Option<Self> {
        match kind {
            "issue.select" => Some(Self::Select),
            "issue.claim" => Some(Self::Claim),
            "issue.cancel" => Some(Self::Cancel),
            "issue.reopen" => Some(Self::Reopen),
            _ => None,
        }
    }

    pub(in crate::dolt) fn name(self) -> &'static str {
        match self {
            Self::Select => "issue.select",
            Self::Claim => "issue.claim",
            Self::Cancel => "issue.cancel",
            Self::Reopen => "issue.reopen",
        }
    }

    fn next_status(self, previous: &str) -> Option<&'static str> {
        match (self, IssueStatus::parse(previous)?) {
            (Self::Select, IssueStatus::Backlog) => Some("todo"),
            (Self::Claim, IssueStatus::Todo) => Some("in-progress"),
            (Self::Cancel | Self::Reopen, status) => reconcile::next_status(self, status),
            _ => None,
        }
    }

    fn is_reconciliation(self) -> bool {
        matches!(self, Self::Cancel | Self::Reopen)
    }
}

#[derive(Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
struct EvidenceRefs {
    authorization_ref: String,
    evidence_id: String,
    revision: String,
    source_ref: String,
    issuer_ref: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Evidence {
    version: u8,
    operation: Operation,
    workspace_id: String,
    request_id: String,
    fingerprint: String,
    actor_ref: String,
    bot_id: Option<String>,
    area_id: String,
    issue_id: String,
    expected_issue_revision: String,
    holder_ref: Option<String>,
    authorization_ref: String,
    evidence_id: String,
    revision: String,
    source_ref: String,
    issuer_ref: String,
    valid_from: String,
    expires_at: String,
    withdrawn: bool,
}

impl Evidence {
    fn references(&self) -> EvidenceRefs {
        EvidenceRefs {
            authorization_ref: self.authorization_ref.clone(),
            evidence_id: self.evidence_id.clone(),
            revision: self.revision.clone(),
            source_ref: self.source_ref.clone(),
            issuer_ref: self.issuer_ref.clone(),
        }
    }
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "operation", deny_unknown_fields)]
enum Effect {
    #[serde(rename = "issue.select")]
    Select {},
    #[serde(rename = "issue.claim")]
    Claim {
        claim_id: String,
        holder_ref: String,
    },
    #[serde(rename = "issue.cancel")]
    Cancel { reason_sha256: String },
    #[serde(rename = "issue.reopen")]
    Reopen { reason_sha256: String },
}

impl Effect {
    fn operation(&self) -> Operation {
        match self {
            Self::Select {} => Operation::Select,
            Self::Claim { .. } => Operation::Claim,
            Self::Cancel { .. } => Operation::Cancel,
            Self::Reopen { .. } => Operation::Reopen,
        }
    }

    fn claim(&self) -> Option<(&str, &str)> {
        match self {
            Self::Select {} | Self::Cancel { .. } | Self::Reopen { .. } => None,
            Self::Claim {
                claim_id,
                holder_ref,
            } => Some((claim_id, holder_ref)),
        }
    }

    fn reason_sha256(&self) -> Option<&str> {
        match self {
            Self::Cancel { reason_sha256 } | Self::Reopen { reason_sha256 } => Some(reason_sha256),
            _ => None,
        }
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(in crate::dolt) struct Record {
    version: u8,
    transition_id: String,
    request_id: String,
    area_id: String,
    issue_id: String,
    previous_issue_revision: String,
    issue_revision: String,
    previous_status: String,
    status: String,
    actor_ref: String,
    evidence: EvidenceRefs,
    effect: Effect,
}

struct Request<'a> {
    operation: Operation,
    workspace: &'a str,
    area: &'a str,
    issue: &'a str,
    actor: &'a str,
    id: &'a str,
    expected: &'a str,
    holder: Option<&'a str>,
    reason_sha256: Option<&'a str>,
}

impl Request<'_> {
    fn fingerprint(&self) -> String {
        digest(
            json!([
                1,
                self.operation,
                self.workspace,
                self.area,
                self.issue,
                self.actor,
                self.expected,
                self.holder.or(self.reason_sha256),
            ])
            .to_string()
            .as_bytes(),
        )
    }

    fn verify(
        &self,
        evidence: &Evidence,
        fingerprint: &str,
        now: OffsetDateTime,
    ) -> Result<(), HiveError> {
        if evidence.version != 1
            || evidence.operation != self.operation
            || evidence.workspace_id != self.workspace
            || evidence.area_id != self.area
            || evidence.issue_id != self.issue
            || evidence.actor_ref != self.actor
            || evidence.request_id != self.id
            || evidence.expected_issue_revision != self.expected
            || evidence.holder_ref.as_deref() != self.holder
            || evidence.fingerprint != fingerprint
            || evidence.bot_id.is_some()
            || evidence.withdrawn
            || ![
                &evidence.authorization_ref,
                &evidence.evidence_id,
                &evidence.revision,
                &evidence.source_ref,
                &evidence.issuer_ref,
            ]
            .into_iter()
            .all(|value| workflow::valid_ref(value))
            || !matches!((workflow::parse_time(&evidence.valid_from), workflow::parse_time(&evidence.expires_at)),
                (Some(start), Some(end)) if start <= now && now < end)
        {
            return Err(workflow::unverified());
        }
        Ok(())
    }
}

struct Transition<'a> {
    request: Request<'a>,
    fingerprint: String,
    decision: Decision<Evidence>,
    reason: Option<&'a str>,
}

pub(super) fn write(
    common: &CommonOptions,
    args: &IssueTransitionArgs,
    input: Input<'_>,
) -> Result<DoltSuccess, HiveError> {
    let target = DoltTarget::parse(&args.target)?;
    let workspace =
        crate::config_v2::read_explicit(common.workspace.as_deref().expect("validated workspace"))?;
    let (operation, holder, reason) = match input {
        Input::Select => (Operation::Select, None, None),
        Input::Claim(holder) => (Operation::Claim, Some(holder), None),
        Input::Cancel(reason) => (Operation::Cancel, None, Some(reason)),
        Input::Reopen(reason) => (Operation::Reopen, None, Some(reason)),
    };
    let reason_sha256 = reason.map(reconcile::reason_digest).transpose()?;
    if !is_uuid_v7(&args.area)
        || !is_uuid_v7(&args.issue)
        || !is_uuid(&args.request_id)
        || !workflow::valid_ref(&args.actor)
        || holder.is_some_and(|value| !workflow::valid_ref(value))
    {
        return Err(invalid(
            "Supply exact Area and Issue UUIDv7s, a request UUID, and valid actor and applicable holder references.",
        ));
    }
    let expected = args
        .if_match
        .split_once('=')
        .filter(|(issue, rev)| *issue == args.issue && valid_revision(rev))
        .map(|(_, rev)| rev)
        .ok_or_else(|| invalid("Supply one Issue revision as --if-match ISSUE=REV."))?;
    let request = Request {
        operation,
        workspace: workspace.workspace_id(),
        area: &args.area,
        issue: &args.issue,
        actor: &args.actor,
        id: &args.request_id,
        expected,
        holder,
        reason_sha256: reason_sha256.as_deref(),
    };
    let transition = Transition {
        fingerprint: request.fingerprint(),
        request,
        decision: issue_writer::read_decision(&args.evidence_file)?,
        reason,
    };
    issue_writer::execute(&workspace, target, &transition)
}

impl Mutation for Transition<'_> {
    type Guard = Issue;

    fn identity(&self) -> WriteIdentity<'_> {
        WriteIdentity {
            kind: Kind::Transition(self.request.operation),
            request_id: self.request.id,
            area_id: Some(self.request.area),
            record_id: Some(self.request.issue),
            actor: self.request.actor,
            fingerprint: &self.fingerprint,
            authorization_ref: &self.decision.evidence.authorization_ref,
        }
    }

    fn verify_decision(&self, now: OffsetDateTime) -> Result<(), HiveError> {
        if self.decision.decision != "allow" {
            return Err(workflow::unverified());
        }
        self.request
            .verify(&self.decision.evidence, &self.fingerprint, now)
    }

    fn evidence(&self) -> Result<String, HiveError> {
        issue_writer::encode(&self.decision.evidence, self.identity().kind)
    }

    fn check_scope(&self, snapshot: &mut Snapshot) -> Result<(), HiveError> {
        let owners: Vec<String> = snapshot.exec_rows(
            |commit| {
                format!(
                    "SELECT i.area_id FROM issues AS OF '{commit}' i JOIN areas AS OF '{commit}' a
                 ON CAST(a.id AS BINARY) = CAST(i.area_id AS BINARY)
                 WHERE CAST(i.id AS BINARY) = ? LIMIT 2"
                )
            },
            (self.request.issue,),
        )?;
        if matches!(owners.as_slice(), [owner] if owner == self.request.area) {
            Ok(())
        } else {
            Err(workflow::unverified())
        }
    }

    fn guard(&self, db: &mut Transaction<'_>, snapshot: &mut Snapshot) -> Result<Issue, HiveError> {
        let request = &self.request;
        let issue = Issue::read(db, request.issue, None)?;
        request.check_issue(&issue)?;
        request.check_readiness(db, &issue, None)?;
        if request.operation.is_reconciliation() {
            reconcile::check_closure(db, request, None)?;
            reconcile::check_graph(
                &mut super::super::records::RecordReader::Online(snapshot),
                request,
            )?;
        }
        Ok(issue)
    }

    fn stage(
        &self,
        db: &mut Transaction<'_>,
        before: Issue,
        now: OffsetDateTime,
        sql_time: &str,
    ) -> Result<StagedMutation, HiveError> {
        let request = &self.request;
        let previous_status = &before.status;
        let status = request
            .operation
            .next_status(previous_status)
            .ok_or_else(workflow::unverified)?;
        let record = Record {
            version: 1,
            transition_id: new_uuid_v7(now)?,
            request_id: request.id.to_owned(),
            area_id: request.area.to_owned(),
            issue_id: request.issue.to_owned(),
            previous_issue_revision: request.expected.to_owned(),
            issue_revision: new_uuid_v7(now)?,
            previous_status: previous_status.to_owned(),
            status: status.to_owned(),
            actor_ref: request.actor.to_owned(),
            evidence: self.decision.evidence.references(),
            effect: match request.operation {
                Operation::Select => Effect::Select {},
                Operation::Claim => Effect::Claim {
                    claim_id: new_uuid_v7(now)?,
                    holder_ref: request.holder.ok_or_else(workflow::unverified)?.to_owned(),
                },
                Operation::Cancel | Operation::Reopen => reconcile::effect(request)?,
            },
        };
        record.require_new_ids(db, None)?;
        if let Some(reason) = self.reason {
            reconcile::stage_issue(db, request, &record, sql_time, reason)?;
        } else {
            db.exec_drop(
                "UPDATE issues SET status = ?, revision = ?, updated_at = ?
             WHERE CAST(id AS BINARY) = ? AND CAST(area_id AS BINARY) = ?
               AND CAST(revision AS BINARY) = ? AND CAST(status AS BINARY) = ?",
                (
                    status,
                    &record.issue_revision,
                    sql_time,
                    request.issue,
                    request.area,
                    request.expected,
                    previous_status,
                ),
            )
            .map_err(read_error)?;
        }
        if db.affected_rows() != 1 {
            return Err(stale());
        }
        if let Some((claim, holder)) = record.effect.claim() {
            db.exec_drop(
                "INSERT INTO issue_claims (issue_id, claim_id, holder_ref, acquired_at)
                 VALUES (?, ?, ?, ?)",
                (request.issue, claim, holder, sql_time),
            )
            .map_err(read_error)?;
        }
        let kind = self.identity().kind;
        let encoded = issue_writer::encode(&record, kind)?;
        db.exec_drop(
            "INSERT INTO issue_updates (id, issue_id, kind, body, author_ref, created_at, transition_json)
             VALUES (?, ?, 'transition', ?, ?, ?, ?)",
            (&record.transition_id, request.issue, self.reason.unwrap_or(""), request.actor, sql_time, &encoded),
        ).map_err(read_error)?;
        Ok(StagedMutation {
            record_id: record.issue_id,
            result: encoded,
        })
    }
}

struct Issue {
    area: String,
    kind: String,
    status: String,
    revision: String,
    updated_at: String,
    acceptance: bool,
    hold: Option<String>,
    claimed: bool,
    valid_metadata: bool,
    open_fields: bool,
    unchanged_fields: String,
}

fn as_of(commit: Option<&str>) -> Result<String, HiveError> {
    match commit {
        None => Ok(String::new()),
        Some(commit) if is_commit_hash(commit) => Ok(format!(" AS OF '{commit}'")),
        _ => Err(workflow::unverified()),
    }
}

impl Issue {
    fn read(db: &mut impl Queryable, issue: &str, commit: Option<&str>) -> Result<Self, HiveError> {
        let at = as_of(commit)?;
        let (area, kind, status, revision, updated_at, acceptance, hold, claimed, valid_metadata, open_fields, unchanged_fields) =
            db.exec_first(
                format!(
                    "SELECT i.area_id, i.type, i.status, i.revision,
                     DATE_FORMAT(i.updated_at, '%Y-%m-%dT%H:%i:%s.%fZ'),
                     COALESCE(i.acceptance_criteria REGEXP ?, FALSE), i.hold_reason,
                     EXISTS(SELECT 1 FROM issue_claims{at} c WHERE c.issue_id = i.id),
                     i.number > 0 AND i.title REGEXP ? AND YEAR(i.created_at) > 0 AND YEAR(i.updated_at) > 0
                         AND (i.priority IS NULL OR i.priority BETWEEN 0 AND 3),
                     i.closed_at IS NULL AND i.closed_by_ref IS NULL AND i.resolution_note IS NULL,
                     JSON_ARRAY(i.number, i.parent_id, i.title, LOWER(SHA2(CAST(i.body AS BINARY), 256)),
                                i.acceptance_criteria, i.priority, i.hold_reason, i.created_at)
                     FROM issues{at} i WHERE CAST(i.id AS BINARY) = ?"
                ),
                (NON_WHITESPACE, NON_WHITESPACE, issue),
            ).map_err(read_error)?.ok_or_else(|| invalid_record("The exact Issue is missing."))?;
        Ok(Self {
            area,
            kind,
            status,
            revision,
            updated_at,
            acceptance,
            hold,
            claimed,
            valid_metadata,
            open_fields,
            unchanged_fields,
        })
    }
}

impl Request<'_> {
    fn check_issue(&self, issue: &Issue) -> Result<(), HiveError> {
        if issue.area != self.area {
            return Err(workflow::unverified());
        }
        let status = IssueStatus::parse(&issue.status);
        if status.is_none() || !valid_revision(&issue.revision) || !is_issue_type(&issue.kind) {
            return Err(invalid_record("The Issue lifecycle metadata is invalid."));
        }
        if issue.claimed {
            return Err(HiveError::new(
                ErrorCode::ClaimConflict,
                "The Issue already has a Claim row.",
            ));
        }
        if status == Some(IssueStatus::InProgress) {
            return Err(invalid_record("An in-progress Issue is missing its Claim."));
        }
        if issue.revision != self.expected {
            return Err(stale());
        }
        if self.operation.next_status(&issue.status).is_none()
            || (self.operation == Operation::Claim && issue.kind == "epic")
        {
            return Err(HiveError::new(
                ErrorCode::InvalidTransition,
                "The current Issue state or type does not permit this transition.",
            ));
        }
        if !issue.valid_metadata
            || (!status.is_some_and(IssueStatus::is_terminal) && !issue.open_fields)
            || issue
                .hold
                .as_deref()
                .is_some_and(|hold| hold.trim().is_empty() || hold.chars().any(char::is_control))
        {
            return Err(invalid_record("The Issue has invalid open-state metadata."));
        }
        if self.operation == Operation::Select && !issue.acceptance {
            return Err(HiveError::new(
                ErrorCode::InvalidTransition,
                "The Issue requires nonblank acceptance criteria.",
            ));
        }
        Ok(())
    }

    fn check_readiness(
        &self,
        db: &mut impl Queryable,
        issue: &Issue,
        commit: Option<&str>,
    ) -> Result<(), HiveError> {
        let archived = "The Issue's Area ancestry is archived.";
        let active = match commit {
            Some(commit) => require_active_area_at(db, self.area, commit, archived),
            None => require_active_area(db, self.area, archived),
        };
        if self.operation != Operation::Claim {
            return active;
        }
        let mut blockers = Vec::new();
        match active {
            Err(error) if matches!(error.code, ErrorCode::InvalidTransition) => {
                blockers.push(Blocker::ArchivedArea)
            }
            other => other?,
        }
        if issue.hold.is_some() {
            blockers.push(Blocker::Hold);
        }
        let at = as_of(commit)?;
        type PrerequisiteRow = (
            String,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            bool,
            bool,
        );
        let prerequisites: Vec<PrerequisiteRow> = db.exec(
            format!(
                "SELECT d.issue_id, d.prerequisite_id, p.type, p.status, p.revision,
                 a.id, p.closed_by_ref,
                 COALESCE(CASE WHEN CAST(p.status AS BINARY) IN ('done', 'cancelled')
                    THEN YEAR(p.closed_at) > 0 AND p.resolution_note REGEXP ?
                    ELSE p.closed_at IS NULL AND p.closed_by_ref IS NULL AND p.resolution_note IS NULL
                 END, FALSE),
                 EXISTS(SELECT 1 FROM issue_claims{at} c WHERE c.issue_id = p.id)
                 FROM issue_dependencies{at} d LEFT JOIN issues{at} p
                 ON CAST(p.id AS BINARY) = CAST(d.prerequisite_id AS BINARY)
                 LEFT JOIN areas{at} a ON CAST(a.id AS BINARY) = CAST(p.area_id AS BINARY)
                 WHERE d.issue_id = ? LIMIT ?"
            ),
            (NON_WHITESPACE, self.issue, MAX_ROWS_PER_TABLE + 1),
        ).map_err(read_error)?;
        if prerequisites.len() > MAX_ROWS_PER_TABLE {
            return Err(HiveError::new(
                ErrorCode::ContextLimit,
                "The Issue exceeds the prerequisite read limit.",
            ));
        }
        let mut unmet = false;
        for (owner, prerequisite, kind, status, revision, area, reviewer, valid_closure, claimed) in
            prerequisites
        {
            let status = status.as_deref().and_then(IssueStatus::parse);
            if owner != self.issue
                || prerequisite == self.issue
                || !is_uuid_v7(&prerequisite)
                || !kind.as_deref().is_some_and(is_issue_type)
                || !revision.as_deref().is_some_and(valid_revision)
                || status.is_none()
                || !area.as_deref().is_some_and(is_uuid_v7)
                || !valid_closure
                || (status.is_some_and(IssueStatus::is_terminal)
                    && !reviewer.as_deref().is_some_and(workflow::valid_ref))
                || (status == Some(IssueStatus::InProgress) && kind.as_deref() == Some("epic"))
                || claimed != (status == Some(IssueStatus::InProgress))
            {
                return Err(invalid_record(
                    "The Issue has a missing or invalid prerequisite.",
                ));
            }
            unmet |= status != Some(IssueStatus::Done);
        }
        if unmet {
            blockers.push(Blocker::PrerequisiteNotDone);
        }
        if blockers.is_empty() {
            Ok(())
        } else {
            Err(HiveError::blocked(blockers))
        }
    }
}

fn invalid_record(message: &'static str) -> HiveError {
    HiveError::new(ErrorCode::InvalidRecord, message)
}

fn stale() -> HiveError {
    HiveError::new(
        ErrorCode::StaleRevision,
        "The expected Issue revision is stale.",
    )
}

impl Record {
    fn valid_ids(&self) -> bool {
        is_uuid_v7(&self.transition_id)
            && is_uuid_v7(&self.issue_revision)
            && self.issue_revision != self.previous_issue_revision
            && self.transition_id != self.issue_revision
            && self
                .effect
                .reason_sha256()
                .is_none_or(super::super::receipt_history::valid_sha256)
            && self.effect.claim().is_none_or(|(id, holder)| {
                is_uuid_v7(id)
                    && id != self.transition_id
                    && id != self.issue_revision
                    && workflow::valid_ref(holder)
            })
    }

    fn require_new_ids(
        &self,
        db: &mut impl Queryable,
        commit: Option<&str>,
    ) -> Result<(), HiveError> {
        if !self.valid_ids() {
            return Err(workflow::unverified());
        }
        let at = as_of(commit)?;
        let reused: Option<u8> = db
            .exec_first(
                format!(
                    "SELECT 1 FROM dolt_diff_issue_updates{at}
                     WHERE from_id = ? OR to_id = ? LIMIT 1"
                ),
                (&self.transition_id, &self.transition_id),
            )
            .map_err(read_error)?;
        require_complete_history(db, reused.is_some())?;
        if let Some((claim, _)) = self.effect.claim() {
            let reused: Option<u8> = db
                .exec_first(
                    format!(
                        "SELECT 1 FROM dolt_diff_issue_claims{at}
                         WHERE from_claim_id = ? OR to_claim_id = ? LIMIT 1"
                    ),
                    (claim, claim),
                )
                .map_err(read_error)?;
            require_complete_history(db, reused.is_some())?;
        }
        Ok(())
    }

    fn request<'a>(&'a self, workspace: &'a str) -> Request<'a> {
        Request {
            operation: self.effect.operation(),
            workspace,
            area: &self.area_id,
            issue: &self.issue_id,
            actor: &self.actor_ref,
            id: &self.request_id,
            expected: &self.previous_issue_revision,
            holder: self.effect.claim().map(|(_, holder)| holder),
            reason_sha256: self.effect.reason_sha256(),
        }
    }
}

fn require_complete_history(db: &mut impl Queryable, reused: bool) -> Result<(), HiveError> {
    if reused
        || db
            .query_first::<mysql::Row, _>("SHOW WARNINGS LIMIT 1")
            .map_err(read_error)?
            .is_some()
    {
        return Err(invalid_record(
            "The allocated transition or Claim ID could not be verified against retained history.",
        ));
    }
    Ok(())
}

pub(in crate::dolt) fn check_scope(
    snapshot: &Snapshot,
    operation: Operation,
    request: &str,
    area: &str,
    issue: &str,
    actor: &str,
    receipt: &HistoricalReceipt,
) -> Result<Record, HiveError> {
    let record: Record =
        serde_json::from_str(&receipt.result).map_err(|_| workflow::unverified())?;
    let evidence: Evidence =
        serde_json::from_str(&receipt.workflow_evidence).map_err(|_| workflow::unverified())?;
    if receipt.kind != operation.name()
        || receipt.actor_ref != actor
        || record.version != 1
        || record.effect.operation() != operation
        || record.request_id != request
        || record.area_id != area
        || record.issue_id != issue
        || record.actor_ref != actor
        || operation.next_status(&record.previous_status) != Some(record.status.as_str())
        || !record.valid_ids()
        || !valid_revision(&record.previous_issue_revision)
        || evidence.authorization_ref != receipt.authorization_ref
        || record.evidence != evidence.references()
    {
        return Err(workflow::unverified());
    }
    let binding = record.request(&snapshot.workspace_id);
    if binding.fingerprint() != receipt.fingerprint {
        return Err(workflow::unverified());
    }
    let created = workflow::parse_time(&receipt.created_at).ok_or_else(workflow::unverified)?;
    binding.verify(&evidence, &receipt.fingerprint, created)?;
    if operation.is_reconciliation()
        && (issue_writer::encode(&record, Kind::Transition(operation))? != receipt.result
            || issue_writer::encode(&evidence, Kind::Transition(operation))?
                != receipt.workflow_evidence)
    {
        return Err(workflow::unverified());
    }
    Ok(record)
}

pub(in crate::dolt) fn verify_effect(
    snapshot: &mut Snapshot,
    introduced: &str,
    receipt: &HistoricalReceipt,
    record: &Record,
) -> Result<Value, HiveError> {
    let base = &receipt.base_commit;
    let db = &mut snapshot.connection;
    let binding = record.request(&snapshot.workspace_id);
    let before = Issue::read(db, binding.issue, Some(base))?;
    let after = Issue::read(db, binding.issue, Some(introduced))?;
    binding.check_issue(&before)?;
    binding.check_readiness(db, &before, Some(base))?;
    if before.status != record.previous_status {
        return Err(workflow::unverified());
    }
    if binding.operation.is_reconciliation() {
        reconcile::verify_issue(db, &binding, introduced, receipt)?;
    } else if !after.open_fields {
        return Err(workflow::unverified());
    }
    record.require_new_ids(db, Some(base))?;

    if after.area != before.area
        || after.kind != before.kind
        || after.status != record.status
        || after.revision != record.issue_revision
        || after.updated_at != receipt.created_at
        || after.unchanged_fields != before.unchanged_fields
        || after.claimed != record.effect.claim().is_some()
    {
        return Err(workflow::unverified());
    }

    // Read only the exact update and expected Claim, and compare the complete row.
    let at = as_of(Some(introduced))?;
    type UpdateRow = (
        String,
        String,
        String,
        String,
        Option<String>,
        Option<String>,
    );
    let update: Option<UpdateRow> = db.exec_first(
        format!("SELECT issue_id, kind, author_ref, DATE_FORMAT(created_at, '%Y-%m-%dT%H:%i:%s.%fZ'),
                 CASE WHEN OCTET_LENGTH(body) <= ? THEN body END,
                 CASE WHEN OCTET_LENGTH(transition_json) <= ? THEN transition_json END
                 FROM issue_updates{at} WHERE CAST(id AS BINARY) = ?"),
        (reconcile::MAX_REASON_BYTES, workflow::MAX_DECLARATION_BYTES, &record.transition_id),
    ).map_err(read_error)?;
    if !matches!(update, Some((issue, kind, actor, created, Some(body), Some(typed)))
    if issue == record.issue_id && kind == "transition"
       && actor == record.actor_ref && created == receipt.created_at && typed == receipt.result
       && match binding.reason_sha256 {
           Some(expected) => reconcile::reason_digest(&body).is_ok_and(|digest| digest == expected),
           None => body.is_empty(),
       })
    {
        return Err(workflow::unverified());
    }

    if let Some((id, holder)) = record.effect.claim() {
        let claim: Option<(String, String, String)> = db
            .exec_first(
                format!(
                    "SELECT claim_id, holder_ref, DATE_FORMAT(acquired_at, '%Y-%m-%dT%H:%i:%s.%fZ')
                     FROM issue_claims{at} WHERE CAST(issue_id AS BINARY) = ?"
                ),
                (&record.issue_id,),
            )
            .map_err(read_error)?;
        if !matches!(claim, Some((actual_id, actual_holder, acquired))
            if actual_id == id && actual_holder == holder && acquired == receipt.created_at)
        {
            return Err(workflow::unverified());
        }
    }

    let mut expected = vec![
        ("issue_updates", 1, 0, 0),
        ("issues", 0, 0, 1),
        ("operation_receipts", 1, 0, 0),
    ];
    if record.effect.operation() == Operation::Claim {
        expected.insert(0, ("issue_claims", 1, 0, 0));
    }
    verify_delta(db, base, introduced, &expected)?;
    let mut result = json!({
        "version": record.version, "area_id": record.area_id, "issue_id": record.issue_id,
        "issue_revision": record.issue_revision, "status": record.status, "transition_id": record.transition_id,
    });
    if let Some((claim, holder)) = record.effect.claim() {
        result["claim_id"] = json!(claim);
        result["holder_ref"] = json!(holder);
    }
    Ok(result)
}
