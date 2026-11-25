use clap::{Parser, Subcommand, ValueEnum};
use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum Format {
    Text,
    Json,
}

#[derive(Debug, Parser)]
#[command(name = "hive", version, about = "Synthetic Hive CLI contract probe")]
struct Cli {
    #[arg(long, global = true)]
    workspace: Option<std::path::PathBuf>,
    #[arg(long, global = true)]
    project: Option<String>,
    #[arg(long, global = true)]
    project_root: Option<std::path::PathBuf>,
    #[arg(long, global = true)]
    format: Option<Format>,
    #[arg(long, global = true)]
    json: bool,
    #[arg(long, global = true, conflicts_with = "authority")]
    at: Option<String>,
    #[arg(long, global = true)]
    authority: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Context,
}

#[derive(Debug, thiserror::Error)]
#[error("Conflicting output options")]
struct OutputConflict;

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
    fn output_format(&self) -> Result<Format, OutputConflict> {
        match (self.json, self.format) {
            (true, Some(Format::Text)) => Err(OutputConflict),
            (true, _) => Ok(Format::Json),
            (false, value) => Ok(value.unwrap_or(Format::Text)),
        }
    }
}

fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    let conflict = cli.output_format().is_err();
    let (code, message) = if conflict {
        ("INVALID_ARGUMENT", "Conflicting output options")
    } else {
        ("INTERNAL_ERROR", "Design probe only; no workspace operation")
    };
    if cli.json || cli.format == Some(Format::Json) {
        let failure = Failure {
            schema_version: 1,
            ok: false,
            command: "context",
            error: ErrorDetail { code, message },
            warnings: vec![],
        };
        println!(
            "{}",
            serde_json::to_string(&failure).expect("serializable failure")
        );
    } else {
        eprintln!("{message}");
    }
    std::process::ExitCode::from(if conflict { 2 } else { 1 })
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
            assert_eq!(cli.project.as_deref(), Some("example"));
            assert_eq!(cli.output_format().unwrap(), Format::Json);
        }
    }

    #[test]
    fn incompatible_views_are_rejected() {
        assert!(Cli::try_parse_from(["hive", "--at", "abc", "context", "--authority"]).is_err());
    }

    #[test]
    fn conflicting_format_is_rejected() {
        let cli = Cli::try_parse_from(["hive", "context", "--json", "--format", "text"]).unwrap();
        assert!(cli.output_format().is_err());
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
