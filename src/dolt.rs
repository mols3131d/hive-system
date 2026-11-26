mod graph;
mod records;

use std::time::Duration;

use mysql::prelude::{FromRow, Queryable};
use mysql::{Conn, OptsBuilder};
use sha2::{Digest, Sha256};

use crate::cli::ValidateArgs;
use crate::error::{ErrorCode, HiveError};

const DOLT_VERSION: &str = "2.7.6";
pub(crate) const STORAGE_SCHEMA_VERSION: u32 = 1;
const INITIAL_SCHEMA: &[u8] = include_bytes!("../assets/schema/dolt/0001_initial.sql");
const REQUIRED_TABLES: &[&str] = &[
    "area_resources",
    "areas",
    "artifact_areas",
    "artifact_issues",
    "artifact_projects",
    "artifact_resources",
    "artifacts",
    "bot_memories",
    "bots",
    "directives",
    "issue_claims",
    "issue_dependencies",
    "issue_labels",
    "issue_resources",
    "issue_updates",
    "issues",
    "labels",
    "operation_receipts",
    "project_issues",
    "projects",
    "resources",
    "schema_migrations",
    "workspace_counters",
    "workspace_meta",
];
const MAX_ROWS_PER_TABLE: usize = 100_000;

pub(crate) struct ValidationReport {
    pub(crate) workspace_id: String,
    pub(crate) db_commit: String,
    pub(crate) diagnostics: records::Diagnostics,
}

pub(crate) fn validate(args: &ValidateArgs) -> Result<ValidationReport, HiveError> {
    let (Some(database), Some(port), Some(workspace_id)) = (
        args.database.as_deref(),
        args.port,
        args.workspace_id.as_deref(),
    ) else {
        return Err(HiveError::new(
            ErrorCode::InvalidArgument,
            "Supply the database, port, and expected workspace ID.",
        ));
    };

    if !is_uuid(workspace_id) {
        return Err(HiveError::new(
            ErrorCode::InvalidArgument,
            "The expected workspace ID must be a lowercase UUID.",
        ));
    }
    if database.is_empty()
        || database.len() > 64
        || !database
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(HiveError::new(
            ErrorCode::InvalidArgument,
            "Supply a Dolt database name using letters, digits, underscores, or hyphens.",
        ));
    }

    let user = reader_credential("HIVE_DOLT_USER")?;
    let password = reader_credential("HIVE_DOLT_PASSWORD")?;
    if user.eq_ignore_ascii_case("root") {
        return Err(HiveError::new(
            ErrorCode::InvalidArgument,
            "Supply a dedicated Dolt reader account.",
        ));
    }
    let timeout = Some(Duration::from_secs(5));
    let mut connection = Conn::new(
        OptsBuilder::new()
            .ip_or_hostname(Some("127.0.0.1"))
            .tcp_port(port.get())
            .prefer_socket(false)
            .tcp_connect_timeout(timeout)
            .read_timeout(timeout)
            .write_timeout(timeout)
            .db_name(Some(database))
            .user(Some(user))
            .pass(Some(password)),
    )
    .map_err(read_error)?;

    let Some(version): Option<String> = first_row(&mut connection, "SELECT DOLT_VERSION()")? else {
        return Err(incompatible_schema(
            "The server did not report a Dolt version.",
        ));
    };
    if version != DOLT_VERSION {
        return Err(incompatible_schema(format!(
            "Validation requires Dolt {DOLT_VERSION}."
        )));
    }

    let selection: Option<(Option<String>, Option<String>, Option<String>)> = first_row(
        &mut connection,
        "SELECT DATABASE(), ACTIVE_BRANCH(), DOLT_HASHOF('HEAD')",
    )?;
    let Some((Some(selected_db), Some(branch), Some(commit))) = selection else {
        return Err(unverified_authority(
            "The Dolt database or committed main branch is unavailable.",
        ));
    };
    if selected_db != database || branch != "main" || !is_commit_hash(&commit) {
        return Err(unverified_authority(
            "The connection must resolve to the selected Dolt database and committed main branch.",
        ));
    }

    let mut snapshot = Snapshot { connection, commit };
    snapshot.verify_schema()?;
    let identity: Vec<(i8, String)> = snapshot.rows(
        "workspace_meta",
        "singleton_id, workspace_id",
        "ORDER BY singleton_id LIMIT 2",
    )?;
    if !matches!(identity.as_slice(), [(1, actual)] if actual == workspace_id) {
        return Err(unverified_authority(
            "The committed workspace identity does not match the expected UUID.",
        ));
    }

    let diagnostics = records::validate(&mut snapshot)?;
    Ok(ValidationReport {
        workspace_id: workspace_id.to_owned(),
        db_commit: snapshot.commit,
        diagnostics,
    })
}

fn reader_credential(name: &'static str) -> Result<String, HiveError> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            HiveError::new(
                ErrorCode::InvalidArgument,
                format!("Set {name} for the selected Dolt reader."),
            )
        })
}

struct Snapshot {
    connection: Conn,
    commit: String,
}

impl Snapshot {
    fn verify_schema(&mut self) -> Result<(), HiveError> {
        let tables: Vec<(String, String)> = query_rows(
            &mut self.connection,
            format!("SHOW FULL TABLES AS OF '{}'", self.commit),
        )?;
        for required in REQUIRED_TABLES {
            if !tables
                .iter()
                .any(|(name, kind)| name == required && kind == "BASE TABLE")
            {
                return Err(incompatible_schema(format!(
                    "The committed schema is missing required table {required}."
                )));
            }
        }
        let ledger: Vec<(u32, String)> = self.rows(
            "schema_migrations",
            "version, migration_digest",
            "ORDER BY version LIMIT 2",
        )?;
        let expected_digest = format!("{:x}", Sha256::digest(INITIAL_SCHEMA));
        if !matches!(ledger.as_slice(), [(STORAGE_SCHEMA_VERSION, digest)] if *digest == expected_digest)
        {
            return Err(incompatible_schema(
                "The committed migration ledger must contain exactly the supported schema version and digest.",
            ));
        }
        Ok(())
    }

    // Dolt 2.7.6 requires a literal in AS OF. Only a verified, fixed-alphabet
    // commit hash and static column and table names are interpolated.
    fn rows<T: FromRow>(
        &mut self,
        table: &'static str,
        columns: &'static str,
        suffix: &str,
    ) -> Result<Vec<T>, HiveError> {
        query_rows(
            &mut self.connection,
            format!(
                "SELECT {columns} FROM {table} AS OF '{}' {suffix}",
                self.commit
            ),
        )
    }

    fn records<T: FromRow>(
        &mut self,
        table: &'static str,
        columns: &'static str,
        order: &'static str,
    ) -> Result<Vec<T>, HiveError> {
        let rows = self.rows(
            table,
            columns,
            &format!("ORDER BY {order} LIMIT {}", MAX_ROWS_PER_TABLE + 1),
        )?;
        if rows.len() > MAX_ROWS_PER_TABLE {
            return Err(HiveError::new(
                ErrorCode::ContextLimit,
                format!("Validation exceeds the limit of {MAX_ROWS_PER_TABLE} rows in {table}."),
            ));
        }
        Ok(rows)
    }
}

fn first_row<T: FromRow>(connection: &mut Conn, sql: &'static str) -> Result<Option<T>, HiveError> {
    connection
        .query_first_opt(sql)
        .map_err(read_error)?
        .transpose()
        .map_err(|_| incompatible_schema("Dolt returned unexpected metadata."))
}

fn query_rows<T: FromRow>(connection: &mut Conn, sql: String) -> Result<Vec<T>, HiveError> {
    connection
        .query_opt(sql)
        .map_err(read_error)?
        .into_iter()
        .collect::<Result<_, _>>()
        .map_err(|_| incompatible_schema("A committed table returned unexpected column types."))
}

fn read_error(error: mysql::Error) -> HiveError {
    // Do not relay driver/server messages, which can contain SQL or connection details.
    match error {
        mysql::Error::MySqlError(error) => match error.code {
            1044 | 1045 | 1142 | 1143 => HiveError::new(
                ErrorCode::Unauthorized,
                "Dolt refused the configured reader.",
            ),
            1054 | 1146 | 1305 => {
                incompatible_schema("Dolt could not read the required Hive schema.")
            }
            _ => HiveError::new(
                ErrorCode::RuntimeUnavailable,
                "Dolt could not read the selected snapshot.",
            ),
        },
        _ => HiveError::new(
            ErrorCode::RuntimeUnavailable,
            "The local Dolt SQL connection is unavailable.",
        ),
    }
}

fn incompatible_schema(message: impl Into<String>) -> HiveError {
    HiveError::new(ErrorCode::SchemaIncompatible, message)
}

fn unverified_authority(message: &'static str) -> HiveError {
    HiveError::new(ErrorCode::AuthorityUnverified, message)
}

fn is_commit_hash(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'v').contains(&byte))
}

fn is_uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => byte == b'-',
            14 => (b'1'..=b'8').contains(&byte),
            19 => matches!(byte, b'8' | b'9' | b'a' | b'b'),
            _ => byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte),
        })
}

fn is_uuid_v7(value: &str) -> bool {
    is_uuid(value) && value.as_bytes()[14] == b'7'
}
