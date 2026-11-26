//! MySQL sessions for the synthetic Dolt spikes.

use mysql::prelude::Queryable;
use mysql::{Conn, DriverError, OptsBuilder, Transaction, TxOpts};
use std::fmt;
use std::io::ErrorKind;
use std::time::Duration;

#[derive(Clone)]
pub struct Target {
    pub port: u16,
    pub database: String,
    pub workspace_id: String,
    pub user: String,
    pub password: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ConnectError {
    FixtureVariable(&'static str),
    IdentityMismatch,
    Server(u16),
    Transport(ErrorKind),
    Protocol,
}

impl fmt::Display for ConnectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FixtureVariable(name) => write!(f, "invalid or missing fixture variable {name}"),
            Self::IdentityMismatch => {
                f.write_str("database, branch, or workspace identity mismatch")
            }
            Self::Server(code) => {
                write!(f, "Dolt refused the connection or identity query ({code})")
            }
            Self::Transport(kind) => write!(f, "loopback transport failed ({kind:?})"),
            Self::Protocol => f.write_str("Dolt connection protocol failed"),
        }
    }
}

impl std::error::Error for ConnectError {}

impl From<mysql::Error> for ConnectError {
    fn from(error: mysql::Error) -> Self {
        // The driver and server may include configuration or SQL in their messages.
        match error {
            mysql::Error::MySqlError(error) => Self::Server(error.code),
            mysql::Error::IoError(error) => Self::Transport(error.kind()),
            mysql::Error::DriverError(DriverError::CouldNotConnect(Some((_, _, kind)))) => {
                Self::Transport(kind)
            }
            mysql::Error::DriverError(DriverError::ConnectTimeout) => {
                Self::Transport(ErrorKind::TimedOut)
            }
            _ => Self::Protocol,
        }
    }
}

impl Target {
    /// Load the explicit test binding supplied by `dolt_rust.fixture()`.
    pub fn from_fixture_env() -> Result<Self, ConnectError> {
        let read = |name| std::env::var(name).map_err(|_| ConnectError::FixtureVariable(name));
        Ok(Self {
            port: read("HIVE_SPIKE_PORT")?
                .parse()
                .map_err(|_| ConnectError::FixtureVariable("HIVE_SPIKE_PORT"))?,
            database: read("HIVE_SPIKE_DATABASE")?,
            workspace_id: read("HIVE_SPIKE_WORKSPACE_ID")?,
            user: read("HIVE_SPIKE_USER")?,
            password: read("HIVE_SPIKE_PASSWORD")?,
        })
    }

    /// Open one loopback TCP session and verify its workspace on main.
    pub fn connect(&self) -> Result<Conn, ConnectError> {
        let timeout = Some(Duration::from_secs(2));
        let mut connection = Conn::new(
            OptsBuilder::new()
                .ip_or_hostname(Some("127.0.0.1"))
                .tcp_port(self.port)
                .prefer_socket(false)
                .tcp_connect_timeout(timeout)
                .read_timeout(timeout)
                .write_timeout(timeout)
                .db_name(Some(&self.database))
                .user(Some(&self.user))
                .pass(Some(&self.password)),
        )?;
        let identity: Option<(Option<String>, Option<String>, String)> = connection.query_first(
            "SELECT DATABASE(), ACTIVE_BRANCH(), workspace_id FROM workspace_meta WHERE singleton_id = 1",
        )?;
        match identity {
            Some((database, branch, workspace_id))
                if database.as_deref() == Some(self.database.as_str())
                    && branch.as_deref() == Some("main")
                    && workspace_id == self.workspace_id =>
            {
                Ok(connection)
            }
            _ => Err(ConnectError::IdentityMismatch),
        }
    }
}

/// Start an explicit SQL transaction, optionally publishing a Dolt commit on COMMIT.
pub fn begin(connection: &mut Conn, version_commit: bool) -> mysql::Result<Transaction<'_>> {
    connection.query_drop(format!(
        "SET @@SESSION.dolt_force_transaction_commit = 0, @@SESSION.dolt_transaction_commit = {}",
        u8::from(version_commit),
    ))?;
    connection.start_transaction(TxOpts::default())
}
