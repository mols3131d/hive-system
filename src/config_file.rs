use std::path::{Path, PathBuf};
use std::str;

use serde::{Deserialize, Deserializer, de};
use serde_json::Value;
use serde_saphyr::{DuplicateKeyPolicy, MergeKeyPolicy, Tagged};

use crate::cli::{Format, ProjectId};
use crate::error::{ErrorCode, HiveError};

pub(crate) use filesystem::read_scoped;
#[cfg(unix)]
pub(crate) use filesystem::{ScopedDirectory, read_regular};

const MAX_FILE_BYTES: usize = 64 * 1024;
const MAX_YAML_DEPTH: usize = 8;
const MAX_YAML_NODES: usize = 128;

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
pub(crate) struct OutputFields {
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
    Ok(UserConfig {
        format: output_format(fields.output)?,
        workspace: workspace_path(fields.workspace)?,
    })
}

pub(crate) fn output_format(output: Option<OutputFields>) -> Result<Option<Format>, HiveError> {
    match output.and_then(|output| output.format).as_deref() {
        None => Ok(None),
        Some("text") => Ok(Some(Format::Text)),
        Some("json") => Ok(Some(Format::Json)),
        Some(_) => Err(invalid("User output.format must be text or json")),
    }
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
    deserialize_yaml(yaml_input(bytes, source)?, source)
}

#[cfg(unix)]
pub(crate) fn parse_v2<T: de::DeserializeOwned>(
    bytes: &[u8],
    source: &str,
) -> Result<T, HiveError> {
    use serde_saphyr::granit_parser::{self, Event, Parser};

    enum Collection {
        Mapping { next_is_key: bool },
        Sequence,
    }

    let input = yaml_input(bytes, source)?;
    // serde-saphyr 2.2.0 allows explicit string tags on struct fields even
    // with reject_tagged_field_names. Check the original node events for v2.
    let options = granit_parser::options! {
        strict_indentation: true,
        flow_nesting_limit: MAX_YAML_DEPTH,
        block_nesting_limit: MAX_YAML_DEPTH,
    };
    let malformed = || invalid(format!("{source} contains invalid YAML or fields"));
    let mut collections = Vec::new();
    let mut nodes = 0;
    for result in Parser::new_from_str_with_options(input, options) {
        let (event, _) = result.map_err(|_| malformed())?;
        if event.is_node() {
            nodes += 1;
            if nodes > MAX_YAML_NODES || collections.len() > MAX_YAML_DEPTH {
                return Err(malformed());
            }
            if let Some(Collection::Mapping { next_is_key }) = collections.last_mut() {
                if *next_is_key && event.tag().is_some() {
                    return Err(invalid(format!("{source} contains a tagged field name")));
                }
                *next_is_key = !*next_is_key;
            }
        }
        match event {
            Event::MappingStart(..) => collections.push(Collection::Mapping { next_is_key: true }),
            Event::SequenceStart(..) => collections.push(Collection::Sequence),
            Event::MappingEnd | Event::SequenceEnd => {
                collections.pop();
            }
            _ => {}
        }
    }
    deserialize_yaml(input, source)
}

fn yaml_input<'a>(bytes: &'a [u8], source: &str) -> Result<&'a str, HiveError> {
    if bytes.len() > MAX_FILE_BYTES {
        return Err(invalid(format!("{source} exceeds the 64 KiB size limit")));
    }
    str::from_utf8(bytes).map_err(|_| invalid(format!("{source} must be UTF-8")))
}

fn deserialize_yaml<T: de::DeserializeOwned>(input: &str, source: &str) -> Result<T, HiveError> {
    let options = serde_saphyr::options! {
        budget: serde_saphyr::budget! {
            max_documents: 1,
            max_depth: MAX_YAML_DEPTH,
            max_nodes: MAX_YAML_NODES,
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

pub(crate) fn untagged<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    deserializer: D,
) -> Result<T, D::Error> {
    let Tagged(value, tag) = Tagged::deserialize(deserializer)?;
    if tag.is_some() {
        return Err(de::Error::custom("configuration values must be untagged"));
    }
    Ok(value)
}

pub(crate) fn present<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
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
    use std::path::{Component, Path, PathBuf};

    use super::{MAX_FILE_BYTES, invalid};
    use crate::error::{ErrorCode, HiveError};

    pub(crate) fn read_regular(path: &Path) -> Result<Option<Vec<u8>>, HiveError> {
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

    pub(crate) struct ScopedDirectory {
        file: File,
        pub(crate) path: PathBuf,
    }

    impl ScopedDirectory {
        pub(crate) fn open_absolute(path: &Path) -> Result<Self, HiveError> {
            let mut components = path.components();
            if components.next() != Some(Component::RootDir) {
                return Err(invalid("Workspace root must be absolute"));
            }
            let mut resolved = PathBuf::from("/");
            let mut file = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_DIRECTORY)
                .open(&resolved)
                .map_err(read_error)?;
            let mut ancestors = Vec::new();
            // Open each component before processing any following ".." so
            // missing directories and symlinks cannot be hidden by normalization.
            for component in components {
                match component {
                    Component::CurDir => {}
                    Component::Normal(name) => {
                        let child =
                            open_child(&file, name, libc::O_DIRECTORY)?.ok_or_else(|| {
                                HiveError::new(
                                    ErrorCode::NotFound,
                                    "Workspace directory does not exist",
                                )
                            })?;
                        ancestors.push(std::mem::replace(&mut file, child));
                        resolved.push(name);
                    }
                    Component::ParentDir => {
                        // A directory moved after open may have a different OS
                        // parent. Return to the ancestor we already traversed.
                        if let Some(parent) = ancestors.pop() {
                            file = parent;
                            resolved.pop();
                        }
                    }
                    _ => return Err(invalid("Workspace root has an invalid path component")),
                }
            }
            Ok(Self {
                file,
                path: resolved,
            })
        }

        pub(crate) fn contains(&self, name: &std::ffi::CStr) -> Result<bool, HiveError> {
            let mut metadata = std::mem::MaybeUninit::<libc::stat>::uninit();
            // SAFETY: The directory descriptor, name, and output buffer are valid.
            let result = unsafe {
                libc::fstatat(
                    self.file.as_raw_fd(),
                    name.as_ptr(),
                    metadata.as_mut_ptr(),
                    libc::AT_SYMLINK_NOFOLLOW,
                )
            };
            if result == 0 {
                return Ok(true);
            }
            let error = Error::last_os_error();
            if error.kind() == ErrorKind::NotFound {
                Ok(false)
            } else {
                Err(read_error(error))
            }
        }

        pub(crate) fn read(&self, name: &OsStr) -> Result<Option<Vec<u8>>, HiveError> {
            open_child(&self.file, name, libc::O_NONBLOCK)?
                .map(read_file)
                .transpose()
        }
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

    pub(crate) fn read_regular(_path: &Path) -> Result<Option<Vec<u8>>, HiveError> {
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
