use std::collections::{BTreeMap, HashMap};
use std::io::Write;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

use super::{Installed, Path, io_error, new_uuid_v7, sql_text, unverified, verify_rows};
use crate::config_git::migration::PreparedProjects;
use crate::error::HiveError;

const COUNTS: &str = "SELECT (SELECT COUNT(*) FROM projects) AS projects, \
    (SELECT COUNT(*) FROM project_issues) AS project_issues";

pub(super) type IssueIds<'a> = HashMap<(&'a str, &'a str), &'a str>;

pub(super) struct StagedProjects {
    projects: BTreeMap<String, Value>,
    links: Vec<Value>,
}

impl StagedProjects {
    pub(super) fn append(
        input: &mut impl Write,
        reviewed: Option<(&PreparedProjects<'_>, &IssueIds<'_>)>,
        now: OffsetDateTime,
    ) -> Result<Self, HiveError> {
        let mut projects = BTreeMap::new();
        let mut links = BTreeMap::new();
        if let Some((reviewed, ids)) = reviewed {
            for project in reviewed.projects() {
                let revision = new_uuid_v7(now)?;
                let description = project.description.as_deref();
                let description_sql = description.map_or_else(|| "NULL".to_owned(), sql_text);
                writeln!(
                    input,
                    "INSERT INTO projects (id, name, description, status, revision) VALUES ({}, {}, {description_sql}, {}, {});",
                    sql_text(&project.id),
                    sql_text(&project.name),
                    sql_text(project.status()),
                    sql_text(&revision),
                ).map_err(io_error)?;
                let mut row = json!({
                    "id":project.id, "name_sha256":digest(&project.name),
                    "status":project.status(), "revision":revision,
                });
                if let Some(description) = description {
                    row["description_sha256"] = json!(digest(description));
                }
                projects.insert(project.id.clone(), row);
            }

            // The combined transaction has already inserted both endpoints.
            for (project_id, key) in reviewed.memberships() {
                let issue_id = ids.get(&key).ok_or_else(|| {
                    unverified("A reviewed Project membership has no assigned Issue endpoint.")
                })?;
                writeln!(
                    input,
                    "INSERT INTO project_issues (project_id, issue_id) VALUES ({}, {});",
                    sql_text(project_id),
                    sql_text(issue_id),
                )
                .map_err(io_error)?;
                links.insert(
                    (project_id, *issue_id),
                    json!({"project_id":project_id, "issue_id":issue_id}),
                );
            }
        }
        Ok(Self {
            projects,
            links: links.into_values().collect(),
        })
    }

    /// Project INSERTs appended to the transaction before dependent rows.
    pub(super) fn inserted_project_ids(&self) -> impl ExactSizeIterator<Item = &str> {
        self.projects.keys().map(String::as_str)
    }

    pub(super) fn checks(&self, commit: Option<&str>) -> [(String, Vec<Value>); 2] {
        let snapshot = commit.map_or_else(String::new, |commit| format!(" AS OF '{commit}'"));
        [
            (
                format!(
                    "SELECT id, SHA2(name, 256) AS name_sha256, SHA2(description, 256) AS description_sha256, \
                 status, revision FROM projects{snapshot} ORDER BY id"
                ),
                self.projects.values().cloned().collect(),
            ),
            (
                format!(
                    "SELECT project_id, issue_id FROM project_issues{snapshot} ORDER BY project_id, issue_id"
                ),
                self.links.clone(),
            ),
        ]
    }

    pub(super) fn transaction_checks(&self) -> impl Iterator<Item = (String, Vec<Value>)> {
        // Dolt prints the same empty object for an empty SELECT and a write.
        // Count both tables even when there are no rows to compare in detail.
        let counts = json!({"projects":self.inserted_project_ids().len(), "project_issues":self.links.len()});
        std::iter::once((COUNTS.to_owned(), vec![counts])).chain(
            self.checks(None)
                .into_iter()
                .filter(|(_, expected)| !expected.is_empty()),
        )
    }

    pub(super) fn verify_rollback(installed: &Installed<'_>, db: &Path) -> Result<(), HiveError> {
        verify_rows(
            &installed.query(db, COUNTS)?,
            &[json!({"projects":0, "project_issues":0})],
        )
    }

    pub(super) fn add_report(
        &self,
        report: &mut Value,
        reviewed: &PreparedProjects<'_>,
        ids: &IssueIds<'_>,
        dry_run: bool,
    ) {
        let mut section = reviewed.report(Some(ids));
        let counts = section
            .as_object_mut()
            .expect("Project report object")
            .remove("counts")
            .expect("Project counts");
        report["coverage"] = json!("scopes_and_projects");
        report["counts"]["projects"] = counts["projects"].clone();
        report["counts"]["project_issues"] = counts["project_issues"].clone();
        report["project_membership"] = section;
        report["checks"]["target_project_rows"] = json!(if dry_run {
            0
        } else {
            self.inserted_project_ids().len()
        });
        report["checks"]["target_project_issue_rows"] =
            json!(if dry_run { 0 } else { self.links.len() });
    }
}

fn digest(value: &str) -> String {
    format!("{:x}", Sha256::digest(value))
}
