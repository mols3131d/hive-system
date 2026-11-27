mod cli;
mod config;
mod config_file;
mod config_git;
mod config_v2;
mod dolt;
mod error;
mod output;

use std::process::ExitCode;

use cli::{
    AreaCommand, BotCommand, Command, ConfigCommand, Contract, Invocation, IssueCommand,
    ParseFailure,
};
use error::{ErrorCode, HiveError};

fn main() -> ExitCode {
    match cli::parse() {
        Ok(invocation) => dispatch(invocation),
        Err(ParseFailure::Display(output)) => match output.print() {
            Ok(()) => ExitCode::SUCCESS,
            Err(_) => ExitCode::FAILURE,
        },
        Err(ParseFailure::Invalid {
            contract,
            format,
            command,
            message,
        }) => output::failure(
            contract,
            format,
            command.as_deref(),
            None,
            HiveError::new(ErrorCode::InvalidArgument, message),
        ),
    }
}

fn dispatch(invocation: Invocation) -> ExitCode {
    if invocation.common.contract == Contract::V2 {
        return dispatch_dolt(invocation);
    }
    let name = &invocation.command_name;
    let diagnose = matches!(
        &invocation.command,
        Command::Config(ConfigCommand::Validate(_)) | Command::Doctor
    );
    let operation = match invocation.command {
        Command::Config(ConfigCommand::Show(args) | ConfigCommand::Validate(args)) => {
            config::Operation::Config(args.scope)
        }
        Command::Context => config::Operation::Context,
        Command::Doctor => config::Operation::Doctor,
        _ => {
            return output::failure(
                Contract::V1,
                invocation.common.format.unwrap_or_default(),
                Some(name),
                None,
                HiveError::new(
                    ErrorCode::NotImplemented,
                    "This operation is not available in this build.",
                ),
            );
        }
    };
    let (format, result) = config::run(&invocation.common, operation);
    match result {
        Ok(report) => output::success(
            Contract::V1,
            format,
            name,
            report.context.as_ref(),
            &report.data,
        ),
        Err(error) => output::failure(
            Contract::V1,
            format,
            Some(name),
            None,
            if diagnose {
                error.diagnostic(None)
            } else {
                error
            },
        ),
    }
}

fn dispatch_dolt(invocation: Invocation) -> ExitCode {
    let name = &invocation.command_name;
    let format = invocation.common.format.unwrap_or_default();
    if matches!(
        invocation.command,
        Command::Config(ConfigCommand::Show(cli::ConfigScopeArgs {
            scope: cli::ConfigScope::Workspace,
        }))
    ) {
        let (format, result) = config_v2::show(&invocation.common);
        return match result {
            Ok(data) => output::success(Contract::V2, format, name, None, &data),
            Err(error) => output::failure(Contract::V2, format, Some(name), None, error),
        };
    }
    let result = match invocation.command {
        Command::Validate(args) => dolt::validate(&args),
        Command::Area(AreaCommand::List(args)) => dolt::area::list(&args),
        Command::Area(AreaCommand::Show(args)) => dolt::area::show(&args),
        Command::Bot(BotCommand::List {
            target,
            status,
            page,
            cursor,
            ..
        }) => dolt::bot::list(&target, status.as_deref(), page.limit, cursor.as_deref()),
        Command::Bot(BotCommand::Show { target, bot_id }) => dolt::bot::show(&target, &bot_id),
        Command::Issue(IssueCommand::Show(args)) => dolt::issue_show(&args),
        Command::Issue(IssueCommand::List(args)) => {
            dolt::issue_list(&args, invocation.common.project.as_ref())
        }
        _ => Err(dolt::DoltFailure::without_context(HiveError::new(
            ErrorCode::NotImplemented,
            "This operation is unavailable under contract 2.",
        ))),
    };
    match result {
        Ok(report) => output::success(
            Contract::V2,
            format,
            name,
            Some(&report.context),
            &report.data,
        ),
        Err(failure) => output::failure(
            Contract::V2,
            format,
            Some(name),
            failure.context.as_ref(),
            failure.error,
        ),
    }
}
