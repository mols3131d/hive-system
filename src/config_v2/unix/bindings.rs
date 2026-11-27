use std::collections::HashSet;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::Value;

use crate::config::{current_dir, resolve_path};
use crate::config_file::{ScopedDirectory, parse_v2, present, untagged};
use crate::dolt::{is_uuid, is_uuid_v7};
use crate::error::{ErrorCode, HiveError};

use super::{invalid, normalized_root, require_version, source_error, validate_path, xdg_root};

const FILE: &str = "bindings.yml";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BindingFile {
    #[serde(deserialize_with = "untagged")]
    version: Value,
    bindings: Vec<Binding>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Binding {
    path: String,
    #[serde(default, deserialize_with = "present")]
    pub(super) workspace: Option<PathBuf>,
    #[serde(default, deserialize_with = "present")]
    pub(super) workspace_id: Option<String>,
    pub(super) resource_id: String,
    #[serde(default, deserialize_with = "present")]
    pub(super) area_id: Option<String>,
    #[serde(default, deserialize_with = "present")]
    pub(super) project_id: Option<String>,
}

pub(super) struct Selected {
    pub(super) file: PathBuf,
    pub(super) root: PathBuf,
    pub(super) binding: Binding,
}

impl Binding {
    pub(super) fn validate_scope(&self) -> Result<(), HiveError> {
        if self.workspace_id.as_deref().is_some_and(|id| !is_uuid(id)) {
            return Err(invalid(
                "Binding workspace_id must be a canonical lowercase UUID.",
            ));
        }
        for (field, id) in [
            ("resource_id", Some(self.resource_id.as_str())),
            ("area_id", self.area_id.as_deref()),
            ("project_id", self.project_id.as_deref()),
        ] {
            if id.is_some_and(|id| !is_uuid_v7(id)) {
                return Err(invalid(format!(
                    "Binding {field} must be a canonical lowercase UUIDv7."
                )));
            }
        }
        Ok(())
    }
}

pub(super) fn explicit_root(path: &Path, home: Option<&Path>) -> Result<PathBuf, HiveError> {
    directory(path, &current_dir()?, home).map_err(|error| source_error("CLI Resource root", error))
}

pub(super) fn select(
    exact_root: Option<&Path>,
    home: Option<&Path>,
) -> Result<Option<Selected>, HiveError> {
    let config = xdg_root("XDG_CONFIG_HOME", home, ".config")?;
    let selected = select_binding(&config, exact_root, home)
        .map_err(|error| source_error("Machine-local bindings", error))?;
    if exact_root.is_some() && selected.is_none() {
        return Err(HiveError::new(
            ErrorCode::NotFound,
            "The CLI Resource directory has no exact machine-local binding.",
        ));
    }
    Ok(selected)
}

fn select_binding(
    config: &Path,
    exact_root: Option<&Path>,
    home: Option<&Path>,
) -> Result<Option<Selected>, HiveError> {
    // Do not follow symlinks through the binding file or its containing path.
    let directory_handle = match ScopedDirectory::open_absolute(config) {
        Ok(directory) => directory,
        Err(error) if matches!(error.code, ErrorCode::NotFound) => return Ok(None),
        Err(error) => return Err(error),
    };
    let Some(bytes) = directory_handle.read(OsStr::new(FILE))? else {
        return Ok(None);
    };
    let fields: BindingFile = parse_v2(&bytes, "Bindings file")?;
    require_version(&fields.version, 2, "Bindings version")?;
    let invocation = match exact_root {
        Some(root) => root.to_path_buf(),
        None => current_dir()?,
    };
    let base = &directory_handle.path;
    let mut roots = HashSet::new();
    let mut selected = None;
    let mut depth = 0;
    for binding in fields.bindings {
        let path = Path::new(&binding.path);
        validate_path(path)?;
        let path = resolve_path(path, base, home)?;
        let root = normalized_root(&path);
        if !roots.insert(root.clone()) {
            return Err(invalid("Duplicate canonical Resource directories."));
        }
        let applies = if exact_root.is_some() {
            invocation == root
        } else {
            invocation.starts_with(&root)
        };
        let root_depth = root.components().count();
        if applies && root_depth > depth {
            depth = root_depth;
            selected = Some((path, binding));
        }
    }
    // Verify the original spelling of the winner, including components erased
    // by normalization. A missing directory or symlink fails this selection.
    selected
        .map(|(path, binding)| {
            Ok(Selected {
                file: base.join(FILE),
                root: ScopedDirectory::open_absolute(&path)?.path,
                binding,
            })
        })
        .transpose()
}

fn directory(path: &Path, base: &Path, home: Option<&Path>) -> Result<PathBuf, HiveError> {
    validate_path(path)?;
    let path = resolve_path(path, base, home)?;
    Ok(ScopedDirectory::open_absolute(&path)?.path)
}
