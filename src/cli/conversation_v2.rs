use std::ffi::OsString;

use clap::{Arg, ArgAction, Args, FromArgMatches};

use super::{
    Command, CommonArgs, Contract, DoltTargetArgs, Invocation, ParseFailure, View, matched_leaf,
    requested_contract, requested_format, retry_conversation_failure, safe_parse_message,
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

pub(super) fn parse(
    legacy: &clap::Command,
    args: &[OsString],
) -> Option<Result<Invocation, ParseFailure>> {
    if !args.iter().skip(1).any(|arg| arg == "conversation")
        || !args.iter().skip(1).any(|arg| arg == "list")
    {
        return None;
    }
    let list = clap::Command::new("list")
        .about("List published metadata for one Queen.")
        .disable_help_flag(true)
        .arg(
            Arg::new("help")
                .short('h')
                .long("help")
                .action(ArgAction::SetTrue)
                .help("Print help"),
        );
    let list = CommonArgs::augment_args(ConversationListArgs::augment_args(list));
    let cli = legacy
        .clone()
        .mut_subcommand("conversation", |command| command.subcommand(list));
    let (matches, error) = match cli.clone().try_get_matches_from(args) {
        Ok(matches) => (matches, None),
        Err(error) => {
            let partial = cli
                .clone()
                .ignore_errors(true)
                .try_get_matches_from(args)
                .ok();
            let matches = retry_conversation_failure(&cli, args, error.kind(), "list")
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
    if name != "conversation list" || requested_contract(Some(&matches), Some(leaf)) != Contract::V2
    {
        return None;
    }
    if error.is_none() && leaf.get_flag("help") {
        let help = cli.mut_subcommand("conversation", |command| {
            command.mut_subcommand("list", |list| {
                list.mut_arg("help", |arg| arg.action(ArgAction::Help))
            })
        });
        return Some(Err(ParseFailure::Display(
            help.try_get_matches_from(args)
                .expect_err("matched conversation list help"),
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
