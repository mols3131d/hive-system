use std::io::{self, Write};
use std::process::ExitCode;

use serde::Serialize;
use serde_json::Value;

use crate::cli::{Contract, Format};
use crate::error::HiveError;

#[derive(Serialize)]
struct Envelope<'a> {
    schema_version: u8,
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    command: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    context: Option<&'a Value>,
    #[serde(flatten)]
    outcome: Outcome<'a>,
    warnings: &'a [&'a str],
}

#[derive(Serialize)]
#[serde(untagged)]
enum Outcome<'a> {
    Success { data: &'a Value },
    Failure { error: &'a HiveError },
}

pub(crate) fn success(
    contract: Contract,
    format: Format,
    command: &str,
    context: Option<&Value>,
    data: &Value,
) -> ExitCode {
    let mut stdout = io::stdout().lock();
    let written = match format {
        Format::Text => {
            let scope = context.map(serde_json::to_string_pretty).transpose();
            let text = serde_json::to_string_pretty(data);
            match (scope, text) {
                (Ok(Some(scope)), Ok(text)) => {
                    writeln!(stdout, "{command}\n\nContext\n{scope}\n\nResult\n{text}").is_ok()
                }
                (Ok(None), Ok(text)) => writeln!(stdout, "{command}\n{text}").is_ok(),
                _ => false,
            }
        }
        Format::Json => write_json(
            &mut stdout,
            &Envelope {
                schema_version: contract.schema_version(),
                ok: true,
                command: Some(command),
                context,
                outcome: Outcome::Success { data },
                warnings: &[],
            },
        ),
    };
    if written && stdout.flush().is_ok() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn write_json(output: &mut impl Write, envelope: &Envelope<'_>) -> bool {
    serde_json::to_writer(&mut *output, envelope).is_ok() && writeln!(output).is_ok()
}

pub(crate) fn failure(
    contract: Contract,
    format: Format,
    command: Option<&str>,
    context: Option<&Value>,
    error: HiveError,
) -> ExitCode {
    let written = match format {
        Format::Text => write_failure(io::stderr().lock(), &error).is_ok(),
        Format::Json => {
            let envelope = Envelope {
                schema_version: contract.schema_version(),
                ok: false,
                command,
                context,
                outcome: Outcome::Failure { error: &error },
                warnings: &[],
            };
            let mut stdout = io::stdout().lock();
            write_json(&mut stdout, &envelope) && stdout.flush().is_ok()
        }
    };
    if written {
        ExitCode::from(error.code.exit_status())
    } else {
        ExitCode::FAILURE
    }
}

fn write_failure(mut output: impl Write, error: &HiveError) -> io::Result<()> {
    writeln!(output, "error: {error}")?;
    #[cfg(unix)]
    if let Some((candidates, truncated)) = error.area_candidates() {
        for area in candidates {
            writeln!(output, "{} ({})", area.id, area.slug)?;
        }
        if truncated {
            writeln!(output, "More linked Areas were found.")?;
        }
    }
    let Some((diagnostics, truncated)) = error.record_diagnostics() else {
        return Ok(());
    };
    for diagnostic in diagnostics {
        write!(output, "{} ({})", diagnostic.code, diagnostic.table)?;
        for id in &diagnostic.record_ids {
            write!(output, " {id}")?;
        }
        writeln!(output, " — {}", diagnostic.message)?;
    }
    if truncated {
        writeln!(output, "More record violations were found.")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{Contract, Envelope, Outcome};

    #[test]
    fn v1_success_preserves_the_scope_data_and_warning_fields() {
        let context = json!({"project_id": "example-project", "view": "working-tree"});
        let data = json!({"items": [], "truncated": false});
        let envelope = Envelope {
            schema_version: Contract::V1.schema_version(),
            ok: true,
            command: Some("task list"),
            context: Some(&context),
            outcome: Outcome::Success { data: &data },
            warnings: &[],
        };

        assert_eq!(
            serde_json::to_value(envelope).unwrap(),
            json!({"schema_version": 1, "ok": true, "command": "task list", "context": context, "data": data, "warnings": []}),
        );
    }
}
