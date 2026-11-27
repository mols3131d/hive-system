//! Exact Issue endpoint scopes, decisions and historical row effects.

use mysql::prelude::Queryable;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::error::{ErrorCode, HiveError};

use super::{
    Snapshot, context_workflow as workflow, is_commit_hash, is_uuid_v7,
    issue_readiness::NON_WHITESPACE,
    issue_writer::{invalid, valid_revision},
    read_error,
    records::{self, IssueStatus},
};

#[derive(Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct Endpoint {
    pub(super) area_id: String,
    pub(super) issue_id: String,
    pub(super) expected_issue_revision: String,
}

impl Endpoint {
    pub(super) fn canonical(&self) -> serde_json::Value {
        serde_json::json!([self.area_id, self.issue_id, self.expected_issue_revision])
    }

    pub(super) fn check_scope(&self, snapshot: &mut Snapshot) -> Result<(), HiveError> {
        let owners: Vec<String> = snapshot.exec_rows(
            |commit| {
                format!(
                    "SELECT i.area_id FROM issues AS OF '{commit}' i
                 JOIN areas AS OF '{commit}' a ON CAST(a.id AS BINARY) = CAST(i.area_id AS BINARY)
                 WHERE CAST(i.id AS BINARY) = ? LIMIT 2"
                )
            },
            (&self.issue_id,),
        )?;
        if !matches!(owners.as_slice(), [owner] if *owner == self.area_id) {
            return Err(workflow::unverified());
        }
        Ok(())
    }

    fn valid(&self) -> bool {
        is_uuid_v7(&self.area_id)
            && is_uuid_v7(&self.issue_id)
            && valid_revision(&self.expected_issue_revision)
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Grant {
    pub(super) endpoint: Endpoint,
    decision: String,
    evidence_id: String,
    revision: String,
    source_ref: String,
    issuer_ref: String,
    valid_from: String,
    expires_at: String,
    withdrawn: bool,
}

impl Grant {
    pub(super) fn allows(&self, endpoint: &Endpoint, now: OffsetDateTime) -> bool {
        self.endpoint == *endpoint
            && endpoint.valid()
            && self.decision == "allow"
            && !self.withdrawn
            && [
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

pub(super) fn stale() -> HiveError {
    HiveError::new(
        ErrorCode::StaleRevision,
        "An expected Issue revision is stale.",
    )
}

pub(super) fn as_of(commit: Option<&str>) -> Result<String, HiveError> {
    match commit {
        None => Ok(String::new()),
        Some(commit) if is_commit_hash(commit) => Ok(format!(" AS OF '{commit}'")),
        _ => Err(workflow::unverified()),
    }
}

pub(super) struct Issue {
    area: String,
    pub(super) revision: String,
    pub(super) updated_at: String,
    pub(super) parent: Option<String>,
    protected: String,
    status: String,
    kind: String,
    closed_well: bool,
    closure_absent: bool,
}

impl Issue {
    pub(super) fn read(
        db: &mut impl Queryable,
        endpoint: &Endpoint,
        commit: Option<&str>,
    ) -> Result<Self, HiveError> {
        let as_of = as_of(commit)?;
        type Row = (
            String,
            String,
            String,
            Option<String>,
            String,
            String,
            String,
            bool,
            bool,
        );
        let row: Option<Row> = db.exec_first(
            format!(
                "SELECT area_id, revision, DATE_FORMAT(updated_at, '%Y-%m-%dT%H:%i:%s.%fZ'), parent_id,
                    JSON_ARRAY(CAST(number AS CHAR), type, status, priority, created_at, closed_at,
                      SHA2(CAST(title AS BINARY), 256), SHA2(CAST(body AS BINARY), 256),
                      SHA2(CAST(acceptance_criteria AS BINARY), 256), SHA2(CAST(hold_reason AS BINARY), 256),
                      SHA2(CAST(closed_by_ref AS BINARY), 256), SHA2(CAST(resolution_note AS BINARY), 256)),
                    status, type,
                    COALESCE(YEAR(closed_at) > 0 AND closed_by_ref REGEXP ? AND resolution_note REGEXP ?, FALSE),
                    closed_at IS NULL AND closed_by_ref IS NULL AND resolution_note IS NULL
                 FROM issues{as_of} WHERE CAST(id AS BINARY) = ?"
            ),
            (NON_WHITESPACE, NON_WHITESPACE, &endpoint.issue_id),
        ).map_err(read_error)?;
        let Some((
            area,
            revision,
            updated_at,
            parent,
            protected,
            status,
            kind,
            closed_well,
            closure_absent,
        )) = row
        else {
            return Err(workflow::unverified());
        };
        if area != endpoint.area_id {
            return Err(workflow::unverified());
        }
        Ok(Self {
            area,
            revision,
            updated_at,
            parent,
            protected,
            status,
            kind,
            closed_well,
            closure_absent,
        })
    }

    pub(super) fn verify_revision(
        &self,
        before: &Self,
        expected: &str,
        updated_at: &str,
    ) -> Result<(), HiveError> {
        if self.area != before.area
            || self.revision != expected
            || self.revision == before.revision
            || self.updated_at != updated_at
            || self.protected != before.protected
        {
            return Err(workflow::unverified());
        }
        Ok(())
    }

    pub(super) fn validate_state(
        &self,
        db: &mut impl Queryable,
        endpoint: &Endpoint,
        commit: Option<&str>,
    ) -> Result<IssueStatus, HiveError> {
        let invalid_state = || {
            HiveError::new(
                ErrorCode::InvalidRecord,
                "An endpoint has invalid lifecycle or Claim state.",
            )
        };
        let status = IssueStatus::parse(&self.status).ok_or_else(invalid_state)?;
        let as_of = as_of(commit)?;
        let claims: Vec<(String, String, String)> = db
            .exec(
                format!(
                    "SELECT issue_id, claim_id, holder_ref FROM issue_claims{as_of}
                 WHERE issue_id = ? LIMIT 2"
                ),
                (&endpoint.issue_id,),
            )
            .map_err(read_error)?;
        let valid_claim = if status == IssueStatus::InProgress {
            self.kind != "epic"
                && matches!(claims.as_slice(), [(id, claim, holder)]
                if *id == endpoint.issue_id && is_uuid_v7(claim) && workflow::valid_ref(holder))
        } else {
            claims.is_empty()
        };
        if !records::is_issue_type(&self.kind)
            || !valid_claim
            || (status.is_terminal() && !self.closed_well)
            || (!status.is_terminal() && !self.closure_absent)
        {
            return Err(invalid_state());
        }
        Ok(status)
    }
}

pub(super) fn bind_revisions<'a>(
    revisions: &[String],
    endpoints: impl IntoIterator<Item = &'a mut Endpoint>,
) -> Result<(), HiveError> {
    let invalid_matches = || invalid("Supply exactly one --if-match ISSUE=REV for each endpoint.");
    let mut count = 0;
    for endpoint in endpoints {
        let mut matching = revisions
            .iter()
            .filter_map(|revision| revision.split_once('='))
            .filter(|(id, _)| *id == endpoint.issue_id);
        let (_, revision) = matching.next().ok_or_else(invalid_matches)?;
        if matching.next().is_some() || !valid_revision(revision) {
            return Err(invalid_matches());
        }
        endpoint.expected_issue_revision = revision.to_owned();
        if !endpoint.valid() {
            return Err(invalid("Supply exact Issue and Area UUIDv7s."));
        }
        count += 1;
    }
    if count != revisions.len() {
        return Err(invalid_matches());
    }
    Ok(())
}
