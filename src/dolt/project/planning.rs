//! Bounded Project planning over the shared owned-runtime writer.

mod history;

use mysql::{Transaction, prelude::Queryable};
use serde::{Deserialize, Serialize};
use serde_json::json;
use time::OffsetDateTime;

use crate::cli::{CommonOptions, ProjectWrite};
use crate::dolt::{
    DoltSuccess, DoltTarget, Snapshot, context_workflow as workflow, is_uuid, is_uuid_v7,
    issue_writer::{
        self, Decision, Kind, Mutation, StagedMutation, WriteIdentity, digest, invalid,
        new_uuid_v7, valid_revision, valid_title,
    },
    read_error,
    scope::require_active_area,
};
use crate::error::{ErrorCode, HiveError};

pub(in crate::dolt) use history::{ScopedProject, check_scope, verify_effect};

pub(in crate::dolt) const CREATE: &str = "project.create";
pub(in crate::dolt) const ADD_ISSUE: &str = "project.add-issue";
const ARCHIVED_AREA: &str = "Project membership requires active ancestry for the Issue's Area.";

#[derive(Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Membership {
    project_id: String,
    issue_id: String,
    area_id: String,
    expected_project_revision: String,
    expected_issue_revision: String,
}

impl Membership {
    fn valid(&self) -> bool {
        is_uuid_v7(&self.project_id)
            && is_uuid_v7(&self.issue_id)
            && self.project_id != self.issue_id
            && is_uuid_v7(&self.area_id)
            && valid_revision(&self.expected_project_revision)
            && valid_revision(&self.expected_issue_revision)
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Evidence {
    version: u8,
    operation: String,
    workspace_id: String,
    request_id: String,
    fingerprint: String,
    actor_ref: String,
    bot_id: Option<String>,
    membership: Option<Membership>,
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
    fn verifies(
        &self,
        workspace: &str,
        actor: &str,
        request: &str,
        fingerprint: &str,
        now: OffsetDateTime,
    ) -> bool {
        self.version == 1
            && self.workspace_id == workspace
            && self.actor_ref == actor
            && self.request_id == request
            && self.fingerprint == fingerprint
            && self.bot_id.is_none()
            && !self.withdrawn
            && [
                &self.authorization_ref,
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

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ResultRow {
    version: u8,
    project_id: String,
    project_revision: String,
    membership: Option<MemberResult>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct MemberResult {
    area_id: String,
    issue_id: String,
    issue_revision: String,
}

enum Intent<'a> {
    Create(&'a str),
    AddIssue(Membership),
}

impl Intent<'_> {
    fn kind(&self) -> Kind {
        match self {
            Self::Create(_) => Kind::ProjectCreate,
            Self::AddIssue(_) => Kind::ProjectAddIssue,
        }
    }

    fn fingerprint(&self, workspace: &str, actor: &str) -> String {
        let payload = match self {
            Self::Create(name) => json!([1, CREATE, workspace, actor, name]),
            Self::AddIssue(scope) => json!([
                1,
                ADD_ISSUE,
                workspace,
                actor,
                scope.project_id,
                scope.issue_id,
                scope.area_id,
                scope.expected_project_revision,
                scope.expected_issue_revision,
            ]),
        };
        digest(payload.to_string().as_bytes())
    }

    fn membership(&self) -> Option<&Membership> {
        match self {
            Self::Create(_) => None,
            Self::AddIssue(scope) => Some(scope),
        }
    }
}

struct Planning<'a> {
    workspace: &'a str,
    actor: &'a str,
    request: &'a str,
    fingerprint: String,
    intent: Intent<'a>,
    decision: Decision<Evidence>,
}

pub(super) fn write(
    common: &CommonOptions,
    command: &ProjectWrite,
) -> Result<DoltSuccess, HiveError> {
    let args = command.request();
    let target = DoltTarget::parse(&args.target)?;
    let workspace =
        crate::config_v2::read_explicit(common.workspace.as_deref().expect("validated workspace"))?;
    if !is_uuid(&args.request_id) || !workflow::valid_ref(&args.actor) {
        return Err(invalid("Supply a request UUID and an actor reference."));
    }
    let intent = match command {
        ProjectWrite::Create(args) => {
            if !valid_title(&args.name) {
                return Err(invalid(
                    "Supply a nonblank Project name of up to 255 characters without control characters.",
                ));
            }
            Intent::Create(&args.name)
        }
        ProjectWrite::AddIssue(args) => {
            let revision_error = || {
                invalid("Supply exactly one --if-match PROJECT=REV and one --if-match ISSUE=REV.")
            };
            let [first, second] = args.if_match.as_slice() else {
                return Err(revision_error());
            };
            let first = first.split_once('=').ok_or_else(revision_error)?;
            let second = second.split_once('=').ok_or_else(revision_error)?;
            let (project, issue) = if first.0 == args.project_id {
                (first, second)
            } else {
                (second, first)
            };
            if project.0 != args.project_id || issue.0 != args.issue_id {
                return Err(revision_error());
            }
            let scope = Membership {
                project_id: args.project_id.clone(),
                issue_id: args.issue_id.clone(),
                area_id: args.area.clone(),
                expected_project_revision: project.1.to_owned(),
                expected_issue_revision: issue.1.to_owned(),
            };
            if !scope.valid() {
                return Err(invalid(
                    "Supply distinct exact Project and Issue UUIDv7s, an owning Area UUIDv7, and valid revisions.",
                ));
            }
            Intent::AddIssue(scope)
        }
    };
    let workspace_id = workspace.workspace_id();
    let mutation = Planning {
        workspace: workspace_id,
        actor: &args.actor,
        request: &args.request_id,
        fingerprint: intent.fingerprint(workspace_id, &args.actor),
        intent,
        decision: issue_writer::read_decision(&args.evidence_file)?,
    };
    issue_writer::execute(&workspace, target, &mutation)
}

impl Mutation for Planning<'_> {
    type Guard = ();

    fn identity(&self) -> WriteIdentity<'_> {
        let scope = self.intent.membership();
        WriteIdentity {
            kind: self.intent.kind(),
            request_id: self.request,
            area_id: scope.map(|scope| scope.area_id.as_str()),
            record_id: scope.map(|scope| scope.project_id.as_str()),
            actor: self.actor,
            fingerprint: &self.fingerprint,
            authorization_ref: &self.decision.evidence.authorization_ref,
        }
    }

    fn verify_decision(&self, now: OffsetDateTime) -> Result<(), HiveError> {
        let evidence = &self.decision.evidence;
        if self.decision.decision == "allow"
            && evidence.operation == self.intent.kind().name()
            && evidence.membership.as_ref() == self.intent.membership()
            && evidence.verifies(
                self.workspace,
                self.actor,
                self.request,
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
        issue_writer::encode(&self.decision.evidence, self.intent.kind())
    }

    fn check_scope(&self, snapshot: &mut Snapshot) -> Result<(), HiveError> {
        if let Some(scope) = self.intent.membership() {
            let owners: Vec<String> = snapshot.exec_rows(
                |commit| format!(
                    "SELECT i.area_id FROM issues AS OF '{commit}' i
                     JOIN areas AS OF '{commit}' a ON CAST(a.id AS BINARY) = CAST(i.area_id AS BINARY)
                     WHERE CAST(i.id AS BINARY) = ? LIMIT 2"
                ),
                (&scope.issue_id,),
            )?;
            if owners != [scope.area_id.as_str()] {
                return Err(workflow::unverified());
            }
            let projects: Vec<String> = snapshot.exec_rows(
                |commit| format!(
                    "SELECT id FROM projects AS OF '{commit}' WHERE CAST(id AS BINARY) = ? LIMIT 1"
                ),
                (&scope.project_id,),
            )?;
            if projects.is_empty() {
                return Err(HiveError::new(
                    ErrorCode::NotFound,
                    "The selected Project was not found.",
                ));
            }
        }
        Ok(())
    }

    fn guard(&self, db: &mut Transaction<'_>, _: &mut Snapshot) -> Result<(), HiveError> {
        let Some(scope) = self.intent.membership() else {
            return Ok(());
        };
        require_active_area(db, &scope.area_id, ARCHIVED_AREA)?;
        let issue: Option<(String, String)> = db
            .exec_first(
                "SELECT area_id, revision FROM issues WHERE CAST(id AS BINARY) = ?",
                (&scope.issue_id,),
            )
            .map_err(read_error)?;
        let project: Option<(String, String)> = db
            .exec_first(
                "SELECT status, revision FROM projects WHERE CAST(id AS BINARY) = ?",
                (&scope.project_id,),
            )
            .map_err(read_error)?;
        let (Some((owner, issue_revision)), Some((status, project_revision))) = (issue, project)
        else {
            return Err(workflow::unverified());
        };
        if owner != scope.area_id {
            return Err(workflow::unverified());
        }
        if issue_revision != scope.expected_issue_revision
            || project_revision != scope.expected_project_revision
        {
            return Err(stale());
        }
        if !super::is_project_status(&status) {
            return Err(HiveError::new(
                ErrorCode::InvalidRecord,
                "The selected Project status is invalid.",
            ));
        }
        if status != "active" {
            return Err(HiveError::new(
                ErrorCode::InvalidTransition,
                "Only an active Project accepts new members.",
            ));
        }
        let existing: Option<u8> = db
            .exec_first(
                "SELECT 1 FROM project_issues WHERE project_id = ? AND issue_id = ? LIMIT 1",
                (&scope.project_id, &scope.issue_id),
            )
            .map_err(read_error)?;
        if existing.is_some() {
            return Err(HiveError::new(
                ErrorCode::AlreadyExists,
                "The Issue is already a direct member of the Project.",
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
        let project_revision = new_uuid_v7(now)?;
        let (project_id, membership) = match &self.intent {
            Intent::Create(name) => {
                let project_id = new_uuid_v7(now)?;
                db.exec_drop(
                    "INSERT INTO projects (id, name, status, revision) VALUES (?, ?, 'active', ?)",
                    (&project_id, name, &project_revision),
                )
                .map_err(read_error)?;
                (project_id, None)
            }
            Intent::AddIssue(scope) => {
                let issue_revision = new_uuid_v7(now)?;
                db.exec_drop(
                    "UPDATE projects SET revision = ?
                     WHERE CAST(id AS BINARY) = ? AND CAST(revision AS BINARY) = ?",
                    (
                        &project_revision,
                        &scope.project_id,
                        &scope.expected_project_revision,
                    ),
                )
                .map_err(read_error)?;
                if db.affected_rows() != 1 {
                    return Err(stale());
                }
                db.exec_drop(
                    "UPDATE issues SET revision = ?
                     WHERE CAST(id AS BINARY) = ? AND CAST(area_id AS BINARY) = ? AND CAST(revision AS BINARY) = ?",
                    (&issue_revision, &scope.issue_id, &scope.area_id, &scope.expected_issue_revision),
                ).map_err(read_error)?;
                if db.affected_rows() != 1 {
                    return Err(stale());
                }
                db.exec_drop(
                    "INSERT INTO project_issues (project_id, issue_id) VALUES (?, ?)",
                    (&scope.project_id, &scope.issue_id),
                )
                .map_err(read_error)?;
                (
                    scope.project_id.clone(),
                    Some(MemberResult {
                        area_id: scope.area_id.clone(),
                        issue_id: scope.issue_id.clone(),
                        issue_revision,
                    }),
                )
            }
        };
        let result = ResultRow {
            version: 1,
            project_id,
            project_revision,
            membership,
        };
        Ok(StagedMutation {
            record_id: result.project_id.clone(),
            result: issue_writer::encode(&result, self.intent.kind())?,
        })
    }
}

fn stale() -> HiveError {
    HiveError::new(
        ErrorCode::StaleRevision,
        "The selected Project or Issue revision changed.",
    )
}
