#[cfg(unix)]
mod unix {
    use std::ffi::OsStr;
    use std::path::{Path, PathBuf};

    use serde::Deserialize;
    use serde_json::{Value, json};

    use crate::cli::{CommonOptions, Format, View};
    use crate::config::{absolute_env, current_dir, json_path, resolve_path};
    use crate::config_file::{self, OutputFields, ScopedDirectory, parse_v2, present, untagged};
    use crate::dolt::{STORAGE_SCHEMA_VERSION, is_uuid};
    use crate::error::{ErrorCode, HiveError};

    const MARKER: &str = "hive-workspace.yml";

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

    // A descriptor supplies expected identity only. A future runtime integration
    // must verify that the explicitly selected reader endpoint serves this root's
    // db/ before passing the expected UUID to the committed snapshot reader.
    struct Candidate {
        root: PathBuf,
        descriptor: Descriptor,
    }

    pub(super) fn show(options: &CommonOptions) -> (Format, Result<Value, HiveError>) {
        let mut format = options.format.unwrap_or_default();
        let result = inspect(options, &mut format);
        (format, result)
    }

    fn inspect(options: &CommonOptions, format: &mut Format) -> Result<Value, HiveError> {
        if options.project.is_some()
            || options.project_root.is_some()
            || !matches!(options.view, View::WorkingTree)
        {
            return Err(HiveError::new(
                ErrorCode::InvalidArgument,
                "Workspace descriptor inspection accepts only workspace and output selection.",
            ));
        }

        let home = absolute_env("HOME");
        let home = home.as_deref();
        let explicit = options
            .workspace
            .as_deref()
            .map(|path| {
                validate_path(path)?;
                let target = resolve_path(path, &current_dir()?, home)?;
                read_candidate(&target)
            })
            .transpose()?;

        // With both inputs explicit, there is no user-level setting to resolve.
        let user = if explicit.is_some() && options.format.is_some() {
            None
        } else {
            read_user(home)?
        };
        let output = user.as_ref().and_then(|source| source.format);
        let output_source = match (options.format, output) {
            (Some(_), _) => "cli",
            (None, Some(user_format)) => {
                *format = user_format;
                "user"
            }
            (None, None) => "default",
        };

        let (candidate, workspace_source) = if let Some(candidate) = explicit {
            (candidate, "cli")
        } else if let Some(UserSource {
            path,
            workspace: Some(workspace),
            ..
        }) = &user
        {
            let base = path.parent().expect("versioned user config has a parent");
            let target = resolve_path(workspace, base, home)?;
            (read_candidate(&target)?, "user")
        } else {
            let path = xdg_root("XDG_DATA_HOME", home, ".local/share")?.join("userdata");
            (read_candidate(&path)?, "xdg-default")
        };

        let descriptor = candidate.descriptor;
        let mut report = json!({
            "scope": "workspace",
            "workspace_root": json_path(&candidate.root)?,
            "workspace_id": descriptor.workspace_id,
            "policy_revision": descriptor.policy_revision,
            "storage_schema_version": STORAGE_SCHEMA_VERSION,
            "descriptor": json_path(&candidate.root.join(MARKER))?,
            "output_format": format.as_str(),
            "sources": {"workspace": workspace_source, "output_format": output_source},
            "verification": {
                "descriptor": "verified",
                "committed_identity": "unverified",
                "runtime": "unverified"
            }
        });
        if let Some(source) = user
            && (workspace_source == "user" || output_source == "user")
        {
            report["user_config"] = json!(json_path(&source.path)?);
        }
        Ok(report)
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

    fn read_candidate(path: &Path) -> Result<Candidate, HiveError> {
        let root = ScopedDirectory::open_absolute(path)?;
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
        let descriptor: Descriptor = parse_v2(&bytes, "Workspace descriptor")?;
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
        Ok(Candidate {
            root: root.path,
            descriptor,
        })
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

use crate::cli::{CommonOptions, Format};
use crate::error::HiveError;
use serde_json::Value;

pub(crate) fn show(options: &CommonOptions) -> (Format, Result<Value, HiveError>) {
    #[cfg(unix)]
    {
        unix::show(options)
    }
    #[cfg(not(unix))]
    {
        (
            options.format.unwrap_or_default(),
            Err(HiveError::new(
                crate::error::ErrorCode::NotImplemented,
                "Workspace descriptor inspection is unavailable on this platform.",
            )),
        )
    }
}
