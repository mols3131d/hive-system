use std::ffi::OsString;
use std::path::PathBuf;

use clap::{Arg, ArgAction, Args, FromArgMatches, Subcommand};

use super::{
    Command, CommonArgs, Contract, DoltTargetArgs, Invocation, ParseFailure, View, matched_leaf,
    requested_contract, requested_format, safe_parse_message,
};

// The conversation parser uses this path to the shared recovery helper.
pub(super) use super::recover_unknown_long;

#[derive(Subcommand)]
pub(crate) enum ProjectRead {
    /// List committed Project summaries.
    List(ProjectListArgs),
    /// Show a committed Project and its direct member Issue IDs.
    Show(ProjectShowArgs),
}

#[derive(Subcommand)]
pub(crate) enum ProjectWrite {
    /// Create an active Project.
    Create(ProjectCreateArgs),
    /// Add an Issue directly to an active Project.
    AddIssue(ProjectAddIssueArgs),
}

#[derive(Args)]
pub(crate) struct ProjectCreateArgs {
    #[arg(long)]
    pub(crate) name: String,
    #[command(flatten)]
    pub(crate) request: ProjectRequestArgs,
}

#[derive(Args)]
pub(crate) struct ProjectAddIssueArgs {
    #[arg(value_name = "PROJECT-ID")]
    pub(crate) project_id: String,
    #[arg(value_name = "ISSUE-ID")]
    pub(crate) issue_id: String,
    /// Exact owning Area UUIDv7.
    #[arg(long)]
    pub(crate) area: String,
    /// Expected revision as ID=REV. Supply once for the Project and once for the Issue.
    #[arg(long, required = true)]
    pub(crate) if_match: Vec<String>,
    #[command(flatten)]
    pub(crate) request: ProjectRequestArgs,
}

#[derive(Args)]
pub(crate) struct ProjectRequestArgs {
    #[arg(long)]
    pub(crate) actor: String,
    #[arg(long)]
    pub(crate) request_id: String,
    /// Current request-bound operator decision.
    #[arg(long)]
    pub(crate) evidence_file: PathBuf,
    #[command(flatten)]
    pub(crate) target: DoltTargetArgs,
}

impl ProjectWrite {
    pub(crate) fn request(&self) -> &ProjectRequestArgs {
        match self {
            Self::Create(args) => &args.request,
            Self::AddIssue(args) => &args.request,
        }
    }
}

#[derive(Args)]
pub(crate) struct ProjectListArgs {
    /// Filter by exact status (active, completed, archived).
    #[arg(long, num_args = 0..=1, default_missing_value = "", allow_negative_numbers = true)]
    pub(crate) status: Option<String>,
    /// Page size (default 25, maximum 100).
    #[arg(long, default_value = "25", num_args = 0..=1, default_missing_value = "", allow_negative_numbers = true)]
    pub(crate) limit: String,
    /// Continue from the previous page's next_cursor.
    #[arg(long, num_args = 0..=1, default_missing_value = "", allow_negative_numbers = true)]
    pub(crate) cursor: Option<String>,
    #[command(flatten)]
    pub(crate) target: DoltTargetArgs,
}

#[derive(Args)]
pub(crate) struct ProjectShowArgs {
    /// Canonical lowercase UUIDv7 Project selector.
    #[arg(value_name = "PROJECT-ID", allow_negative_numbers = true)]
    pub(crate) project_id: Option<String>,
    #[command(flatten)]
    pub(crate) target: DoltTargetArgs,
}

impl ProjectRead {
    pub(crate) fn target(&self) -> &DoltTargetArgs {
        match self {
            Self::List(args) => &args.target,
            Self::Show(args) => &args.target,
        }
    }
}

pub(super) fn parse(
    legacy: &clap::Command,
    args: &[OsString],
) -> Option<Result<Invocation, ParseFailure>> {
    if !args.iter().skip(1).any(|arg| arg == "project") {
        return None;
    }
    // Both grammars use Clap so values and the -- delimiter retain their
    // meaning. Opt into the replacement only after parsing contract 2.
    let cli = legacy.clone().mut_subcommand("project", |_| {
        ProjectWrite::augment_subcommands(ProjectRead::augment_subcommands(
            clap::Command::new("project").about("Inspect and plan Projects."),
        ))
        .mut_subcommands(add_options)
    });
    let (matches, parse_error) = match cli.clone().try_get_matches_from(args) {
        Ok(matches) => (matches, None),
        Err(error) => {
            let partial = cli
                .clone()
                .ignore_errors(true)
                .try_get_matches_from(args)
                .ok()?;
            let matches = super::retry_write_failure(
                &cli,
                args,
                error.kind(),
                "project",
                &["create", "add-issue"],
            )
            .or_else(|| recover_unknown_long(&cli, args, &partial, &error, "project"))
            .unwrap_or(partial);
            (matches, Some(error.kind()))
        }
    };
    let (name, leaf) = matched_leaf(&cli, &matches)?;
    let writes = matches!(name.as_str(), "project create" | "project add-issue");
    if !(writes || matches!(name.as_str(), "project list" | "project show"))
        || requested_contract(Some(&matches), Some(leaf)) != Contract::V2
    {
        return None;
    }
    let format = requested_format(Some(&matches), Some(leaf));
    let parsed = || -> Result<Invocation, &'static str> {
        if let Some(error) = parse_error {
            return Err(safe_parse_message(error));
        }
        let root = CommonArgs::from_arg_matches(&matches)
            .map_err(|error| safe_parse_message(error.kind()))?;
        let options =
            CommonArgs::from_arg_matches(leaf).map_err(|error| safe_parse_message(error.kind()))?;
        let common = root.resolve(options)?;
        let (_, project) = matches.subcommand().expect("matched project command");
        if writes {
            let command = ProjectWrite::from_arg_matches(project)
                .map_err(|error| safe_parse_message(error.kind()))?;
            if common.workspace.is_none() || !command.request().target.has_all() {
                return Err(
                    "Project mutations require --workspace, --workspace-id, --database, and --port.",
                );
            }
            if common.project.is_some()
                || common.project_root.is_some()
                || !matches!(common.view, View::WorkingTree)
            {
                return Err(
                    "Project mutations require an explicit current workspace and positional record IDs.",
                );
            }
            return Ok(Invocation {
                common,
                command: Command::ProjectWrite(command),
                command_name: name.clone(),
            });
        }
        let command = ProjectRead::from_arg_matches(project)
            .map_err(|error| safe_parse_message(error.kind()))?;
        let target = command.target();
        target.validate_common(
            &common,
            "Dolt Project reads accept only an explicit local database and workspace ID.",
        )?;
        if !target.has_all() {
            return Err("Dolt Project reads require --workspace-id, --database, and --port.");
        }
        Ok(Invocation {
            common,
            command: Command::ProjectRead(command),
            command_name: name.clone(),
        })
    };
    if matches!(
        parse_error,
        None | Some(clap::error::ErrorKind::MissingRequiredArgument)
    ) && leaf.get_flag("help")
    {
        let (_, action) = name.split_once(' ').expect("matched project action");
        let help = cli.mut_subcommand("project", |project| {
            project.mut_subcommand(action, |leaf| {
                leaf.mut_arg("help", |arg| arg.action(ArgAction::Help))
            })
        });
        return Some(Err(ParseFailure::Display(
            help.try_get_matches_from(args)
                .expect_err("matched project help"),
        )));
    }
    Some(parsed().map_err(|message| ParseFailure::Invalid {
        contract: Contract::V2,
        format,
        command: Some(name),
        message: message.to_owned(),
    }))
}

fn add_options(command: clap::Command) -> clap::Command {
    let read = matches!(command.get_name(), "list" | "show");
    // Keep parsing through --help so contract selection can appear after it.
    CommonArgs::augment_args(command.disable_help_flag(true))
        .mut_arg("workspace", |arg| arg.hide(read))
        .mut_arg("project", |arg| arg.hide(true))
        .mut_arg("project_root", |arg| arg.hide(true))
        .mut_arg("at", |arg| arg.hide(true))
        .mut_arg("authority", |arg| arg.hide(true))
        .arg(
            Arg::new("help")
                .short('h')
                .long("help")
                .action(ArgAction::SetTrue)
                .help("Print help"),
        )
}
