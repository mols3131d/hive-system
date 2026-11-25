use clap::{Args, Parser, Subcommand, ValueEnum};
use std::path::PathBuf;
use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum Format {
    Text,
    Json,
}

#[derive(Debug, Parser)]
#[command(name = "hive", version, about = "Synthetic Hive CLI contract probe")]
struct Cli {
    #[command(flatten)]
    before: Common,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Context(Common),
}

#[derive(Debug, Args)]
struct Common {
    #[arg(long)]
    workspace: Vec<PathBuf>,
    #[arg(long)]
    project: Vec<String>,
    #[arg(long)]
    project_root: Vec<PathBuf>,
    #[arg(long)]
    format: Vec<Format>,
    #[arg(long)]
    json: bool,
    #[arg(long)]
    at: Vec<String>,
    #[arg(long)]
    authority: bool,
}

#[derive(Debug, thiserror::Error)]
enum OptionConflict {
    #[error("Conflicting output options")]
    Output,
    #[error("Conflicting read views")]
    View,
    #[error("Repeated conflicting values for {0}")]
    Repeated(&'static str),
}

fn unique<'a, T: Eq>(
    before: &'a [T],
    after: &'a [T],
    option: &'static str,
) -> Result<Option<&'a T>, OptionConflict> {
    let mut values = before.iter().chain(after);
    let first = values.next();
    if let Some(first) = first
        && values.any(|value| value != first)
    {
        return Err(OptionConflict::Repeated(option));
    }
    Ok(first)
}

#[derive(Serialize)]
struct Failure<'a> {
    schema_version: u8,
    ok: bool,
    command: &'a str,
    error: ErrorDetail<'a>,
    warnings: Vec<&'a str>,
}

#[derive(Serialize)]
struct ErrorDetail<'a> {
    code: &'a str,
    message: &'a str,
}

impl Cli {
    fn after(&self) -> &Common {
        match &self.command {
            Command::Context(common) => common,
        }
    }

    fn json_requested(&self) -> bool {
        self.before.json
            || self.after().json
            || self.before.format.contains(&Format::Json)
            || self.after().format.contains(&Format::Json)
    }

    fn output_format(&self) -> Result<Format, OptionConflict> {
        let explicit = unique(&self.before.format, &self.after().format, "--format")?;
        match (self.before.json || self.after().json, explicit) {
            (true, Some(Format::Text)) => Err(OptionConflict::Output),
            (true, _) => Ok(Format::Json),
            (false, value) => Ok(value.copied().unwrap_or(Format::Text)),
        }
    }

    fn project(&self) -> Result<Option<&String>, OptionConflict> {
        unique(&self.before.project, &self.after().project, "--project")
    }

    fn validate_options(&self) -> Result<(), OptionConflict> {
        self.output_format()?;
        self.project()?;
        unique(&self.before.workspace, &self.after().workspace, "--workspace")?;
        unique(
            &self.before.project_root,
            &self.after().project_root,
            "--project-root",
        )?;
        let at = unique(&self.before.at, &self.after().at, "--at")?;
        if at.is_some() && (self.before.authority || self.after().authority) {
            return Err(OptionConflict::View);
        }
        Ok(())
    }
}

fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    let conflict = cli.validate_options().err();
    let (code, message) = match &conflict {
        Some(error) => ("INVALID_ARGUMENT", error.to_string()),
        None => (
            "INTERNAL_ERROR",
            "Design probe only; no workspace operation".to_owned(),
        ),
    };
    if cli.json_requested() {
        let failure = Failure {
            schema_version: 1,
            ok: false,
            command: "context",
            error: ErrorDetail {
                code,
                message: &message,
            },
            warnings: vec![],
        };
        println!(
            "{}",
            serde_json::to_string(&failure).expect("serializable failure")
        );
    } else {
        eprintln!("{message}");
    }
    std::process::ExitCode::from(if conflict.is_some() { 2 } else { 1 })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn yaml(input: &str) -> Result<Value, serde_saphyr::Error> {
        let mut options = serde_saphyr::Options::default();
        options.duplicate_keys = serde_saphyr::DuplicateKeyPolicy::Error;
        options.reject_unsupported_tags = true;
        serde_saphyr::from_str_with_options(input, options)
    }

    #[test]
    fn help_and_version_do_not_require_workspace() {
        use clap::error::ErrorKind;
        for (option, expected) in [
            ("--help", ErrorKind::DisplayHelp),
            ("--version", ErrorKind::DisplayVersion),
        ] {
            let error = Cli::try_parse_from(["hive", option]).unwrap_err();
            assert_eq!(error.kind(), expected);
        }
    }

    #[test]
    fn global_options_work_on_both_sides() {
        for args in [
            ["hive", "--project", "example", "context", "--json"],
            ["hive", "--json", "context", "--project", "example"],
        ] {
            let cli = Cli::try_parse_from(args).unwrap();
            assert_eq!(cli.project().unwrap().map(String::as_str), Some("example"));
            assert_eq!(cli.output_format().unwrap(), Format::Json);
        }
    }

    #[test]
    fn incompatible_views_are_rejected() {
        for args in [
            ["hive", "--at", "abc", "context", "--authority"],
            ["hive", "--authority", "context", "--at", "abc"],
            ["hive", "context", "--authority", "--at", "abc"],
        ] {
            let cli = Cli::try_parse_from(args).unwrap();
            assert!(matches!(cli.validate_options(), Err(OptionConflict::View)));
        }
    }

    #[test]
    fn conflicting_format_is_rejected() {
        let cli = Cli::try_parse_from(["hive", "context", "--json", "--format", "text"]).unwrap();
        assert!(cli.output_format().is_err());
    }

    #[test]
    fn conflicting_repeated_values_are_not_overwritten() {
        for option in ["--workspace", "--project", "--project-root", "--at"] {
            for args in [
                ["hive", option, "a", "context", option, "b"],
                ["hive", "context", option, "a", option, "b"],
                ["hive", option, "a", option, "b", "context"],
            ] {
                let cli = Cli::try_parse_from(args).unwrap();
                assert!(matches!(
                    cli.validate_options(),
                    Err(OptionConflict::Repeated(name)) if name == option
                ));
            }
        }
    }

    #[test]
    fn identical_repeated_values_remain_valid() {
        let cli = Cli::try_parse_from([
            "hive",
            "--project",
            "example",
            "context",
            "--project",
            "example",
        ])
        .unwrap();
        assert!(cli.validate_options().is_ok());
    }

    #[test]
    fn repeated_formats_conflict_in_both_orders() {
        for (first, second) in [("text", "json"), ("json", "text")] {
            let cli =
                Cli::try_parse_from(["hive", "--format", first, "context", "--format", second])
                    .unwrap();
            assert!(cli.json_requested());
            assert!(matches!(
                cli.validate_options(),
                Err(OptionConflict::Repeated("--format"))
            ));
        }
    }

    #[test]
    fn yaml_rejects_duplicate_keys_and_unsupported_tags() {
        assert!(yaml("workspace: a\nworkspace: b\n").is_err());
        assert!(yaml("workspace: !include other.yml\n").is_err());
    }

    #[test]
    fn yaml_preserves_scalar_types_in_intermediate_value() {
        let doc = yaml("workspace: 123\noutput: {format: true}\n").unwrap();
        assert!(doc["workspace"].is_number());
        assert!(doc["output"]["format"].is_boolean());
        assert!(yaml("workspace: \"123\"\n").unwrap()["workspace"].is_string());
    }

    #[test]
    fn yaml_does_not_expand_environment_placeholders() {
        let doc = yaml("workspace: ${HIVE_SYNTHETIC_UNSET}\n").unwrap();
        assert_eq!(doc["workspace"], "${HIVE_SYNTHETIC_UNSET}");
    }
}
