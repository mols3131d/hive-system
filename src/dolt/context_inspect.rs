//! Entry point for explicit current context inspection. Policy preflight can
//! complete independently of the workspace runtime's snapshot pairing check.

use crate::cli::{CommonOptions, ContextInspectArgs, ContextRole};
use crate::error::{ErrorCode, HiveError};

use super::{DoltFailure, DoltSuccess, is_uuid_v7};

const DEFAULT_BUDGET: usize = 32 * 1024;
const MAX_BUDGET: usize = 64 * 1024;

impl ContextRole {
    pub(super) fn policy_name(self) -> &'static str {
        match self {
            Self::Queen => "Queen",
            Self::Overlord => "Overlord",
            Self::Drone => "Drone",
        }
    }
}

pub(crate) fn inspect(
    common: &CommonOptions,
    args: &ContextInspectArgs,
) -> Result<DoltSuccess, Box<DoltFailure>> {
    let (role, budget) = selection(common, args).map_err(DoltFailure::without_context)?;
    preflight(common, args, role, budget)
}

fn selection(
    common: &CommonOptions,
    args: &ContextInspectArgs,
) -> Result<(ContextRole, usize), HiveError> {
    let actor = args
        .actor
        .as_deref()
        .ok_or_else(|| invalid("Select an explicit --actor."))?;
    if actor.is_empty()
        || actor.len() > 255
        || actor.trim() != actor
        || actor.chars().any(char::is_control)
    {
        return Err(invalid("Supply a bounded nonempty actor reference."));
    }
    let project = common.project.as_ref().map(|value| value.as_str());
    if [args.area.as_deref(), project, args.bot.as_deref()]
        .into_iter()
        .flatten()
        .any(|id| !is_uuid_v7(id))
    {
        return Err(invalid(
            "Supply exact lowercase UUIDv7 Area, Project and Bot selectors.",
        ));
    }
    if let Some(issue) = &args.issue {
        super::issue::parse_selector(issue)?;
    }
    if args.area.is_none() && project.is_none() && args.issue.is_none() {
        return Err(HiveError::new(
            ErrorCode::AmbiguousScope,
            "Select an Area, Project, or Issue explicitly.",
        ));
    }
    let role = match (args.bot.as_ref(), args.role) {
        (Some(_), Some(ContextRole::Overlord | ContextRole::Drone)) => {
            return Err(invalid("A registered Bot requires the Queen role."));
        }
        (Some(_), _) => ContextRole::Queen,
        (None, Some(role)) => role,
        (None, None) => return Err(invalid("Select a role or a registered Queen.")),
    };
    let budget = match args.budget.as_deref() {
        Some(value) => value
            .parse::<usize>()
            .ok()
            .filter(|bytes| (1..=MAX_BUDGET).contains(bytes))
            .ok_or_else(|| invalid("Supply an inline budget from 1 to 65536 bytes."))?,
        None => DEFAULT_BUDGET,
    };
    Ok((role, budget))
}

#[cfg(unix)]
fn preflight(
    common: &CommonOptions,
    args: &ContextInspectArgs,
    role: ContextRole,
    budget: usize,
) -> Result<DoltSuccess, Box<DoltFailure>> {
    use crate::{config, config_v2};
    use serde_json::json;

    let workspace =
        config_v2::read_explicit(common.workspace.as_deref().expect("validated workspace"))
            .map_err(DoltFailure::without_context)?;
    if args.target.workspace_id.as_deref() != Some(workspace.workspace_id()) {
        return Err(DoltFailure::without_context(HiveError::new(
            ErrorCode::AuthorityUnverified,
            "The descriptor identity differs from the explicit workspace ID.",
        )));
    }
    let policy = config::current_dir()
        .and_then(|cwd| {
            config::resolve_path(
                args.policy_root.as_deref().expect("validated policy root"),
                &cwd,
                config::absolute_env("HOME").as_deref(),
            )
        })
        .and_then(|root| read_role(&root, workspace.policy_revision(), role))
        .map_err(DoltFailure::without_context)?;
    let context = json!({"policy_revision": workspace.policy_revision()});
    if policy.len() > budget {
        return Err(DoltFailure::with_context(
            context,
            HiveError::new(
                ErrorCode::ContextLimit,
                "The required Role policy exceeds the inline context budget.",
            ),
        ));
    }
    Err(DoltFailure::with_context(
        context,
        HiveError::new(
            ErrorCode::AuthorityUnverified,
            "Current context requires verified pairing of the workspace DB endpoint and asset commit.",
        ),
    ))
}

#[cfg(not(unix))]
fn preflight(
    _common: &CommonOptions,
    _args: &ContextInspectArgs,
    _role: ContextRole,
    _budget: usize,
) -> Result<DoltSuccess, Box<DoltFailure>> {
    Err(DoltFailure::without_context(HiveError::new(
        ErrorCode::NotImplemented,
        "Safe workspace descriptor inspection requires Unix.",
    )))
}

#[cfg(unix)]
fn read_role(root: &std::path::Path, pinned: &str, role: ContextRole) -> Result<String, HiveError> {
    use crate::config_git;
    use std::path::Path;

    let unavailable = || {
        HiveError::new(
            ErrorCode::PolicyUnavailable,
            "The required local Role policy is unavailable.",
        )
    };
    let repository = config_git::verify_workspace(root).map_err(|_| unavailable())?;
    let installed = repository.revision.as_deref().ok_or_else(unavailable)?;
    if installed != pinned {
        return Err(HiveError::new(
            ErrorCode::IncompatiblePolicy,
            "The installed policy commit differs from the workspace descriptor pin.",
        ));
    }
    // The adopted contract owns the role table and the shared identity rules.
    // Read that section from the exact commit, independent of working files.
    let bytes = config_git::read_blob(
        &repository.root,
        installed,
        Path::new("docs/architecture/dolt-mvp-contract.md"),
    )
    .map_err(|_| unavailable())?
    .ok_or_else(unavailable)?;
    let text = std::str::from_utf8(&bytes).map_err(|_| unavailable())?;
    let (_, section) = text
        .split_once("\n### Bot and curated memory\n")
        .ok_or_else(unavailable)?;
    let (body, _) = section
        .split_once("\n#### Continuity boundaries and proposed MVP scope\n")
        .ok_or_else(unavailable)?;
    let body = body.trim();
    let prefix = format!("| {} |", role.policy_name());
    if body.contains('\0')
        || body
            .lines()
            .filter(|line| line.starts_with(&prefix))
            .count()
            != 1
    {
        return Err(unavailable());
    }
    Ok(body.to_owned())
}

fn invalid(message: &'static str) -> HiveError {
    HiveError::new(ErrorCode::InvalidArgument, message)
}
