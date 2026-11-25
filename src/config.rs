use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::cli::{CommonOptions, ConfigScope, Format, ProjectId, View};
use crate::config_file::{self, Coordination, ProjectConfig, UserConfig};
use crate::config_git::{self as git, GitWorkspace};
use crate::error::{ErrorCode, HiveError};

const PROJECT_FILE: &str = ".local/hive/config.yml";
const WORKSPACE_FILE: &str = "hive.yml";
const POLICY_FILE: &str = "AGENTS.md";
const ADOPTED_POLICY: &str = "0e584830a7eff314d85b7af8c6f8cd69e735730a";
const LEGACY_POLICY: &str = "0fd0f8dbe632d14094a93599cb1b5f653789ac9c";

pub(crate) enum Operation {
    Config(ConfigScope),
    Context,
    Doctor,
}

pub(crate) struct Report {
    pub(crate) data: Value,
    pub(crate) context: Option<Value>,
}

pub(crate) fn run(
    options: &CommonOptions,
    operation: Operation,
) -> (Format, Result<Report, HiveError>) {
    if !cfg!(unix) {
        return (
            options.format.unwrap_or_default(),
            Err(failure(
                ErrorCode::NotImplemented,
                "Configuration resolution is unavailable on this platform.",
            )),
        );
    }
    let home = absolute_env("HOME");
    let user = UserSource::load(home.as_deref());
    let format = options
        .format
        .or_else(|| user.as_ref().ok().and_then(UserSource::format))
        .unwrap_or_default();
    let resolver = Resolver {
        options,
        home,
        user,
        format,
    };
    (format, resolver.run(operation))
}

struct UserSource {
    path: Option<PathBuf>,
    config: Option<UserConfig>,
}

impl UserSource {
    fn load(home: Option<&Path>) -> Result<Self, HiveError> {
        let path = absolute_env("XDG_CONFIG_HOME")
            .or_else(|| home.map(|home| home.join(".config")))
            .map(|root| root.join("hive/config.yml"));
        let config = match &path {
            Some(path) => config_file::read_user(path)
                .map_err(|error| error.diagnostic(Some("hive/config.yml")))?,
            None => None,
        };
        Ok(Self { path, config })
    }

    fn format(&self) -> Option<Format> {
        self.config.as_ref()?.format
    }

    fn workspace(&self, home: Option<&Path>) -> Result<Option<PathBuf>, HiveError> {
        let Some(config) = &self.config else {
            return Ok(None);
        };
        let base = self
            .path
            .as_deref()
            .and_then(Path::parent)
            .ok_or_else(|| invalid("The user configuration path has no parent directory."))?;
        config
            .workspace
            .as_deref()
            .map(|path| resolve_path(path, base, home))
            .transpose()
    }
}

struct ProjectSource {
    root: PathBuf,
    path: PathBuf,
    config: Option<ProjectConfig>,
}

impl ProjectSource {
    fn workspace(&self, home: Option<&Path>) -> Result<Option<PathBuf>, HiveError> {
        let Some(config) = &self.config else {
            return Ok(None);
        };
        let base = self.root.join(".local/hive");
        config
            .workspace
            .as_deref()
            .map(|path| resolve_path(path, &base, home))
            .transpose()
    }
}

struct Resolver<'a> {
    options: &'a CommonOptions,
    home: Option<PathBuf>,
    user: Result<UserSource, HiveError>,
    format: Format,
}

struct Selection {
    workspace: GitWorkspace,
    project: Option<ProjectId>,
    project_root: Option<PathBuf>,
    workspace_source: &'static str,
    project_source: Option<&'static str>,
}

impl Resolver<'_> {
    fn run(self, operation: Operation) -> Result<Report, HiveError> {
        if matches!(self.options.view, View::Authority) {
            return Err(failure(
                ErrorCode::NotImplemented,
                "Authority inspection is unavailable in this build.",
            ));
        }

        match operation {
            Operation::Config(ConfigScope::User) => {
                self.require_local_source()?;
                let user = self.user()?;
                Ok(source_report(json!({
                    "scope": "user",
                    "present": user.config.is_some(),
                    "source": json_path_option(user.path.as_deref())?,
                    "workspace": json_path_option(user.workspace(self.home.as_deref())?.as_deref())?,
                    "output_format": user.format().map(Format::as_str)
                })))
            }
            Operation::Config(ConfigScope::Project) => {
                self.require_local_source()?;
                let project = self.project_source()?.ok_or_else(|| {
                    invalid("Project scope requires a verified Git project root.")
                })?;
                Ok(source_report(json!({
                    "scope": "project",
                    "present": project.config.is_some(),
                    "source": json_path(&project.path)?,
                    "project_root": json_path(&project.root)?,
                    "project_id": project.config.as_ref().map(|config| config.project_id.as_str()),
                    "workspace": json_path_option(project.workspace(self.home.as_deref())?.as_deref())?
                })))
            }
            Operation::Config(ConfigScope::Workspace) => {
                let selection = if let Some(path) = self.explicit_workspace()? {
                    Selection {
                        workspace: git::verify_workspace(&path)?,
                        project: None,
                        project_root: None,
                        workspace_source: "cli",
                        project_source: None,
                    }
                } else {
                    self.select()?
                };
                self.report(selection, Some("workspace"), false)
            }
            Operation::Config(ConfigScope::Resolved) => {
                let selection = self.select()?;
                self.report(selection, Some("resolved"), false)
            }
            Operation::Context | Operation::Doctor => {
                let selection = self.select()?;
                self.report(selection, None, matches!(operation, Operation::Doctor))
            }
        }
    }

    fn require_local_source(&self) -> Result<(), HiveError> {
        if matches!(self.options.view, View::WorkingTree) {
            Ok(())
        } else {
            Err(failure(
                ErrorCode::InvalidArgument,
                "This configuration source does not have a Git snapshot view.",
            ))
        }
    }

    fn user(&self) -> Result<&UserSource, HiveError> {
        self.user.as_ref().map_err(Clone::clone)
    }

    fn explicit_workspace(&self) -> Result<Option<PathBuf>, HiveError> {
        self.options
            .workspace
            .as_deref()
            .map(|path| resolve_path(path, &current_dir()?, self.home.as_deref()))
            .transpose()
    }

    fn project_source(&self) -> Result<Option<ProjectSource>, HiveError> {
        let cwd = current_dir()?;
        let explicit = self
            .options
            .project_root
            .as_deref()
            .map(|path| resolve_path(path, &cwd, self.home.as_deref()))
            .transpose()?;
        let explicit_root = explicit
            .as_deref()
            .map(|path| {
                fs::canonicalize(path)
                    .map_err(|_| invalid("The explicit project root is unavailable."))
            })
            .transpose()?;
        let root = git::project_root(explicit_root.as_deref().unwrap_or(&cwd))?;
        if explicit_root.is_some() && root != explicit_root {
            return Err(invalid(
                "The explicit project root must be the root of a Git worktree.",
            ));
        }
        let Some(root) = root else {
            return Ok(None);
        };
        let relative = Path::new(PROJECT_FILE);
        let config = match config_file::read_scoped(&root, relative)
            .map_err(|error| error.diagnostic(Some(PROJECT_FILE)))?
        {
            Some(bytes) => {
                if git::is_tracked(&root, relative)? || !git::is_ignored(&root, relative)? {
                    return Err(invalid(
                        "The project configuration must be ignored and untracked.",
                    ));
                }
                Some(
                    config_file::parse_project(&bytes)
                        .map_err(|error| error.diagnostic(Some(PROJECT_FILE)))?,
                )
            }
            None => None,
        };
        let path = root.join(relative);
        Ok(Some(ProjectSource { root, path, config }))
    }

    fn select(&self) -> Result<Selection, HiveError> {
        let user = self.user()?;
        let explicit_workspace = self.explicit_workspace()?;
        let user_workspace = user.workspace(self.home.as_deref())?;
        let project = self.project_source()?;
        let project_workspace = project
            .as_ref()
            .map(|source| source.workspace(self.home.as_deref()))
            .transpose()?
            .flatten();

        let (path, workspace_source) = if let Some(path) = explicit_workspace.as_ref() {
            (path, "cli")
        } else if let Some(path) = project_workspace.as_ref() {
            (path, "project")
        } else if let Some(path) = user_workspace.as_ref() {
            (path, "user")
        } else {
            return Err(invalid(
                "Specify a workspace or configure a workspace default.",
            ));
        };
        let workspace = git::verify_workspace(path)?;
        let binding = project
            .as_ref()
            .and_then(|source| source.config.as_ref().map(|config| (source, config)));
        let (project_id, project_source) = if let Some(id) = &self.options.project {
            (Some(id.clone()), Some("cli"))
        } else if let Some((_, binding)) = binding {
            (Some(binding.project_id.clone()), Some("project"))
        } else {
            (None, None)
        };

        let mut selected_project_root = None;
        if let Some((source, binding)) = binding {
            let path = project_workspace
                .as_ref()
                .or(user_workspace.as_ref())
                .ok_or_else(|| invalid("The project binding has no workspace target."))?;
            let bound = git::verify_workspace(path)?;
            let same_workspace = bound.root == workspace.root;
            let changes_target =
                !same_workspace || project_id.as_ref() != Some(&binding.project_id);
            if changes_target && (explicit_workspace.is_none() || self.options.project.is_none()) {
                return Err(failure(
                    ErrorCode::AmbiguousScope,
                    "Changing a project binding requires an explicit workspace and project together.",
                ));
            }
            let revision = if same_workspace {
                self.view_revision()
            } else {
                None
            };
            verify_project_remote(&bound, &source.root, &binding.project_id, revision)?;
            if !changes_target {
                selected_project_root = Some(source.root.clone());
            }
            // A complete override cannot bypass an invalid binding in another workspace.
            if !same_workspace {
                read_workspace(&bound, None)?;
            }
        }

        if let (Some(root), Some(project_id)) = (
            project
                .as_ref()
                .filter(|_| self.options.project_root.is_some()),
            project_id.as_ref(),
        ) && matches!(project_id, ProjectId::Ordinary(_))
        {
            verify_project_remote(&workspace, &root.root, project_id, self.view_revision())?;
            selected_project_root = Some(root.root.clone());
        }

        Ok(Selection {
            workspace,
            project: project_id,
            project_root: selected_project_root,
            workspace_source,
            project_source,
        })
    }

    fn view_revision(&self) -> Option<&str> {
        match &self.options.view {
            View::Commit(revision) => Some(revision.as_str()),
            _ => None,
        }
    }

    fn report(
        &self,
        selection: Selection,
        scope: Option<&str>,
        doctor: bool,
    ) -> Result<Report, HiveError> {
        let workspace = &selection.workspace;
        let view_revision = self.view_revision();
        let (record_policy, coordination) = read_workspace(workspace, view_revision)?;
        if let Some(project) = &selection.project {
            verify_scope(workspace, project, view_revision)?;
        }
        let coordination = coordination.map(|coordination| match coordination {
            Coordination::Local => json!({"mode": "local", "ref": "refs/heads/main"}),
            Coordination::Remote(remote) => {
                json!({"mode": "remote", "remote": remote, "ref": "refs/heads/main"})
            }
        });
        let workspace_root = json_path(&workspace.root)?;
        let mut context = json!({
            "workspace_root": workspace_root,
            "project_id": selection.project.as_ref().map(ProjectId::as_str),
            "policy_revision": record_policy,
            "view": if view_revision.is_some() { "commit" } else { "working-tree" },
            "revision": view_revision.or(workspace.revision.as_deref()),
            "freshness": "local"
        });
        if view_revision.is_none() {
            context["dirty"] = json!(git::status(&workspace.root)?);
        }
        let mut data = json!({
            "scope": scope,
            "workspace_root": workspace_root,
            "project_id": selection.project.as_ref().map(ProjectId::as_str),
            "project_root": json_path_option(selection.project_root.as_deref())?,
            "policy_revision": record_policy,
            "coordination": coordination,
            "output_format": self.format.as_str(),
            "sources": without_unavailable(json!({
                "workspace": selection.workspace_source,
                "project": selection.project_source,
                "output_format": if self.options.format.is_some() { "cli" }
                    else if self.user.as_ref().ok().and_then(UserSource::format).is_some() { "user" }
                    else { "default" }
            }))
        });
        if coordination.is_some() {
            data["coordination_source"] = json!(if view_revision.is_some() {
                "snapshot"
            } else {
                "workspace"
            });
        }
        if scope == Some("workspace") {
            data["present"] = json!(coordination.is_some());
            data["source"] = json!(json_path(&workspace.root.join(WORKSPACE_FILE))?);
        }
        if doctor {
            data["checks"] = json!({
                "git": "available",
                "configuration": "valid",
                "policy": "compatible",
                "workspace": "readable"
            });
        }
        Ok(Report {
            data: without_unavailable(data),
            context: Some(without_unavailable(context)),
        })
    }
}

fn source_report(data: Value) -> Report {
    Report {
        data: without_unavailable(data),
        context: None,
    }
}

fn without_unavailable(mut data: Value) -> Value {
    if let Some(object) = data.as_object_mut() {
        object.retain(|_, value| !value.is_null());
    }
    data
}

fn json_path(path: &Path) -> Result<&str, HiveError> {
    path.to_str()
        .ok_or_else(|| invalid("A configuration path cannot be represented as UTF-8."))
}

fn json_path_option(path: Option<&Path>) -> Result<Option<&str>, HiveError> {
    path.map(json_path).transpose()
}

fn read_workspace(
    workspace: &GitWorkspace,
    revision: Option<&str>,
) -> Result<(&'static str, Option<Coordination>), HiveError> {
    let policy = read_policy(workspace, revision)?;
    let file = reviewed_file(workspace, revision, Path::new(WORKSPACE_FILE))
        .map_err(|error| error.diagnostic(Some(WORKSPACE_FILE)))?;
    let coordination = match (policy, file) {
        (LEGACY_POLICY, None) => None,
        (LEGACY_POLICY, Some(_)) => {
            return Err(failure(
                ErrorCode::IncompatiblePolicy,
                "Coordination requires an adopted workspace policy.",
            ));
        }
        (_, None) => {
            return Err(invalid(
                "The adopted workspace requires a reviewed hive.yml.",
            ));
        }
        (_, Some(bytes)) => Some(
            config_file::parse_workspace(&bytes)
                .map_err(|error| error.diagnostic(Some(WORKSPACE_FILE)))?
                .coordination,
        ),
    };
    if let (None, Some(Coordination::Remote(remote))) = (revision, &coordination) {
        git::verify_remote(&workspace.root, remote)?;
    }
    Ok((policy, coordination))
}

fn read_policy(
    workspace: &GitWorkspace,
    revision: Option<&str>,
) -> Result<&'static str, HiveError> {
    let bytes = reviewed_file(workspace, revision, Path::new(POLICY_FILE))
        .map_err(|error| error.diagnostic(Some(POLICY_FILE)))?
        .ok_or_else(|| {
            failure(
                ErrorCode::PolicyUnavailable,
                "The workspace has no reviewed AGENTS.md policy pin.",
            )
            .diagnostic(Some(POLICY_FILE))
        })?;
    let text = std::str::from_utf8(&bytes).map_err(|_| {
        failure(
            ErrorCode::PolicyUnavailable,
            "The workspace policy pin is unreadable.",
        )
    })?;
    let mut pins = reference_tokens(text).filter_map(policy_pin);
    let first = pins.next().ok_or_else(|| {
        failure(
            ErrorCode::PolicyUnavailable,
            "The workspace has no full system policy revision.",
        )
    })?;
    if pins.any(|pin| !pin.eq_ignore_ascii_case(first)) {
        return Err(failure(
            ErrorCode::IncompatiblePolicy,
            "The workspace declares conflicting policy revisions.",
        ));
    }
    if first.eq_ignore_ascii_case(ADOPTED_POLICY) {
        Ok(ADOPTED_POLICY)
    } else if first.eq_ignore_ascii_case(LEGACY_POLICY) {
        Ok(LEGACY_POLICY)
    } else {
        Err(failure(
            ErrorCode::IncompatiblePolicy,
            "The workspace policy revision is unsupported.",
        ))
    }
}

fn policy_pin(token: &str) -> Option<&str> {
    let pin = if let Some(pin) = token.strip_prefix("hive-system@") {
        pin
    } else {
        let relative = [
            "https://github.com/mols3131d/hive-system/blob/",
            "https://github.com/mols3131d/hive-system/tree/",
            "https://raw.githubusercontent.com/mols3131d/hive-system/",
        ]
        .into_iter()
        .find_map(|prefix| token.strip_prefix(prefix))?;
        let (pin, path) = relative.split_once('/')?;
        if !matches!(
            path.split('#').next()?,
            "docs/policies/workspace"
                | "docs/policies/workspace/"
                | "docs/policies/workspace/README.md"
        ) {
            return None;
        }
        pin
    };
    (pin.len() == 40 && pin.bytes().all(|byte| byte.is_ascii_hexdigit())).then_some(pin)
}

fn reference_tokens(text: &str) -> impl Iterator<Item = &str> {
    text.split(|character: char| {
        character.is_whitespace()
            || matches!(
                character,
                '(' | ')' | '[' | ']' | '<' | '>' | '"' | '\'' | '`'
            )
    })
    .map(|token| token.trim_end_matches([',', ';', '.']))
    .filter(|token| !token.is_empty())
}

fn verify_scope(
    workspace: &GitWorkspace,
    project: &ProjectId,
    revision: Option<&str>,
) -> Result<Option<String>, HiveError> {
    let mut found = false;
    let mut reference = None;
    for area in ["tasks", "directives"] {
        let relative = Path::new(area).join(project.as_str()).join("README.md");
        let Some(bytes) = reviewed_file(workspace, revision, &relative)? else {
            continue;
        };
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| invalid("A project scope entrypoint is unreadable."))?;
        found = true;
        if !matches!(project, ProjectId::Ordinary(_)) {
            continue;
        }
        let declared: Vec<_> = text
            .lines()
            .filter_map(|line| line.trim().strip_prefix("Project:"))
            .collect();
        let identities: Vec<_> = if declared.is_empty() {
            text.lines().filter_map(project_identity).collect()
        } else {
            declared
                .into_iter()
                .map(project_identity)
                .collect::<Option<_>>()
                .ok_or_else(|| {
                    failure(
                        ErrorCode::AmbiguousScope,
                        "A project scope has an invalid canonical reference.",
                    )
                })?
        };
        let mut identity = identities.into_iter();
        let Some(first) = identity.next() else {
            return Err(failure(
                ErrorCode::AmbiguousScope,
                "A project scope has no verified canonical reference.",
            ));
        };
        if identity.any(|other| other != first)
            || reference.as_ref().is_some_and(|other| other != &first)
        {
            return Err(failure(
                ErrorCode::AmbiguousScope,
                "Project scope entrypoints disagree on their canonical reference.",
            ));
        }
        reference = Some(first);
    }
    if found {
        Ok(reference)
    } else {
        Err(failure(
            ErrorCode::NotFound,
            "The selected project has no reviewed scope entrypoint.",
        ))
    }
}

fn verify_project_remote(
    workspace: &GitWorkspace,
    project_root: &Path,
    project: &ProjectId,
    revision: Option<&str>,
) -> Result<(), HiveError> {
    let reference = verify_scope(workspace, project, revision)?;
    if let Some(reference) = reference
        && git::project_fetch_urls(project_root)?
            .iter()
            .any(|url| url == &reference)
    {
        Ok(())
    } else {
        Err(failure(
            ErrorCode::AmbiguousScope,
            "The Git project does not match the workspace's canonical project reference.",
        ))
    }
}

fn project_identity(reference: &str) -> Option<String> {
    let value = reference.trim();
    let value = if value.starts_with('[') {
        let (_, target) = value.split_once("](")?;
        target.strip_suffix(')')?.trim()
    } else {
        value
    };
    let value = if let Some(target) = value.strip_prefix('<') {
        target.strip_suffix('>')?
    } else {
        value
    };
    if value.contains(['?', '#']) || value.chars().any(char::is_control) {
        return None;
    }
    let path = if let Some((protocol, rest)) = value.split_once("://") {
        match protocol {
            "https" | "http" | "ssh" | "git" => {
                let (host, path) = rest.split_once('/')?;
                if host.rsplit('@').next()?.is_empty() {
                    return None;
                }
                path
            }
            "file" if rest.starts_with('/') => rest,
            _ => return None,
        }
    } else if let Some((host, path)) = value.split_once(':') {
        if !host.contains('@') {
            return None;
        }
        path
    } else if value.starts_with('/') {
        value
    } else {
        return None;
    };
    let path = path.trim_matches('/');
    if path.is_empty() || path.split('/').any(|part| matches!(part, "" | "." | "..")) {
        return None;
    }
    // Different SSH users, transports and path spellings can refer to
    // different repositories on the same server.
    Some(value.to_owned())
}

fn reviewed_file(
    workspace: &GitWorkspace,
    revision: Option<&str>,
    relative: &Path,
) -> Result<Option<Vec<u8>>, HiveError> {
    let committed_revision = revision.or(workspace.revision.as_deref()).ok_or_else(|| {
        failure(
            ErrorCode::PolicyUnavailable,
            "The workspace has no committed policy.",
        )
    })?;
    let committed = git::read_blob(&workspace.root, committed_revision, relative)?;
    if revision.is_none()
        && (config_file::read_scoped(&workspace.root, relative)? != committed
            || (committed.is_some() && !git::is_tracked(&workspace.root, relative)?))
    {
        return Err(invalid(
            "A workspace configuration or identity file differs from its reviewed Git version.",
        ));
    }
    Ok(committed)
}

fn absolute_env(name: &str) -> Option<PathBuf> {
    env::var_os(name)
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
}

fn current_dir() -> Result<PathBuf, HiveError> {
    env::current_dir().map_err(|_| {
        failure(
            ErrorCode::IoError,
            "The invocation directory is unavailable.",
        )
    })
}

fn resolve_path(path: &Path, base: &Path, home: Option<&Path>) -> Result<PathBuf, HiveError> {
    let display = path.to_string_lossy();
    if path.as_os_str().is_empty()
        || display.contains("://")
        || display.chars().any(char::is_control)
    {
        return Err(invalid("A configured target must be a local path."));
    }
    if display.starts_with("~/") {
        let home =
            home.ok_or_else(|| invalid("Home expansion requires an absolute home directory."))?;
        let relative = path
            .strip_prefix("~")
            .map_err(|_| invalid("Invalid home-relative path."))?;
        Ok(home.join(relative))
    } else {
        Ok(base.join(path))
    }
}

fn invalid(message: &'static str) -> HiveError {
    failure(ErrorCode::InvalidConfig, message)
}

fn failure(code: ErrorCode, message: impl Into<String>) -> HiveError {
    HiveError::new(code, message)
}
