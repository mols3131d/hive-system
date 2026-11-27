#[cfg(unix)]
mod unix {
    use std::ffi::OsStr;
    use std::path::{Component, Path, PathBuf};

    use serde::Deserialize;
    use serde_json::{Value, json};

    use crate::cli::{CommonOptions, ConfigScope, ConfigShowArgs, Format, View};
    use crate::config::{absolute_env, current_dir, json_path, resolve_path};
    use crate::config_file::{self, OutputFields, ScopedDirectory, parse_v2, present, untagged};
    use crate::dolt::{STORAGE_SCHEMA_VERSION, is_uuid, is_uuid_v7};
    use crate::error::{ErrorCode, HiveError};

    use super::{INIT_PENDING, MARKER, has_init_stage_component};

    mod bindings;

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct UserFields {
        #[serde(deserialize_with = "untagged")]
        version: Value,
        #[serde(default, deserialize_with = "present")]
        workspace: Option<String>,
        #[serde(default, deserialize_with = "present")]
        output: Option<OutputFields>,
    }

    struct UserSource {
        path: PathBuf,
        workspace: Option<PathBuf>,
        format: Option<Format>,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Descriptor {
        #[serde(deserialize_with = "untagged")]
        version: Value,
        #[serde(deserialize_with = "untagged")]
        workspace_id: String,
        #[serde(deserialize_with = "untagged")]
        schema_version: Value,
        #[serde(deserialize_with = "untagged")]
        policy_revision: String,
    }

    pub(crate) struct Candidate {
        pub(crate) root: ScopedDirectory,
        descriptor: Descriptor,
    }

    impl Candidate {
        pub(crate) fn workspace_id(&self) -> &str {
            &self.descriptor.workspace_id
        }

        pub(crate) fn policy_revision(&self) -> &str {
            &self.descriptor.policy_revision
        }
    }

    pub(crate) fn read_explicit(path: &Path) -> Result<Candidate, HiveError> {
        validate_path(path)?;
        let target = resolve_path(path, &current_dir()?, absolute_env("HOME").as_deref())?;
        read_candidate(&target)
    }

    pub(super) fn show(
        options: &CommonOptions,
        args: &ConfigShowArgs,
    ) -> (Format, Result<Value, HiveError>) {
        let mut format = options.format.unwrap_or_default();
        let result = inspect(options, args, &mut format);
        (format, result)
    }

    fn inspect(
        options: &CommonOptions,
        args: &ConfigShowArgs,
        format: &mut Format,
    ) -> Result<Value, HiveError> {
        let resolved = matches!(args.target.scope, ConfigScope::Resolved);
        if options.project_root.is_some()
            || !matches!(options.view, View::WorkingTree)
            || (!resolved
                && (options.project.is_some() || args.area.is_some() || args.resource.is_some()))
        {
            return Err(HiveError::new(
                ErrorCode::InvalidArgument,
                if resolved {
                    "Unsupported scope or view option for this configuration report."
                } else {
                    "Workspace descriptor inspection accepts only workspace and output selection."
                },
            ));
        }

        let workspace_error = |source, error| {
            if resolved {
                source_error(source, error)
            } else {
                error
            }
        };
        let home = absolute_env("HOME");
        let home = home.as_deref();
        let explicit = options
            .workspace
            .as_deref()
            .map(read_explicit)
            .transpose()
            .map_err(|error| workspace_error("CLI workspace", error))?;

        let area = args.area.as_deref();
        let project = options.project.as_ref().map(|id| id.as_str());
        for (flag, id) in [("--area", area), ("--project", project)] {
            if id.is_some_and(|id| !is_uuid_v7(id)) {
                return Err(HiveError::new(
                    ErrorCode::InvalidArgument,
                    format!("CLI {flag} must be a canonical lowercase UUIDv7."),
                ));
            }
        }
        let resource_root = args
            .resource
            .as_deref()
            .map(|path| bindings::explicit_root(path, home))
            .transpose()?;
        let mut binding = if let Some(root) = &resource_root {
            bindings::select(Some(root), home)?
        } else {
            None
        };

        let mut read_user_for_report = || -> Result<_, HiveError> {
            let user = read_user(home)?;
            if options.format.is_none()
                && let Some(user_format) = user.as_ref().and_then(|source| source.format)
            {
                *format = user_format;
            }
            Ok(user)
        };
        // Implicit lookup errors use the configured format. Exact Resource
        // lookup only reads user config early when it supplies the workspace.
        let format_from_user_early = options.format.is_none() && resource_root.is_none();
        let mut user = if format_from_user_early {
            read_user_for_report()?
        } else {
            None
        };

        if resolved && resource_root.is_none() {
            binding = bindings::select(None, home)?;
        }
        let needs_user_workspace = binding.as_ref().map_or(explicit.is_none(), |selected| {
            selected.binding.workspace.is_none()
        });
        if needs_user_workspace && !format_from_user_early {
            user = read_user_for_report()?;
        }
        let user_workspace = user.as_ref().and_then(|source| {
            source
                .workspace
                .as_deref()
                .map(|path| (path, source.path.as_path()))
        });

        let mut binding = binding.as_ref();
        let (candidate, workspace_source) = if let Some(selected) = binding {
            let source = if explicit.is_some() { "cli" } else { "binding" };
            let (candidate, applies) =
                resolve_bound_workspace(selected, explicit, user_workspace, home)
                    .map_err(|error| source_error("Selected binding", error))?;
            if !applies {
                if resource_root.is_some() {
                    return Err(HiveError::new(
                        ErrorCode::NotFound,
                        "The CLI Resource binding belongs to a different workspace.",
                    ));
                }
                binding = None;
            }
            (candidate, source)
        } else if let Some(candidate) = explicit {
            (candidate, "cli")
        } else if let Some((path, source)) = user_workspace {
            let target = resolve_path(path, config_parent(source), home)?;
            let candidate = read_candidate(&target)
                .map_err(|error| workspace_error("User workspace", error))?;
            (candidate, "user")
        } else {
            let path = xdg_root("XDG_DATA_HOME", home, ".local/share")?.join("userdata");
            let candidate = read_candidate(&path)
                .map_err(|error| workspace_error("XDG default workspace", error))?;
            (candidate, "xdg-default")
        };

        if options.format.is_none() && resource_root.is_some() && !needs_user_workspace {
            user = read_user_for_report()?;
        }
        let output_source = match (
            options.format,
            user.as_ref().and_then(|source| source.format),
        ) {
            (Some(_), _) => "cli",
            (None, Some(_)) => "user",
            (None, None) => "default",
        };
        let descriptor = candidate.descriptor;
        let mut report = json!({
            "scope": if resolved { "resolved" } else { "workspace" },
            "workspace_root": json_path(&candidate.root.path)?,
            "workspace_id": descriptor.workspace_id,
            "policy_revision": descriptor.policy_revision,
            "storage_schema_version": STORAGE_SCHEMA_VERSION,
            "descriptor": json_path(&candidate.root.path.join(MARKER))?,
            "output_format": format.as_str(),
            "sources": {"workspace": workspace_source, "output_format": output_source},
            "verification": {
                "descriptor": "verified",
                "committed_identity": "unverified",
                "runtime": "unverified"
            }
        });
        if let Some(source) = user
            && (workspace_source == "user"
                || output_source == "user"
                || binding.is_some_and(|selected| selected.binding.workspace.is_none()))
        {
            report["user_config"] = json!(json_path(&source.path)?);
        }
        if resolved {
            report["verification"]["scope_membership"] = json!("unverified");
            let mut bound_area = None;
            let mut bound_project = None;
            if let Some(selected) = binding {
                report["binding"] = json!(json_path(&selected.file)?);
                report["resource_root"] = json!(json_path(&selected.root)?);
                report["resource_id"] = json!(selected.binding.resource_id);
                report["sources"]["resource"] = json!("binding");
                bound_area = selected.binding.area_id.as_deref();
                bound_project = selected.binding.project_id.as_deref();
            }
            for (scope, field, cli, bound) in [
                ("area", "area_id", area, bound_area),
                ("project", "project_id", project, bound_project),
            ] {
                if let Some(id) = cli.or(bound) {
                    report[field] = json!(id);
                    report["sources"][scope] = json!(if cli.is_some() { "cli" } else { "binding" });
                }
            }
        }
        Ok(report)
    }

    fn resolve_bound_workspace(
        selected: &bindings::Selected,
        explicit: Option<Candidate>,
        user_workspace: Option<(&Path, &Path)>,
        home: Option<&Path>,
    ) -> Result<(Candidate, bool), HiveError> {
        let (path, source) = match selected.binding.workspace.as_deref() {
            Some(path) => (path, selected.file.as_path()),
            None => user_workspace.ok_or_else(|| {
                invalid("A binding without a workspace requires a workspace in user configuration.")
            })?,
        };
        validate_path(path)?;
        let target = resolve_path(path, config_parent(source), home)?;
        let candidate = if let Some(candidate) = explicit {
            if normalized_root(&target) != candidate.root.path {
                return Ok((candidate, false));
            }
            // Verify the matching spelling still traverses only real directories.
            refuse_init_stage(&target)?;
            ScopedDirectory::open_absolute(&target)?;
            candidate
        } else {
            read_candidate(&target)?
        };
        selected.binding.validate_scope()?;
        if selected
            .binding
            .workspace_id
            .as_ref()
            .is_some_and(|id| *id != candidate.descriptor.workspace_id)
        {
            return Err(HiveError::new(
                ErrorCode::AuthorityUnverified,
                "The binding workspace ID does not match the selected descriptor.",
            ));
        }
        Ok((candidate, true))
    }

    fn source_error(source: &str, error: HiveError) -> HiveError {
        HiveError::new(error.code, format!("{source}: {error}"))
    }

    fn config_parent(path: &Path) -> &Path {
        path.parent().expect("versioned config file has a parent")
    }

    // Compare an untrusted target to the explicit root without inspecting a
    // different workspace. An equal spelling still needs a no-follow traversal.
    fn normalized_root(path: &Path) -> PathBuf {
        let mut root = PathBuf::from("/");
        for component in path.components() {
            match component {
                Component::Normal(name) => root.push(name),
                Component::ParentDir => {
                    root.pop();
                }
                _ => {}
            }
        }
        root
    }

    fn xdg_root(variable: &str, home: Option<&Path>, fallback: &str) -> Result<PathBuf, HiveError> {
        absolute_env(variable)
            .or_else(|| home.map(|path| path.join(fallback)))
            .map(|path| path.join("hive/v2"))
            .ok_or_else(|| invalid("The v2 XDG fallback requires an absolute home directory."))
    }

    fn read_user(home: Option<&Path>) -> Result<Option<UserSource>, HiveError> {
        let path = xdg_root("XDG_CONFIG_HOME", home, ".config")?.join("config.yml");
        let Some(bytes) = config_file::read_regular(&path).map_err(|error| {
            HiveError::new(
                error.code,
                format!("Could not read v2 user configuration. {error}"),
            )
        })?
        else {
            return Ok(None);
        };
        let fields: UserFields = parse_v2(&bytes, "V2 user configuration")?;
        require_version(&fields.version, 2, "User configuration version")?;
        let workspace = fields.workspace.map(PathBuf::from);
        if let Some(path) = &workspace {
            validate_path(path)?;
        }
        Ok(Some(UserSource {
            path,
            workspace,
            format: config_file::output_format(fields.output)?,
        }))
    }

    fn refuse_init_stage(path: &Path) -> Result<(), HiveError> {
        if has_init_stage_component(path) {
            return Err(invalid(
                "Incomplete initialization staging cannot be selected as a workspace.",
            ));
        }
        Ok(())
    }

    fn refuse_incomplete_publication(root: &ScopedDirectory) -> Result<(), HiveError> {
        if root.contains(INIT_PENDING)? {
            return Err(invalid("Workspace initialization is incomplete."));
        }
        Ok(())
    }

    fn read_candidate(path: &Path) -> Result<Candidate, HiveError> {
        refuse_init_stage(path)?;
        let root = ScopedDirectory::open_absolute(path)?;
        refuse_incomplete_publication(&root)?;
        if root.contains(c"hive.yml")? {
            return Err(HiveError::new(
                ErrorCode::SchemaIncompatible,
                "A v2 workspace cannot contain a v1 marker.",
            ));
        }
        let bytes = root
            .read(OsStr::new(MARKER))
            .map_err(|error| {
                HiveError::new(
                    error.code,
                    format!("Could not read the workspace descriptor. {error}"),
                )
            })?
            .ok_or_else(|| {
                HiveError::new(
                    ErrorCode::NotFound,
                    "The v2 workspace descriptor is missing.",
                )
            })?;
        let descriptor = parse_descriptor(&bytes)?;
        // A reader may open the directory before init installs the sentinel,
        // then read the descriptor after it appears. Check again on the same FD.
        refuse_incomplete_publication(&root)?;
        Ok(Candidate { root, descriptor })
    }

    #[cfg(target_os = "linux")]
    pub(super) fn verify_descriptor(bytes: &[u8]) -> Result<(), HiveError> {
        parse_descriptor(bytes).map(|_| ())
    }

    fn parse_descriptor(bytes: &[u8]) -> Result<Descriptor, HiveError> {
        let descriptor: Descriptor = parse_v2(bytes, "Workspace descriptor")?;
        require_version(&descriptor.version, 2, "Workspace descriptor version")?;
        require_version(
            &descriptor.schema_version,
            STORAGE_SCHEMA_VERSION,
            "Workspace descriptor schema version",
        )?;
        if !is_uuid(&descriptor.workspace_id) {
            return Err(invalid(
                "Workspace descriptor identity must be a canonical lowercase UUID.",
            ));
        }
        if descriptor.policy_revision.len() != 40
            || !descriptor
                .policy_revision
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(invalid(
                "Workspace descriptor policy revision must be a lowercase Git commit SHA.",
            ));
        }
        Ok(descriptor)
    }

    fn require_version(value: &Value, expected: u32, field: &str) -> Result<(), HiveError> {
        if !value.is_i64() && !value.is_u64() {
            return Err(invalid(format!("{field} must be an integer.")));
        }
        if value.as_u64() != Some(u64::from(expected)) {
            return Err(HiveError::new(
                ErrorCode::SchemaIncompatible,
                format!("{field} is unsupported."),
            ));
        }
        Ok(())
    }

    fn validate_path(path: &Path) -> Result<(), HiveError> {
        let value = json_path(path)?;
        if value.trim().is_empty() || value.contains("://") || value.chars().any(char::is_control) {
            return Err(invalid("A workspace target must be a nonempty local path."));
        }
        Ok(())
    }

    fn invalid(message: impl Into<String>) -> HiveError {
        HiveError::new(ErrorCode::InvalidConfig, message)
    }
}

#[cfg(unix)]
pub(crate) use unix::read_explicit;

use crate::cli::{CommonOptions, ConfigShowArgs, Format};
use crate::error::HiveError;
use serde_json::Value;

pub(crate) fn show(
    options: &CommonOptions,
    args: &ConfigShowArgs,
) -> (Format, Result<Value, HiveError>) {
    #[cfg(unix)]
    {
        unix::show(options, args)
    }
    #[cfg(not(unix))]
    {
        let _ = args;
        (
            options.format.unwrap_or_default(),
            Err(HiveError::new(
                crate::error::ErrorCode::NotImplemented,
                "Workspace descriptor inspection is unavailable on this platform.",
            )),
        )
    }
}

#[cfg(unix)]
pub(crate) const MARKER: &str = "hive-workspace.yml";

#[cfg(unix)]
pub(crate) const INIT_PENDING: &std::ffi::CStr = c".hive-init-publication-incomplete";

#[cfg(unix)]
pub(crate) const INIT_STAGE_PREFIX: &str = ".hive-init-incomplete-";

#[cfg(unix)]
pub(crate) fn has_init_stage_component(path: &std::path::Path) -> bool {
    path.components().any(|component| {
        matches!(component, std::path::Component::Normal(name)
            if name.as_encoded_bytes().starts_with(INIT_STAGE_PREFIX.as_bytes()))
    })
}

#[cfg(target_os = "linux")]
pub(crate) fn verify_descriptor(bytes: &[u8]) -> Result<(), HiveError> {
    unix::verify_descriptor(bytes)
}
