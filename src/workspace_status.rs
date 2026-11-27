use serde_json::Value;

use crate::cli::{CommonOptions, DoltTargetArgs, Format};
use crate::error::HiveError;

#[cfg(unix)]
#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Registration {
    pub(crate) version: u32,
    pub(crate) workspace_id: String,
    pub(crate) workspace_root: String,
}

pub(crate) fn observe(
    options: &CommonOptions,
    target: &DoltTargetArgs,
) -> (Format, Result<Value, HiveError>) {
    #[cfg(unix)]
    {
        let mut format = options.format.unwrap_or_default();
        let result = unix::observe(options, target, &mut format);
        (format, result)
    }
    #[cfg(not(unix))]
    {
        let _ = target;
        (
            options.format.unwrap_or_default(),
            Err(HiveError::new(
                crate::error::ErrorCode::NotImplemented,
                "Workspace status is unavailable on this platform.",
            )),
        )
    }
}

#[cfg(unix)]
mod unix {
    use std::ffi::OsStr;
    use std::num::NonZeroU16;

    use serde::{Deserialize, de::DeserializeOwned};
    use serde_json::{Value, json};

    use super::Registration;
    use crate::cli::{CommonOptions, DoltTargetArgs, Format};
    use crate::config::{absolute_env, json_path};
    use crate::config_file::ScopedDirectory;
    use crate::config_v2::{select_workspace, xdg_root};
    use crate::dolt::{
        DoltTarget, STORAGE_SCHEMA_VERSION, committed_identity, is_database_name, is_uuid,
    };
    use crate::error::{ErrorCode, HiveError};

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Server {
        version: u32,
        workspace_id: String,
        workspace_root: String,
        pid: i32,
        boot_id: String,
        start_ticks: u64,
        nonce: String,
        endpoint: Loopback,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Loopback {
        port: NonZeroU16,
        database: String,
    }

    pub(super) fn observe(
        options: &CommonOptions,
        target: &DoltTargetArgs,
        format: &mut Format,
    ) -> Result<Value, HiveError> {
        let target = target
            .has_any()
            .then(|| DoltTarget::parse(target))
            .transpose()?;
        let (workspace, source) = select_workspace(options, format)?;
        let workspace_id = workspace.workspace_id();
        let root = json_path(&workspace.root.path)?;
        // An absent DB is valid for descriptor-only inspection. An existing
        // DB entry must be a directory reached through the open workspace.
        workspace.root.open_child_directory(OsStr::new("db"))?;
        let state_path = xdg_root(
            "XDG_STATE_HOME",
            absolute_env("HOME").as_deref(),
            ".local/state",
        )?;
        let state = match ScopedDirectory::open_absolute(&state_path) {
            Ok(directory) => Some(directory),
            Err(error) if matches!(error.code, ErrorCode::NotFound) => None,
            Err(error) => return Err(error),
        };
        if state
            .as_ref()
            .is_some_and(|state| state.path.starts_with(&workspace.root.path))
        {
            return Err(invalid(
                "The v2 state directory must be outside the selected workspace.",
            ));
        }
        let state = match state {
            Some(state) => match state.open_child_directory(OsStr::new("workspaces"))? {
                Some(workspaces) => workspaces.open_child_directory(OsStr::new(workspace_id))?,
                None => None,
            },
            None => None,
        };
        let state = state.as_ref();
        let registration: Option<Registration> = read_json(state, "active-root.json")?;
        if let Some(registration) = &registration {
            check_identity(
                registration.version,
                &registration.workspace_id,
                &registration.workspace_root,
                workspace_id,
                root,
            )?;
        }

        let server: Option<Server> = read_json(state, "server.json")?;
        let process = if let Some(server) = &server {
            check_identity(
                server.version,
                &server.workspace_id,
                &server.workspace_root,
                workspace_id,
                root,
            )?;
            if registration.is_none() {
                return Err(unverified(
                    "The recorded server requires an active-root registration.",
                ));
            }
            if server.pid <= 0
                || server.start_ticks == 0
                || !is_uuid(&server.boot_id)
                || !is_uuid(&server.nonce)
                || !is_database_name(&server.endpoint.database)
            {
                return Err(invalid(
                    "The server record has invalid process or endpoint fields.",
                ));
            }
            process_evidence(server)?
        } else {
            "unrecorded"
        };

        let pending = read_state(state, "pending-request.json")?.is_some();
        let mut committed = None;
        if let Some(target) = target {
            if target.workspace_id != workspace_id {
                return Err(unverified(
                    "The expected workspace ID does not match the descriptor.",
                ));
            }
            if let Some(server) = &server
                && (server.endpoint.database != target.database
                    || server.endpoint.port != target.port)
            {
                return Err(unverified(
                    "The explicit database target differs from the recorded endpoint.",
                ));
            }
            committed = Some(committed_identity(target)?);
        }
        // Recheck process identity after the optional connection. The SQL result
        // establishes committed identity at the endpoint, independently of this PID.
        let process = if committed.is_some()
            && let Some(server) = &server
        {
            process_evidence(server)?
        } else {
            process
        };

        Ok(json!({
            "workspace_root": root,
            "workspace_id": workspace_id,
            "workspace_source": source,
            "policy_revision": workspace.policy_revision(),
            "storage_schema_version": STORAGE_SCHEMA_VERSION,
            "active_root": if registration.is_some() { "matched" } else { "unregistered" },
            "state": if pending { "recovery_required" } else { "unknown" },
            "pending_request": if pending { "recorded" } else { "unrecorded" },
            "runtime": {
                "process": process,
                "endpoint": match (&server, &committed) {
                    (Some(_), Some(_)) => "committed_identity_verified",
                    (Some(_), None) => "recorded",
                    (None, _) => "unrecorded",
                },
                "endpoint_ownership": "unverified",
            },
            "committed_identity": committed,
        }))
    }

    fn read_state(
        state: Option<&ScopedDirectory>,
        name: &str,
    ) -> Result<Option<Vec<u8>>, HiveError> {
        let Some(state) = state else { return Ok(None) };
        state.read(OsStr::new(name)).map_err(|error| {
            HiveError::new(error.code, format!("Could not safely read {name}. {error}"))
        })
    }

    fn read_json<T: DeserializeOwned>(
        state: Option<&ScopedDirectory>,
        name: &str,
    ) -> Result<Option<T>, HiveError> {
        read_state(state, name)?
            .map(|bytes| {
                serde_json::from_slice(&bytes).map_err(|_| invalid(format!("Malformed {name}.")))
            })
            .transpose()
    }

    fn check_identity(
        version: u32,
        recorded_id: &str,
        recorded_root: &str,
        workspace_id: &str,
        root: &str,
    ) -> Result<(), HiveError> {
        if version != 2 {
            return Err(HiveError::new(
                ErrorCode::SchemaIncompatible,
                "Unsupported workspace runtime record version.",
            ));
        }
        if !is_uuid(recorded_id) {
            return Err(invalid(
                "The runtime record workspace ID must be a lowercase UUID.",
            ));
        }
        if recorded_id != workspace_id || recorded_root != root {
            return Err(unverified(
                "Local state does not match the selected workspace ID and canonical root.",
            ));
        }
        Ok(())
    }

    #[cfg(target_os = "linux")]
    fn process_evidence(server: &Server) -> Result<&'static str, HiveError> {
        use std::{fs, io::ErrorKind};

        let Ok(boot_id) = fs::read_to_string("/proc/sys/kernel/random/boot_id") else {
            return Ok("recorded");
        };
        if boot_id.trim_end() != server.boot_id {
            return Err(unverified(
                "The recorded server belongs to a different boot.",
            ));
        }
        let stat = match fs::read_to_string(format!("/proc/{}/stat", server.pid)) {
            Ok(stat) => stat,
            Err(error) if error.kind() == ErrorKind::NotFound => {
                return Err(unverified(
                    "The recorded server PID is absent from the local process view.",
                ));
            }
            Err(_) => return Ok("recorded"),
        };
        // comm is parenthesized and may contain spaces and parentheses.
        let Some((_, fields)) = stat.rsplit_once(") ") else {
            return Ok("recorded");
        };
        let mut fields = fields.split_ascii_whitespace();
        let Some(state) = fields.next() else {
            return Ok("recorded");
        };
        let Some(ticks) = fields.nth(18).and_then(|value| value.parse::<u64>().ok()) else {
            return Ok("recorded");
        };
        if matches!(state, "Z" | "X" | "x") || ticks != server.start_ticks {
            return Err(unverified("The recorded server process identity is stale."));
        }
        Ok("pid_start_verified")
    }

    #[cfg(not(target_os = "linux"))]
    fn process_evidence(_server: &Server) -> Result<&'static str, HiveError> {
        Ok("recorded")
    }

    fn invalid(message: impl Into<String>) -> HiveError {
        HiveError::new(ErrorCode::InvalidConfig, message)
    }

    fn unverified(message: &'static str) -> HiveError {
        HiveError::new(ErrorCode::AuthorityUnverified, message)
    }
}
