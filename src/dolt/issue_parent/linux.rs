use mysql::{Transaction, prelude::Queryable};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use time::OffsetDateTime;

use crate::{config_v2, error::HiveError};

use super::{CommonOptions, DoltSuccess, IssueParentArgs};
use crate::dolt::{
    DoltTarget, Snapshot, context_workflow as workflow, is_uuid, is_uuid_v7,
    issue_relation::{Endpoint, Grant, Issue, bind_revisions, stale},
    issue_writer::{
        self, Decision, Kind, Mutation, StagedMutation, WriteIdentity, digest, invalid, new_uuid_v7,
    },
    read_error,
    receipt_history::{HistoricalReceipt, verify_delta},
    records::{self, RecordReader},
    scope::{require_active_area, require_active_area_at},
};

const KIND: Kind = Kind::SetParent;
const ARCHIVED: &str = "Issue parent edits require active ancestry for every endpoint Area.";

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
    old_parent: Option<Grant>,
    parent: Option<Grant>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Revision {
    area_id: String,
    issue_id: String,
    issue_revision: String,
}

impl Revision {
    fn matches(&self, endpoint: &Endpoint) -> bool {
        self.area_id == endpoint.area_id
            && self.issue_id == endpoint.issue_id
            && is_uuid_v7(&self.issue_revision)
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ParentResult {
    version: u8,
    issue: Revision,
    old_parent: Option<Revision>,
    parent: Option<Revision>,
}

struct Relation<'a> {
    issue: &'a Endpoint,
    old_parent: Option<&'a Endpoint>,
    parent: Option<&'a Endpoint>,
}

impl Relation<'_> {
    fn valid(&self) -> bool {
        let (old_parent, parent) = self.parent_ids();
        old_parent != parent
            && old_parent != Some(&self.issue.issue_id)
            && parent != Some(&self.issue.issue_id)
    }

    fn endpoints(&self) -> impl Iterator<Item = &Endpoint> {
        [Some(self.issue), self.old_parent, self.parent]
            .into_iter()
            .flatten()
    }

    fn parent_ids(&self) -> (Option<&str>, Option<&str>) {
        (
            self.old_parent.map(|endpoint| endpoint.issue_id.as_str()),
            self.parent.map(|endpoint| endpoint.issue_id.as_str()),
        )
    }

    fn check_graph(&self, reader: &mut RecordReader<'_>) -> Result<(), HiveError> {
        let (old_parent, parent) = self.parent_ids();
        records::check_parent_edit(reader, &self.issue.issue_id, old_parent, parent)
    }
}

struct Request<'a> {
    workspace: &'a str,
    request: &'a str,
    actor: &'a str,
    relation: Relation<'a>,
}

impl Request<'_> {
    fn fingerprint(&self) -> String {
        digest(
            json!([
                1,
                KIND.name(),
                self.workspace,
                self.actor,
                self.relation.issue.canonical(),
                self.relation.old_parent.map(Endpoint::canonical),
                self.relation.parent.map(Endpoint::canonical),
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
        let allows = |grant: Option<&Grant>, endpoint: Option<&Endpoint>| match (grant, endpoint) {
            (None, None) => true,
            (Some(grant), Some(endpoint)) => grant.allows(endpoint, now),
            _ => false,
        };
        if evidence.version != 1
            || evidence.operation != KIND.name()
            || evidence.workspace_id != self.workspace
            || evidence.request_id != self.request
            || evidence.actor_ref != self.actor
            || evidence.fingerprint != fingerprint
            || !workflow::valid_ref(&evidence.authorization_ref)
            || !self.relation.valid()
            || !evidence.issue.allows(self.relation.issue, now)
            || !allows(evidence.old_parent.as_ref(), self.relation.old_parent)
            || !allows(evidence.parent.as_ref(), self.relation.parent)
        {
            return Err(workflow::unverified());
        }
        Ok(())
    }
}

struct SetParent<'a> {
    request: Request<'a>,
    fingerprint: String,
    decision: Decision<Evidence>,
}

pub(super) fn write(
    common: &CommonOptions,
    args: &IssueParentArgs,
) -> Result<DoltSuccess, HiveError> {
    let endpoint = |issue_id: &String, area_id: &String| Endpoint {
        area_id: area_id.clone(),
        issue_id: issue_id.clone(),
        expected_issue_revision: String::new(),
    };
    let mut issue = endpoint(&args.issue, &args.area);
    let mut old_parent = args
        .old_parent
        .as_ref()
        .zip(args.old_parent_area.as_ref())
        .map(|(id, area)| endpoint(id, area));
    let mut parent = if args.clear {
        None
    } else {
        args.parent
            .as_ref()
            .zip(args.parent_area.as_ref())
            .map(|(id, area)| endpoint(id, area))
    };
    bind_revisions(
        &args.if_match,
        [&mut issue]
            .into_iter()
            .chain(old_parent.as_mut())
            .chain(parent.as_mut()),
    )?;
    let relation = Relation {
        issue: &issue,
        old_parent: old_parent.as_ref(),
        parent: parent.as_ref(),
    };
    if !relation.valid() {
        return Err(invalid(
            "Select distinct child, old parent and new parent Issues.",
        ));
    }
    if !is_uuid(&args.request_id) || !workflow::valid_ref(&args.actor) {
        return Err(invalid(
            "Supply a request UUID and a bounded actor reference.",
        ));
    }
    let workspace =
        config_v2::read_explicit(common.workspace.as_deref().expect("validated workspace"))?;
    let target = DoltTarget::parse(&args.target)?;
    let request = Request {
        workspace: workspace.workspace_id(),
        request: &args.request_id,
        actor: &args.actor,
        relation,
    };
    let mutation = SetParent {
        fingerprint: request.fingerprint(),
        decision: issue_writer::read_decision(&args.evidence_file)?,
        request,
    };
    issue_writer::execute(&workspace, target, &mutation)
}

impl Mutation for SetParent<'_> {
    type Guard = ();

    fn identity(&self) -> WriteIdentity<'_> {
        WriteIdentity {
            kind: KIND,
            request_id: self.request.request,
            area_id: Some(&self.request.relation.issue.area_id),
            record_id: Some(&self.request.relation.issue.issue_id),
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
        issue_writer::encode(&self.decision.evidence, KIND)
    }

    fn check_scope(&self, snapshot: &mut Snapshot) -> Result<(), HiveError> {
        let relation = &self.request.relation;
        for endpoint in relation.endpoints() {
            endpoint.check_scope(snapshot)?;
            require_active_area_at(
                &mut snapshot.connection,
                &endpoint.area_id,
                &snapshot.commit,
                ARCHIVED,
            )?;
        }
        Ok(())
    }

    fn guard(&self, db: &mut Transaction<'_>, snapshot: &mut Snapshot) -> Result<(), HiveError> {
        let relation = &self.request.relation;
        for endpoint in relation.endpoints() {
            require_active_area(db, &endpoint.area_id, ARCHIVED)?;
            check_endpoint(db, endpoint, None)?;
        }
        relation.check_graph(&mut RecordReader::Online(snapshot))
    }

    fn stage(
        &self,
        db: &mut Transaction<'_>,
        (): (),
        now: OffsetDateTime,
        _sql_time: &str,
    ) -> Result<StagedMutation, HiveError> {
        let relation = &self.request.relation;
        let (old_parent, parent) = relation.parent_ids();
        let mut advance = |endpoint: &Endpoint| {
            let issue_revision = new_uuid_v7(now)?;
            if issue_revision == endpoint.expected_issue_revision {
                return Err(stale());
            }
            if endpoint == relation.issue {
                db.exec_drop(
                    "UPDATE issues SET parent_id = ?, revision = ? WHERE CAST(id AS BINARY) = ?
                     AND CAST(area_id AS BINARY) = ? AND CAST(revision AS BINARY) = ?
                     AND CAST(parent_id AS BINARY) <=> ?",
                    (
                        parent,
                        &issue_revision,
                        &endpoint.issue_id,
                        &endpoint.area_id,
                        &endpoint.expected_issue_revision,
                        old_parent,
                    ),
                )
            } else {
                db.exec_drop(
                    "UPDATE issues SET revision = ? WHERE CAST(id AS BINARY) = ?
                     AND CAST(area_id AS BINARY) = ? AND CAST(revision AS BINARY) = ?",
                    (
                        &issue_revision,
                        &endpoint.issue_id,
                        &endpoint.area_id,
                        &endpoint.expected_issue_revision,
                    ),
                )
            }
            .map_err(read_error)?;
            if db.affected_rows() != 1 {
                return Err(stale());
            }
            Ok(Revision {
                area_id: endpoint.area_id.clone(),
                issue_id: endpoint.issue_id.clone(),
                issue_revision,
            })
        };
        let result = ParentResult {
            version: 1,
            issue: advance(relation.issue)?,
            old_parent: relation.old_parent.map(&mut advance).transpose()?,
            parent: relation.parent.map(advance).transpose()?,
        };
        Ok(StagedMutation {
            result: issue_writer::encode(&result, KIND)?,
            record_id: result.issue.issue_id,
        })
    }
}

fn check_endpoint(
    db: &mut impl Queryable,
    endpoint: &Endpoint,
    commit: Option<&str>,
) -> Result<Issue, HiveError> {
    let row = Issue::read(db, endpoint, commit)?;
    if row.revision != endpoint.expected_issue_revision {
        return Err(stale());
    }
    row.validate_state(db, endpoint, commit)?;
    Ok(row)
}

impl Evidence {
    fn relation(&self) -> Relation<'_> {
        Relation {
            issue: &self.issue.endpoint,
            old_parent: self.old_parent.as_ref().map(|grant| &grant.endpoint),
            parent: self.parent.as_ref().map(|grant| &grant.endpoint),
        }
    }
}

pub(in crate::dolt) struct ScopedParent {
    evidence: Evidence,
    result: ParentResult,
}

pub(in crate::dolt) fn check_receipt(
    snapshot: &Snapshot,
    request_id: &str,
    area: &str,
    issue: &str,
    actor: &str,
    receipt: &HistoricalReceipt,
) -> Result<ScopedParent, HiveError> {
    let evidence: Evidence =
        serde_json::from_str(&receipt.workflow_evidence).map_err(|_| workflow::unverified())?;
    let result: ParentResult =
        serde_json::from_str(&receipt.result).map_err(|_| workflow::unverified())?;
    let request = Request {
        workspace: &snapshot.workspace_id,
        request: request_id,
        actor,
        relation: evidence.relation(),
    };
    if receipt.kind != KIND.name()
        || receipt.actor_ref != actor
        || result.version != 1
        || result.issue.area_id != area
        || result.issue.issue_id != issue
        || !result.issue.matches(request.relation.issue)
        || receipt.authorization_ref != evidence.authorization_ref
        || receipt.fingerprint != request.fingerprint()
    {
        return Err(workflow::unverified());
    }
    for (revision, endpoint) in [
        (result.old_parent.as_ref(), request.relation.old_parent),
        (result.parent.as_ref(), request.relation.parent),
    ] {
        match (revision, endpoint) {
            (None, None) => (),
            (Some(revision), Some(endpoint)) if revision.matches(endpoint) => (),
            _ => return Err(workflow::unverified()),
        }
    }
    let created = workflow::parse_time(&receipt.created_at).ok_or_else(workflow::unverified)?;
    request.verify(&evidence, &receipt.fingerprint, created)?;
    Ok(ScopedParent { evidence, result })
}

impl ScopedParent {
    pub(in crate::dolt) fn verify_effect(
        &self,
        snapshot: &mut Snapshot,
        introduced: &str,
        receipt: &HistoricalReceipt,
    ) -> Result<Value, HiveError> {
        let base = &receipt.base_commit;
        let db = &mut snapshot.connection;
        let relation = self.evidence.relation();
        relation.check_graph(&mut RecordReader::Historical {
            connection: db,
            commit: base,
        })?;
        let mut count = 0;
        for (endpoint, result) in [
            Some((relation.issue, &self.result.issue)),
            relation.old_parent.zip(self.result.old_parent.as_ref()),
            relation.parent.zip(self.result.parent.as_ref()),
        ]
        .into_iter()
        .flatten()
        {
            require_active_area_at(db, &endpoint.area_id, base, ARCHIVED)?;
            let before = check_endpoint(db, endpoint, Some(base))?;
            let after = Issue::read(db, endpoint, Some(introduced))?;
            after.verify_revision(&before, &result.issue_revision, &before.updated_at)?;
            let expected_parent = if endpoint == relation.issue {
                relation.parent_ids().1
            } else {
                before.parent.as_deref()
            };
            if after.parent.as_deref() != expected_parent {
                return Err(workflow::unverified());
            }
            count += 1;
        }
        verify_delta(
            db,
            base,
            introduced,
            &[("issues", 0, 0, count), ("operation_receipts", 1, 0, 0)],
        )?;
        Ok(json!(self.result))
    }
}
