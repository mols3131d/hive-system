use std::collections::{BTreeSet, HashSet};
use std::io::{Seek, Write};
use std::path::Path;

use serde_json::{Value, json};

use super::{
    File, Installed, fd_path, io_error, sql_text, transaction_rows, unverified, verify_rows,
};
use crate::error::HiveError;

pub(super) fn collisions(
    installed: &Installed<'_>,
    db: &Path,
    scratch: &File,
    names: &BTreeSet<&str>,
) -> Result<HashSet<String>, HiveError> {
    if names.is_empty() {
        return Ok(HashSet::new());
    }
    let rows = installed.query(
        db,
        "SELECT COLLATION_NAME AS collation_name FROM information_schema.COLUMNS \
         WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME = 'labels' AND COLUMN_NAME = 'name'",
    )?;
    let collation = match rows.as_slice() {
        [row] => row["collation_name"].as_str().filter(|name| {
            name.starts_with("utf8mb4_")
                && name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        }),
        _ => None,
    }
    .ok_or_else(|| unverified("Could not verify the target label collation."))?;

    // Group the distinct source spellings with the target column's comparison
    // rules. Return positions only, so the engine output contains no names.
    let names: Vec<_> = names.iter().copied().collect();
    let encoded = serde_json::to_string(&names).expect("serialize label spellings");
    let mut input = tempfile::tempfile_in(fd_path(scratch)).map_err(io_error)?;
    write!(input,
        "SELECT ordinal FROM (SELECT ordinal, COUNT(*) OVER (PARTITION BY spelling) AS spellings \
         FROM (SELECT ordinal, CONVERT(name USING utf8mb4) COLLATE {collation} AS spelling \
         FROM JSON_TABLE({}, '$[*]' COLUMNS (ordinal FOR ORDINALITY, name VARCHAR(100) PATH '$')) AS names) AS reviewed) AS grouped \
         WHERE spellings > 1 ORDER BY ordinal;",
        sql_text(&encoded),
    ).map_err(io_error)?;
    input.rewind().map_err(io_error)?;
    let output = installed.dolt(db, &["sql", "-r", "json"], Some(input))?;
    if output.trim() == "{}" {
        return Ok(HashSet::new());
    }
    let mut results = serde_json::Deserializer::from_str(&output).into_iter::<Value>();
    let mut collisions = HashSet::new();
    for row in transaction_rows(&mut results)? {
        let name = row["ordinal"]
            .as_u64()
            .and_then(|ordinal| ordinal.checked_sub(1))
            .and_then(|index| usize::try_from(index).ok())
            .and_then(|index| names.get(index))
            .ok_or_else(|| {
                unverified("The target label collation check returned an invalid row.")
            })?;
        if !collisions.insert((*name).to_owned()) {
            return Err(unverified(
                "The target label collation check returned a repeated row.",
            ));
        }
    }
    if results.next().is_some() {
        return Err(unverified(
            "The target label collation check returned unexpected results.",
        ));
    }
    Ok(collisions)
}

pub(super) fn verify_empty(installed: &Installed<'_>, db: &Path) -> Result<(), HiveError> {
    verify_rows(
        &installed.query(db, &count_sql(""))?,
        &[json!({"labels": 0, "links": 0})],
    )
}

pub(super) fn count_sql(snapshot: &str) -> String {
    format!(
        "SELECT (SELECT COUNT(*) FROM labels{snapshot}) AS labels, \
         (SELECT COUNT(*) FROM issue_labels{snapshot}) AS links"
    )
}
