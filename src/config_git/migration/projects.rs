use std::collections::{HashMap, HashSet, hash_map::Entry};
use std::hash::Hash;

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::is_full_lowercase_oid;
use super::json_object;
use super::scope_plan::is_portable_scope;
use super::task_intake::{TaskPlan, TaskSource, TaskState, task_key_digest};
use super::task_provenance::{Key, LegacyKey, fits_text_column};
use crate::cli::is_slug;
use crate::dolt::is_uuid_v7;
use crate::error::{ErrorCode, HiveError};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Mapping {
    #[serde(deserialize_with = "json_object::deserialize_vec")]
    projects: Vec<Project>,
    #[serde(deserialize_with = "json_object::deserialize_vec")]
    memberships: Vec<Membership>,
}

pub(super) fn deserialize_section<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Mapping>, D::Error> {
    json_object::deserialize(deserializer).map(Some)
}

#[derive(Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Project {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) description: Option<String>,
    status: ProjectStatus,
}

#[derive(Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ProjectStatus {
    Active,
    Completed,
    Archived,
}

impl Project {
    pub(crate) fn status(&self) -> &'static str {
        match self.status {
            ProjectStatus::Active => "active",
            ProjectStatus::Completed => "completed",
            ProjectStatus::Archived => "archived",
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Membership {
    project_id: String,
    #[serde(deserialize_with = "json_object::deserialize")]
    legacy_key: LegacyKey,
    source_git_blob_sha: String,
    decision: Decision,
}

#[derive(Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Decision {
    Link,
    Unresolved,
    Exclude,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum Hold {
    DuplicateProjectDefinition,
    ConflictingProjectDefinition,
    DuplicateProjectMembership,
    ConflictingProjectMembership,
    UnresolvedProjectMembership,
    ExcludedProjectMembership,
    AmbiguousProjectMemberTask,
    ProjectMembershipBindingMismatch,
}

pub(crate) struct PreparedProjects<'a> {
    projects: Vec<(&'a Project, Option<Hold>)>,
    memberships: Vec<(&'a Membership, Option<Hold>)>,
}

impl Mapping {
    pub(super) fn prepare<'a>(
        &'a self,
        tasks: Option<&TaskPlan<'_>>,
    ) -> Result<PreparedProjects<'a>, HiveError> {
        for project in &self.projects {
            if !is_uuid_v7(&project.id)
                || project.name.trim().is_empty()
                || project.name.trim() != project.name
                || project.name.chars().count() > 255
                || project.name.chars().any(char::is_control)
                || project
                    .description
                    .as_deref()
                    .is_some_and(|text| text.trim().is_empty() || !fits_text_column(text))
            {
                return Err(HiveError::new(
                    ErrorCode::InvalidConfig,
                    "A Project definition has an invalid ID, name, or description.",
                ));
            }
        }
        for member in &self.memberships {
            let (scope, work_id) = member.legacy_key.key();
            if !is_uuid_v7(&member.project_id)
                || !is_portable_scope(scope)
                || !is_slug(work_id)
                || !is_full_lowercase_oid(&member.source_git_blob_sha)
            {
                return Err(HiveError::new(
                    ErrorCode::InvalidConfig,
                    "A Project membership has an invalid Project ID, Task key, or blob ID.",
                ));
            }
        }

        let project_duplicates = repeated(self.projects.iter().map(|row| (row.id.as_str(), row)));
        let projects: Vec<_> = self
            .projects
            .iter()
            .map(|project| {
                let hold = project_duplicates
                    .get(project.id.as_str())
                    .map(|conflicting| {
                        if *conflicting {
                            Hold::ConflictingProjectDefinition
                        } else {
                            Hold::DuplicateProjectDefinition
                        }
                    });
                (project, hold)
            })
            .collect();
        let project_ids: HashSet<_> = projects
            .iter()
            .filter(|(_, hold)| hold.is_none())
            .map(|(project, _)| project.id.as_str())
            .collect();

        let membership_duplicates = repeated(self.memberships.iter().map(|row| {
            (
                (row.project_id.as_str(), row.legacy_key.key()),
                (row.source_git_blob_sha.as_str(), row.decision),
            )
        }));
        let memberships = self
            .memberships
            .iter()
            .map(|member| {
                let key = (member.project_id.as_str(), member.legacy_key.key());
                let hold = match membership_duplicates.get(&key) {
                    Some(true) => Some(Hold::ConflictingProjectMembership),
                    Some(false) => Some(Hold::DuplicateProjectMembership),
                    None => member.classify(tasks, project_ids.contains(key.0)).err(),
                };
                (member, hold)
            })
            .collect();

        Ok(PreparedProjects {
            projects,
            memberships,
        })
    }
}

impl Membership {
    fn classify(&self, tasks: Option<&TaskPlan<'_>>, project_prepared: bool) -> Result<(), Hold> {
        let source = tasks
            .and_then(|tasks| tasks.member_source(self.legacy_key.key()))
            .ok_or(Hold::UnresolvedProjectMembership)?;
        let (oid, state) = match source {
            TaskSource::Duplicate => return Err(Hold::AmbiguousProjectMemberTask),
            TaskSource::Unique { oid, state } => (oid, state),
        };
        let oid = oid.ok_or(Hold::UnresolvedProjectMembership)?;
        if oid != self.source_git_blob_sha {
            return Err(Hold::ProjectMembershipBindingMismatch);
        }
        if matches!(state, TaskState::Excluded) || self.decision == Decision::Exclude {
            return Err(Hold::ExcludedProjectMembership);
        }
        if !project_prepared
            || !matches!(state, TaskState::Prepared)
            || self.decision == Decision::Unresolved
        {
            return Err(Hold::UnresolvedProjectMembership);
        }
        Ok(())
    }
}

/// Record every repeated key and whether its declarations disagree.
fn repeated<K: Copy + Eq + Hash, T: Eq>(rows: impl Iterator<Item = (K, T)>) -> HashMap<K, bool> {
    let mut first = HashMap::new();
    let mut repeats = HashMap::new();
    for (key, row) in rows {
        match first.entry(key) {
            Entry::Vacant(entry) => {
                entry.insert(row);
            }
            Entry::Occupied(entry) => {
                *repeats.entry(key).or_default() |= entry.get() != &row;
            }
        }
    }
    repeats
}

impl PreparedProjects<'_> {
    pub(crate) fn projects(&self) -> impl Iterator<Item = &Project> {
        self.projects
            .iter()
            .filter_map(|(project, hold)| hold.is_none().then_some(*project))
    }

    /// Canonical Project IDs available to other reviewed staging surfaces.
    pub(crate) fn project_ids(&self) -> impl Iterator<Item = &str> {
        self.projects().map(|project| project.id.as_str())
    }

    pub(crate) fn memberships(&self) -> impl Iterator<Item = (&str, Key<'_>)> {
        self.memberships.iter().filter_map(|(row, hold)| {
            hold.is_none()
                .then_some((row.project_id.as_str(), row.legacy_key.key()))
        })
    }

    pub(crate) fn report(&self, ids: Option<&HashMap<Key<'_>, &str>>) -> Value {
        let label = if ids.is_some() {
            "imported"
        } else {
            "prepared"
        };
        let projects: Vec<_> = self.projects.iter().map(|(project, hold)| match hold {
            Some(reason) => json!({"project_id":project.id, "status":"held", "reason_code":reason}),
            None => json!({
                "project_id":project.id, "status":label, "project_status":project.status(),
                "name_sha256":format!("{:x}", Sha256::digest(&project.name)),
                "description_sha256":project.description.as_ref().map(|text| format!("{:x}", Sha256::digest(text))),
            }),
        }).collect();
        let memberships: Vec<_> = self
            .memberships
            .iter()
            .map(|(member, hold)| {
                let key = member.legacy_key.key();
                let mut row = json!({
                    "project_id":member.project_id, "legacy_key_sha256":task_key_digest(key),
                    "source_git_blob_sha":member.source_git_blob_sha,
                });
                if let Some(reason) = hold {
                    row["status"] = json!("held");
                    row["reason_code"] = json!(reason);
                } else {
                    row["status"] = json!(label);
                    if let Some(ids) = ids {
                        row["issue_id"] = json!(ids[&key]);
                    }
                }
                row
            })
            .collect();
        let counts = |total: usize, positive| {
            json!({
                "inventoried":total, (label):positive, "held":total - positive, "excluded_non_record":0,
            })
        };
        json!({
            "counts": {
                "projects": counts(projects.len(), self.project_ids().count()),
                "project_issues": counts(memberships.len(), self.memberships().count()),
            },
            "projects":projects, "memberships":memberships,
        })
    }
}
