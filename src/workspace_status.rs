use serde_json::Value;

use crate::cli::{CommonOptions, DoltTargetArgs, Format, WorkspaceCommand};
use crate::error::HiveError;

#[cfg(unix)]
#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Registration {
    pub(crate) version: u32,
    pub(crate) workspace_id: String,
    pub(crate) workspace_root: String,
}

#[cfg(target_os = "linux")]
#[path = "workspace_runtime.rs"]
mod runtime;

#[cfg(target_os = "linux")]
pub(crate) use runtime::{
    WriterRuntime, require_local_filesystem, token, with_exclusive_owned_snapshot,
    with_operation_snapshot, with_owned_snapshot, with_writer_runtime,
};

#[cfg(target_os = "linux")]
pub(crate) use unix::require_inactive_target;

pub(crate) fn run(
    options: &CommonOptions,
    command: &WorkspaceCommand,
) -> (Format, Result<Value, HiveError>) {
    if let WorkspaceCommand::Status(target) = command {
        return observe(options, target);
    }
    let mut format = options.format.unwrap_or_default();
    #[cfg(target_os = "linux")]
    let result = runtime::run(options, command, &mut format);
    #[cfg(not(target_os = "linux"))]
    let result = Err(HiveError::new(
        crate::error::ErrorCode::NotImplemented,
        "Workspace lifecycle is available only on Linux.",
    ));
    (format, result)
}

pub(crate) fn backup(
    options: &CommonOptions,
    command: &crate::cli::BackupCommand,
) -> (Format, Result<Value, HiveError>) {
    use crate::error::ErrorCode;

    use crate::cli::BackupCommand;

    let format = options.format.unwrap_or_default();
    if let BackupCommand::Restore {
        source,
        target,
        include_conversations,
    } = command
    {
        let result = if *include_conversations {
            Err(HiveError::new(
                ErrorCode::NotImplemented,
                "Conversation restoration requires a later contract-2 implementation.",
            ))
        } else {
            crate::workspace_restore::run(source, target)
        };
        return (format, result);
    }
    let BackupCommand::Create {
        destination,
        include_conversations,
    } = command
    else {
        unreachable!("restore returned above");
    };
    if *include_conversations {
        return (
            format,
            Err(HiveError::new(
                ErrorCode::NotImplemented,
                "Backup conversation inclusion requires a later contract-2 implementation.",
            )),
        );
    }
    #[cfg(target_os = "linux")]
    {
        let mut format = format;
        let result = runtime::backup(options, destination, &mut format);
        (format, result)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = destination;
        (
            format,
            Err(HiveError::new(
                ErrorCode::NotImplemented,
                "Backup creation is supported only on Linux.",
            )),
        )
    }
}

pub(crate) fn observe(
    options: &CommonOptions,
    target: &DoltTargetArgs,
) -> (Format, Result<Value, HiveError>) {
    #[cfg(unix)]
    {
        let mut format = options.format.unwrap_or_default();
        let result = unix::observe(options, target, &mut format, false);
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

#[cfg(target_os = "linux")]
pub(crate) fn diagnose(
    options: &CommonOptions,
    target: &DoltTargetArgs,
) -> (Format, Result<Value, HiveError>) {
    let mut format = options.format.unwrap_or_default();
    let result = unix::observe(options, target, &mut format, true);
    (format, result)
}

#[cfg(unix)]
mod unix {
    use std::ffi::OsStr;
    use std::num::NonZeroU16;
    use std::os::unix::fs::MetadataExt;

    use serde::{Deserialize, Serialize, de::DeserializeOwned};
    use serde_json::{Value, json};

    use super::Registration;
    use crate::cli::{CommonOptions, DoltTargetArgs, Format};
    use crate::config::{absolute_env, json_path};
    use crate::config_file::ScopedDirectory;
    use crate::config_v2::{select_workspace, xdg_root};
    use crate::dolt::{
        DoltTarget, STORAGE_SCHEMA_VERSION, committed_identity, inspect_workspace_db,
        is_database_name, is_uuid,
    };
    use crate::error::{ErrorCode, HiveError};

    #[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
    #[serde(deny_unknown_fields)]
    pub(super) struct Server {
        pub(super) version: u32,
        pub(super) workspace_id: String,
        pub(super) workspace_root: String,
        pub(super) pid: i32,
        pub(super) boot_id: String,
        pub(super) start_ticks: u64,
        pub(super) nonce: String,
        pub(super) endpoint: Loopback,
    }

    #[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
    #[serde(deny_unknown_fields)]
    pub(super) struct Loopback {
        pub(super) port: NonZeroU16,
        pub(super) database: String,
    }

    pub(super) fn observe(
        options: &CommonOptions,
        target: &DoltTargetArgs,
        format: &mut Format,
        doctor: bool,
    ) -> Result<Value, HiveError> {
        let target = target
            .has_any()
            .then(|| DoltTarget::parse(target))
            .transpose()?;
        let selection = select_workspace(options, format)?;
        let workspace = &selection.candidate;
        let workspace_id = workspace.workspace_id();
        let root = json_path(&workspace.root.path)?;
        // An absent DB is valid for descriptor-only inspection. An existing
        // DB entry must be a directory reached through the open workspace.
        workspace.root.open_child_directory(OsStr::new("db"))?;
        let state = find_state(workspace_id, &workspace.root.path)?;
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
            check_server(server, workspace_id, root)?;
            if registration.is_none() {
                return Err(unverified(
                    "The recorded server requires an active-root registration.",
                ));
            }
            process_evidence(server)?
        } else {
            "unrecorded"
        };

        let pending = read_state(state, "pending-request.json")?.is_some();
        let mut committed = None;
        let mut working_set = None;
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
            if doctor {
                let (identity, evidence) = inspect_workspace_db(target)?;
                committed = Some(identity);
                working_set = Some(evidence);
            } else {
                committed = Some(committed_identity(target)?);
            }
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

        let mut report = json!({
            "workspace_root": root,
            "workspace_id": workspace_id,
            "workspace_source": selection.workspace_source,
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
        });
        if doctor {
            report["sources"] = json!({
                "workspace": selection.workspace_source,
                "output_format": selection.output_source,
            });
            report["descriptor"] = json!(json_path(
                &workspace.root.path.join(crate::config_v2::MARKER)
            )?);
            if let Some(path) = selection.user_config() {
                report["user_config"] = json!(json_path(path)?);
            }
            report["state_directory"] = state_permissions(state);
            report["working_set"] = working_set
                .unwrap_or_else(|| json!({"unavailable": "authenticated_endpoint_not_supplied"}));
        }
        Ok(report)
    }

    fn state_permissions(state: Option<&ScopedDirectory>) -> Value {
        let Some(state) = state else {
            return json!({"unavailable": "state_directory_missing"});
        };
        let Ok(metadata) = state.directory_file().metadata() else {
            return json!({"unavailable": "state_metadata_unavailable"});
        };
        let mode = metadata.mode() & 0o7777;
        // SAFETY: geteuid takes no arguments.
        let owned = metadata.uid() == unsafe { libc::geteuid() };
        json!({
            "owned_by_caller": owned,
            "mode": format!("{mode:04o}"),
            "private": owned && mode == 0o700,
        })
    }

    pub(super) fn find_state(
        workspace_id: &str,
        root: &std::path::Path,
    ) -> Result<Option<ScopedDirectory>, HiveError> {
        let state_path = xdg_root(
            "XDG_STATE_HOME",
            absolute_env("HOME").as_deref(),
            ".local/state",
        )?;
        let state = match ScopedDirectory::open_absolute(&state_path) {
            Ok(directory) => directory,
            Err(error) if matches!(error.code, ErrorCode::NotFound) => return Ok(None),
            Err(error) => return Err(error),
        };
        // The XDG base can be an ancestor of the selected workspace while
        // the directory that holds this UUID's lock is inside the workspace.
        if state
            .path
            .join("workspaces")
            .join(workspace_id)
            .starts_with(root)
        {
            return Err(invalid(
                "The v2 state directory must be outside the selected workspace.",
            ));
        }
        match state.open_child_directory(OsStr::new("workspaces"))? {
            Some(workspaces) => workspaces.open_child_directory(OsStr::new(workspace_id)),
            None => Ok(None),
        }
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn require_inactive_target(
        workspace_id: &str,
        target: &std::path::Path,
    ) -> Result<(), HiveError> {
        let Some(state) = find_state(workspace_id, target)? else {
            return Ok(());
        };
        if target.starts_with(&state.path) {
            return Err(invalid(
                "The restore target must be outside workspace runtime state.",
            ));
        }
        // A stale registration can still match an absent or empty path.
        // Runtime compares the exact canonical path from the descriptor.
        let path = json_path(target)?;
        if read_json::<Registration>(Some(&state), "active-root.json")?
            .is_some_and(|record| record.workspace_root == path)
        {
            return Err(unverified(
                "The restore target is the registered active root.",
            ));
        }
        Ok(())
    }

    pub(super) fn check_server(server: &Server, id: &str, root: &str) -> Result<(), HiveError> {
        check_identity(
            server.version,
            &server.workspace_id,
            &server.workspace_root,
            id,
            root,
        )?;
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
        Ok(())
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

    pub(super) fn read_json<T: DeserializeOwned>(
        state: Option<&ScopedDirectory>,
        name: &str,
    ) -> Result<Option<T>, HiveError> {
        read_state(state, name)?
            .map(|bytes| {
                serde_json::from_slice(&bytes).map_err(|_| invalid(format!("Malformed {name}.")))
            })
            .transpose()
    }

    pub(super) fn check_identity(
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
    pub(super) fn process_evidence(server: &Server) -> Result<&'static str, HiveError> {
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
        let Some((state, ticks)) = process_start(&stat) else {
            return Ok("recorded");
        };
        if matches!(state, "Z" | "X" | "x") || ticks != server.start_ticks {
            return Err(unverified("The recorded server process identity is stale."));
        }
        Ok("pid_start_verified")
    }

    #[cfg(target_os = "linux")]
    pub(super) fn process_start(stat: &str) -> Option<(&str, u64)> {
        // comm is parenthesized and may contain spaces and parentheses.
        let (_, fields) = stat.rsplit_once(") ")?;
        let mut fields = fields.split_ascii_whitespace();
        Some((fields.next()?, fields.nth(18)?.parse().ok()?))
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
