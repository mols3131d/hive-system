use std::path::Path;

mod reconcile;

use super::Input as BasicInput;
use mysql::{Transaction, prelude::Queryable};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use time::OffsetDateTime;

use crate::cli::{
    CommonOptions, IssueReleaseArgs, IssueReviewArgs, IssueStopArgs, IssueSubmitArgs,
    IssueSubmitEpicArgs, IssueTransitionArgs,
};
use crate::error::{ErrorCode, HiveError};

mod submission_history;

use super::super::{
    DoltSuccess, DoltTarget, MAX_ROWS_PER_TABLE, Snapshot,
    context_evidence::read_evidence_bytes,
    context_workflow as workflow, is_commit_hash, is_uuid, is_uuid_v7,
    issue_readiness::{Blocker, NON_WHITESPACE},
    issue_writer::{
        self, Decision, Kind, Mutation, StagedMutation, WriteIdentity, digest, invalid,
        new_uuid_v7, valid_revision,
    },
    read_error,
    receipt_history::{HistoricalReceipt, valid_sha256, verify_delta},
    records::{IssueStatus, is_issue_type},
    scope::{require_active_area, require_active_area_at},
};

#[derive(Clone, Copy, Deserialize, PartialEq, Eq, Serialize)]
pub(in crate::dolt) enum Operation {
    #[serde(rename = "issue.select")]
    Select,
    #[serde(rename = "issue.claim")]
    Claim,
    #[serde(rename = "issue.release")]
    Release,
    #[serde(rename = "issue.recover")]
    Recover,
    #[serde(rename = "issue.submit")]
    Submit,
    #[serde(rename = "issue.submit-epic")]
    SubmitEpic,
    #[serde(rename = "issue.request-changes")]
    RequestChanges,
    #[serde(rename = "issue.accept")]
    Accept,
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
            "issue.release" => Some(Self::Release),
            "issue.recover" => Some(Self::Recover),
            "issue.submit" => Some(Self::Submit),
            "issue.submit-epic" => Some(Self::SubmitEpic),
            "issue.request-changes" => Some(Self::RequestChanges),
            "issue.accept" => Some(Self::Accept),
            "issue.cancel" => Some(Self::Cancel),
            "issue.reopen" => Some(Self::Reopen),
            _ => None,
        }
    }

    pub(in crate::dolt) fn name(self) -> &'static str {
        match self {
            Self::Select => "issue.select",
            Self::Claim => "issue.claim",
            Self::Submit => "issue.submit",
            Self::SubmitEpic => "issue.submit-epic",
            Self::RequestChanges => "issue.request-changes",
            Self::Accept => "issue.accept",
            Self::Recover => "issue.recover",
            Self::Release => "issue.release",
            Self::Cancel => "issue.cancel",
            Self::Reopen => "issue.reopen",
        }
    }

    fn body_limit(self) -> usize {
        match self {
            Self::Accept => MAX_RESOLUTION_BYTES,
            Self::Submit | Self::SubmitEpic | Self::RequestChanges => MAX_RESULT_BYTES,
            Self::Cancel | Self::Reopen => reconcile::MAX_REASON_BYTES,
            _ => MAX_REASON_BYTES,
        }
    }

    fn next_status(self, previous: &str) -> Option<&'static str> {
        match (self, IssueStatus::parse(previous)?) {
            (Self::Select, IssueStatus::Backlog) => Some("todo"),
            (Self::Claim, IssueStatus::Todo) => Some("in-progress"),
            (Self::Release | Self::Recover, IssueStatus::InProgress) => Some("todo"),
            (Self::Submit, IssueStatus::InProgress) => Some("review"),
            (Self::SubmitEpic, IssueStatus::Todo) => Some("review"),
            (Self::RequestChanges, IssueStatus::Review) => Some("todo"),
            (Self::Accept, IssueStatus::Review) => Some("done"),
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

const MAX_REASON_BYTES: usize = 1024;
const MAX_RESULT_BYTES: usize = 256 * 1024;
const MAX_RESOLUTION_BYTES: usize = 65_535;
const MAX_CANCELLED_CHILDREN: usize = 256;

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct EpicSubmission {
    result_evidence_ref: String,
    body_sha256: String,
}

#[derive(Clone, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
struct ReviewBinding {
    submission_id: String,
    body_sha256: String,
    cancelled_child_ids: Vec<String>,
}

impl ReviewBinding {
    fn valid(&self) -> bool {
        is_uuid_v7(&self.submission_id)
            && valid_sha256(&self.body_sha256)
            && self.cancelled_child_ids.len() <= MAX_CANCELLED_CHILDREN
            && self.cancelled_child_ids.iter().all(|id| is_uuid_v7(id))
            && self
                .cancelled_child_ids
                .windows(2)
                .all(|ids| ids[0] < ids[1])
    }
}

#[derive(Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum ReviewOutcome {
    RequestChanges,
    Accept,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ReviewDecision {
    binding: ReviewBinding,
    outcome: ReviewOutcome,
    reviewer_ref: String,
    reviewed_at: String,
}

#[derive(Clone, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum StopKind {
    ExecutionStopped,
}

#[derive(Clone, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
struct StopEvidence {
    version: u8,
    kind: StopKind,
    evidence_ref: String,
    workspace_id: String,
    issue_id: String,
    claim_id: String,
    holder_ref: String,
    source_ref: String,
    observed_at: String,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum RecoveryRole {
    Owner,
    Reviewer,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RecoveryReview {
    role: RecoveryRole,
    reviewer_ref: String,
    reviewed_at: String,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Stopped {
    stop: StopEvidence,
    body_sha256: String,
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
    #[serde(
        default,
        deserialize_with = "optional_object",
        skip_serializing_if = "Option::is_none"
    )]
    stop: Option<StopEvidence>,
    #[serde(
        default,
        deserialize_with = "optional_object",
        skip_serializing_if = "Option::is_none"
    )]
    recovery_review: Option<RecoveryReview>,
    #[serde(
        default,
        deserialize_with = "optional_object",
        skip_serializing_if = "Option::is_none"
    )]
    review: Option<ReviewDecision>,
}

fn optional_object<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    deserializer: D,
) -> Result<Option<T>, D::Error> {
    Option::<&serde_json::value::RawValue>::deserialize(deserializer)?
        .map(workflow::parse_json_object)
        .transpose()
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
    #[serde(rename = "issue.release")]
    Release { stopped: Stopped },
    #[serde(rename = "issue.recover")]
    Recover { stopped: Stopped },
    #[serde(rename = "issue.submit")]
    Submit {
        stopped: Stopped,
        result_evidence_ref: String,
    },
    #[serde(rename = "issue.submit-epic")]
    SubmitEpic { submission: EpicSubmission },
    #[serde(rename = "issue.request-changes")]
    RequestChanges { review: ReviewBinding },
    #[serde(rename = "issue.accept")]
    Accept { review: ReviewBinding },
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
            Self::Release { .. } => Operation::Release,
            Self::Recover { .. } => Operation::Recover,
            Self::Submit { .. } => Operation::Submit,
            Self::SubmitEpic { .. } => Operation::SubmitEpic,
            Self::RequestChanges { .. } => Operation::RequestChanges,
            Self::Accept { .. } => Operation::Accept,
            Self::Cancel { .. } => Operation::Cancel,
            Self::Reopen { .. } => Operation::Reopen,
        }
    }

    fn claim(&self) -> Option<(&str, &str)> {
        match self {
            Self::Claim {
                claim_id,
                holder_ref,
            } => Some((claim_id, holder_ref)),
            _ => None,
        }
    }

    fn stopped(&self) -> Option<&Stopped> {
        match self {
            Self::Release { stopped }
            | Self::Recover { stopped }
            | Self::Submit { stopped, .. } => Some(stopped),
            _ => None,
        }
    }

    fn result_evidence_ref(&self) -> Option<&str> {
        match self {
            Self::Submit {
                result_evidence_ref,
                ..
            } => Some(result_evidence_ref),
            _ => None,
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
    stopped: Option<&'a Stopped>,
    result_evidence_ref: Option<&'a str>,
    epic: Option<&'a EpicSubmission>,
    review: Option<&'a ReviewBinding>,
    reason_sha256: Option<&'a str>,
}

impl Request<'_> {
    fn fingerprint(&self) -> String {
        let mut fields = vec![
            json!(1),
            json!(self.operation),
            json!(self.workspace),
            json!(self.area),
            json!(self.issue),
            json!(self.actor),
            json!(self.expected),
        ];
        if let Some(stopped) = self.stopped {
            fields.extend([
                json!(stopped.stop),
                json!(stopped.body_sha256),
                json!(self.result_evidence_ref),
            ]);
        } else if let Some(epic) = self.epic {
            fields.extend([json!(epic.result_evidence_ref), json!(epic.body_sha256)]);
        } else if let Some(review) = self.review {
            fields.extend([
                json!(review.submission_id),
                json!(review.body_sha256),
                json!(review.cancelled_child_ids),
            ]);
        } else {
            fields.push(json!(self.holder.or(self.reason_sha256)));
        }
        digest(Value::Array(fields).to_string().as_bytes())
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
        if let Some(epic) = self.epic
            && (!workflow::valid_ref(&epic.result_evidence_ref) || !valid_sha256(&epic.body_sha256))
        {
            return Err(workflow::unverified());
        }
        match (self.review, &evidence.review, self.operation) {
            (Some(binding), Some(review), Operation::RequestChanges | Operation::Accept)
                if binding.valid()
                    && binding == &review.binding
                    && (review.outcome == ReviewOutcome::Accept)
                        == (self.operation == Operation::Accept)
                    && (self.operation == Operation::Accept
                        || binding.cancelled_child_ids.is_empty())
                    && review.reviewer_ref == self.actor
                    && review.reviewer_ref == evidence.issuer_ref
                    && workflow::parse_time(&review.reviewed_at)
                        .is_some_and(|time| time <= now) => {}
            (None, None, _) => {}
            _ => return Err(workflow::unverified()),
        }
        if self.stopped.map(|stopped| &stopped.stop) != evidence.stop.as_ref() {
            return Err(workflow::unverified());
        }
        if let Some(stopped) = self.stopped {
            let stop = &stopped.stop;
            if stop.version != 1
                || stop.workspace_id != self.workspace
                || stop.issue_id != self.issue
                || Some(stop.holder_ref.as_str()) != self.holder
                || !is_uuid_v7(&stop.claim_id)
                || ![&stop.holder_ref, &stop.source_ref, &stop.evidence_ref]
                    .into_iter()
                    .all(|value| workflow::valid_ref(value))
                || self
                    .result_evidence_ref
                    .is_some_and(|value| !workflow::valid_ref(value) || value == stop.evidence_ref)
                || !valid_sha256(&stopped.body_sha256)
            {
                return Err(workflow::unverified());
            }
            let observed =
                workflow::parse_time(&stop.observed_at).ok_or_else(workflow::unverified)?;
            if observed > now {
                return Err(workflow::unverified());
            }
            match (self.operation, &evidence.recovery_review) {
                (Operation::Recover, Some(review))
                    if review.reviewer_ref == evidence.issuer_ref
                        && workflow::parse_time(&review.reviewed_at)
                            .is_some_and(|time| observed <= time && time <= now) => {}
                (Operation::Release | Operation::Submit, None) if self.actor == stop.holder_ref => {
                }
                _ => return Err(workflow::unverified()),
            }
        } else if evidence.recovery_review.is_some() {
            return Err(workflow::unverified());
        }
        Ok(())
    }
}

struct Transition<'a> {
    request: Request<'a>,
    fingerprint: String,
    decision: Decision<Evidence>,
    body: String,
}

pub(super) fn write(
    common: &CommonOptions,
    args: &IssueTransitionArgs,
    input: BasicInput<'_>,
) -> Result<DoltSuccess, HiveError> {
    let (operation, holder, reason) = match input {
        BasicInput::Select => (Operation::Select, None, None),
        BasicInput::Claim(holder) => (Operation::Claim, Some(holder), None),
        BasicInput::Cancel(reason) => (Operation::Cancel, None, Some(reason)),
        BasicInput::Reopen(reason) => (Operation::Reopen, None, Some(reason)),
    };
    execute(
        common,
        args,
        holder,
        operation,
        reason.map_or(Input::Empty, Input::Reason),
    )
}

struct StoppedInput<'a> {
    args: &'a IssueStopArgs,
    body: String,
    result_evidence_ref: Option<&'a str>,
}

enum Input<'a> {
    Empty,
    Reason(&'a str),
    Stopped(StoppedInput<'a>),
    Epic {
        body: String,
        evidence: &'a str,
    },
    Review {
        body: String,
        binding: ReviewBinding,
        approval: &'a str,
    },
}

pub(super) fn submit_epic(
    common: &CommonOptions,
    args: &IssueSubmitEpicArgs,
) -> Result<DoltSuccess, HiveError> {
    if !workflow::valid_ref(&args.evidence) {
        return Err(invalid("Supply a valid result evidence reference."));
    }
    execute(
        common,
        &args.transition,
        None,
        Operation::SubmitEpic,
        Input::Epic {
            body: read_body(&args.file, MAX_RESULT_BYTES)?,
            evidence: &args.evidence,
        },
    )
}

pub(super) fn review(
    common: &CommonOptions,
    args: &IssueReviewArgs,
    acknowledged: Option<&[String]>,
) -> Result<DoltSuccess, HiveError> {
    let operation = if acknowledged.is_some() {
        Operation::Accept
    } else {
        Operation::RequestChanges
    };
    if !is_uuid_v7(&args.submission) || !workflow::valid_ref(&args.approval) {
        return Err(invalid(
            "Supply an exact submission UUIDv7 and a valid approval reference.",
        ));
    }
    let body = read_body(&args.file, operation.body_limit())?;
    let mut cancelled_child_ids = acknowledged.unwrap_or_default().to_vec();
    cancelled_child_ids.sort_unstable();
    let binding = ReviewBinding {
        submission_id: args.submission.clone(),
        body_sha256: digest(body.as_bytes()),
        cancelled_child_ids,
    };
    if !binding.valid() {
        return Err(invalid(
            "Supply at most 256 distinct cancelled child UUIDv7s.",
        ));
    }
    execute(
        common,
        &args.transition,
        None,
        operation,
        Input::Review {
            body,
            binding,
            approval: &args.approval,
        },
    )
}

fn read_body(path: &Path, limit: usize) -> Result<String, HiveError> {
    let bytes = read_evidence_bytes(path, limit)?;
    let body = String::from_utf8(bytes).map_err(|_| invalid("The body file must be UTF-8."))?;
    if !valid_result(&body) {
        return Err(invalid(
            "Supply a nonblank UTF-8 body file without NUL bytes.",
        ));
    }
    Ok(body)
}

pub(super) fn release(
    common: &CommonOptions,
    args: &IssueReleaseArgs,
    recovery: bool,
) -> Result<DoltSuccess, HiveError> {
    if !valid_reason(&args.reason) {
        return Err(invalid(
            "Supply a nonblank reason of at most 1024 bytes without control characters.",
        ));
    }
    execute_stopped(
        common,
        if recovery {
            Operation::Recover
        } else {
            Operation::Release
        },
        StoppedInput {
            args: &args.stopped,
            body: args.reason.clone(),
            result_evidence_ref: None,
        },
    )
}

pub(super) fn submit(
    common: &CommonOptions,
    args: &IssueSubmitArgs,
) -> Result<DoltSuccess, HiveError> {
    if !workflow::valid_ref(&args.evidence) {
        return Err(invalid("Supply a valid result evidence reference."));
    }
    let body = read_body(&args.file, MAX_RESULT_BYTES)?;
    execute_stopped(
        common,
        Operation::Submit,
        StoppedInput {
            args: &args.stopped,
            body,
            result_evidence_ref: Some(&args.evidence),
        },
    )
}

fn valid_reason(body: &str) -> bool {
    body.len() <= MAX_REASON_BYTES && !body.trim().is_empty() && !body.chars().any(char::is_control)
}

fn valid_result(body: &str) -> bool {
    body.len() <= MAX_RESULT_BYTES && !body.trim().is_empty() && !body.contains('\0')
}

fn execute_stopped(
    common: &CommonOptions,
    operation: Operation,
    input: StoppedInput<'_>,
) -> Result<DoltSuccess, HiveError> {
    let args = &input.args.claim_args;
    execute(
        common,
        &args.transition,
        Some(&args.holder),
        operation,
        Input::Stopped(input),
    )
}

fn execute(
    common: &CommonOptions,
    args: &IssueTransitionArgs,
    holder: Option<&str>,
    operation: Operation,
    input: Input<'_>,
) -> Result<DoltSuccess, HiveError> {
    let target = DoltTarget::parse(&args.target)?;
    let workspace =
        crate::config_v2::read_explicit(common.workspace.as_deref().expect("validated workspace"))?;
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
    let decision: Decision<Evidence> = issue_writer::read_decision(&args.evidence_file)?;
    let mut stopped = None;
    let mut result_evidence_ref = None;
    let mut epic = None;
    let mut review = None;
    let mut reason_sha256 = None;
    let body = match input {
        Input::Empty => String::new(),
        Input::Reason(reason) => {
            reason_sha256 = Some(reconcile::reason_digest(reason)?);
            reason.to_owned()
        }
        Input::Stopped(input) => {
            let args = input.args;
            if !is_uuid_v7(&args.claim) || !workflow::valid_ref(&args.stop_evidence) {
                return Err(invalid(
                    "Supply an exact Claim UUIDv7 and a valid stop evidence reference.",
                ));
            }
            let stop = decision
                .evidence
                .stop
                .as_ref()
                .filter(|stop| {
                    stop.evidence_ref == args.stop_evidence && stop.claim_id == args.claim
                })
                .ok_or_else(workflow::unverified)?;
            stopped = Some(Stopped {
                stop: stop.clone(),
                body_sha256: digest(input.body.as_bytes()),
            });
            result_evidence_ref = input.result_evidence_ref;
            input.body
        }
        Input::Epic { body, evidence } => {
            epic = Some(EpicSubmission {
                result_evidence_ref: evidence.to_owned(),
                body_sha256: digest(body.as_bytes()),
            });
            body
        }
        Input::Review {
            body,
            binding,
            approval,
        } => {
            if decision.evidence.authorization_ref != approval {
                return Err(workflow::unverified());
            }
            review = Some(binding);
            body
        }
    };
    let request = Request {
        operation,
        workspace: workspace.workspace_id(),
        area: &args.area,
        issue: &args.issue,
        actor: &args.actor,
        id: &args.request_id,
        expected,
        holder,
        stopped: stopped.as_ref(),
        result_evidence_ref,
        epic: epic.as_ref(),
        review: review.as_ref(),
        reason_sha256: reason_sha256.as_deref(),
    };
    let transition = Transition {
        fingerprint: request.fingerprint(),
        request,
        decision,
        body,
    };
    issue_writer::execute(&workspace, target, &transition)
}

impl Transition<'_> {
    fn verify_submission_review(
        &self,
        snapshot: &mut Snapshot,
        base: &str,
    ) -> Result<(), HiveError> {
        if let Some(review) = &self.decision.evidence.review {
            submission_history::verify(snapshot, base, &self.request, review)?;
        }
        Ok(())
    }
}

impl Mutation for Transition<'_> {
    type Guard = (Issue, Option<Claim>);

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

    fn verify_replay_decision(&self, snapshot: &mut Snapshot, base: &str) -> Result<(), HiveError> {
        self.verify_submission_review(snapshot, base)
    }

    fn guard(
        &self,
        db: &mut Transaction<'_>,
        snapshot: &mut Snapshot,
    ) -> Result<Self::Guard, HiveError> {
        let request = &self.request;
        let issue = Issue::read(db, request.issue, None)?;
        request.check_issue(&issue)?;
        let claim = if request.stopped.is_some() {
            Claim::read(db, request.issue, None)?
        } else {
            None
        };
        request.check_readiness(db, &issue, None)?;
        let base = snapshot.commit.clone();
        self.verify_submission_review(snapshot, &base)?;
        if request.operation.is_reconciliation() {
            reconcile::check_closure(db, request, None)?;
            reconcile::check_graph(
                &mut super::super::records::RecordReader::Online(snapshot),
                request,
            )?;
        }
        Ok((issue, claim))
    }

    fn stage(
        &self,
        db: &mut Transaction<'_>,
        (before, claim): Self::Guard,
        now: OffsetDateTime,
        sql_time: &str,
    ) -> Result<StagedMutation, HiveError> {
        let request = &self.request;
        let previous_status = &before.status;
        let status = request
            .operation
            .next_status(previous_status)
            .ok_or_else(workflow::unverified)?;
        let effect = if let Some(stopped) = request.stopped {
            request.check_stop(claim.as_ref(), now)?;
            let stopped = stopped.clone();
            match request.operation {
                Operation::Release => Effect::Release { stopped },
                Operation::Recover => Effect::Recover { stopped },
                Operation::Submit => Effect::Submit {
                    stopped,
                    result_evidence_ref: request
                        .result_evidence_ref
                        .ok_or_else(workflow::unverified)?
                        .to_owned(),
                },
                _ => return Err(workflow::unverified()),
            }
        } else if let Some(epic) = request.epic {
            Effect::SubmitEpic {
                submission: epic.clone(),
            }
        } else if let Some(review) = request.review {
            match request.operation {
                Operation::RequestChanges => Effect::RequestChanges {
                    review: review.clone(),
                },
                Operation::Accept => Effect::Accept {
                    review: review.clone(),
                },
                _ => return Err(workflow::unverified()),
            }
        } else if request.operation.is_reconciliation() {
            reconcile::effect(request)?
        } else if let Some(holder) = request.holder {
            Effect::Claim {
                claim_id: new_uuid_v7(now)?,
                holder_ref: holder.to_owned(),
            }
        } else {
            Effect::Select {}
        };
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
            effect,
        };
        record.require_new_ids(db, None)?;
        if request.operation.is_reconciliation() {
            reconcile::stage_issue(db, request, &record, sql_time, &self.body)?;
        } else {
            let closure = (request.operation == Operation::Accept).then_some((
                sql_time,
                request.actor,
                self.body.as_str(),
            ));
            db.exec_drop(
                "UPDATE issues SET status = ?, revision = ?, updated_at = ?,
                closed_at = ?, closed_by_ref = ?, resolution_note = ?
             WHERE CAST(id AS BINARY) = ? AND CAST(area_id AS BINARY) = ?
               AND CAST(revision AS BINARY) = ? AND CAST(status AS BINARY) = ?",
                (
                    status,
                    &record.issue_revision,
                    sql_time,
                    closure.map(|(time, _, _)| time),
                    closure.map(|(_, actor, _)| actor),
                    closure.map(|(_, _, body)| body),
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
        if let Some(stopped) = record.effect.stopped() {
            db.exec_drop(
                "DELETE FROM issue_claims WHERE CAST(issue_id AS BINARY) = ?
                 AND CAST(claim_id AS BINARY) = ? AND CAST(holder_ref AS BINARY) = ?",
                (
                    request.issue,
                    &stopped.stop.claim_id,
                    &stopped.stop.holder_ref,
                ),
            )
            .map_err(read_error)?;
            if db.affected_rows() != 1 {
                return Err(claim_mismatch());
            }
        }
        let kind = self.identity().kind;
        let encoded = issue_writer::encode(&record, kind)?;
        db.exec_drop(
            "INSERT INTO issue_updates (id, issue_id, kind, body, author_ref, created_at, transition_json)
             VALUES (?, ?, 'transition', ?, ?, ?, ?)",
            (&record.transition_id, request.issue, &self.body, request.actor, sql_time, &encoded),
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
    closed_at: Option<String>,
    closed_by: Option<String>,
    resolution_digest: Option<String>,
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
        let (area, kind, status, revision, updated_at, acceptance, hold, claimed, valid_metadata,
            open_fields, unchanged_fields, closure): (_, _, _, _, _, _, _, _, _, _, _, String) =
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
                                i.acceptance_criteria, i.priority, i.hold_reason, i.created_at),
                     JSON_ARRAY(DATE_FORMAT(i.closed_at, '%Y-%m-%dT%H:%i:%s.%fZ'), i.closed_by_ref,
                                LOWER(SHA2(CAST(i.resolution_note AS BINARY), 256)))
                     FROM issues{at} i WHERE CAST(i.id AS BINARY) = ?"
                ),
                (NON_WHITESPACE, NON_WHITESPACE, issue),
            ).map_err(read_error)?.ok_or_else(|| invalid_record("The exact Issue is missing."))?;
        let (closed_at, closed_by, resolution_digest) =
            serde_json::from_str(&closure).map_err(|_| workflow::unverified())?;
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
            closed_at,
            closed_by,
            resolution_digest,
        })
    }
}

pub(in crate::dolt) struct Claim {
    pub(in crate::dolt) id: String,
    pub(in crate::dolt) holder: String,
    acquired_at: String,
}

impl Claim {
    pub(in crate::dolt) fn read(
        db: &mut impl Queryable,
        issue: &str,
        commit: Option<&str>,
    ) -> Result<Option<Self>, HiveError> {
        let at = as_of(commit)?;
        let rows: Vec<(String, String, String, String)> = db
            .exec(
                format!(
                    "SELECT issue_id, claim_id, holder_ref,
                     DATE_FORMAT(acquired_at, '%Y-%m-%dT%H:%i:%s.%fZ')
                     FROM issue_claims{at} WHERE issue_id = ? LIMIT 2"
                ),
                (issue,),
            )
            .map_err(read_error)?;
        let mut rows = rows.into_iter();
        let Some((owner, id, holder, acquired_at)) = rows.next() else {
            return Ok(None);
        };
        if rows.next().is_some()
            || owner != issue
            || !is_uuid_v7(&id)
            || !workflow::valid_ref(&holder)
            || workflow::parse_time(&acquired_at).is_none()
        {
            return Err(invalid_record(
                "The Issue has invalid or ambiguous Claim metadata.",
            ));
        }
        Ok(Some(Self {
            id,
            holder,
            acquired_at,
        }))
    }
}

fn claim_mismatch() -> HiveError {
    HiveError::new(
        ErrorCode::ClaimConflict,
        "The exact current Claim and holder do not match.",
    )
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
        if issue.claimed && self.stopped.is_none() {
            return Err(HiveError::new(
                ErrorCode::ClaimConflict,
                "The Issue already has a Claim row.",
            ));
        }
        if status == Some(IssueStatus::InProgress) && !issue.claimed {
            return Err(invalid_record("An in-progress Issue is missing its Claim."));
        }
        if issue.revision != self.expected {
            return Err(stale());
        }
        if self.operation.next_status(&issue.status).is_none()
            || match self.operation {
                Operation::SubmitEpic => issue.kind != "epic",
                Operation::Claim | Operation::Release | Operation::Recover | Operation::Submit => {
                    issue.kind == "epic"
                }
                _ => false,
            }
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

    fn check_stop(&self, claim: Option<&Claim>, now: OffsetDateTime) -> Result<(), HiveError> {
        let stop = &self.stopped.ok_or_else(workflow::unverified)?.stop;
        let claim = claim
            .filter(|claim| claim.id == stop.claim_id && claim.holder == stop.holder_ref)
            .ok_or_else(claim_mismatch)?;
        let acquired = workflow::parse_time(&claim.acquired_at).ok_or_else(workflow::unverified)?;
        if !workflow::parse_time(&stop.observed_at)
            .is_some_and(|time| acquired <= time && time <= now)
        {
            return Err(workflow::unverified());
        }
        Ok(())
    }

    fn check_readiness(
        &self,
        db: &mut impl Queryable,
        issue: &Issue,
        commit: Option<&str>,
    ) -> Result<(), HiveError> {
        if matches!(self.operation, Operation::Release | Operation::Recover) {
            return Ok(());
        }
        let archived = "The Issue's Area ancestry is archived.";
        let active = match commit {
            Some(commit) => require_active_area_at(db, self.area, commit, archived),
            None => require_active_area(db, self.area, archived),
        };
        if !matches!(
            self.operation,
            Operation::Claim | Operation::Submit | Operation::SubmitEpic | Operation::Accept
        ) {
            return active;
        }
        let mut blockers = Vec::new();
        match active {
            Err(error) if matches!(error.code, ErrorCode::InvalidTransition) => {
                blockers.push(Blocker::ArchivedArea)
            }
            other => other?,
        }
        if self.operation == Operation::Claim && issue.hold.is_some() {
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
        if !blockers.is_empty() {
            return Err(HiveError::blocked(blockers));
        }
        if matches!(self.operation, Operation::SubmitEpic | Operation::Accept) {
            self.check_children(db, &at)?;
        }
        Ok(())
    }

    fn check_children(&self, db: &mut impl Queryable, at: &str) -> Result<(), HiveError> {
        type Child = (
            String,
            String,
            String,
            String,
            Option<String>,
            Option<String>,
            bool,
            bool,
        );
        let children: Vec<Child> = db
            .exec(
                format!(
                    "SELECT c.id, c.type, c.status, c.revision, a.id, c.closed_by_ref,
                    COALESCE(YEAR(c.closed_at) > 0 AND c.resolution_note REGEXP ?, FALSE),
                    EXISTS(SELECT 1 FROM issue_claims{at} cl WHERE cl.issue_id = c.id)
                 FROM issues{at} c LEFT JOIN areas{at} a
                    ON CAST(a.id AS BINARY) = CAST(c.area_id AS BINARY)
                 WHERE CAST(c.parent_id AS BINARY) = ? LIMIT ?"
                ),
                (NON_WHITESPACE, self.issue, MAX_ROWS_PER_TABLE + 1),
            )
            .map_err(read_error)?;
        if children.len() > MAX_ROWS_PER_TABLE {
            return Err(HiveError::new(
                ErrorCode::ContextLimit,
                "The Issue exceeds the child read limit.",
            ));
        }
        let mut cancelled = Vec::new();
        for (id, kind, status, revision, area, reviewer, closed, claimed) in children {
            let status = IssueStatus::parse(&status).ok_or_else(|| {
                invalid_record("An immediate child has an invalid lifecycle state.")
            })?;
            if id == self.issue
                || !is_uuid_v7(&id)
                || !is_issue_type(&kind)
                || !valid_revision(&revision)
                || !area.as_deref().is_some_and(is_uuid_v7)
            {
                return Err(invalid_record(
                    "An immediate child has invalid Issue metadata.",
                ));
            }
            if !status.is_terminal() {
                return Err(HiveError::new(
                    ErrorCode::InvalidTransition,
                    "An immediate child is unresolved.",
                ));
            }
            if claimed || !closed || !reviewer.as_deref().is_some_and(workflow::valid_ref) {
                return Err(invalid_record(
                    "An immediate child has invalid closure or Claim metadata.",
                ));
            }
            if status == IssueStatus::Cancelled {
                cancelled.push(id);
            }
        }
        if let Some(review) = self.review {
            cancelled.sort_unstable();
            if review.cancelled_child_ids != cancelled {
                return Err(HiveError::new(
                    ErrorCode::InvalidTransition,
                    "Acknowledge exactly the cancelled immediate children in the reviewed scope change.",
                ));
            }
        }
        Ok(())
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
            holder: self.effect.claim().map(|(_, holder)| holder).or_else(|| {
                self.effect
                    .stopped()
                    .map(|stopped| stopped.stop.holder_ref.as_str())
            }),
            stopped: self.effect.stopped(),
            result_evidence_ref: self.effect.result_evidence_ref(),
            epic: match &self.effect {
                Effect::SubmitEpic { submission } => Some(submission),
                _ => None,
            },
            review: match &self.effect {
                Effect::RequestChanges { review } | Effect::Accept { review } => Some(review),
                _ => None,
            },
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
    let binding = record.request(&snapshot.workspace_id);
    if binding.review.is_some() {
        let evidence: Evidence =
            serde_json::from_str(&receipt.workflow_evidence).map_err(|_| workflow::unverified())?;
        let review = evidence.review.as_ref().ok_or_else(workflow::unverified)?;
        let workspace = snapshot.workspace_id.clone();
        let binding = record.request(&workspace);
        submission_history::verify(snapshot, &receipt.base_commit, &binding, review)?;
    }
    verify_rows(snapshot, introduced, receipt, record)
}

fn verify_rows(
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
    }
    record.require_new_ids(db, Some(base))?;

    if binding.stopped.is_some() {
        let claim = Claim::read(db, binding.issue, Some(base))?;
        let created = workflow::parse_time(&receipt.created_at).ok_or_else(workflow::unverified)?;
        binding.check_stop(claim.as_ref(), created)?;
    }

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

    let expected_closure = if binding.operation == Operation::Accept {
        let review = binding.review.ok_or_else(workflow::unverified)?;
        (
            Some(receipt.created_at.as_str()),
            Some(record.actor_ref.as_str()),
            Some(review.body_sha256.as_str()),
        )
    } else {
        (None, None, None)
    };
    if !binding.operation.is_reconciliation()
        && (
            after.closed_at.as_deref(),
            after.closed_by.as_deref(),
            after.resolution_digest.as_deref(),
        ) != expected_closure
    {
        return Err(workflow::unverified());
    }

    // Read only the exact update and expected Claim, and compare the complete row.
    let at = as_of(Some(introduced))?;
    type UpdateRow = (
        String,
        String,
        Option<String>,
        String,
        String,
        Option<String>,
    );
    let update: Option<UpdateRow> = db.exec_first(
        format!("SELECT issue_id, kind, CASE WHEN OCTET_LENGTH(body) <= ? THEN body END, author_ref, DATE_FORMAT(created_at, '%Y-%m-%dT%H:%i:%s.%fZ'),
                 CASE WHEN OCTET_LENGTH(transition_json) <= ? THEN transition_json END
                 FROM issue_updates{at} WHERE CAST(id AS BINARY) = ?"),
        (binding.operation.body_limit(),
         workflow::MAX_DECLARATION_BYTES, &record.transition_id),
    ).map_err(read_error)?;
    if !matches!(update, Some((issue, kind, Some(body), actor, created, Some(typed)))
    if issue == record.issue_id && kind == "transition"
       && actor == record.actor_ref && created == receipt.created_at && typed == receipt.result
       && match binding.stopped {
            Some(stopped) => digest(body.as_bytes()) == stopped.body_sha256
                && if binding.operation == Operation::Submit { valid_result(&body) } else { valid_reason(&body) },
            None => {
                let sha = binding.epic.map(|epic| epic.body_sha256.as_str())
                    .or_else(|| binding.review.map(|review| review.body_sha256.as_str()));
                match sha {
                    Some(sha) => valid_result(&body) && digest(body.as_bytes()) == sha,
                    None => match binding.reason_sha256 {
                        Some(expected) => reconcile::reason_digest(&body).is_ok_and(|digest| digest == expected),
                        None => body.is_empty(),
                    },
                }
            },
       })
    {
        return Err(workflow::unverified());
    }

    if let Some((id, holder)) = record.effect.claim() {
        let claim = Claim::read(db, &record.issue_id, Some(introduced))?;
        if !matches!(claim, Some(claim)
            if claim.id == id && claim.holder == holder && claim.acquired_at == receipt.created_at)
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
    } else if binding.stopped.is_some() {
        expected.insert(0, ("issue_claims", 0, 1, 0));
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
    if let Some(stopped) = binding.stopped {
        result["former_claim_id"] = json!(stopped.stop.claim_id);
        result["former_holder_ref"] = json!(stopped.stop.holder_ref);
        result["stop_evidence_ref"] = json!(stopped.stop.evidence_ref);
        result["request_id"] = json!(record.request_id);
        result["previous_status"] = json!(record.previous_status);
        result["previous_issue_revision"] = json!(record.previous_issue_revision);
        if let Some(reference) = binding.result_evidence_ref {
            result["result_evidence_ref"] = json!(reference);
            result["result_sha256"] = json!(stopped.body_sha256);
        }
    }
    if let Some(epic) = binding.epic {
        result["result_evidence_ref"] = json!(epic.result_evidence_ref);
        result["result_sha256"] = json!(epic.body_sha256);
    }
    if let Some(review) = binding.review {
        result["submission_id"] = json!(review.submission_id);
        result["cancelled_child_ids"] = json!(review.cancelled_child_ids);
    }
    Ok(result)
}
