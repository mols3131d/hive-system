//! Optional metadata pages from the same committed context as the required bodies.

use serde_json::{Value, json};

use super::{REFERENCE_LIMIT, Scope, Snapshot, Work, load_updates};
use crate::dolt::{artifact, resource};
use crate::error::{ErrorCode, HiveError};

const GROUPS: [&str; 4] = ["prerequisites", "updates", "resources", "artifacts"];

pub(super) struct Page {
    pub(super) group: &'static str,
    pub(super) offset: u64,
    pub(super) items: Vec<Value>,
    pub(super) omitted: u64,
}

impl Page {
    pub(super) fn next_cursor(&self, binding: &Value) -> String {
        let key = resource::encode_cursor(
            self.offset + self.items.len() as u64,
            &json!([binding, self.group]),
        );
        format!("{}:{key}", self.group)
    }
}

pub(super) fn collect(
    snapshot: &mut Snapshot,
    scope: &Scope,
    work: Option<&Work>,
    cursor: Option<&str>,
    binding: &Value,
) -> Result<Vec<Page>, HiveError> {
    let position = cursor.map(|cursor| decode(cursor, binding)).transpose()?;
    let mut pages = Vec::new();
    for group in GROUPS {
        if position.is_some_and(|(selected, _)| selected != group) {
            continue;
        }
        let offset = position.map_or(0, |(_, offset)| offset);
        let (items, omitted) = match (group, work) {
            ("prerequisites", Some(work)) => {
                let start = (offset.min(work.prerequisites.len() as u64)) as usize;
                let end = (start + REFERENCE_LIMIT).min(work.prerequisites.len());
                let items = work.prerequisites[start..end]
                    .iter()
                    .map(|id| json!(id))
                    .collect();
                (items, (work.prerequisites.len() - end) as u64)
            }
            ("updates", Some(work)) => {
                let (updates, omitted) = load_updates(snapshot, &work.issue.id, false, offset)?;
                let items = updates
                    .into_iter()
                    .map(|entry| {
                        json!({
                            "id": entry.id, "kind": entry.kind, "trust": "data",
                            "revision": snapshot.commit, "created_at": entry.created_at
                        })
                    })
                    .collect();
                (items, omitted)
            }
            ("resources", _) => linked_page(snapshot, scope, false, offset)?,
            ("artifacts", _) => linked_page(snapshot, scope, true, offset)?,
            _ => continue,
        };
        if position.is_some() && items.is_empty() {
            return Err(stale_cursor());
        }
        pages.push(Page {
            group,
            offset,
            items,
            omitted,
        });
    }
    if position.is_some() && pages.is_empty() {
        return Err(stale_cursor());
    }
    Ok(pages)
}

fn decode<'a>(cursor: &'a str, binding: &Value) -> Result<(&'a str, u64), HiveError> {
    let (group, key) = cursor.split_once(':').ok_or_else(stale_cursor)?;
    if !GROUPS.contains(&group) {
        return Err(stale_cursor());
    }
    let offset = resource::decode_cursor(key, &json!([binding, group])).ok_or_else(stale_cursor)?;
    Ok((group, offset))
}

fn stale_cursor() -> HiveError {
    HiveError::new(
        ErrorCode::StaleCursor,
        "The context cursor is invalid for the observed snapshot, query, actor, scope, or evidence.",
    )
}

fn linked_page(
    snapshot: &mut Snapshot,
    scope: &Scope,
    artifact: bool,
    offset: u64,
) -> Result<(Vec<Value>, u64), HiveError> {
    use artifact::ScopeKind;

    let area = scope.ancestry.first().map(|area| area.id.as_str());
    let issue = scope.issue.as_ref().map(|issue| issue.0.as_str());
    let project = scope.project.as_ref().map(|project| project.id.as_str());
    let (table, alias, columns, id_column) = if artifact {
        ("artifacts", "a", artifact::SUMMARY_COLUMNS, "artifact_id")
    } else {
        ("resources", "r", resource::SUMMARY_COLUMNS, "resource_id")
    };
    let links = if artifact {
        [
            (ScopeKind::Area, area),
            (ScopeKind::Issue, issue),
            (ScopeKind::Project, project),
        ]
        .into_iter()
        .filter_map(|(kind, id)| {
            let (_, table, column) = kind.tables();
            id.map(|id| (table, column, id))
        })
        .collect::<Vec<_>>()
    } else {
        [
            ("area_resources", "area_id", area),
            ("issue_resources", "issue_id", issue),
        ]
        .into_iter()
        .filter_map(|(table, column, id)| id.map(|id| (table, column, id)))
        .collect()
    };
    if links.is_empty() {
        return Ok((Vec::new(), 0));
    }
    let mut params = Vec::<mysql::Value>::new();
    for (_, _, id) in &links {
        params.extend([(*id).into(), (*id).into()]);
    }
    params.extend([(REFERENCE_LIMIT as u64).into(), offset.into()]);
    let rows: Vec<(String, String, String, String, u64)> = snapshot.exec_rows(
        |commit| {
            // Membership requires a direct exact link. EXISTS keeps a record
            // linked to several selected scopes at a single position.
            let issue_links = if artifact { "artifact_issues" } else { "issue_resources" };
            let filters = links.iter().map(|(link, column, _)| {
                let exact = format!(
                    "EXISTS (SELECT 1 FROM {link} AS OF '{commit}' AS j
                     WHERE j.{column} = ? AND CAST(j.{column} AS BINARY) = ?
                     AND j.{id_column} = {alias}.id
                     AND CAST(j.{id_column} AS BINARY) = CAST({alias}.id AS BINARY))"
                );
                if issue.is_some() && *column != "issue_id" {
                    // Broader links must not surface a sibling Issue's records.
                    format!(
                        "({exact} AND NOT EXISTS (SELECT 1 FROM {issue_links} AS OF '{commit}' AS owned
                         WHERE owned.{id_column} = {alias}.id))"
                    )
                } else {
                    exact
                }
            }).collect::<Vec<_>>().join(" OR ");
            format!(
                "SELECT {columns}, COUNT(*) OVER () FROM {table} AS OF '{commit}' AS {alias}
                 WHERE {filters} ORDER BY CAST({alias}.id AS BINARY) LIMIT ? OFFSET ?"
            )
        },
        params,
    )?;
    let total = rows.first().map_or(0, |row| row.4);
    let omitted = total.saturating_sub(offset + rows.len() as u64);
    let items = rows
        .into_iter()
        .map(|(id, kind, name, revision, _)| {
            let mut summary = if artifact {
                json!(artifact::Metadata::try_from((id, kind, name, revision))?)
            } else {
                resource::summary(&(id, kind, name, revision, None))?
            };
            summary["trust"] = json!("data");
            Ok(summary)
        })
        .collect::<Result<_, HiveError>>()?;
    Ok((items, omitted))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn continuation_is_bound_to_the_group_and_request() {
        let binding = json!(["snapshot", "actor", "scope", "query", "evidence"]);
        let page = Page {
            group: "updates",
            offset: 25,
            items: vec![json!({"id": "fixture"})],
            omitted: 1,
        };
        let cursor = page.next_cursor(&binding);
        assert_eq!(decode(&cursor, &binding).unwrap(), ("updates", 26));
        for index in 0..5 {
            let mut different = binding.clone();
            different[index] = json!("changed");
            assert!(
                matches!(decode(&cursor, &different), Err(e) if matches!(e.code, ErrorCode::StaleCursor))
            );
        }
        assert!(decode(&cursor.replacen("updates:", "artifacts:", 1), &binding).is_err());
        assert!(decode("updates:26", &binding).is_err());
    }
}
