mod cli;
mod config;
mod config_file;
mod config_git;
mod config_v2;
mod dolt;
mod error;
mod output;
#[cfg(target_os = "linux")]
mod workspace_backup;
mod workspace_doctor;
mod workspace_init;
mod workspace_status;

use std::io::Write;
use std::process::ExitCode;

use cli::{
    AreaCommand, BotCommand, Command, ConfigCommand, ContextArgs, ContextCommand, Contract,
    ConversationCommand, DirectiveCommand, DoltArtifactCommand, Invocation, IssueCommand,
    ParseFailure, ResourceCommand, WorkspaceAction,
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
        Command::Config(ConfigCommand::Show(args)) => config::Operation::Config(args.target.scope),
        Command::Config(ConfigCommand::Validate(args)) => config::Operation::Config(args.scope),
        Command::Context(_) => config::Operation::Context,
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
    if let Command::Backup(command) = &invocation.command {
        let (format, result) = workspace_status::backup(&invocation.common, command);
        return match result {
            Ok(data) => {
                let status = output::success(Contract::V2, format, name, None, &data);
                if status != ExitCode::SUCCESS {
                    let _ = writeln!(
                        std::io::stderr().lock(),
                        "error: The backup was published, but its success response could not be written."
                    );
                }
                status
            }
            Err(error) => output::failure(Contract::V2, format, Some(name), None, error),
        };
    }
    if let Command::Workspace(action) = &invocation.command {
        let (format, result) = match action {
            WorkspaceAction::Init {
                target,
                register_runtime,
            } => (format, workspace_init::run(target, *register_runtime)),
            WorkspaceAction::Doctor { target, endpoint } => {
                workspace_doctor::observe(&invocation.common, target.as_deref(), endpoint)
            }
            WorkspaceAction::Runtime(command) => workspace_status::run(&invocation.common, command),
        };
        return match result {
            Ok(data) => output::success(Contract::V2, format, name, None, &data),
            Err(error) => output::failure(Contract::V2, format, Some(name), None, error),
        };
    }
    if let Command::Config(ConfigCommand::Show(args)) = &invocation.command
        && matches!(
            args.target.scope,
            cli::ConfigScope::Workspace | cli::ConfigScope::Resolved
        )
    {
        let (format, result) = config_v2::show(&invocation.common, args);
        return match result {
            Ok(data) => output::success(Contract::V2, format, name, None, &data),
            Err(error) => output::failure(Contract::V2, format, Some(name), None, error),
        };
    }
    let result = match invocation.command {
        Command::DoltArtifact(DoltArtifactCommand::List(args)) => dolt::artifact::list(&args),
        Command::DoltArtifact(DoltArtifactCommand::Show(args)) => dolt::artifact::show(&args),
        Command::DoltArtifact(DoltArtifactCommand::Create(args)) => {
            dolt::artifact::create(&invocation.common, &args)
        }
        Command::Validate(args) => dolt::validate(&args),
        Command::Operation(cli::OperationCommand::Show(args)) => {
            dolt::operation::show(&invocation.common, &args)
        }
        Command::Context(ContextArgs {
            command: Some(ContextCommand::Resolve(args)),
        }) => dolt::context_resolve::resolve(&invocation.common, &args),
        Command::Area(AreaCommand::List(args)) => dolt::area::list(&args),
        Command::Area(AreaCommand::Tree(args)) => dolt::area::tree(&args),
        Command::Area(AreaCommand::Show(args)) => dolt::area::show(&args),
        Command::Area(AreaCommand::Create(args)) => dolt::area::create(&invocation.common, &args),
        Command::Label(cli::LabelCommand::List(args)) => {
            dolt::label::list(&args, invocation.common.project.as_ref())
        }
        Command::Resource(ResourceCommand::List(args)) => dolt::resource::list(&args),
        Command::Resource(ResourceCommand::Show(args)) => dolt::resource::show(&args),
        Command::ProjectRead(args) => dolt::project_read(&args),
        Command::Bot(BotCommand::List {
            target,
            status,
            page,
            cursor,
            ..
        }) => dolt::bot::list(&target, status.as_deref(), page.limit, cursor.as_deref()),
        Command::Bot(BotCommand::Show {
            target,
            bot_id,
            asset_commit,
        }) => dolt::bot::show(
            &target,
            &bot_id,
            invocation
                .common
                .workspace
                .as_deref()
                .zip(asset_commit.as_deref()),
        ),
        Command::Directive(DirectiveCommand::List {
            selection,
            status,
            page,
            cursor,
        }) => dolt::directive::list(
            &selection,
            invocation.common.project.as_ref(),
            status,
            page.limit,
            cursor.as_deref(),
        ),
        Command::Directive(DirectiveCommand::Show { selection, id }) => {
            dolt::directive::show(&selection, invocation.common.project.as_ref(), &id)
        }
        Command::Issue(IssueCommand::Show(args)) => dolt::issue_show(&args),
        Command::IssueNote(args) => dolt::issue_note::write(&invocation.common, &args),
        Command::IssueCreate(args) => dolt::issue_create::write(&invocation.common, &args),
        Command::IssueUpdate(args) => dolt::issue_update::write(&invocation.common, &args),
        Command::ConversationImport(args) => dolt::conversation_import(&invocation.common, &args),
        Command::Conversation(ConversationCommand::Show(args)) => {
            dolt::conversation_read(&invocation.common, &args, None)
        }
        Command::Conversation(ConversationCommand::Export {
            selection,
            destination: Some(destination),
        }) => dolt::conversation_read(&invocation.common, &selection, Some(&destination)),
        Command::ConversationList(args) => dolt::conversation_list(&invocation.common, &args),
        Command::Issue(IssueCommand::List(args)) => dolt::issue_list(
            &args,
            invocation.common.project.as_ref(),
            dolt::IssueView::List,
        ),
        Command::Issue(IssueCommand::Ready(args)) => dolt::issue_list(
            &args,
            invocation.common.project.as_ref(),
            dolt::IssueView::Ready,
        ),
        Command::Issue(IssueCommand::Blocked(args)) => dolt::issue_list(
            &args,
            invocation.common.project.as_ref(),
            dolt::IssueView::Blocked,
        ),
        _ => Err(dolt::DoltFailure::without_context(HiveError::new(
            ErrorCode::NotImplemented,
            "This operation is unavailable under contract 2.",
        ))),
    };
    match result {
        Ok(report) => {
            let status = output::success(
                Contract::V2,
                format,
                name,
                Some(&report.context),
                &report.data,
            );
            if status != ExitCode::SUCCESS {
                let mut stderr = std::io::stderr().lock();
                match name.as_str() {
                    "conversation export" => {
                        let _ = writeln!(
                            stderr,
                            "error: The conversation export was published, but its success response could not be written."
                        );
                    }
                    "conversation import" => {
                        let _ = writeln!(
                            stderr,
                            "error: Conversation {} for Queen {} was published, but its success response could not be written.",
                            report.data["conversation_id"], report.data["bot_id"],
                        );
                    }
                    _ => {}
                }
            }
            status
        }
        Err(failure) => output::failure(
            Contract::V2,
            format,
            Some(name),
            failure.context.as_ref(),
            failure.error,
        ),
    }
}
