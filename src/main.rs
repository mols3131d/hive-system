mod cli;
mod error;
mod output;

use std::process::ExitCode;

use cli::ParseFailure;
use error::{ErrorCode, HiveError};

fn main() -> ExitCode {
    match cli::parse() {
        Ok(invocation) => output::failure(
            invocation.common.format.unwrap_or_default(),
            Some(&invocation.command_name),
            HiveError::new(
                ErrorCode::NotImplemented,
                "This operation is not available in this build.",
            ),
        ),
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
