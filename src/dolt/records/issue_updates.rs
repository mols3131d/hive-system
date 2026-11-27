use std::collections::HashSet;

use super::{Diagnostics, HiveError, RecordReader, is_uuid_v7};

const TABLE: &str = "issue_updates";
const COLUMNS: &str = "id, issue_id, kind, transition_json IS NOT NULL, \
    (CASE WHEN JSON_VALID(transition_json) THEN JSON_TYPE(transition_json) = 'OBJECT' ELSE FALSE END) = TRUE";

pub(super) fn validate(
    snapshot: &mut RecordReader<'_>,
    issue_ids: &HashSet<&str>,
    diagnostics: &mut Diagnostics,
) -> Result<(), HiveError> {
    let rows = snapshot.records::<(String, String, String, bool, bool)>(TABLE, COLUMNS, "id")?;
    for (id, issue_id, kind, has_transition, transition_is_object) in rows {
        validate_row(
            &id,
            &issue_id,
            &kind,
            has_transition,
            transition_is_object,
            issue_ids,
            diagnostics,
        );
    }
    Ok(())
}

fn validate_row(
    id: &str,
    issue_id: &str,
    kind: &str,
    has_transition: bool,
    transition_is_object: bool,
    issue_ids: &HashSet<&str>,
    diagnostics: &mut Diagnostics,
) {
    diagnostics.check_id(TABLE, id, &[]);
    if !is_uuid_v7(issue_id) {
        diagnostics.check_id(TABLE, issue_id, &[id]);
    } else if !issue_ids.contains(issue_id) {
        diagnostics.add(
            "MISSING_REFERENCE",
            TABLE,
            &[id, issue_id],
            "The owning Issue is missing.",
        );
    }
    if !matches!(
        kind,
        "progress" | "review" | "handoff" | "evidence" | "comment" | "transition"
    ) {
        diagnostics.add(
            "INVALID_ISSUE_UPDATE_KIND",
            TABLE,
            &[id],
            "Use progress, review, handoff, evidence, comment, or transition as the Issue update kind.",
        );
    }
    let expects_transition = kind.eq_ignore_ascii_case("transition");
    match (expects_transition, has_transition, transition_is_object) {
        (true, true, true) | (false, false, false) => {}
        (true, true, false) => diagnostics.add(
            "INVALID_ISSUE_UPDATE_TRANSITION",
            TABLE,
            &[id],
            "Transition updates require a JSON object in transition_json.",
        ),
        _ => diagnostics.add(
            "INVALID_ISSUE_UPDATE_TRANSITION",
            TABLE,
            &[id],
            "Transition updates require transition_json, and other Issue update kinds must omit it.",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checks_paired_transition_payload_and_json_object_shape() {
        let id = "01900000-0000-7000-8000-000000000001";
        let issue = "01900000-0000-7000-8000-000000000002";
        let issue_ids = HashSet::from([issue]);
        for (kind, has_transition, transition_is_object, expected) in [
            ("progress", false, false, None),
            ("transition", true, true, None),
            (
                "transition",
                false,
                false,
                Some("INVALID_ISSUE_UPDATE_TRANSITION"),
            ),
            (
                "progress",
                true,
                true,
                Some("INVALID_ISSUE_UPDATE_TRANSITION"),
            ),
            (
                "progress",
                true,
                false,
                Some("INVALID_ISSUE_UPDATE_TRANSITION"),
            ),
            (
                "transition",
                true,
                false,
                Some("INVALID_ISSUE_UPDATE_TRANSITION"),
            ),
        ] {
            let mut diagnostics = Diagnostics::default();
            validate_row(
                id,
                issue,
                kind,
                has_transition,
                transition_is_object,
                &issue_ids,
                &mut diagnostics,
            );
            let codes: Vec<_> = diagnostics.items.iter().map(|item| item.code).collect();
            assert_eq!(
                codes,
                expected.as_slice(),
                "{kind} {has_transition} {transition_is_object}"
            );
        }
    }
}
