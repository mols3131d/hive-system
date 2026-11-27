//! Entry point for explicit current context resolveion. Policy preflight can
//! complete independently of the workspace runtime's snapshot pairing check.

use crate::cli::{CommonOptions, ContextResolveArgs, ContextRole};
use crate::error::{ErrorCode, HiveError};

use super::{DoltFailure, DoltSuccess, DoltTarget, is_uuid, is_uuid_v7};

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

pub(crate) fn resolve(
    common: &CommonOptions,
    args: &ContextResolveArgs,
) -> Result<DoltSuccess, Box<DoltFailure>> {
    let (role, budget) = selection(common, args).map_err(DoltFailure::without_context)?;
    let target = DoltTarget::parse(&args.target).map_err(DoltFailure::without_context)?;
    preflight(common, args, target, role, budget)
}

fn selection(
    common: &CommonOptions,
    args: &ContextResolveArgs,
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
    if args.include_artifact.len() > crate::cli::MAX_CONTEXT_ARTIFACTS {
        return Err(HiveError::new(
            ErrorCode::ContextLimit,
            "Select at most 25 explicit Artifact bodies.",
        ));
    }
    let mut artifacts = std::collections::BTreeSet::new();
    if args
        .include_artifact
        .iter()
        .any(|id| !is_uuid_v7(id) || !artifacts.insert(id))
    {
        return Err(invalid(
            "Each included Artifact must have a distinct exact lowercase UUIDv7.",
        ));
    }
    if !artifacts.is_empty() && args.issue.is_none() {
        return Err(HiveError::new(
            ErrorCode::Unauthorized,
            "Explicit Artifact body disclosure requires an exact Issue selection.",
        ));
    }
    if !artifacts.is_empty() && args.evidence_file.is_none() {
        return Err(HiveError::new(
            ErrorCode::AuthorityUnverified,
            "Explicit Artifact bodies require current workflow evidence.",
        ));
    }
    if args.invocation_id.as_deref().is_some_and(|id| !is_uuid(id)) {
        return Err(invalid("Supply a canonical lowercase invocation UUID."));
    }
    if args
        .asset_commit
        .as_deref()
        .is_some_and(|commit| !crate::config_git::is_full_lowercase_oid(commit))
    {
        return Err(invalid(
            "Supply a full lowercase Git asset commit object ID.",
        ));
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
    args: &ContextResolveArgs,
    target: DoltTarget<'_>,
    role: ContextRole,
    budget: usize,
) -> Result<DoltSuccess, Box<DoltFailure>> {
    use crate::{config, config_v2};
    use serde_json::json;

    let workspace =
        config_v2::read_explicit(common.workspace.as_deref().expect("validated workspace"))
            .map_err(DoltFailure::without_context)?;
    if target.workspace_id != workspace.workspace_id() {
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
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        use super::context_evidence::{CheckedEvidence, EvidenceFile, Selection};

        let observed_at =
            observe_time().map_err(|error| DoltFailure::with_context(context.clone(), error))?;
        let evidence = args
            .evidence_file
            .as_deref()
            .map(EvidenceFile::read)
            .transpose()
            .map_err(|error| DoltFailure::with_context(context.clone(), error))?;
        let checked = match evidence.as_ref() {
            Some(file) => file
                .validate(
                    &Selection {
                        workspace_id: workspace.workspace_id(),
                        actor_ref: args.actor.as_deref().expect("validated actor"),
                        bot_id: args.bot.as_deref(),
                        invocation_id: args.invocation_id.as_deref(),
                        issue: args.issue.as_deref(),
                        area: args.area.as_deref(),
                        artifacts: &args.include_artifact,
                    },
                    observed_at,
                )
                .map_err(|error| DoltFailure::with_context(context.clone(), error))?,
            None => CheckedEvidence::default(),
        };
        #[cfg(target_os = "linux")]
        if let Some(asset_commit) = args.asset_commit.as_deref() {
            return crate::workspace_status::with_owned_snapshot(&workspace, target, |snapshot| {
                super::issue::context::resolve_snapshot(
                    snapshot,
                    &workspace,
                    common,
                    args,
                    asset_commit,
                    &VerifiedInputs {
                        role,
                        body: &policy,
                        budget,
                        observed_at,
                        evidence: checked,
                    },
                )
            })
            .map_err(|error| DoltFailure::with_context(context, error));
        }
        #[cfg(target_os = "macos")]
        let _ = checked;
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    if args.evidence_file.is_some() {
        return Err(DoltFailure::with_context(
            context,
            HiveError::new(
                ErrorCode::NotImplemented,
                "Safe context evidence files require Linux or macOS.",
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

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn observe_time() -> Result<time::OffsetDateTime, HiveError> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|duration| {
            time::OffsetDateTime::from_unix_timestamp_nanos(duration.as_nanos() as i128).ok()
        })
        .ok_or_else(|| {
            HiveError::new(
                ErrorCode::RuntimeUnavailable,
                "The context observation time is unavailable.",
            )
        })
}

#[cfg(target_os = "linux")]
pub(super) struct VerifiedInputs<'a> {
    pub(super) role: ContextRole,
    pub(super) body: &'a str,
    pub(super) budget: usize,
    pub(super) observed_at: time::OffsetDateTime,
    pub(super) evidence: super::context_evidence::CheckedEvidence<'a>,
}

#[cfg(not(unix))]
fn preflight(
    _common: &CommonOptions,
    _args: &ContextResolveArgs,
    _target: DoltTarget<'_>,
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
