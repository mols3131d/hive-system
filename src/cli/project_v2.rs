use std::ffi::OsString;

use clap::{Arg, ArgAction, Args, FromArgMatches, Subcommand};

use super::{
    Command, CommonArgs, Contract, DoltTargetArgs, Invocation, ParseFailure, matched_leaf,
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
        ProjectRead::augment_subcommands(
            clap::Command::new("project").about("Inspect committed Projects."),
        )
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
            let matches =
                recover_unknown_long(&cli, args, &partial, &error, "project").unwrap_or(partial);
            (matches, Some(error.kind()))
        }
    };
    let (name, leaf) = matched_leaf(&cli, &matches)?;
    if !matches!(name.as_str(), "project list" | "project show")
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
    if parse_error.is_none() && leaf.get_flag("help") {
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
    // Keep parsing through --help so contract selection can appear after it.
    CommonArgs::augment_args(command.disable_help_flag(true))
        .mut_arg("workspace", |arg| arg.hide(true))
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
