use std::ffi::OsString;
use std::path::PathBuf;

use clap::{Arg, ArgAction, Args, FromArgMatches};

use super::{
    Command, CommonArgs, CommonOptions, Contract, DoltTargetArgs, Invocation, ParseFailure, View,
    matched_leaf, requested_contract, requested_format, retry_conversation_failure,
    safe_parse_message,
};

#[derive(Args)]
pub(crate) struct ConversationListArgs {
    /// Canonical lowercase UUIDv7 of the registered Queen.
    #[arg(long, value_name = "UUID", allow_negative_numbers = true)]
    pub(crate) bot: Option<OsString>,
    /// Page size (default 25, maximum 100).
    #[arg(long, default_value = "25", allow_negative_numbers = true)]
    pub(crate) limit: String,
    /// Continue from the previous page's next_cursor.
    #[arg(long, allow_negative_numbers = true)]
    pub(crate) cursor: Option<String>,
    #[command(flatten)]
    pub(crate) target: DoltTargetArgs,
}

#[derive(Args)]
pub(crate) struct ConversationImportArgs {
    /// Canonical lowercase UUIDv7 of the active registered Queen.
    #[arg(long, value_name = "UUID", allow_negative_numbers = true)]
    pub(crate) bot: Option<OsString>,
    /// Read Markdown from this regular file.
    #[arg(long, value_name = "PATH")]
    pub(crate) file: Option<PathBuf>,
    /// Read Markdown from standard input.
    #[arg(long)]
    pub(crate) stdin: bool,
    /// Optional single-line title.
    #[arg(long)]
    pub(crate) title: Option<String>,
    /// Optional opaque host session reference.
    #[arg(long)]
    pub(crate) session_ref: Option<String>,
    #[command(flatten)]
    pub(crate) target: DoltTargetArgs,
}

impl ConversationImportArgs {
    fn validate(&self, common: &CommonOptions) -> Result<(), &'static str> {
        if common.workspace.is_none() || !self.target.has_all() {
            return Err(
                "Conversation import requires --workspace, --workspace-id, --database, and --port.",
            );
        }
        if common.project.is_some()
            || common.project_root.is_some()
            || !matches!(common.view, View::WorkingTree)
        {
            return Err("Conversation import accepts only explicit workspace and Queen selection.");
        }
        if self.file.is_some() == self.stdin {
            return Err("Conversation import requires exactly one of --file or --stdin.");
        }
        Ok(())
    }
}

pub(super) fn parse(
    legacy: &clap::Command,
    args: &[OsString],
) -> Option<Result<Invocation, ParseFailure>> {
    if !args.iter().skip(1).any(|arg| arg == "conversation")
        || !args
            .iter()
            .skip(1)
            .any(|arg| arg == "list" || arg == "import")
    {
        return None;
    }
    let leaf = |name, about| {
        clap::Command::new(name)
            .about(about)
            .disable_help_flag(true)
            .arg(
                Arg::new("help")
                    .short('h')
                    .long("help")
                    .action(ArgAction::SetTrue)
                    .help("Print help"),
            )
    };
    let list =
        ConversationListArgs::augment_args(leaf("list", "List published metadata for one Queen."));
    let import = ConversationImportArgs::augment_args(leaf(
        "import",
        "Import explicitly selected Markdown for an active registered Queen.",
    ));
    let cli = legacy.clone().mut_subcommand("conversation", |command| {
        command
            .subcommand(CommonArgs::augment_args(list))
            .subcommand(CommonArgs::augment_args(import))
    });
    let (matches, error) = match cli.clone().try_get_matches_from(args) {
        Ok(matches) => (matches, None),
        Err(error) => {
            let partial = cli
                .clone()
                .ignore_errors(true)
                .try_get_matches_from(args)
                .ok();
            let matches = retry_conversation_failure(&cli, args, error.kind(), "list")
                .or_else(|| retry_conversation_failure(&cli, args, error.kind(), "import"))
                .or_else(|| {
                    super::project_v2::recover_unknown_long(
                        &cli,
                        args,
                        partial.as_ref()?,
                        &error,
                        "conversation",
                    )
                })
                .or(partial)?;
            (matches, Some(error.kind()))
        }
    };
    let (name, leaf) = matched_leaf(&cli, &matches)?;
    let action = match name.as_str() {
        "conversation list" => "list",
        "conversation import" => "import",
        _ => return None,
    };
    if requested_contract(Some(&matches), Some(leaf)) != Contract::V2 {
        return None;
    }
    if error.is_none() && leaf.get_flag("help") {
        let help = cli.mut_subcommand("conversation", |command| {
            command.mut_subcommand(action, |leaf| {
                leaf.mut_arg("help", |arg| arg.action(ArgAction::Help))
            })
        });
        return Some(Err(ParseFailure::Display(
            help.try_get_matches_from(args)
                .expect_err("matched v2 conversation help"),
        )));
    }
    let format = requested_format(Some(&matches), Some(leaf));
    let parsed = || -> Result<Invocation, &'static str> {
        if let Some(error) = error {
            return Err(safe_parse_message(error));
        }
        let root = CommonArgs::from_arg_matches(&matches)
            .map_err(|error| safe_parse_message(error.kind()))?;
        let options =
            CommonArgs::from_arg_matches(leaf).map_err(|error| safe_parse_message(error.kind()))?;
        let common = root.resolve(options)?;
        if action == "import" {
            let args = ConversationImportArgs::from_arg_matches(leaf)
                .map_err(|error| safe_parse_message(error.kind()))?;
            args.validate(&common)?;
            return Ok(Invocation {
                common,
                command: Command::ConversationImport(args),
                command_name: name.clone(),
            });
        }
        let args = ConversationListArgs::from_arg_matches(leaf)
            .map_err(|error| safe_parse_message(error.kind()))?;
        if common.workspace.is_none() || !args.target.has_all() {
            return Err(
                "Conversation list requires --workspace, --workspace-id, --database, and --port.",
            );
        }
        if common.project.is_some()
            || common.project_root.is_some()
            || !matches!(common.view, View::WorkingTree)
        {
            return Err("Conversation list accepts only explicit workspace and Queen selection.");
        }
        Ok(Invocation {
            common,
            command: Command::ConversationList(args),
            command_name: name.clone(),
        })
    };
    Some(parsed().map_err(|message| ParseFailure::Invalid {
        contract: Contract::V2,
        format,
        command: Some(name),
        message: message.to_owned(),
    }))
}
