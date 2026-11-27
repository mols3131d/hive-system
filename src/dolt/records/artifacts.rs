use super::{Diagnostics, HiveError, RecordReader};

const TABLE: &str = "artifacts";

pub(super) fn validate(
    snapshot: &mut RecordReader<'_>,
    diagnostics: &mut Diagnostics,
) -> Result<Vec<String>, HiveError> {
    let rows = snapshot.records::<(String, String, bool, bool)>(
        TABLE,
        "id, kind, archived_at IS NOT NULL, archive_reason IS NOT NULL",
        "id",
    )?;
    let mut ids = Vec::with_capacity(rows.len());
    for (id, kind, has_archived_at, has_archive_reason) in rows {
        validate_shape(&id, &kind, has_archived_at, has_archive_reason, diagnostics);
        ids.push(id);
    }
    Ok(ids)
}

fn validate_shape(
    id: &str,
    kind: &str,
    has_archived_at: bool,
    has_archive_reason: bool,
    diagnostics: &mut Diagnostics,
) {
    diagnostics.check_id(TABLE, id, &[]);
    if !is_kind(kind) {
        diagnostics.add(
            "INVALID_ARTIFACT_KIND",
            TABLE,
            &[id],
            "Use a documented lowercase Artifact kind.",
        );
    }
    if has_archived_at != has_archive_reason {
        diagnostics.add(
            "INVALID_ARTIFACT_ARCHIVE",
            TABLE,
            &[id],
            "The Artifact archive timestamp and reason must both be set or both be absent.",
        );
    }
}

pub(in crate::dolt) fn is_kind(kind: &str) -> bool {
    // The workspace frontmatter contract also classifies message, checkpoint, and manifest.
    matches!(
        kind,
        "research"
            | "design"
            | "decision"
            | "report"
            | "review"
            | "handoff"
            | "evidence"
            | "work-note"
            | "message"
            | "checkpoint"
            | "manifest"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requires_artifact_archive_fields_as_a_pair() {
        let id = "01900000-0000-7000-8000-000000000001";
        for has_archived_at in [false, true] {
            for has_archive_reason in [false, true] {
                let mut diagnostics = Diagnostics::default();
                validate_shape(
                    id,
                    "report",
                    has_archived_at,
                    has_archive_reason,
                    &mut diagnostics,
                );
                let codes: Vec<_> = diagnostics.items.iter().map(|item| item.code).collect();
                let expected =
                    (has_archived_at != has_archive_reason).then_some("INVALID_ARTIFACT_ARCHIVE");
                assert_eq!(codes, expected.as_slice());
            }
        }
    }
}
