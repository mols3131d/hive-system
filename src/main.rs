mod cli;
mod config;
mod config_file;
mod config_git;
mod dolt;
mod error;
mod output;

use std::process::ExitCode;

use cli::{Command, ConfigCommand, Contract, Invocation, ParseFailure};
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
    let result = match invocation.command {
        Command::Validate(args) => dolt::validate(&args),
        _ => Err(HiveError::new(
            ErrorCode::NotImplemented,
            "This operation is unavailable under contract 2.",
        )),
    };
    let report = match result {
        Ok(report) => report,
        Err(error) => return output::failure(Contract::V2, format, Some(name), None, error),
    };
    let context = serde_json::json!({
        "workspace_id": report.workspace_id,
        "db_commit": report.db_commit,
        "scope": "workspace",
    });
    if !report.diagnostics.items.is_empty() {
        return output::failure(
            Contract::V2,
            format,
            Some(name),
            Some(&context),
            HiveError::record_violations(report.diagnostics.items, report.diagnostics.truncated),
        );
    }
    let data = serde_json::json!({
        "storage_schema_version": dolt::STORAGE_SCHEMA_VERSION,
        "diagnostics": report.diagnostics.items,
    });
    output::success(Contract::V2, format, name, Some(&context), &data)
}
