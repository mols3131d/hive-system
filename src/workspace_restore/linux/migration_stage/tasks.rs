use std::collections::{BTreeMap, HashMap, VecDeque, btree_map::Entry};
use std::io::Write;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

use super::{io_error, labels, sql_text, unverified};
use crate::config_git::migration::StagingTask;
use crate::config_git::migration::prepared_records::IssueLink;
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
    label_ids: Vec<String>,
}

struct Label<'a> {
    name: &'a str,
    id: String,
    revision: String,
}

pub(super) struct TaskImport<'a> {
    tasks: Vec<AssignedTask<'a>>,
    indices: HashMap<(&'a str, &'a str), usize>,
    placement: Vec<usize>,
    rows: Vec<Value>,
    dependencies: Vec<Value>,
    dependency_count: [Value; 1],
    labels: Vec<Label<'a>>,
    label_rows: Vec<Value>,
    label_links: Vec<Value>,
    label_counts: [Value; 1],
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
                label_ids: Vec::new(),
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
        let mut labels = BTreeMap::new();
        let mut label_links = Vec::new();
        for task in &mut assigned {
            for name in task.task.fields.labels.iter().map(String::as_str) {
                let label = match labels.entry(name) {
                    Entry::Occupied(entry) => entry.into_mut(),
                    Entry::Vacant(entry) => entry.insert(Label {
                        name,
                        id: new_uuid_v7(now)?,
                        revision: new_uuid_v7(now)?,
                    }),
                };
                task.label_ids.push(label.id.clone());
                label_links.push((task.id.as_str(), label.id.clone()));
            }
            task.label_ids.sort_unstable();
        }
        label_links.sort_unstable();
        let label_counts = [json!({"labels": labels.len(), "links": label_links.len()})];
        let label_links = label_links
            .into_iter()
            .map(|(issue, label)| json!({"issue_id": issue, "label_id": label}))
            .collect();
        let mut labels: Vec<_> = labels.into_values().collect();
        labels.sort_unstable_by(|left, right| left.id.cmp(&right.id));
        let label_rows = labels
            .iter()
            .map(|label| {
                json!({
                    "id": label.id, "name_sha256": sha256(label.name), "revision": label.revision,
                })
            })
            .collect();
        Ok(Self {
            tasks: assigned,
            indices,
            placement,
            rows,
            dependencies,
            dependency_count,
            labels,
            label_rows,
            label_links,
            label_counts,
        })
    }

    pub(super) fn issue_id(&self, link: &IssueLink) -> Option<&str> {
        let task = self.assigned(link.key())?;
        (task.task.area_id == link.area_id
            && task.task.source_path == link.source_path
            && task.task.source_git_blob_sha == link.source_git_blob_sha)
            .then_some(task.id.as_str())
    }

    pub(super) fn dependency_count(&self) -> usize {
        self.dependencies.len()
    }

    pub(super) fn len(&self) -> usize {
        self.tasks.len()
    }

    pub(super) fn append_sql(&self, input: &mut impl Write) -> Result<(), HiveError> {
        for label in &self.labels {
            writeln!(
                input,
                "INSERT INTO labels (id, name, revision) VALUES ({}, {}, {});",
                sql_text(&label.id),
                sql_text(label.name),
                sql_text(&label.revision),
            )
            .map_err(io_error)?;
        }
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
                 VALUES ({}, {}, {}, {}, {}, {}, {}, {}, {}, {priority}, {}, {}, {}, {closed_at}, {actor}, {resolution});",
                sql_text(&task.id), task.number, sql_text(task.task.area_id), nullable_text(parent),
                sql_text(fields.title), sql_text(fields.body), nullable_text(fields.criteria), sql_text(fields.issue_type), sql_text(task.task.status),
                sql_text(&task.revision), sql_text(&sql_time(fields.created_at)), sql_text(&sql_time(fields.updated_at)),
            ).map_err(io_error)?;
        }
        // Both endpoint IDs exist before putting any prerequisite into the graph.
        for task in &self.tasks {
            for label_id in &task.label_ids {
                writeln!(
                    input,
                    "INSERT INTO issue_labels (issue_id, label_id) VALUES ({}, {});",
                    sql_text(&task.id),
                    sql_text(label_id),
                )
                .map_err(io_error)?;
            }
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
        checks.push((labels::count_sql(&snapshot), &self.label_counts));
        if !self.label_rows.is_empty() {
            checks.push((format!(
                "SELECT id, LOWER(SHA2(CAST(name AS BINARY), 256)) AS name_sha256, revision, description FROM labels{snapshot} ORDER BY id"
            ), &self.label_rows));
            checks.push((
                format!(
                    "SELECT issue_id, label_id FROM issue_labels{snapshot} ORDER BY issue_id, label_id"
                ),
                &self.label_links,
            ));
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
                if let Some(fallback) = task.task.fields.todo_fallback {
                    row["todo_fallback"] = json!(fallback);
                }
                row["target_issue_type"] = stored["issue_type"].clone();
                row["target_title_sha256"] = stored["title_sha256"].clone();
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
                if !task.label_ids.is_empty() {
                    row["label_ids"] = json!(task.label_ids);
                }
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
            "status": self.task.status, "issue_type": fields.issue_type, "revision": self.revision, "unheld": 1,
            "title_sha256": fields.title_sha256, "body_sha256": fields.body_sha256,
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
