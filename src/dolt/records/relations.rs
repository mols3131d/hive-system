use std::collections::HashSet;

use crate::dolt::{Snapshot, is_uuid_v7};
use crate::error::HiveError;

use super::{Diagnostics, artifacts, directives};

pub(super) fn validate(
    snapshot: &mut Snapshot,
    area_ids: &HashSet<&str>,
    issue_ids: &HashSet<&str>,
    diagnostics: &mut Diagnostics,
) -> Result<(), HiveError> {
    let projects = snapshot.records::<(String, String)>("projects", "id, status", "id")?;
    for (id, status) in &projects {
        diagnostics.check_id("projects", id, &[]);
        // SQL CHECK comparisons can accept other spellings under a case-insensitive collation.
        if !matches!(status.as_str(), "active" | "completed" | "archived") {
            diagnostics.add(
                "INVALID_PROJECT_STATUS",
                "projects",
                &[id],
                "Use active, completed, or archived as the Project status.",
            );
        }
    }

    let resources = snapshot.records::<String>("resources", "id", "id")?;
    let labels = snapshot.records::<String>("labels", "id", "id")?;
    for (table, records) in [("resources", &resources), ("labels", &labels)] {
        for id in records {
            diagnostics.check_id(table, id, &[]);
        }
    }

    let artifacts = artifacts::validate(snapshot, diagnostics)?;
    let artifact_ids = artifacts.iter().map(String::as_str).collect();
    let project_ids = projects.iter().map(|(id, _)| id.as_str()).collect();
    let resource_ids = resources.iter().map(String::as_str).collect();
    let label_ids = labels.iter().map(String::as_str).collect();
    for (table, columns, left_ids, right_ids) in [
        (
            "project_issues",
            "project_id, issue_id",
            &project_ids,
            issue_ids,
        ),
        (
            "area_resources",
            "area_id, resource_id",
            area_ids,
            &resource_ids,
        ),
        (
            "issue_resources",
            "issue_id, resource_id",
            issue_ids,
            &resource_ids,
        ),
        ("issue_labels", "issue_id, label_id", issue_ids, &label_ids),
        (
            "artifact_areas",
            "artifact_id, area_id",
            &artifact_ids,
            area_ids,
        ),
        (
            "artifact_projects",
            "artifact_id, project_id",
            &artifact_ids,
            &project_ids,
        ),
        (
            "artifact_issues",
            "artifact_id, issue_id",
            &artifact_ids,
            issue_ids,
        ),
        (
            "artifact_resources",
            "artifact_id, resource_id",
            &artifact_ids,
            &resource_ids,
        ),
    ] {
        for (left, right) in snapshot.records::<(String, String)>(table, columns, columns)? {
            let ids = [left.as_str(), right.as_str()];
            let left_canonical = is_uuid_v7(&left);
            let right_canonical = is_uuid_v7(&right);
            if !left_canonical || !right_canonical {
                diagnostics.add(
                    "INVALID_ID",
                    table,
                    &ids,
                    "Link endpoints must use canonical lowercase UUIDv7 identifiers.",
                );
            }
            if (left_canonical && !left_ids.contains(left.as_str()))
                || (right_canonical && !right_ids.contains(right.as_str()))
            {
                diagnostics.add(
                    "MISSING_REFERENCE",
                    table,
                    &ids,
                    "A link endpoint is missing.",
                );
            }
        }
    }
    directives::validate(snapshot, area_ids, &project_ids, diagnostics)
}
