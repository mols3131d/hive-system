use std::collections::HashSet;

use super::{Diagnostics, Graph, HiveError, RecordReader, is_uuid_v7};

const TABLE: &str = "directives";

struct DirectiveRow {
    id: String,
    scope_kind: String,
    area_id: Option<String>,
    project_id: Option<String>,
    include_descendants: i8,
    status: String,
    has_approval: bool,
    validity_ordered: bool,
    supersedes_id: Option<String>,
}

impl DirectiveRow {
    fn validate_shape(&self, diagnostics: &mut Diagnostics) {
        let id = self.id.as_str();
        diagnostics.check_id(TABLE, id, &[]);
        let scope = (
            self.scope_kind.as_str(),
            self.area_id.as_deref(),
            self.project_id.as_deref(),
            self.include_descendants,
        );
        // SQL CHECK comparisons may be case-insensitive and BOOLEAN is stored as TINYINT.
        if !matches!(
            scope,
            ("workspace", None, None, 0)
                | ("area", Some(_), None, 0 | 1)
                | ("project", None, Some(_), 0)
        ) {
            diagnostics.add(
                "INVALID_DIRECTIVE_SCOPE",
                TABLE,
                &[id],
                "Use one workspace, Area, or Project scope with matching references. Set the descendant flag to true or false, with true allowed only for Area scope.",
            );
        }
        if !matches!(
            self.status.as_str(),
            "proposed" | "active" | "superseded" | "withdrawn"
        ) {
            diagnostics.add(
                "INVALID_DIRECTIVE_STATUS",
                TABLE,
                &[id],
                "Use proposed, active, superseded, or withdrawn as the Directive status.",
            );
        }
        if self.status == "active" && !self.has_approval {
            diagnostics.add(
                "INVALID_DIRECTIVE_APPROVAL",
                TABLE,
                &[id],
                "An active Directive requires an approval reference.",
            );
        }
        if !self.validity_ordered {
            diagnostics.add(
                "INVALID_DIRECTIVE_VALIDITY",
                TABLE,
                &[id],
                "When both validity bounds are set, expiry must be after the start.",
            );
        }
    }
}

pub(super) fn validate(
    snapshot: &mut RecordReader<'_>,
    area_ids: &HashSet<&str>,
    project_ids: &HashSet<&str>,
    diagnostics: &mut Diagnostics,
) -> Result<(), HiveError> {
    let directives: Vec<_> = snapshot
        .records::<(
            String,
            String,
            Option<String>,
            Option<String>,
            i8,
            String,
            bool,
            bool,
            Option<String>,
        )>(
            TABLE,
            "id, scope_kind, area_id, project_id, CAST(include_descendants AS SIGNED), status,
             approval_ref IS NOT NULL, (valid_from IS NULL OR expires_at IS NULL OR expires_at > valid_from),
             supersedes_id",
            "id",
        )?
        .into_iter()
        .map(
            |(id, scope_kind, area_id, project_id, include_descendants, status, has_approval, validity_ordered, supersedes_id)| DirectiveRow {
                id,
                scope_kind,
                area_id,
                project_id,
                include_descendants,
                status,
                has_approval,
                validity_ordered,
                supersedes_id,
            },
        )
        .collect();
    let directive_ids = directives.iter().map(|row| row.id.as_str()).collect();
    let mut supersession = Graph::default();
    for directive in &directives {
        directive.validate_shape(diagnostics);
        let id = directive.id.as_str();
        for (target, ids, message) in [
            (
                directive.area_id.as_deref(),
                area_ids,
                "The scoped Area is missing.",
            ),
            (
                directive.project_id.as_deref(),
                project_ids,
                "The scoped Project is missing.",
            ),
            (
                directive.supersedes_id.as_deref(),
                &directive_ids,
                "The superseded Directive is missing.",
            ),
        ] {
            let Some(target) = target else { continue };
            if !is_uuid_v7(target) {
                diagnostics.check_id(TABLE, target, &[id]);
            } else if !ids.contains(target) {
                diagnostics.add("MISSING_REFERENCE", TABLE, &[id, target], message);
            }
        }
        if let Some(target) = directive.supersedes_id.as_deref()
            && directive_ids.contains(target)
        {
            supersession.add(id, target);
        }
    }
    if let Some(cycle) = supersession.cycle() {
        diagnostics.add(
            "DIRECTIVE_SUPERSESSION_CYCLE",
            TABLE,
            &cycle,
            "Directive supersession contains a cycle.",
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checks_scope_approval_and_validity_fields() {
        let area = "01900000-0000-7000-8000-000000000001";
        let project = "01900000-0000-7000-8000-000000000002";
        for (scope_kind, area_id, project_id, include_descendants) in [
            ("workspace", Some(area), None, 0),
            ("area", None, None, 0),
            ("area", Some(area), Some(project), 0),
            ("project", None, None, 0),
            ("project", None, Some(project), 1),
        ] {
            let row = DirectiveRow {
                id: "01900000-0000-7000-8000-000000000003".to_owned(),
                scope_kind: scope_kind.to_owned(),
                area_id: area_id.map(str::to_owned),
                project_id: project_id.map(str::to_owned),
                include_descendants,
                status: "active".to_owned(),
                has_approval: false,
                validity_ordered: false,
                supersedes_id: None,
            };
            let mut diagnostics = Diagnostics::default();
            row.validate_shape(&mut diagnostics);
            let codes: Vec<_> = diagnostics.items.iter().map(|item| item.code).collect();
            assert_eq!(
                codes,
                [
                    "INVALID_DIRECTIVE_SCOPE",
                    "INVALID_DIRECTIVE_APPROVAL",
                    "INVALID_DIRECTIVE_VALIDITY",
                ]
            );
        }
    }
}
