use std::collections::{HashMap, VecDeque};
use std::io::Write;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

use super::{io_error, sql_text, unverified};
use crate::config_git::migration::StagingTask;
use crate::config_git::migration::prepared_records::{IssueLink, ReviewedIssue};
use crate::dolt::issue_writer::new_uuid_v7;
use crate::error::{ErrorCode, HiveError};

pub(super) const COUNTER: &str = "SELECT name, next_value FROM workspace_counters ORDER BY name";

struct AssignedTask<'a> {
    task: StagingTask<'a>,
    number: usize,
    id: String,
    revision: String,
    parent: Option<usize>,
    prerequisites: Vec<usize>,
    scope_acceptance: Vec<usize>,
}

pub(super) struct TaskImport<'a> {
    tasks: Vec<AssignedTask<'a>>,
    indices: HashMap<(&'a str, &'a str), usize>,
    placement: Vec<usize>,
    rows: Vec<Value>,
    dependencies: Vec<Value>,
    dependency_count: [Value; 1],
}

impl<'a> TaskImport<'a> {
    pub(super) fn new(
        mut tasks: Vec<StagingTask<'a>>,
        now: OffsetDateTime,
    ) -> Result<Self, HiveError> {
        // A catalog may reference the same blob many times. Bound the expanded
        // body bytes written through the SQL client as well as the Git inventory.
        if tasks
            .iter()
            .map(|task| task.fields.body.len())
            .fold(0usize, usize::saturating_add)
            > 64 * 1024 * 1024
        {
            return Err(HiveError::new(
                ErrorCode::InvalidConfig,
                "Prepared Task bodies exceed the 64 MiB migration transaction limit.",
            ));
        }
        tasks.sort_unstable_by_key(|task| task.key);
        let indices: HashMap<_, _> = tasks
            .iter()
            .enumerate()
            .map(|(index, task)| (task.key, index))
            .collect();
        let endpoint = |key| {
            indices
                .get(&key)
                .copied()
                .ok_or_else(|| unverified("A prepared Task relationship has no eligible endpoint."))
        };
        let mut assigned = Vec::with_capacity(tasks.len());
        let mut children = vec![Vec::new(); tasks.len()];
        let mut pending = VecDeque::new();
        for (index, task) in tasks.into_iter().enumerate() {
            let parent = task.fields.parent.map(&endpoint).transpose()?;
            if let Some(parent) = parent {
                children[parent].push(index);
            } else {
                pending.push_back(index);
            }
            assigned.push(AssignedTask {
                parent,
                prerequisites: task
                    .fields
                    .prerequisites
                    .iter()
                    .copied()
                    .map(&endpoint)
                    .collect::<Result<_, _>>()?,
                scope_acceptance: task
                    .fields
                    .scope_acceptance
                    .iter()
                    .map(|proof| endpoint(proof.child))
                    .collect::<Result<_, _>>()?,
                task,
                number: index + 1,
                id: new_uuid_v7(now)?,
                revision: new_uuid_v7(now)?,
            });
        }
        let mut placement = Vec::with_capacity(assigned.len());
        while let Some(index) = pending.pop_front() {
            placement.push(index);
            pending.extend(children[index].iter().copied());
        }
        if placement.len() != assigned.len() {
            return Err(unverified(
                "Prepared Task parent placement contains a cycle.",
            ));
        }
        let rows = assigned
            .iter()
            .map(|task| task.expected_row(&assigned))
            .collect();
        let mut links: Vec<_> = assigned
            .iter()
            .flat_map(|task| {
                task.prerequisites
                    .iter()
                    .map(|&index| (task.id.as_str(), assigned[index].id.as_str()))
            })
            .collect();
        links.sort_unstable();
        let dependencies: Vec<_> = links
            .into_iter()
            .map(
                |(issue, prerequisite)| json!({"issue_id": issue, "prerequisite_id": prerequisite}),
            )
            .collect();
        let dependency_count = [json!({"dependencies": dependencies.len()})];
        Ok(Self {
            tasks: assigned,
            indices,
            placement,
            rows,
            dependencies,
            dependency_count,
        })
    }

    pub(super) fn issue_id(&self, link: &IssueLink) -> Option<&str> {
        self.verified_issue_id(
            link.key(),
            &link.area_id,
            &link.source_path,
            &link.source_git_blob_sha,
        )
    }

    pub(super) fn standalone_issue_id(&self, link: &ReviewedIssue) -> Option<&str> {
        self.verified_issue_id(
            link.key(),
            &link.area_id,
            &link.source_path,
            &link.source_git_blob_sha,
        )
    }

    fn verified_issue_id(
        &self,
        key: (&str, &str),
        area: &str,
        path: &str,
        blob: &str,
    ) -> Option<&str> {
        let task = self.assigned(key)?;
        (task.task.area_id == area
            && task.task.source_path == path
            && task.task.source_git_blob_sha == blob)
            .then_some(task.id.as_str())
    }

    pub(super) fn dependency_count(&self) -> usize {
        self.dependencies.len()
    }

    pub(super) fn len(&self) -> usize {
        self.tasks.len()
    }

    pub(super) fn append_sql(&self, input: &mut impl Write) -> Result<(), HiveError> {
        for &index in &self.placement {
            let task = &self.tasks[index];
            let fields = &task.task.fields;
            let parent = task.parent.map(|parent| self.tasks[parent].id.as_str());
            let (closed_at, actor, resolution) = fields.closure.as_ref().map_or(
                ("NULL".into(), "NULL".into(), "NULL".into()),
                |closure| {
                    (
                        sql_text(&sql_time(closure.closed_at)),
                        sql_text(closure.closed_by_ref),
                        sql_text(closure.resolution_note),
                    )
                },
            );
            let priority = fields
                .priority
                .map_or_else(|| "NULL".into(), |priority| priority.to_string());
            writeln!(
                input,
                "INSERT INTO issues (id, number, area_id, parent_id, title, body, acceptance_criteria, type, status, priority, revision, created_at, updated_at, closed_at, closed_by_ref, resolution_note) \
                 VALUES ({}, {}, {}, {}, {}, {}, {}, 'task', {}, {priority}, {}, {}, {}, {closed_at}, {actor}, {resolution});",
                sql_text(&task.id), task.number, sql_text(task.task.area_id), nullable_text(parent),
                sql_text(fields.title), sql_text(fields.body), nullable_text(fields.criteria), sql_text(task.task.status),
                sql_text(&task.revision), sql_text(&sql_time(fields.created_at)), sql_text(&sql_time(fields.updated_at)),
            ).map_err(io_error)?;
        }
        // Both endpoint IDs exist before putting any prerequisite into the graph.
        for task in &self.tasks {
            for &prerequisite in &task.prerequisites {
                writeln!(
                    input,
                    "INSERT INTO issue_dependencies (issue_id, prerequisite_id) VALUES ({}, {});",
                    sql_text(&task.id),
                    sql_text(&self.tasks[prerequisite].id),
                )
                .map_err(io_error)?;
            }
        }
        if !self.tasks.is_empty() {
            writeln!(input, "UPDATE workspace_counters SET next_value = {} WHERE name = 'issue_number' AND next_value = 1;", self.len() + 1)
                .map_err(io_error)?;
        }
        Ok(())
    }

    pub(super) fn checks(&self, commit: Option<&str>) -> Vec<(String, &[Value])> {
        let snapshot = commit.map_or_else(String::new, |hash| format!(" AS OF '{hash}'"));
        let mut checks = vec![(
            format!("SELECT COUNT(*) AS dependencies FROM issue_dependencies{snapshot}"),
            self.dependency_count.as_slice(),
        )];
        if !self.rows.is_empty() {
            checks.push((format!(
                "SELECT id, number, area_id, parent_id, status, type AS issue_type, priority, revision, \
                 CASE WHEN hold_reason IS NULL THEN 1 ELSE 0 END AS unheld, \
                 LOWER(SHA2(CAST(title AS BINARY), 256)) AS title_sha256, \
                 LOWER(SHA2(CAST(body AS BINARY), 256)) AS body_sha256, \
                 LOWER(SHA2(CAST(acceptance_criteria AS BINARY), 256)) AS criteria_sha256, \
                 DATE_FORMAT(created_at, '%Y-%m-%d %H:%i:%s.%f') AS created_at, \
                 DATE_FORMAT(updated_at, '%Y-%m-%d %H:%i:%s.%f') AS updated_at, \
                 DATE_FORMAT(closed_at, '%Y-%m-%d %H:%i:%s.%f') AS closed_at, \
                 LOWER(SHA2(CAST(closed_by_ref AS BINARY), 256)) AS actor_sha256, \
                 LOWER(SHA2(CAST(resolution_note AS BINARY), 256)) AS resolution_sha256 \
                 FROM issues{snapshot} ORDER BY number"
            ), self.rows.as_slice()));
        }
        if !self.dependencies.is_empty() {
            checks.push((format!("SELECT issue_id, prerequisite_id FROM issue_dependencies{snapshot} ORDER BY issue_id, prerequisite_id"), self.dependencies.as_slice()));
        }
        checks
    }

    pub(super) fn issue_id_by_key(&self, key: (&str, &str)) -> Option<&str> {
        self.assigned(key).map(|task| task.id.as_str())
    }

    fn assigned(&self, key: (&str, &str)) -> Option<&AssignedTask<'a>> {
        self.indices.get(&key).map(|&index| &self.tasks[index])
    }

    pub(super) fn id_map(&self) -> Vec<Value> {
        self.tasks
            .iter()
            .zip(&self.rows)
            .map(|(task, stored)| {
                let mut row = task.task.locator.clone();
                row["kind"] = json!("task");
                row["issue_id"] = json!(task.id);
                row["issue_number"] = json!(task.number);
                row["target_body_sha256"] = stored["body_sha256"].clone();
                row["target_status"] = json!(task.task.status);
                row["area_id"] = json!(task.task.area_id);
                if let Some(parent) = task.parent {
                    row["parent_issue_id"] = json!(self.tasks[parent].id);
                }
                row["prerequisite_issue_ids"] = json!(
                    task.prerequisites
                        .iter()
                        .map(|&index| self.tasks[index].id.as_str())
                        .collect::<Vec<_>>()
                );
                if !task.scope_acceptance.is_empty() {
                    row["cancelled_child_scope_acceptance"] = json!(
                        task.scope_acceptance
                            .iter()
                            .zip(&task.task.fields.scope_acceptance)
                            .map(|(&index, proof)| json!({
                                "child_issue_id": self.tasks[index].id, "sha256": proof.sha256,
                                "body_range": {"start": proof.start, "end": proof.end},
                            }))
                            .collect::<Vec<_>>()
                    );
                }
                row
            })
            .collect()
    }
}

impl AssignedTask<'_> {
    fn expected_row(&self, assigned: &[AssignedTask<'_>]) -> Value {
        let fields = &self.task.fields;
        let mut row = json!({
            "id": self.id, "number": self.number, "area_id": self.task.area_id,
            "status": self.task.status, "issue_type": "task", "revision": self.revision, "unheld": 1,
            "title_sha256": sha256(fields.title), "body_sha256": fields.body_sha256,
            "created_at": sql_time(fields.created_at), "updated_at": sql_time(fields.updated_at),
        });
        if let Some(parent) = self.parent {
            row["parent_id"] = json!(assigned[parent].id);
        }
        if let Some(priority) = fields.priority {
            row["priority"] = json!(priority);
        }
        if let Some(criteria) = fields.criteria {
            row["criteria_sha256"] = json!(sha256(criteria));
        }
        if let Some(closure) = &fields.closure {
            row["closed_at"] = json!(sql_time(closure.closed_at));
            row["actor_sha256"] = json!(sha256(closure.closed_by_ref));
            row["resolution_sha256"] = json!(sha256(closure.resolution_note));
        }
        row
    }
}

fn nullable_text(value: Option<&str>) -> String {
    value.map_or_else(|| "NULL".into(), sql_text)
}

fn sql_time(value: OffsetDateTime) -> String {
    format!(
        "{} {:02}:{:02}:{:02}.{:06}",
        value.date(),
        value.hour(),
        value.minute(),
        value.second(),
        value.nanosecond() / 1000
    )
}

fn sha256(value: &str) -> String {
    format!("{:x}", Sha256::digest(value))
}
