use std::path::Path;

use mysql::{Transaction, prelude::Queryable};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use time::OffsetDateTime;

use super::{CommonOptions, DoltSuccess, IssueUpdateArgs};
use crate::dolt::{
    DoltTarget, Snapshot,
    context_evidence::read_evidence_bytes,
    context_workflow as workflow, is_commit_hash, is_uuid, is_uuid_v7,
    issue_writer::{
        self, Decision, Kind, Mutation, StagedIssue, WriteIdentity, digest, invalid, new_uuid_v7,
    },
    read_error,
    receipt_history::{HistoricalReceipt, valid_sha256},
    scope::{require_active_area, require_active_area_at},
};
use crate::error::{ErrorCode, HiveError};

pub(in crate::dolt) const KIND: &str = "issue.update";
const MAX_REQUEST_BYTES: usize = 256 * 1024;
const MAX_TEXT_BYTES: usize = 65_535;
const ARCHIVED_AREA: &str = "Issue updates require active Area ancestry.";

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Field {
    Title,
    Body,
    AcceptanceCriteria,
    Priority,
    HoldReason,
}

impl Field {
    const ALL: [Self; 5] = [
        Self::Title,
        Self::Body,
        Self::AcceptanceCriteria,
        Self::Priority,
        Self::HoldReason,
    ];
}

type Values = [Value; 5];

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct UpdateResult {
    version: u8,
    area_id: String,
    issue_id: String,
    issue_revision: String,
    fields: Vec<Field>,
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
    area_id: String,
    issue_id: String,
    expected_issue_revision: String,
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
    actor: &'a str,
    area: &'a str,
    issue: &'a str,
}

impl Scope<'_> {
    fn fingerprint(&self, expected: &str, values: &Values) -> String {
        digest(
            json!([
                1,
                KIND,
                self.workspace,
                self.area,
                self.issue,
                self.actor,
                expected,
                values
            ])
            .to_string()
            .as_bytes(),
        )
    }

    fn verify(
        &self,
        evidence: &UpdateEvidence,
        request: &str,
        fingerprint: &str,
        expected: &str,
        now: OffsetDateTime,
    ) -> Result<(), HiveError> {
        if evidence.version != 1
            || evidence.operation != KIND
            || evidence.workspace_id != self.workspace
            || evidence.request_id != request
            || evidence.fingerprint != fingerprint
            || evidence.actor_ref != self.actor
            || evidence.area_id != self.area
            || evidence.issue_id != self.issue
            || evidence.expected_issue_revision != expected
            || !valid_revision(expected)
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

struct Update<'a> {
    scope: Scope<'a>,
    expected: &'a str,
    args: &'a IssueUpdateArgs,
    body: Option<String>,
    acceptance: Option<String>,
    values: Values,
    fingerprint: String,
    decision: Decision<UpdateEvidence>,
}

pub(super) fn write(
    common: &CommonOptions,
    args: &IssueUpdateArgs,
) -> Result<DoltSuccess, HiveError> {
    let target = DoltTarget::parse(&args.target)?;
    let workspace =
        crate::config_v2::read_explicit(common.workspace.as_deref().expect("validated workspace"))?;
    if !is_uuid_v7(&args.area)
        || !is_uuid_v7(&args.issue)
        || !is_uuid(&args.request_id)
        || !workflow::valid_ref(&args.actor)
    {
        return Err(invalid(
            "Select exact Area and Issue UUIDv7s, a request UUID and an actor reference.",
        ));
    }
    let expected = args
        .if_match
        .split_once('=')
        .filter(|(issue, revision)| *issue == args.issue && valid_revision(revision))
        .map(|(_, revision)| revision)
        .ok_or_else(|| invalid("Supply one Issue revision as --if-match ISSUE=REV."))?;
    let scope = Scope {
        workspace: workspace.workspace_id(),
        actor: &args.actor,
        area: &args.area,
        issue: &args.issue,
    };

    if args
        .title
        .as_deref()
        .is_some_and(|title| !valid_title(title))
    {
        return Err(invalid(
            "Supply a nonblank Issue title of at most 255 characters without control characters.",
        ));
    }
    if args.hold.as_deref().is_some_and(|hold| !valid_hold(hold)) {
        return Err(invalid(
            "Supply a nonblank Issue hold reason without control characters, within the supported byte limit.",
        ));
    }
    let body = read_file(args.file.as_deref(), MAX_REQUEST_BYTES)?;
    let acceptance = read_file(args.acceptance_file.as_deref(), MAX_TEXT_BYTES)?;
    if acceptance.as_deref().is_some_and(|text| !valid_text(text)) {
        return Err(invalid(
            "An explicit Issue acceptance file must contain nonblank criteria.",
        ));
    }
    let bytes = [&args.title, &body, &acceptance, &args.hold]
        .into_iter()
        .filter_map(Option::as_ref)
        .map(String::len)
        .sum::<usize>();
    if bytes > MAX_REQUEST_BYTES {
        return Err(HiveError::new(
            ErrorCode::ContextLimit,
            "The Issue edit exceeds the supported byte limit.",
        ));
    }
    let hold = match &args.hold {
        Some(reason) => json!([digest(reason.as_bytes())]),
        None if args.clear_hold => json!([null]),
        None => Value::Null,
    };
    let values = [
        json!(args.title),
        json!(text_digest(body.as_deref())),
        json!(text_digest(acceptance.as_deref())),
        json!(args.priority),
        hold,
    ];
    if values.iter().all(Value::is_null) {
        return Err(invalid("Supply at least one Issue edit."));
    }
    let fingerprint = scope.fingerprint(expected, &values);
    let decision = issue_writer::read_decision(&args.evidence_file)?;
    let update = Update {
        scope,
        expected,
        args,
        body,
        acceptance,
        values,
        fingerprint,
        decision,
    };
    issue_writer::execute(&workspace, target, &update)
}

fn read_file(path: Option<&Path>, limit: usize) -> Result<Option<String>, HiveError> {
    path.map(|path| {
        let bytes = read_evidence_bytes(path, limit).map_err(|error| {
            let message = if matches!(error.code, ErrorCode::ContextLimit) {
                "An Issue edit file exceeds its supported byte limit."
            } else {
                "Could not read the explicit regular Issue edit file."
            };
            HiveError::new(error.code, message)
        })?;
        String::from_utf8(bytes).map_err(|_| invalid("Issue edit files must be UTF-8."))
    })
    .transpose()
}

fn valid_revision(revision: &str) -> bool {
    revision.chars().count() <= 36 && workflow::valid_ref(revision)
}

fn valid_title(title: &str) -> bool {
    !title.trim().is_empty() && title.chars().count() <= 255 && !title.chars().any(char::is_control)
}

fn valid_text(text: &str) -> bool {
    !text.trim().is_empty() && text.len() <= MAX_TEXT_BYTES
}

fn valid_hold(reason: &str) -> bool {
    valid_text(reason) && !reason.chars().any(char::is_control)
}

fn text_digest(text: Option<&str>) -> Option<String> {
    text.map(|value| digest(value.as_bytes()))
}

impl Mutation for Update<'_> {
    type Guard = ();

    fn identity(&self) -> WriteIdentity<'_> {
        WriteIdentity {
            kind: Kind::Update,
            request_id: &self.args.request_id,
            area_id: self.scope.area,
            issue_id: Some(self.scope.issue),
            actor: self.scope.actor,
            fingerprint: &self.fingerprint,
            authorization_ref: &self.decision.evidence.authorization_ref,
        }
    }

    fn verify_decision(&self, now: OffsetDateTime) -> Result<(), HiveError> {
        if self.decision.decision != "allow" {
            return Err(workflow::unverified());
        }
        self.scope.verify(
            &self.decision.evidence,
            &self.args.request_id,
            &self.fingerprint,
            self.expected,
            now,
        )
    }

    fn evidence(&self) -> Result<String, HiveError> {
        issue_writer::encode(&self.decision.evidence, Kind::Update)
    }

    fn check_scope(&self, snapshot: &mut Snapshot) -> Result<(), HiveError> {
        let owners: Vec<String> = snapshot.exec_rows(
            |commit| {
                format!(
                    "SELECT i.area_id FROM issues AS OF '{commit}' i
                 JOIN areas AS OF '{commit}' a ON CAST(a.id AS BINARY) = CAST(i.area_id AS BINARY)
                 WHERE CAST(i.id AS BINARY) = ? LIMIT 2"
                )
            },
            (self.scope.issue,),
        )?;
        if matches!(owners.as_slice(), [owner] if owner == self.scope.area) {
            Ok(())
        } else {
            Err(workflow::unverified())
        }
    }

    fn guard(&self, db: &mut Transaction<'_>, _: &mut Snapshot) -> Result<(), HiveError> {
        require_active_area(db, self.scope.area, ARCHIVED_AREA)?;
        let issue = Issue::read(db, self.scope.issue, None)?;
        if issue.area != self.scope.area {
            return Err(workflow::unverified());
        }
        if issue.revision != self.expected {
            return Err(stale());
        }
        let before = issue.values();
        let changed = Field::ALL
            .into_iter()
            .filter(|field| {
                let index = *field as usize;
                !self.values[index].is_null() && self.values[index] != before[index]
            })
            .collect::<Vec<_>>();
        issue.check_changes(
            &changed,
            self.acceptance.as_deref().or(issue.acceptance.as_deref()),
        )
    }

    fn stage(
        &self,
        db: &mut Transaction<'_>,
        (): (),
        now: OffsetDateTime,
        sql_time: &str,
    ) -> Result<StagedIssue, HiveError> {
        let result = UpdateResult {
            version: 1,
            area_id: self.scope.area.to_owned(),
            issue_id: self.scope.issue.to_owned(),
            issue_revision: new_uuid_v7(now)?,
            fields: Field::ALL
                .into_iter()
                .filter(|field| !self.values[*field as usize].is_null())
                .collect(),
        };
        db.exec_drop(
            "UPDATE issues SET title = COALESCE(?, title), body = COALESCE(?, body),
             acceptance_criteria = COALESCE(?, acceptance_criteria), priority = COALESCE(?, priority),
             hold_reason = CASE WHEN ? THEN NULL ELSE COALESCE(?, hold_reason) END,
             revision = ?, updated_at = ?
             WHERE CAST(id AS BINARY) = ? AND CAST(area_id AS BINARY) = ? AND CAST(revision AS BINARY) = ?",
            (&self.args.title, &self.body, &self.acceptance, self.args.priority,
             self.args.clear_hold, &self.args.hold, &result.issue_revision, sql_time,
             self.scope.issue, self.scope.area, self.expected),
        ).map_err(read_error)?;
        if db.affected_rows() != 1 {
            return Err(stale());
        }
        let encoded = issue_writer::encode(&result, Kind::Update)?;
        Ok(StagedIssue {
            issue_id: result.issue_id,
            result: encoded,
        })
    }
}

fn stale() -> HiveError {
    HiveError::new(
        ErrorCode::StaleRevision,
        "The expected Issue revision is stale.",
    )
}

struct Issue {
    area: String,
    revision: String,
    status: String,
    updated_at: String,
    protected_fields: String,
    claimed: bool,
    title: String,
    body_sha256: Option<String>,
    body_bytes: Option<usize>,
    acceptance: Option<String>,
    priority: Option<u8>,
    hold: Option<String>,
}

impl Issue {
    fn read(db: &mut impl Queryable, issue: &str, commit: Option<&str>) -> Result<Self, HiveError> {
        let as_of = match commit {
            None => String::new(),
            Some(commit) if is_commit_hash(commit) => format!(" AS OF '{commit}'"),
            _ => return Err(workflow::unverified()),
        };
        type Row = (
            String,
            String,
            String,
            String,
            String,
            bool,
            String,
            Option<String>,
            Option<usize>,
            Option<String>,
            Option<u8>,
            Option<String>,
        );
        let row: Row = db.exec_first(
            format!(
                "SELECT i.area_id, i.revision, i.status,
                 DATE_FORMAT(i.updated_at, '%Y-%m-%dT%H:%i:%s.%fZ'),
                 JSON_ARRAY(i.number, i.parent_id, i.type, i.created_at, i.closed_at, i.closed_by_ref,
                            LOWER(SHA2(CAST(i.resolution_note AS BINARY), 256))),
                 EXISTS (SELECT 1 FROM issue_claims{as_of} c WHERE CAST(c.issue_id AS BINARY) = i.id),
                 i.title, LOWER(SHA2(CAST(i.body AS BINARY), 256)), OCTET_LENGTH(i.body),
                 i.acceptance_criteria, i.priority, i.hold_reason
                 FROM issues{as_of} i WHERE CAST(i.id AS BINARY) = ?"
            ),
            (issue,),
        ).map_err(read_error)?.ok_or_else(workflow::unverified)?;
        let (
            area,
            revision,
            status,
            updated_at,
            protected_fields,
            claimed,
            title,
            body_sha256,
            body_bytes,
            acceptance,
            priority,
            hold,
        ) = row;
        Ok(Self {
            area,
            revision,
            status,
            updated_at,
            protected_fields,
            claimed,
            title,
            body_sha256,
            body_bytes,
            acceptance,
            priority,
            hold,
        })
    }

    fn values(&self) -> Values {
        [
            json!(self.title),
            json!(self.body_sha256),
            json!(text_digest(self.acceptance.as_deref())),
            json!(self.priority),
            json!([text_digest(self.hold.as_deref())]),
        ]
    }

    fn check_changes(&self, changed: &[Field], acceptance: Option<&str>) -> Result<(), HiveError> {
        if self.claimed != (self.status == "in-progress")
            || !matches!(
                self.status.as_str(),
                "backlog" | "todo" | "in-progress" | "review" | "done" | "cancelled"
            )
        {
            return Err(HiveError::new(
                ErrorCode::InvalidRecord,
                "The Issue lifecycle or Claim state is invalid.",
            ));
        }
        if changed.is_empty() {
            return Err(invalid("The Issue edit would make no change."));
        }
        if !matches!(self.status.as_str(), "backlog" | "todo") && changed != [Field::Priority] {
            return Err(HiveError::new(
                ErrorCode::InvalidTransition,
                "Only priority changes are allowed outside backlog or todo.",
            ));
        }
        if self.status == "todo" && !acceptance.is_some_and(valid_text) {
            return Err(HiveError::new(
                ErrorCode::InvalidTransition,
                "A todo Issue requires nonblank acceptance criteria.",
            ));
        }
        Ok(())
    }
}

pub(in crate::dolt) struct ScopedUpdate {
    result: UpdateResult,
    expected: String,
}

pub(in crate::dolt) fn check_scope(
    snapshot: &Snapshot,
    request: &str,
    area: &str,
    issue: &str,
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
        issue,
    };
    if receipt.kind != KIND
        || receipt.actor_ref != actor
        || result.version != 1
        || result.area_id != area
        || result.issue_id != issue
        || !is_uuid_v7(&result.issue_revision)
        || result.fields.is_empty()
        || !result
            .fields
            .windows(2)
            .all(|pair| (pair[0] as usize) < pair[1] as usize)
        || evidence.authorization_ref != receipt.authorization_ref
    {
        return Err(workflow::unverified());
    }
    let created = workflow::parse_time(&receipt.created_at).ok_or_else(workflow::unverified)?;
    scope.verify(
        &evidence,
        request,
        &receipt.fingerprint,
        &evidence.expected_issue_revision,
        created,
    )?;
    Ok(ScopedUpdate {
        result,
        expected: evidence.expected_issue_revision,
    })
}

pub(in crate::dolt) fn verify_effect(
    snapshot: &mut Snapshot,
    introduced: &str,
    receipt: &HistoricalReceipt,
    scoped: &ScopedUpdate,
) -> Result<Value, HiveError> {
    let result = &scoped.result;
    let base = &receipt.base_commit;
    let db = &mut snapshot.connection;
    let before = Issue::read(db, &result.issue_id, Some(base))?;
    let after = Issue::read(db, &result.issue_id, Some(introduced))?;
    if before.area != result.area_id
        || after.area != result.area_id
        || before.revision != scoped.expected
        || after.revision != result.issue_revision
        || after.revision == before.revision
        || after.updated_at != receipt.created_at
        || before.status != after.status
        || before.claimed != after.claimed
        || before.protected_fields != after.protected_fields
    {
        return Err(workflow::unverified());
    }

    require_active_area_at(db, &result.area_id, base, ARCHIVED_AREA)?;

    let old_values = before.values();
    let new_values = after.values();
    let changed = Field::ALL
        .into_iter()
        .filter(|field| old_values[*field as usize] != new_values[*field as usize])
        .collect::<Vec<_>>();
    if !changed.iter().all(|field| result.fields.contains(field)) {
        return Err(workflow::unverified());
    }
    before.check_changes(&changed, after.acceptance.as_deref())?;
    let mut values = Values::default();
    let mut bytes = 0;
    for &field in &result.fields {
        bytes += match field {
            Field::Title => valid_title(&after.title).then_some(after.title.len()),
            Field::Body => after.body_bytes.filter(|bytes| {
                *bytes <= MAX_REQUEST_BYTES
                    && after.body_sha256.as_deref().is_some_and(valid_sha256)
            }),
            Field::AcceptanceCriteria => after
                .acceptance
                .as_deref()
                .filter(|text| valid_text(text))
                .map(str::len),
            Field::Priority => after.priority.filter(|priority| *priority <= 3).map(|_| 0),
            Field::HoldReason => match after.hold.as_deref() {
                None => Some(0),
                Some(reason) if valid_hold(reason) => Some(reason.len()),
                Some(_) => None,
            },
        }
        .ok_or_else(workflow::unverified)?;
        values[field as usize] = new_values[field as usize].clone();
    }
    let scope = Scope {
        workspace: &snapshot.workspace_id,
        actor: &receipt.actor_ref,
        area: &result.area_id,
        issue: &result.issue_id,
    };
    if bytes > MAX_REQUEST_BYTES
        || scope.fingerprint(&scoped.expected, &values) != receipt.fingerprint
    {
        return Err(workflow::unverified());
    }
    verify_commit_delta(db, base, introduced)?;
    Ok(json!({
        "version": result.version, "area_id": result.area_id,
        "issue_id": result.issue_id, "issue_revision": result.issue_revision,
    }))
}

fn verify_commit_delta(
    db: &mut impl Queryable,
    base: &str,
    introduced: &str,
) -> Result<(), HiveError> {
    let tables: Vec<(String, String, String, bool, bool)> = db
        .exec(
            "SELECT from_table_name, to_table_name, diff_type, data_change, schema_change
         FROM DOLT_DIFF_SUMMARY(?, ?) ORDER BY from_table_name LIMIT 3",
            (base, introduced),
        )
        .map_err(read_error)?;
    if !matches!(tables.as_slice(), [(old_issue, new_issue, issue_kind, true, false),
        (old_receipt, new_receipt, receipt_kind, true, false)]
        if old_issue == "issues" && new_issue == "issues" && issue_kind == "modified"
        && old_receipt == "operation_receipts" && new_receipt == "operation_receipts" && receipt_kind == "modified")
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
    if !matches!(rows.as_slice(), [(issue_table, 0, 0, 1), (receipt_table, 1, 0, 0)]
        if issue_table == "issues" && receipt_table == "operation_receipts")
        || db
            .query_first::<mysql::Row, _>("SHOW WARNINGS LIMIT 1")
            .map_err(read_error)?
            .is_some()
    {
        return Err(workflow::unverified());
    }
    Ok(())
}
