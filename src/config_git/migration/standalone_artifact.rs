use std::collections::{BTreeSet, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::Reason;
use super::json_object;
use super::reviewed_records::{AbsenceReview, sha256};
use super::scope_plan::{PreparedResources, ScopeOutcome};
use super::task_intake::{StagingTask, task_key_digest};
use super::task_provenance::LegacyKey;
use super::work_artifact::TaskContext;

pub(super) struct JoinContext<'a> {
    pub(super) tasks: TaskContext<'a>,
    areas: HashSet<&'a str>,
    projects: HashSet<&'a str>,
    resources: HashSet<&'a str>,
}

impl<'a> JoinContext<'a> {
    pub(super) fn new(
        workspace_id: &'a str,
        tasks: Option<&'a [StagingTask<'a>]>,
        scopes: impl Iterator<Item = &'a ScopeOutcome<'a>>,
        project_ids: impl Iterator<Item = &'a str>,
        resources: Option<&'a PreparedResources<'a>>,
    ) -> Self {
        Self {
            tasks: TaskContext::new(workspace_id, tasks),
            areas: scopes
                .filter_map(|scope| match scope {
                    ScopeOutcome::Mapped { area, .. } => Some(area.id.as_str()),
                    _ => None,
                })
                .collect(),
            projects: project_ids.collect(),
            resources: resources
                .into_iter()
                .flat_map(PreparedResources::outcomes)
                .filter_map(|(id, outcome)| outcome.ok().map(|_| id))
                .collect(),
        }
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
pub(super) enum LinkReview {
    Absence(#[serde(deserialize_with = "json_object::string_enum")] AbsenceReview),
    Explicit(#[serde(deserialize_with = "json_object::deserialize")] Targets),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Targets {
    #[serde(default)]
    area_ids: Vec<String>,
    #[serde(default)]
    project_ids: Vec<String>,
    #[serde(default, deserialize_with = "json_object::deserialize_vec")]
    issues: Vec<ReviewedIssue>,
    #[serde(default)]
    resource_ids: Vec<String>,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReviewedIssue {
    #[serde(deserialize_with = "json_object::deserialize")]
    legacy_key: LegacyKey,
    pub(crate) area_id: String,
    pub(crate) source_path: String,
    pub(crate) source_git_blob_sha: String,
}

impl ReviewedIssue {
    pub(crate) fn key(&self) -> (&str, &str) {
        self.legacy_key.key()
    }
}

#[derive(Serialize)]
pub(crate) struct PreparedLinks {
    pub(crate) area_ids: Vec<String>,
    pub(crate) project_ids: Vec<String>,
    #[serde(skip)]
    pub(crate) issues: Vec<ReviewedIssue>,
    pub(crate) resource_ids: Vec<String>,
}

impl PreparedLinks {
    pub(super) fn evidence(&self) -> Value {
        let mut evidence = json!(self);
        evidence["issues"] = json!(
            self.issues
                .iter()
                .map(|issue| json!({
                    "legacy_key_sha256": task_key_digest(issue.key()),
                    "source_path_sha256": sha256(&issue.source_path),
                    "source_git_blob_sha": issue.source_git_blob_sha,
                    "area_id": issue.area_id,
                }))
                .collect::<Vec<_>>()
        );
        evidence
    }
}

impl LinkReview {
    pub(super) fn needs_tasks(&self) -> bool {
        matches!(self, Self::Explicit(targets) if !targets.issues.is_empty())
    }

    pub(super) fn prepare(
        &self,
        ctx: &JoinContext<'_>,
    ) -> Result<Option<Box<PreparedLinks>>, Reason> {
        let targets = match self {
            Self::Absence(AbsenceReview::None) => return Ok(None),
            Self::Absence(AbsenceReview::Unresolved) => {
                return Err(Reason::UnresolvedArtifactLinks);
            }
            Self::Explicit(targets) => targets,
        };
        let hold = Reason::UnresolvedArtifactLinks;
        let eligible = |ids: &[String], available: &HashSet<&str>| {
            let unique: BTreeSet<&str> = ids.iter().map(String::as_str).collect();
            (unique.len() == ids.len() && unique.iter().all(|id| available.contains(id)))
                .then(|| unique.into_iter().map(str::to_owned).collect::<Vec<_>>())
                .ok_or(hold)
        };
        let area_ids = eligible(&targets.area_ids, &ctx.areas)?;
        let project_ids = eligible(&targets.project_ids, &ctx.projects)?;
        let resource_ids = eligible(&targets.resource_ids, &ctx.resources)?;
        let mut issue_keys = HashSet::new();
        for issue in &targets.issues {
            if !issue_keys.insert(issue.key())
                || !ctx.tasks.task(issue.key()).is_some_and(|task| {
                    issue.area_id == task.area_id
                        && issue.source_path == task.source_path
                        && issue.source_git_blob_sha == task.source_git_blob_sha
                })
            {
                return Err(hold);
            }
        }
        if area_ids.is_empty()
            && project_ids.is_empty()
            && resource_ids.is_empty()
            && targets.issues.is_empty()
        {
            return Err(hold);
        }
        let mut issues = targets.issues.clone();
        issues.sort_unstable_by(|left, right| left.key().cmp(&right.key()));
        Ok(Some(Box::new(PreparedLinks {
            area_ids,
            project_ids,
            resource_ids,
            issues,
        })))
    }
}
