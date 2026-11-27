pub(crate) mod area;
pub(crate) mod artifact;
pub(crate) mod bot;
#[cfg(unix)]
mod conversation;
pub(crate) mod directive;
mod graph;
mod issue;
mod issue_list;
mod issue_readiness;
pub(crate) mod label;
mod project;
mod records;
pub(crate) mod resource;

use std::ffi::OsStr;
use std::num::NonZeroU16;
use std::path::Path;
use std::time::Duration;

use mysql::prelude::{FromRow, Queryable};
use mysql::{Conn, OptsBuilder, Params};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::cli::{
    CommonOptions, ConversationListArgs, ConversationSelectionArgs, DoltTargetArgs, IssueShowArgs,
    ValidateArgs,
};
use crate::error::{ErrorCode, HiveError};

pub(crate) use issue::show as issue_show;
pub(crate) use issue_list::{IssueView, list as issue_list};
pub(crate) use project::read as project_read;

const DOLT_VERSION: &str = "2.7.6";
pub(crate) const STORAGE_SCHEMA_VERSION: u32 = 1;
const INITIAL_SCHEMA: &[u8] = include_bytes!("../assets/schema/dolt/0001_initial.sql");
const INITIAL_SCHEMA_MANIFEST: &str = include_str!("../assets/schema/dolt/0001_schema.sha256");
const MAX_ROWS_PER_TABLE: usize = 100_000;

pub(crate) struct DoltSuccess {
    pub(crate) context: Value,
    pub(crate) data: Value,
}

pub(crate) struct DoltFailure {
    pub(crate) context: Option<Value>,
    pub(crate) error: HiveError,
}

impl DoltFailure {
    pub(crate) fn without_context(error: HiveError) -> Box<Self> {
        Box::new(Self {
            context: None,
            error,
        })
    }

    pub(crate) fn with_context(context: Value, error: HiveError) -> Box<Self> {
        Box::new(Self {
            context: Some(context),
            error,
        })
    }
}

pub(crate) fn conversation_read(
    common: &CommonOptions,
    args: &ConversationSelectionArgs,
    destination: Option<&Path>,
) -> Result<DoltSuccess, Box<DoltFailure>> {
    let bot_id = conversation_selector(args.bot.as_deref(), "--bot")?;
    let conversation_id = conversation_selector(args.id.as_deref(), "--id")?;
    #[cfg(unix)]
    {
        conversation::read(common, args, bot_id, conversation_id, destination)
            .map_err(DoltFailure::without_context)
    }
    #[cfg(not(unix))]
    {
        let _ = (common, bot_id, conversation_id);
        Err(DoltFailure::without_context(HiveError::new(
            ErrorCode::NotImplemented,
            if destination.is_some() {
                "Conversation export is unavailable on this platform."
            } else {
                "Conversation inspection is unavailable on this platform."
            },
        )))
    }
}

pub(crate) fn conversation_list(
    common: &CommonOptions,
    args: &ConversationListArgs,
) -> Result<DoltSuccess, Box<DoltFailure>> {
    let bot_id = conversation_selector(args.bot.as_deref(), "--bot")?;
    let limit = args
        .limit
        .parse::<usize>()
        .ok()
        .filter(|limit| (1..=100).contains(limit))
        .ok_or_else(|| {
            DoltFailure::without_context(HiveError::new(
                ErrorCode::InvalidArgument,
                "Use a page size between 1 and 100.",
            ))
        })?;
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        conversation::list(common, args, bot_id, limit)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (common, bot_id, limit, &args.cursor);
        Err(DoltFailure::without_context(HiveError::new(
            ErrorCode::NotImplemented,
            "Conversation listing is unavailable on this platform.",
        )))
    }
}

fn conversation_selector<'a>(
    value: Option<&'a OsStr>,
    flag: &str,
) -> Result<&'a str, Box<DoltFailure>> {
    value
        .and_then(OsStr::to_str)
        .filter(|id| is_uuid_v7(id))
        .ok_or_else(|| {
            DoltFailure::without_context(HiveError::new(
                ErrorCode::InvalidArgument,
                format!("Supply {flag} as a canonical lowercase UUIDv7."),
            ))
        })
}

pub(crate) fn validate(args: &ValidateArgs) -> Result<DoltSuccess, Box<DoltFailure>> {
    let (mut snapshot, workspace_id) =
        Snapshot::open(&args.target).map_err(DoltFailure::without_context)?;
    let diagnostics = records::validate(&mut snapshot).map_err(DoltFailure::without_context)?;
    let context = json!({
        "workspace_id": workspace_id,
        "db_commit": snapshot.commit,
        "scope": "workspace",
    });
    if !diagnostics.items.is_empty() {
        return Err(DoltFailure::with_context(
            context,
            HiveError::record_violations(diagnostics.items, diagnostics.truncated),
        ));
    }
    Ok(DoltSuccess {
        context,
        data: json!({
            "storage_schema_version": STORAGE_SCHEMA_VERSION,
            "diagnostics": diagnostics.items,
        }),
    })
}

fn encode_id_cursor(id: &str, binding: &Value) -> String {
    let digest = Sha256::digest(json!([binding, id]).to_string());
    format!("{id}.{digest:x}")
}

fn id_cursor_key(cursor: &str) -> Option<&str> {
    if cursor.len() == 36 + 1 + 64
        && let Some((id, digest)) = cursor.split_once('.')
        && is_uuid_v7(id)
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Some(id);
    }
    None
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

pub(crate) fn is_database_name(database: &str) -> bool {
    !database.is_empty()
        && database.len() <= 64
        && database
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

pub(crate) struct DoltTarget<'a> {
    pub(crate) database: &'a str,
    pub(crate) port: NonZeroU16,
    pub(crate) workspace_id: &'a str,
}

impl<'a> DoltTarget<'a> {
    pub(crate) fn parse(target: &'a DoltTargetArgs) -> Result<Self, HiveError> {
        let (Some(database), Some(port), Some(workspace_id)) = (
            target.database.as_deref(),
            target.port.as_deref(),
            target.workspace_id.as_deref(),
        ) else {
            return Err(HiveError::new(
                ErrorCode::InvalidArgument,
                "Supply the database, port, and expected workspace ID.",
            ));
        };
        // Parse the value after clap has recognized the contract and output options.
        let port = port
            .to_str()
            .and_then(|value| value.parse().ok())
            .ok_or_else(|| {
                HiveError::new(
                    ErrorCode::InvalidArgument,
                    "Supply a Dolt port between 1 and 65535.",
                )
            })?;
        if !is_uuid(workspace_id) {
            return Err(HiveError::new(
                ErrorCode::InvalidArgument,
                "The expected workspace ID must be a lowercase UUID.",
            ));
        }
        if !is_database_name(database) {
            return Err(HiveError::new(
                ErrorCode::InvalidArgument,
                "Supply a Dolt database name using letters, digits, underscores, or hyphens.",
            ));
        }
        Ok(Self {
            database,
            port,
            workspace_id,
        })
    }
}

#[cfg(unix)]
pub(crate) fn committed_identity(target: DoltTarget<'_>) -> Result<Value, HiveError> {
    let (snapshot, workspace_id) = Snapshot::open_target(target)?;
    Ok(json!({
        "workspace_id": workspace_id,
        "dolt_commit": snapshot.commit,
        "dolt_version": DOLT_VERSION,
        "storage_schema_version": STORAGE_SCHEMA_VERSION,
    }))
}

impl Snapshot {
    fn open(target: &DoltTargetArgs) -> Result<(Self, String), HiveError> {
        Self::open_target(DoltTarget::parse(target)?)
    }

    fn open_target(target: DoltTarget<'_>) -> Result<(Self, String), HiveError> {
        let DoltTarget {
            database,
            port,
            workspace_id,
        } = target;
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

        let Some(version): Option<String> = first_row(&mut connection, "SELECT DOLT_VERSION()")?
        else {
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

        snapshot.verify_table_definitions()?;
        Ok((snapshot, workspace_id.to_owned()))
    }
    fn verify_schema(&mut self) -> Result<(), HiveError> {
        let tables: Vec<(String, String)> = query_rows(
            &mut self.connection,
            format!("SHOW FULL TABLES AS OF '{}'", self.commit),
        )?;
        for (required, _) in initial_schema_tables() {
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

    fn verify_table_definitions(&mut self) -> Result<(), HiveError> {
        for (table, expected_digest) in initial_schema_tables() {
            let definitions: Vec<(String, String)> = query_rows(
                &mut self.connection,
                format!("SHOW CREATE TABLE {table} AS OF '{}'", self.commit),
            )?;
            if !matches!(definitions.as_slice(), [(name, sql)]
                if name == table && format!("{:x}", Sha256::digest(table_shape(sql))) == expected_digest)
            {
                return Err(incompatible_schema(format!(
                    "The committed definition of {table} differs from storage schema version {STORAGE_SCHEMA_VERSION}."
                )));
            }
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

    // Caller values and fetched IDs are bound with SQL parameters; only the
    // verified commit hash and static SQL templates are formatted.
    fn exec_rows<T: FromRow, P: Into<Params>>(
        &mut self,
        sql: impl FnOnce(&str) -> String,
        params: P,
    ) -> Result<Vec<T>, HiveError> {
        let query = sql(&self.commit);
        self.connection
            .exec_opt(query, params)
            .map_err(read_error)?
            .into_iter()
            .collect::<Result<_, _>>()
            .map_err(|_| incompatible_schema("A committed table returned unexpected column types."))
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

fn initial_schema_tables() -> impl Iterator<Item = (&'static str, &'static str)> {
    INITIAL_SCHEMA_MANIFEST.lines().map(|entry| {
        let (digest, table) = entry
            .split_once("  ")
            .expect("schema manifest digest and table");
        (table, digest)
    })
}

fn table_shape(definition: &str) -> &str {
    // The migration inherits the table-default collation. Preserve the charset and all column clauses.
    if let Some((shape, collation)) = definition.rsplit_once(" COLLATE=")
        && shape.ends_with(" DEFAULT CHARSET=utf8mb4")
        && collation.strip_prefix("utf8mb4_").is_some_and(|name| {
            !name.is_empty()
                && name
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        })
    {
        shape
    } else {
        definition
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

pub(crate) fn is_uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => byte == b'-',
            14 => (b'1'..=b'8').contains(&byte),
            19 => matches!(byte, b'8' | b'9' | b'a' | b'b'),
            _ => byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte),
        })
}

pub(crate) fn is_uuid_v7(value: &str) -> bool {
    is_uuid(value) && value.as_bytes()[14] == b'7'
}
