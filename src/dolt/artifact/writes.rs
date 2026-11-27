use std::path::Path;

use mysql::prelude::Queryable;

use crate::dolt::{
    Snapshot, context_evidence::read_evidence_bytes, context_workflow as workflow,
    issue_writer::invalid, read_error,
};
use crate::error::{ErrorCode, HiveError};

use super::{MAX_BODY_BYTES, ScopeKind};

pub(super) fn read_body(path: &Path) -> Result<String, HiveError> {
    let bytes = read_evidence_bytes(path, MAX_BODY_BYTES as usize).map_err(|error| {
        let message = if matches!(error.code, ErrorCode::ContextLimit) {
            "The Artifact body exceeds 64 KiB."
        } else {
            "Could not read the explicit regular Artifact body file."
        };
        HiveError::new(error.code, message)
    })?;
    String::from_utf8(bytes).map_err(|_| invalid("The Artifact body must be UTF-8."))
}

pub(super) fn verify_links(
    snapshot: &mut Snapshot,
    commit: &str,
    artifact_id: &str,
    selected_area: Option<&str>,
) -> Result<(), HiveError> {
    for kind in ScopeKind::ALL {
        let (_, links, column) = kind.tables();
        let rows: Vec<(String, String)> = snapshot.exec_rows(
            |_| format!(
                "SELECT artifact_id, {column} FROM {links} AS OF '{commit}'
                 WHERE artifact_id = CAST(? AS CHAR) OR CAST(LOWER(artifact_id) AS BINARY) = ? LIMIT 2"
            ),
            (artifact_id, artifact_id),
        )?;
        let area = selected_area.filter(|_| kind == ScopeKind::Area);
        match (area, rows.as_slice()) {
            (None, []) => {}
            (Some(area), [(artifact, endpoint)]) if artifact == artifact_id && endpoint == area => {
            }
            _ => return Err(workflow::unverified()),
        }
    }
    Ok(())
}

pub(super) fn verify_delta(
    db: &mut impl Queryable,
    base: &str,
    introduced: &str,
    expected: &[(&str, u64, u64, u64)],
) -> Result<(), HiveError> {
    let tables: Vec<(String, String, String, bool, bool)> = db
        .exec(
            "SELECT from_table_name, to_table_name, diff_type, data_change, schema_change
         FROM DOLT_DIFF_SUMMARY(?, ?) ORDER BY from_table_name LIMIT 5",
            (base, introduced),
        )
        .map_err(read_error)?;
    if tables.len() != expected.len()
        || tables
            .iter()
            .zip(expected)
            .any(|((from, to, kind, data, schema), (name, ..))| {
                from != name || to != name || kind != "modified" || !data || *schema
            })
        || db
            .query_first::<mysql::Row, _>("SHOW WARNINGS LIMIT 1")
            .map_err(read_error)?
            .is_some()
    {
        return Err(workflow::unverified());
    }
    let rows: Vec<(String, u64, u64, u64)> = db
        .exec(
            "SELECT table_name, rows_added, rows_deleted, rows_modified
         FROM DOLT_DIFF_STAT(?, ?) ORDER BY table_name LIMIT 5",
            (base, introduced),
        )
        .map_err(read_error)?;
    if !rows
        .iter()
        .map(|(table, added, deleted, modified)| (table.as_str(), *added, *deleted, *modified))
        .eq(expected.iter().copied())
        || db
            .query_first::<mysql::Row, _>("SHOW WARNINGS LIMIT 1")
            .map_err(read_error)?
            .is_some()
    {
        return Err(workflow::unverified());
    }
    Ok(())
}
