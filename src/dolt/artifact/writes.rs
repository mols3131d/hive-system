use std::path::Path;

use crate::dolt::{
    Snapshot, context_evidence::read_evidence_bytes, context_workflow as workflow,
    issue_writer::invalid,
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
