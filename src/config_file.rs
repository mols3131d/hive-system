use std::path::{Path, PathBuf};
use std::str;

use serde::{Deserialize, Deserializer, de};
use serde_json::Value;
use serde_saphyr::{DuplicateKeyPolicy, MergeKeyPolicy, Tagged};

use crate::cli::{Format, ProjectId};
use crate::error::{ErrorCode, HiveError};

pub(crate) use filesystem::read_scoped;

const MAX_FILE_BYTES: usize = 64 * 1024;

#[derive(Debug)]
pub(crate) struct UserConfig {
    pub(crate) workspace: Option<PathBuf>,
    pub(crate) format: Option<Format>,
}

#[derive(Debug)]
pub(crate) struct ProjectConfig {
    pub(crate) project_id: ProjectId,
    pub(crate) workspace: Option<PathBuf>,
}

#[derive(Debug)]
pub(crate) struct WorkspaceConfig {
    pub(crate) coordination: Coordination,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Coordination {
    Local,
    Remote(String),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UserFields {
    #[serde(default, deserialize_with = "present")]
    workspace: Option<String>,
    #[serde(default, deserialize_with = "present")]
    output: Option<OutputFields>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OutputFields {
    #[serde(default, deserialize_with = "present")]
    format: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectFields {
    #[serde(deserialize_with = "untagged")]
    project_id: String,
    #[serde(default, deserialize_with = "present")]
    workspace: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkspaceFields {
    #[serde(deserialize_with = "untagged")]
    version: Value,
    #[serde(deserialize_with = "untagged")]
    coordination: CoordinationFields,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CoordinationFields {
    #[serde(deserialize_with = "untagged")]
    mode: String,
    #[serde(default, deserialize_with = "present")]
    remote: Option<String>,
}

pub(crate) fn read_user(path: &Path) -> Result<Option<UserConfig>, HiveError> {
    filesystem::read_regular(path)?
        .as_deref()
        .map(parse_user)
        .transpose()
}

fn parse_user(bytes: &[u8]) -> Result<UserConfig, HiveError> {
    let fields: UserFields = parse(bytes, "User configuration")?;
    let format = match fields.output.and_then(|output| output.format).as_deref() {
        None => None,
        Some("text") => Some(Format::Text),
        Some("json") => Some(Format::Json),
        Some(_) => return Err(invalid("User output.format must be text or json")),
    };
    Ok(UserConfig {
        workspace: workspace_path(fields.workspace)?,
        format,
    })
}

pub(crate) fn parse_project(bytes: &[u8]) -> Result<ProjectConfig, HiveError> {
    let fields: ProjectFields = parse(bytes, "Project configuration")?;
    let Ok(project_id @ ProjectId::Ordinary(_)) = fields.project_id.parse() else {
        return Err(invalid("Project project_id must be an ordinary project ID"));
    };
    Ok(ProjectConfig {
        project_id,
        workspace: workspace_path(fields.workspace)?,
    })
}

pub(crate) fn parse_workspace(bytes: &[u8]) -> Result<WorkspaceConfig, HiveError> {
    let fields: WorkspaceFields = parse(bytes, "Workspace configuration")?;
    if !fields.version.is_i64() && !fields.version.is_u64() {
        return Err(invalid("Workspace version must be an integer"));
    }
    if fields.version.as_u64() != Some(1) {
        return Err(HiveError::new(
            ErrorCode::IncompatiblePolicy,
            "Workspace configuration version is unsupported",
        ));
    }

    let coordination = match (
        fields.coordination.mode.as_str(),
        fields.coordination.remote,
    ) {
        ("local", None) => Coordination::Local,
        ("local", Some(_)) => {
            return Err(invalid("Local coordination does not accept a remote"));
        }
        ("remote", Some(remote)) if is_remote_alias(&remote) => Coordination::Remote(remote),
        ("remote", _) => {
            return Err(invalid("Remote coordination requires a Git remote alias"));
        }
        _ => return Err(invalid("Coordination mode must be local or remote")),
    };
    Ok(WorkspaceConfig { coordination })
}

fn workspace_path(path: Option<String>) -> Result<Option<PathBuf>, HiveError> {
    path.map(|path| {
        if path.trim().is_empty() || path.chars().any(char::is_control) {
            return Err(invalid("Configuration workspace must be a nonempty path"));
        }
        Ok(PathBuf::from(path))
    })
    .transpose()
}

fn is_remote_alias(alias: &str) -> bool {
    // Git checks a remote name inside refs/remotes/<alias>/<ref>.
    !alias.starts_with('-')
        && !alias.contains("..")
        && !alias.contains("@{")
        && !alias.chars().any(|character| {
            character.is_control()
                || matches!(character, ' ' | '~' | '^' | ':' | '?' | '*' | '[' | '\\')
        })
        && alias.split('/').all(|component| {
            !component.is_empty() && !component.starts_with('.') && !component.ends_with(".lock")
        })
}

fn parse<T: de::DeserializeOwned>(bytes: &[u8], source: &str) -> Result<T, HiveError> {
    if bytes.len() > MAX_FILE_BYTES {
        return Err(invalid(format!("{source} exceeds the 64 KiB size limit")));
    }
    let input = str::from_utf8(bytes).map_err(|_| invalid(format!("{source} must be UTF-8")))?;
    let options = serde_saphyr::options! {
        budget: serde_saphyr::budget! {
            max_documents: 1,
            max_depth: 8,
            max_nodes: 128,
            max_anchors: 0,
            max_aliases: 0,
        },
        duplicate_keys: DuplicateKeyPolicy::Error,
        merge_keys: MergeKeyPolicy::Error,
        no_schema: true,
        strict_booleans: true,
        strict_indentation: true,
        reject_unsupported_tags: true,
        reject_tagged_field_names: true,
        with_snippet: false,
    };
    // Parser diagnostics may include source text or unrecognized field names.
    serde_saphyr::from_str_with_options(input, options)
        .map_err(|_| invalid(format!("{source} contains invalid YAML or fields")))
}

fn untagged<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    deserializer: D,
) -> Result<T, D::Error> {
    let Tagged(value, tag) = Tagged::deserialize(deserializer)?;
    if tag.is_some() {
        return Err(de::Error::custom("configuration values must be untagged"));
    }
    Ok(value)
}

fn present<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    deserializer: D,
) -> Result<Option<T>, D::Error> {
    // Missing fields use the defaults. Present fields must have the declared type.
    untagged(deserializer).map(Some)
}

fn invalid(message: impl Into<String>) -> HiveError {
    HiveError::new(ErrorCode::InvalidConfig, message)
}

#[cfg(unix)]
mod filesystem {
    use std::ffi::{CString, OsStr};
    use std::fs::{self, File, OpenOptions};
    use std::io::{Error, ErrorKind, Read};
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::{ffi::OsStrExt, fs::OpenOptionsExt};
    use std::path::{Component, Path};

    use super::{MAX_FILE_BYTES, invalid};
    use crate::error::{ErrorCode, HiveError};

    pub(super) fn read_regular(path: &Path) -> Result<Option<Vec<u8>>, HiveError> {
        let metadata = match fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(read_error(error)),
        };
        if !metadata.is_file() {
            return Err(invalid("Configuration source must be a regular file"));
        }

        // Recheck the final path while opening in case the initial file was replaced.
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)
            .map_err(read_error)?;
        read_file(file).map(Some)
    }

    pub(crate) fn read_scoped(root: &Path, relative: &Path) -> Result<Option<Vec<u8>>, HiveError> {
        let components = relative
            .components()
            .map(|component| match component {
                Component::Normal(name) => Ok(name),
                _ => Err(invalid(
                    "Configuration source is outside its expected directory",
                )),
            })
            .collect::<Result<Vec<_>, _>>()?;
        let Some((name, directories)) = components.split_last() else {
            return Err(invalid("Configuration source requires a file path"));
        };

        let mut directory = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
            .open(root)
            .map_err(read_error)?;
        // Open each child relative to the descriptor so replacing a path component
        // cannot redirect a subsequent open through a symlink.
        for name in directories {
            let Some(child) = open_child(&directory, name, libc::O_DIRECTORY)? else {
                return Ok(None);
            };
            directory = child;
        }
        let Some(file) = open_child(&directory, name, libc::O_NONBLOCK)? else {
            return Ok(None);
        };
        read_file(file).map(Some)
    }

    fn open_child(
        directory: &File,
        name: &OsStr,
        flags: libc::c_int,
    ) -> Result<Option<File>, HiveError> {
        let name = CString::new(name.as_bytes())
            .map_err(|_| invalid("Configuration source has an invalid path component"))?;
        // SAFETY: The descriptor and NUL-terminated component remain valid for this call.
        let descriptor = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                name.as_ptr(),
                flags | libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if descriptor == -1 {
            let error = Error::last_os_error();
            return if error.kind() == ErrorKind::NotFound {
                Ok(None)
            } else {
                Err(read_error(error))
            };
        }
        // SAFETY: openat returned a newly owned descriptor.
        Ok(Some(unsafe { File::from_raw_fd(descriptor) }))
    }

    fn read_file(file: File) -> Result<Vec<u8>, HiveError> {
        if !file.metadata().map_err(read_error)?.is_file() {
            return Err(invalid("Configuration source must be a regular file"));
        }

        let mut bytes = Vec::new();
        file.take(MAX_FILE_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(read_error)?;
        if bytes.len() > MAX_FILE_BYTES {
            return Err(invalid(
                "Configuration source exceeds the 64 KiB size limit",
            ));
        }
        Ok(bytes)
    }

    fn read_error(error: Error) -> HiveError {
        if error.kind() == ErrorKind::NotADirectory || error.raw_os_error() == Some(libc::ELOOP) {
            return invalid("Configuration source must be a regular file within its directory");
        }
        HiveError::new(ErrorCode::IoError, "Could not read configuration source")
    }
}

#[cfg(not(unix))]
mod filesystem {
    use std::path::Path;

    use crate::error::{ErrorCode, HiveError};

    pub(super) fn read_regular(_path: &Path) -> Result<Option<Vec<u8>>, HiveError> {
        Err(HiveError::new(
            ErrorCode::IoError,
            "Configuration file access is unsupported on this platform",
        ))
    }

    pub(crate) fn read_scoped(_root: &Path, relative: &Path) -> Result<Option<Vec<u8>>, HiveError> {
        read_regular(relative)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_workspace_paths_owned_by_each_file() {
        let user = parse_user(b"workspace: \"123\"\noutput: {format: json}\n").unwrap();
        assert_eq!(user.workspace, Some(PathBuf::from("123")));
        assert_eq!(user.format, Some(Format::Json));

        let project = parse_project(b"project_id: example-project\nworkspace: ../data\n").unwrap();
        assert_eq!(
            project.project_id,
            ProjectId::Ordinary("example-project".to_owned())
        );
        assert_eq!(project.workspace, Some(PathBuf::from("../data")));
    }

    #[test]
    fn rejects_invalid_yaml_types_and_extensions() {
        for input in [
            "[]",
            "workspace:",
            "workspace: 123",
            "workspace: true",
            "workspace: !include ./other.yml",
            "workspace: !!binary ZGF0YQ==",
            "workspace: &target data",
            "workspace: data\nworkspace: second",
            "output: {format: text, format: json}",
            "output: {format: text, unknown: flag}",
            "output: {<<: {format: text}}",
            "---\nworkspace: data\n---\n{}\n",
        ] {
            let error = parse_user(input.as_bytes()).unwrap_err();
            assert!(matches!(error.code, ErrorCode::InvalidConfig), "{input}");
        }
    }

    #[test]
    fn enforces_workspace_version_and_remote_alias() {
        let local = parse_workspace(b"version: 1\ncoordination: {mode: local}\n").unwrap();
        assert_eq!(local.coordination, Coordination::Local);
        for (input, expected) in [
            (
                "version: 2\ncoordination: {mode: local}",
                "INCOMPATIBLE_POLICY",
            ),
            (
                "version: '1'\ncoordination: {mode: local}",
                "INVALID_CONFIG",
            ),
            (
                "version: 1.0\ncoordination: {mode: local}",
                "INVALID_CONFIG",
            ),
            ("version: 1\ncoordination: {mode: remote}", "INVALID_CONFIG"),
            (
                "version: 1\ncoordination: {mode: local, remote: origin}",
                "INVALID_CONFIG",
            ),
            (
                "version: 1\ncoordination: {mode: remote, remote: --option}",
                "INVALID_CONFIG",
            ),
            (
                "version: 1\ncoordination: {mode: remote, remote: 'https://synthetic-secret@example.invalid'}",
                "INVALID_CONFIG",
            ),
            (
                "version: 1\ncoordination: {mode: local, synthetic-secret: example}",
                "INVALID_CONFIG",
            ),
        ] {
            let error = parse_workspace(input.as_bytes()).unwrap_err();
            assert_eq!(serde_json::to_value(error.code).unwrap(), expected);
            assert!(!error.to_string().contains("synthetic-secret"));
        }
    }

    #[test]
    fn validates_remote_aliases_as_git_ref_components() {
        for alias in [
            "work-origin",
            "team/mirror",
            "_backup",
            "+backup",
            "backup.",
            "équipe/mirror",
        ] {
            let yaml = format!("version: 1\ncoordination: {{mode: remote, remote: '{alias}'}}");
            assert_eq!(
                parse_workspace(yaml.as_bytes()).unwrap().coordination,
                Coordination::Remote(alias.to_owned())
            );
        }
        for alias in [
            "/mirror",
            "team//mirror",
            "team/",
            "team/.mirror",
            "team.lock/mirror",
            "team/../mirror",
            "mirror@{1}",
            "git@example.invalid:repo",
        ] {
            let yaml = format!("version: 1\ncoordination: {{mode: remote, remote: '{alias}'}}");
            assert!(matches!(
                parse_workspace(yaml.as_bytes()).unwrap_err().code,
                ErrorCode::InvalidConfig
            ));
        }
    }
}
