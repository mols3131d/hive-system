mod cli;
mod config;
mod config_file;
mod config_git;
mod error;
mod output;

use std::process::ExitCode;

use cli::{Command, ConfigCommand, Invocation, ParseFailure};
use error::{ErrorCode, HiveError};

fn main() -> ExitCode {
    match cli::parse() {
        Ok(invocation) => dispatch(invocation),
        Err(ParseFailure::Display(output)) => match output.print() {
            Ok(()) => ExitCode::SUCCESS,
            Err(_) => ExitCode::FAILURE,
        },
        Err(ParseFailure::Invalid {
            format,
            command,
            message,
        }) => output::failure(
            format,
            command.as_deref(),
            HiveError::new(ErrorCode::InvalidArgument, message),
        ),
    }
}

fn dispatch(invocation: Invocation) -> ExitCode {
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
                invocation.common.format.unwrap_or_default(),
                Some(name),
                HiveError::new(
                    ErrorCode::NotImplemented,
                    "This operation is not available in this build.",
                ),
            );
        }
    };
    let (format, result) = config::run(&invocation.common, operation);
    match result {
        Ok(report) => output::success(format, name, report.context.as_ref(), &report.data),
        Err(error) => output::failure(
            format,
            Some(name),
            if diagnose {
                error.diagnostic(None)
            } else {
                error
            },
        ),
    }
}
