use std::io::{self, Write};
use std::process::ExitCode;

use serde::Serialize;
use serde_json::Value;

use crate::cli::Format;
use crate::error::HiveError;

const SCHEMA_VERSION: u8 = 1;

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
    #[allow(
        dead_code,
        reason = "Success envelope is part of v1; executable operations follow Bootstrap"
    )]
    Success {
        data: &'a Value,
    },
    Failure {
        error: &'a HiveError,
    },
}

pub(crate) fn failure(format: Format, command: Option<&str>, error: HiveError) -> ExitCode {
    let written = match format {
        Format::Text => writeln!(io::stderr().lock(), "error: {error}").is_ok(),
        Format::Json => {
            let envelope = Envelope {
                schema_version: SCHEMA_VERSION,
                ok: false,
                command,
                context: None,
                outcome: Outcome::Failure { error: &error },
                warnings: &[],
            };
            let mut stdout = io::stdout().lock();
            serde_json::to_writer(&mut stdout, &envelope).is_ok()
                && writeln!(stdout).is_ok()
                && stdout.flush().is_ok()
        }
    };
    if written {
        ExitCode::from(error.code.exit_status())
    } else {
        ExitCode::FAILURE
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{Envelope, Outcome, SCHEMA_VERSION};

    #[test]
    fn v1_success_preserves_the_scope_data_and_warning_fields() {
        let context = json!({"project_id": "example-project", "view": "working-tree"});
        let data = json!({"items": [], "truncated": false});
        let envelope = Envelope {
            schema_version: SCHEMA_VERSION,
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
