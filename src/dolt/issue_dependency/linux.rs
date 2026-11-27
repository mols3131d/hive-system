use mysql::{Transaction, prelude::Queryable};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use time::OffsetDateTime;

use crate::config_v2;
use crate::error::{ErrorCode, HiveError};

use super::{CommonOptions, DoltSuccess, IssueDependencyArgs};
use crate::dolt::{
    DoltTarget, Snapshot, context_workflow as workflow, is_uuid, is_uuid_v7,
    issue_relation::{Endpoint, Grant, Issue, as_of, bind_revisions, stale},
    issue_writer::{
        self, Decision, Kind, Mutation, StagedMutation, WriteIdentity, digest, invalid, new_uuid_v7,
    },
    read_error,
    receipt_history::HistoricalReceipt,
    records::{self, IssueStatus, RecordReader},
    scope::{require_active_area, require_active_area_at},
};

const ARCHIVED: &str = "Dependency edits require active ancestry for both Areas.";

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Evidence {
    version: u8,
    operation: String,
    workspace_id: String,
    request_id: String,
    fingerprint: String,
    actor_ref: String,
    authorization_ref: String,
    issue: Grant,
    prerequisite: Grant,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct DependencyResult {
    version: u8,
    area_id: String,
    issue_id: String,
    issue_revision: String,
    prerequisite_area_id: String,
    prerequisite_id: String,
    prerequisite_revision: String,
}

struct Request<'a> {
    kind: Kind,
    workspace: &'a str,
    request: &'a str,
    actor: &'a str,
    issue: &'a Endpoint,
    prerequisite: &'a Endpoint,
}

impl Request<'_> {
    fn fingerprint(&self) -> String {
        digest(
            json!([
                1,
                self.kind.name(),
                self.workspace,
                self.actor,
                self.issue.canonical(),
                self.prerequisite.canonical(),
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
            || evidence.operation != self.kind.name()
            || evidence.workspace_id != self.workspace
            || evidence.request_id != self.request
            || evidence.actor_ref != self.actor
            || evidence.fingerprint != fingerprint
            || !workflow::valid_ref(&evidence.authorization_ref)
            || self.issue.issue_id == self.prerequisite.issue_id
            || !evidence.issue.allows(self.issue, now)
            || !evidence.prerequisite.allows(self.prerequisite, now)
        {
            return Err(workflow::unverified());
        }
        Ok(())
    }

    fn add(&self) -> bool {
        matches!(self.kind, Kind::DependencyAdd)
    }
}

struct Dependency<'a> {
    request: Request<'a>,
    fingerprint: String,
    decision: Decision<Evidence>,
}

pub(super) fn write(
    common: &CommonOptions,
    args: &IssueDependencyArgs,
    add: bool,
) -> Result<DoltSuccess, HiveError> {
    if args.issue == args.prerequisite {
        return Err(invalid("Select two distinct Issue endpoints."));
    }
    let mut issue = Endpoint {
        area_id: args.area.clone(),
        issue_id: args.issue.clone(),
        expected_issue_revision: String::new(),
    };
    let mut prerequisite = Endpoint {
        area_id: args.prerequisite_area.clone(),
        issue_id: args.prerequisite.clone(),
        expected_issue_revision: String::new(),
    };
    bind_revisions(&args.if_match, [&mut issue, &mut prerequisite])?;
    if !is_uuid(&args.request_id) || !workflow::valid_ref(&args.actor) {
        return Err(invalid(
            "Supply exact Issue and Area UUIDv7s, a request UUID and a bounded actor reference.",
        ));
    }

    let workspace =
        config_v2::read_explicit(common.workspace.as_deref().expect("validated workspace"))?;
    let target = DoltTarget::parse(&args.target)?;
    let request = Request {
        kind: if add {
            Kind::DependencyAdd
        } else {
            Kind::DependencyRemove
        },
        workspace: workspace.workspace_id(),
        request: &args.request_id,
        actor: &args.actor,
        issue: &issue,
        prerequisite: &prerequisite,
    };
    let mutation = Dependency {
        fingerprint: request.fingerprint(),
        decision: issue_writer::read_decision(&args.evidence_file)?,
        request,
    };
    issue_writer::execute(&workspace, target, &mutation)
}

impl Mutation for Dependency<'_> {
    type Guard = ();

    fn identity(&self) -> WriteIdentity<'_> {
        WriteIdentity {
            kind: self.request.kind,
            request_id: self.request.request,
            area_id: Some(&self.request.issue.area_id),
            record_id: Some(&self.request.issue.issue_id),
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
        issue_writer::encode(&self.decision.evidence, self.request.kind)
    }

    fn check_scope(&self, snapshot: &mut Snapshot) -> Result<(), HiveError> {
        for endpoint in [self.request.issue, self.request.prerequisite] {
            endpoint.check_scope(snapshot)?;
        }
        Ok(())
    }

    fn guard(&self, db: &mut Transaction<'_>, snapshot: &mut Snapshot) -> Result<(), HiveError> {
        for endpoint in [self.request.issue, self.request.prerequisite] {
            require_active_area(db, &endpoint.area_id, ARCHIVED)?;
        }
        check_endpoints(db, self.request.issue, self.request.prerequisite, None)?;
        records::check_dependency_edit(
            &mut RecordReader::Online(snapshot),
            &self.request.issue.issue_id,
            &self.request.prerequisite.issue_id,
            self.request.add(),
        )
    }

    fn stage(
        &self,
        db: &mut Transaction<'_>,
        (): (),
        now: OffsetDateTime,
        sql_time: &str,
    ) -> Result<StagedMutation, HiveError> {
        let issue = self.request.issue;
        let prerequisite = self.request.prerequisite;
        let sql = if self.request.add() {
            "INSERT INTO issue_dependencies (issue_id, prerequisite_id) VALUES (?, ?)"
        } else {
            "DELETE FROM issue_dependencies WHERE CAST(issue_id AS BINARY) = ? AND CAST(prerequisite_id AS BINARY) = ?"
        };
        db.exec_drop(sql, (&issue.issue_id, &prerequisite.issue_id))
            .map_err(read_error)?;
        if db.affected_rows() != 1 {
            return Err(HiveError::new(
                ErrorCode::InvalidRecord,
                "The exact dependency edit could not be staged.",
            ));
        }
        let issue_revision = new_uuid_v7(now)?;
        let prerequisite_revision = new_uuid_v7(now)?;
        for (endpoint, revision) in [
            (issue, &issue_revision),
            (prerequisite, &prerequisite_revision),
        ] {
            if *revision == endpoint.expected_issue_revision {
                return Err(stale());
            }
            db.exec_drop(
                "UPDATE issues SET revision = ?, updated_at = ? WHERE CAST(id AS BINARY) = ?
                 AND CAST(area_id AS BINARY) = ? AND CAST(revision AS BINARY) = ?",
                (
                    revision,
                    sql_time,
                    &endpoint.issue_id,
                    &endpoint.area_id,
                    &endpoint.expected_issue_revision,
                ),
            )
            .map_err(read_error)?;
            if db.affected_rows() != 1 {
                return Err(stale());
            }
        }
        let result = DependencyResult {
            version: 1,
            area_id: issue.area_id.clone(),
            issue_id: issue.issue_id.clone(),
            issue_revision,
            prerequisite_area_id: prerequisite.area_id.clone(),
            prerequisite_id: prerequisite.issue_id.clone(),
            prerequisite_revision,
        };
        Ok(StagedMutation {
            result: issue_writer::encode(&result, self.request.kind)?,
            record_id: result.issue_id,
        })
    }
}

fn check_endpoints(
    db: &mut impl Queryable,
    issue: &Endpoint,
    prerequisite: &Endpoint,
    commit: Option<&str>,
) -> Result<[Issue; 2], HiveError> {
    let dependent = Issue::read(db, issue, commit)?;
    let required = Issue::read(db, prerequisite, commit)?;
    if dependent.revision != issue.expected_issue_revision
        || required.revision != prerequisite.expected_issue_revision
    {
        return Err(stale());
    }
    let dependent_status = dependent.validate_state(db, issue, commit)?;
    let required_status = required.validate_state(db, prerequisite, commit)?;
    // A protected Issue must retain accepted prerequisites. An existing
    // unsatisfied edge is also invalid and cannot be repaired by this writer.
    if dependent_status.requires_done_prerequisites() && required_status != IssueStatus::Done {
        return Err(HiveError::new(
            ErrorCode::InvalidTransition,
            "An Issue in progress, in review or done requires an accepted prerequisite.",
        ));
    }
    Ok([dependent, required])
}

pub(in crate::dolt) struct ScopedDependency {
    add: bool,
    evidence: Evidence,
    result: DependencyResult,
}

pub(in crate::dolt) fn check_receipt(
    snapshot: &Snapshot,
    kind: Kind,
    request_id: &str,
    area: &str,
    issue: &str,
    actor: &str,
    receipt: &HistoricalReceipt,
) -> Result<ScopedDependency, HiveError> {
    let evidence: Evidence =
        serde_json::from_str(&receipt.workflow_evidence).map_err(|_| workflow::unverified())?;
    let result: DependencyResult =
        serde_json::from_str(&receipt.result).map_err(|_| workflow::unverified())?;
    let request = Request {
        kind,
        workspace: &snapshot.workspace_id,
        request: request_id,
        actor,
        issue: &evidence.issue.endpoint,
        prerequisite: &evidence.prerequisite.endpoint,
    };
    if receipt.kind != kind.name()
        || receipt.actor_ref != actor
        || result.version != 1
        || result.area_id != area
        || result.issue_id != issue
        || result.area_id != request.issue.area_id
        || result.issue_id != request.issue.issue_id
        || result.prerequisite_area_id != request.prerequisite.area_id
        || result.prerequisite_id != request.prerequisite.issue_id
        || !is_uuid_v7(&result.issue_revision)
        || !is_uuid_v7(&result.prerequisite_revision)
        || receipt.authorization_ref != evidence.authorization_ref
        || receipt.fingerprint != request.fingerprint()
    {
        return Err(workflow::unverified());
    }
    let created = workflow::parse_time(&receipt.created_at).ok_or_else(workflow::unverified)?;
    request.verify(&evidence, &receipt.fingerprint, created)?;
    Ok(ScopedDependency {
        add: request.add(),
        evidence,
        result,
    })
}

impl ScopedDependency {
    pub(in crate::dolt) fn verify_effect(
        &self,
        snapshot: &mut Snapshot,
        introduced: &str,
        receipt: &HistoricalReceipt,
    ) -> Result<Value, HiveError> {
        let base = &receipt.base_commit;
        let db = &mut snapshot.connection;
        let issue = &self.evidence.issue.endpoint;
        let prerequisite = &self.evidence.prerequisite.endpoint;
        let before = check_endpoints(db, issue, prerequisite, Some(base))?;
        for ((endpoint, expected), before) in [
            (issue, &self.result.issue_revision),
            (prerequisite, &self.result.prerequisite_revision),
        ]
        .into_iter()
        .zip(before)
        {
            require_active_area_at(db, &endpoint.area_id, base, ARCHIVED)?;
            let after = Issue::read(db, endpoint, Some(introduced))?;
            after.verify_revision(&before, expected, &receipt.created_at)?;
            if after.parent != before.parent {
                return Err(workflow::unverified());
            }
        }
        records::check_dependency_edit(
            &mut RecordReader::Historical {
                connection: db,
                commit: base,
            },
            &issue.issue_id,
            &prerequisite.issue_id,
            self.add,
        )?;
        let as_of = as_of(Some(introduced))?;
        let count: Option<u64> = db
            .exec_first(
                format!(
                    "SELECT COUNT(*) FROM issue_dependencies{as_of}
                WHERE CAST(issue_id AS BINARY) = ? AND CAST(prerequisite_id AS BINARY) = ?"
                ),
                (&issue.issue_id, &prerequisite.issue_id),
            )
            .map_err(read_error)?;
        if count != Some(u64::from(self.add)) {
            return Err(workflow::unverified());
        }
        crate::dolt::receipt_history::verify_delta(
            db,
            base,
            introduced,
            &[
                (
                    "issue_dependencies",
                    u64::from(self.add),
                    u64::from(!self.add),
                    0,
                ),
                ("issues", 0, 0, 2),
                ("operation_receipts", 1, 0, 0),
            ],
        )?;
        Ok(json!(self.result))
    }
}
