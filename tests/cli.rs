use serde_json::Value;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const TASK: &str = "project-fixture/task-fixture";
const REVISION: &str = "1111111111111111111111111111111111111111";
const WORKSPACE: &str = "workspace-fixture";
const PROJECT: &str = "project-fixture";
const PROJECT_ROOT: &str = "project-root-fixture";
const ARTIFACT: &str = "artifact-fixture";
const BOT: &str = "bot-fixture";
const FILE: &str = "input-fixture.md";
const CLAIM: &str = "claim-fixture";
const CONTENT_TOKEN: &str =
    "sha256:2222222222222222222222222222222222222222222222222222222222222222";

const READ_COMMANDS: &[(&str, &[&str])] = &[
    ("validate", &["validate"]),
    ("task show", &["task", "show", TASK]),
];

struct Sandbox(PathBuf);

impl Sandbox {
    fn new() -> Self {
        static NEXT_ID: AtomicUsize = AtomicUsize::new(0);
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time follows the Unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "hive-cli-{}-{timestamp}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).expect("create empty CLI fixture directory");
        Self(path)
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_hive"))
            .args(args)
            .env_clear()
            .env("HOME", &self.0)
            .env("XDG_CONFIG_HOME", &self.0)
            .env("APPDATA", &self.0)
            .current_dir(&self.0)
            .output()
            .unwrap_or_else(|error| panic!("could not run hive {args:?}: {error}"))
    }

    fn assert_empty(&self) {
        assert!(
            fs::read_dir(&self.0).unwrap().next().is_none(),
            "CLI created files in the empty fixture directory"
        );
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn assert_json_failure(output: &Output, args: &[&str], command: &str, code: &str, exit: i32) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        output.status.code(),
        Some(exit),
        "hive {args:?}\nstdout: {stdout}\nstderr: {stderr}"
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!("hive {args:?} did not print one JSON value: {error}\nstdout: {stdout}\nstderr: {stderr}")
    });
    let result = result.as_object().expect("JSON failure is an object");
    assert_eq!(result["schema_version"], 1, "hive {args:?}");
    assert_eq!(result["ok"], false, "hive {args:?}");
    assert_eq!(result["command"], command, "hive {args:?}");
    assert_eq!(result["error"]["code"], code, "hive {args:?}");
    assert!(
        result["error"]["message"]
            .as_str()
            .is_some_and(|message| !message.trim().is_empty()),
        "hive {args:?} omitted a usable error message: {stdout}"
    );
    assert_eq!(result["warnings"], Value::Array(vec![]), "hive {args:?}");
    assert!(!result.contains_key("context"), "hive {args:?}");
    assert!(!result.contains_key("data"), "hive {args:?}");
}

#[test]
fn top_level_help_and_version_run_without_a_workspace() {
    let sandbox = Sandbox::new();
    for args in [["--help"].as_slice(), &["--json", "--help"]] {
        let output = sandbox.run(args);
        assert_eq!(output.status.code(), Some(0), "hive {args:?}");
        let help = String::from_utf8(output.stdout).unwrap();
        assert!(help.contains("Usage: hive"), "{help}");
        assert!(help.contains("--workspace"), "{help}");
        assert!(!help.contains("\n  issue "), "{help}");
        assert!(output.stderr.is_empty(), "hive {args:?}");
    }
    for args in [["--version"].as_slice(), &["--format=json", "--version"]] {
        let output = sandbox.run(args);
        assert_eq!(output.status.code(), Some(0), "hive {args:?}");
        assert_eq!(
            String::from_utf8(output.stdout).unwrap().trim(),
            concat!("hive ", env!("CARGO_PKG_VERSION"))
        );
        assert!(output.stderr.is_empty(), "hive {args:?}");
    }
    sandbox.assert_empty();
}

#[test]
fn explicit_dolt_validation_refuses_ambiguous_selection_without_touching_config() {
    let sandbox = Sandbox::new();
    assert_json_failure(
        &sandbox.run(&["--json", "--contract", "1", "validate"]),
        &["--json", "--contract", "1", "validate"],
        "validate",
        "NOT_IMPLEMENTED",
        7,
    );
    assert_json_failure(
        &sandbox.run(&["--json", "validate", "--all"]),
        &["--json", "validate", "--all"],
        "validate",
        "INVALID_ARGUMENT",
        2,
    );
    for args in [
        &["--json", "--contract", "2", "validate"][..],
        &[
            "--json",
            "validate",
            "--contract",
            "2",
            "--all",
            "--workspace",
            WORKSPACE,
        ],
        &[
            "--json",
            "--contract",
            "2",
            "validate",
            "--all",
            "--database",
            "fixture_db",
            "--workspace-id",
            "01900000-0000-7000-8000-000000000001",
            "--port",
            "0",
        ],
    ] {
        let output = sandbox.run(args);
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(output.status.code(), Some(2), "{args:?} {report}");
        assert_eq!(report["schema_version"], 2, "{args:?}");
        assert_eq!(report["error"]["code"], "INVALID_ARGUMENT", "{args:?}");
        assert!(output.stderr.is_empty(), "{args:?}");
    }

    let output = sandbox.run(&[
        "--json",
        "--contract",
        "2",
        "config",
        "show",
        "--scope",
        "user",
    ]);
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(output.status.code(), Some(7), "{report}");
    assert_eq!(report["schema_version"], 2);
    assert_eq!(report["error"]["code"], "NOT_IMPLEMENTED");
    sandbox.assert_empty();
}

#[test]
fn malformed_dolt_port_preserves_the_explicit_contract_and_format() {
    let sandbox = Sandbox::new();
    let target = [
        "validate",
        "--all",
        "--database",
        "fixture_db",
        "--workspace-id",
        "01900000-0000-7000-8000-000000000001",
    ];
    for (prefix, options) in [
        (&["--json"][..], &["--contract", "2", "--port", "0"][..]),
        (&["--json"], &["--port", "0", "--contract", "2"]),
        (&[], &["--port", "0", "--json", "--contract", "2"]),
        (&["--json"], &["--port", "-1", "--contract", "2"]),
        (&["--json"], &["--port=-1", "--contract", "2"]),
    ] {
        let args = [prefix, &target, options].concat();
        let output = sandbox.run(&args);
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(output.status.code(), Some(2), "{args:?} {report}");
        assert_eq!(report["schema_version"], 2, "{args:?} {report}");
        assert_eq!(report["command"], "validate", "{args:?} {report}");
        assert_eq!(
            report["error"]["code"], "INVALID_ARGUMENT",
            "{args:?} {report}"
        );
        assert_eq!(
            report["error"]["message"], "Supply a Dolt port between 1 and 65535.",
            "{args:?} {report}"
        );
        assert!(output.stderr.is_empty(), "{args:?}");
    }

    let args = [&target[..], &["--port=--json", "--contract", "2"]].concat();
    let output = sandbox.run(&args);
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(!output.stderr.is_empty());
    sandbox.assert_empty();
}

#[test]
fn recognized_commands_accept_matching_globals_and_report_unavailable_operations() {
    let sandbox = Sandbox::new();
    let commands: &[(&str, &[&str])] = &[
        ("validate", &["validate", "--all-projects"]),
        ("config bind", &["config", "bind"]),
        ("project list", &["project", "list"]),
        ("project show", &["project", "show", PROJECT]),
        ("task list", &["task", "list"]),
        ("task show", &["task", "show", TASK]),
        ("task context", &["task", "context", TASK]),
        ("task create", &["task", "create", TASK, "--file", FILE]),
        (
            "task select",
            &["task", "select", TASK, "--expected-revision", REVISION],
        ),
        (
            "task claim",
            &[
                "task",
                "claim",
                TASK,
                "--holder",
                "worker-fixture",
                "--expected-revision",
                REVISION,
            ],
        ),
        (
            "task release",
            &[
                "task",
                "release",
                TASK,
                "--claim-id",
                CLAIM,
                "--status",
                "review",
                "--expected-revision",
                REVISION,
                "--note-file",
                FILE,
            ],
        ),
        (
            "task transfer",
            &[
                "task",
                "transfer",
                TASK,
                "--claim-id",
                CLAIM,
                "--to",
                "worker-fixture",
                "--status",
                "todo",
                "--expected-revision",
                REVISION,
                "--note-file",
                FILE,
            ],
        ),
        (
            "task recover",
            &[
                "task",
                "recover",
                TASK,
                "--claim-id",
                CLAIM,
                "--status",
                "todo",
                "--expected-revision",
                REVISION,
                "--evidence-file",
                FILE,
            ],
        ),
        (
            "task accept",
            &[
                "task",
                "accept",
                TASK,
                "--expected-revision",
                REVISION,
                "--outcome-file",
                FILE,
            ],
        ),
        (
            "task archive",
            &["task", "archive", TASK, "--expected-revision", REVISION],
        ),
        (
            "task cancel",
            &[
                "task",
                "cancel",
                TASK,
                "--expected-revision",
                REVISION,
                "--outcome-file",
                FILE,
            ],
        ),
        (
            "task reopen",
            &[
                "task",
                "reopen",
                TASK,
                "--expected-revision",
                REVISION,
                "--note-file",
                FILE,
            ],
        ),
        (
            "task update",
            &[
                "task",
                "update",
                TASK,
                "--if-match",
                CONTENT_TOKEN,
                "--status",
                "review",
            ],
        ),
        ("artifact list", &["artifact", "list", "--standalone"]),
        (
            "artifact show",
            &["artifact", "show", ARTIFACT, "--task", TASK],
        ),
        (
            "artifact add",
            &["artifact", "add", ARTIFACT, "--file", FILE, "--standalone"],
        ),
        (
            "artifact update",
            &[
                "artifact",
                "update",
                ARTIFACT,
                "--file",
                FILE,
                "--if-match",
                CONTENT_TOKEN,
                "--standalone",
            ],
        ),
        ("directive list", &["directive", "list"]),
        (
            "directive show",
            &["directive", "show", "directive-fixture"],
        ),
        ("bot list", &["bot", "list"]),
        ("bot show", &["bot", "show", BOT]),
        ("memory list", &["memory", "list", BOT]),
        ("memory show", &["memory", "show", BOT, "topic-fixture"]),
    ];
    let before = [
        "--json",
        "--format=json",
        "--workspace",
        WORKSPACE,
        "--project",
        PROJECT,
        "--project-root",
        PROJECT_ROOT,
    ];
    let after = [
        "--json",
        "--format",
        "json",
        "--workspace",
        WORKSPACE,
        "--project",
        PROJECT,
        "--project-root",
        PROJECT_ROOT,
    ];
    for &(command, leaf) in commands {
        let args = [before.as_slice(), leaf, &after].concat();
        let output = sandbox.run(&args);
        assert_json_failure(&output, &args, command, "NOT_IMPLEMENTED", 7);
    }
    sandbox.assert_empty();
}

#[test]
fn value_options_preserve_every_occurrence_at_each_command_depth() {
    let sandbox = Sandbox::new();
    let values = [
        ("--workspace", WORKSPACE, "different-workspace-fixture"),
        ("--project", PROJECT, "different-project-fixture"),
        ("--project-root", PROJECT_ROOT, "different-root-fixture"),
        ("--at", REVISION, "2222222222222222222222222222222222222222"),
    ];
    for &(command, leaf) in READ_COMMANDS {
        for (option, first, second) in values {
            let original = [option, first];
            let repeat = [option, first, option, first, option, first];
            let conflict = [option, first, option, second, option, first];
            for (before, after) in [
                (repeat.as_slice(), [].as_slice()),
                (&[], &repeat),
                (&original, &original),
            ] {
                let args = [&["--json"], before, leaf, after].concat();
                let output = sandbox.run(&args);
                assert_json_failure(&output, &args, command, "NOT_IMPLEMENTED", 7);
            }
            for (before, after) in [
                (conflict.as_slice(), [].as_slice()),
                (&[], &conflict),
                (&original, &[option, second]),
            ] {
                let args = [&["--json"], before, leaf, after].concat();
                let output = sandbox.run(&args);
                assert_json_failure(&output, &args, command, "INVALID_ARGUMENT", 2);
                for stream in [&output.stdout, &output.stderr] {
                    let diagnostic = String::from_utf8_lossy(stream);
                    assert!(!diagnostic.contains(first), "hive {args:?}: {diagnostic}");
                    assert!(!diagnostic.contains(second), "hive {args:?}: {diagnostic}");
                }
            }
        }
    }
}

#[test]
fn incompatible_output_options_keep_json_in_both_positions() {
    let sandbox = Sandbox::new();
    let conflicts: &[(&[&str], &[&str])] = &[
        (&["--json", "--format", "text"], &[]),
        (&[], &["--format=text", "--json"]),
        (&["--json"], &["--format=text"]),
        (&["--format", "text"], &["--json"]),
        (&["--format", "text", "--format=json", "--format=text"], &[]),
        (&[], &["--format=text", "--format=json", "--format=text"]),
        (&["--format=json"], &["--format", "text"]),
        (&["--format=text"], &["--format", "json"]),
    ];
    for &(command, leaf) in READ_COMMANDS {
        for &(before, after) in conflicts {
            let args = [before, leaf, after].concat();
            let output = sandbox.run(&args);
            assert_json_failure(&output, &args, command, "INVALID_ARGUMENT", 2);
        }
    }
}

#[test]
fn incompatible_read_views_fail_in_both_positions() {
    let sandbox = Sandbox::new();
    let conflicts: &[(&[&str], &[&str])] = &[
        (&["--at", REVISION, "--authority"], &[]),
        (&[], &["--authority", "--at", REVISION]),
        (&["--at", REVISION], &["--authority"]),
        (&["--authority"], &["--at", REVISION]),
    ];
    for &(command, leaf) in READ_COMMANDS {
        for &(before, after) in conflicts {
            let args = [&["--json"], before, leaf, after].concat();
            let output = sandbox.run(&args);
            assert_json_failure(&output, &args, command, "INVALID_ARGUMENT", 2);
        }
    }
}

#[test]
fn validation_compares_against_a_local_candidate_view() {
    let sandbox = Sandbox::new();
    let comparison = ["validate", "--all-projects", "--against", REVISION];
    for (before, after) in [
        (["--authority"].as_slice(), [].as_slice()),
        (&[], &["--authority"]),
    ] {
        let args = [&["--json"], before, &comparison, after].concat();
        let output = sandbox.run(&args);
        assert_json_failure(&output, &args, "validate", "INVALID_ARGUMENT", 2);
    }
    for candidate in [
        [].as_slice(),
        &["--at", "2222222222222222222222222222222222222222"],
    ] {
        let args = [&["--json"], comparison.as_slice(), candidate].concat();
        let output = sandbox.run(&args);
        assert_json_failure(&output, &args, "validate", "NOT_IMPLEMENTED", 7);
    }
    sandbox.assert_empty();
}

#[test]
fn mutations_reject_read_views_with_an_argument_failure() {
    let sandbox = Sandbox::new();
    let mutations: &[(&str, &[&str])] = &[
        (
            "config bind",
            &[
                "config",
                "bind",
                "--workspace",
                WORKSPACE,
                "--project",
                PROJECT,
            ],
        ),
        ("task create", &["task", "create", TASK, "--file", FILE]),
        (
            "artifact add",
            &["artifact", "add", ARTIFACT, "--file", FILE, "--standalone"],
        ),
    ];
    for &(command, leaf) in mutations {
        for view in [["--at", REVISION].as_slice(), &["--authority"]] {
            for (before, after) in [(view, [].as_slice()), (&[], view)] {
                let args = [&["--json"], before, leaf, after].concat();
                let output = sandbox.run(&args);
                assert_json_failure(&output, &args, command, "INVALID_ARGUMENT", 2);
            }
        }
    }
    sandbox.assert_empty();
}

#[test]
fn malformed_command_arguments_fail_with_specific_usage_on_stderr() {
    let sandbox = Sandbox::new();
    let invalid: &[(&str, &[&str])] = &[
        ("task", &["task", "--workspace", WORKSPACE, "show", TASK]),
        ("task show", &["task", "show"]),
        ("task show", &["task", "show", TASK, "--all-projects"]),
        (
            "task show",
            &["task", "show", TASK, "extra-fixture-argument"],
        ),
        ("task select", &["task", "select", TASK]),
        (
            "artifact add",
            &["artifact", "add", ARTIFACT, "--standalone"],
        ),
    ];
    for &(command, args) in invalid {
        let output = sandbox.run(args);
        assert_eq!(output.status.code(), Some(2), "hive {args:?}");
        assert!(output.stdout.is_empty(), "hive {args:?}");
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(
            stderr.contains(&format!("Usage: hive {command}")),
            "hive {args:?}: {stderr}"
        );
    }
    sandbox.assert_empty();
}

#[test]
fn a_recognized_json_selection_survives_leaf_argument_errors() {
    let sandbox = Sandbox::new();
    for args in [
        ["--json", "task", "show"].as_slice(),
        &["task", "show", "--format=json"],
        &["task", "show", TASK, "--json", "--all-projects"],
    ] {
        let output = sandbox.run(args);
        assert_json_failure(&output, args, "task show", "INVALID_ARGUMENT", 2);
    }
    sandbox.assert_empty();
}

#[test]
fn text_operation_failure_uses_stderr() {
    let sandbox = Sandbox::new();
    for args in [
        ["validate"].as_slice(),
        &["--format", "text", "validate", "--format=text"],
    ] {
        let output = sandbox.run(args);
        assert_eq!(output.status.code(), Some(7), "hive {args:?}");
        assert!(output.stdout.is_empty(), "hive {args:?}");
        assert!(
            !String::from_utf8(output.stderr).unwrap().trim().is_empty(),
            "hive {args:?}"
        );
    }
}

#[test]
fn dolt_issue_show_requires_contract_2_and_validates_explicit_target_and_selector() {
    let sandbox = Sandbox::new();
    let uuid = "01900000-0000-7000-8000-000000000010";

    let help = sandbox.run(&["--contract", "2", "issue", "show", "--help"]);
    assert_eq!(help.status.code(), Some(0));
    let help_text = String::from_utf8(help.stdout).unwrap();
    assert!(help_text.contains("Usage: hive issue show"), "{help_text}");
    assert!(help_text.contains("--workspace-id"), "{help_text}");
    assert!(help_text.contains("--database"), "{help_text}");
    assert!(help_text.contains("--port"), "{help_text}");
    assert!(help.stderr.is_empty());

    for args in [
        ["--json", "issue", "show", "#1"].as_slice(),
        &["--json", "--contract", "1", "issue", "show", uuid],
    ] {
        assert_json_failure(
            &sandbox.run(args),
            args,
            "issue show",
            "INVALID_ARGUMENT",
            2,
        );
    }

    for args in [
        ["--json", "--contract", "2", "issue", "show"].as_slice(),
        &["--json", "--contract", "2", "issue", "show", "#1"],
        &[
            "--json",
            "--contract",
            "2",
            "--workspace",
            WORKSPACE,
            "issue",
            "show",
            "#1",
            "--workspace-id",
            uuid,
            "--database",
            "fixture_db",
            "--port",
            "3307",
        ],
        &[
            "--json",
            "--contract",
            "2",
            "--project",
            PROJECT,
            "issue",
            "show",
            "#1",
            "--workspace-id",
            uuid,
            "--database",
            "fixture_db",
            "--port",
            "3307",
        ],
        &[
            "--json",
            "--contract",
            "2",
            "--at",
            REVISION,
            "issue",
            "show",
            "#1",
            "--workspace-id",
            uuid,
            "--database",
            "fixture_db",
            "--port",
            "3307",
        ],
        &[
            "--json",
            "--contract",
            "2",
            "issue",
            "show",
            "#1",
            "--authority",
            "--workspace-id",
            uuid,
            "--database",
            "fixture_db",
            "--port",
            "3307",
        ],
        &[
            "--json",
            "--contract",
            "2",
            "issue",
            "show",
            "#1",
            "--workspace-id",
            uuid,
            "--database",
            "fixture_db",
            "--port",
            "0",
        ],
        &[
            "--json",
            "--contract",
            "2",
            "issue",
            "show",
            "#1",
            "--workspace-id",
            uuid,
            "--database",
            "fixture_db",
            "--port=-1",
        ],
    ] {
        let output = sandbox.run(args);
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(output.status.code(), Some(2), "{args:?} {report}");
        assert_eq!(report["schema_version"], 2, "{args:?}");
        assert_eq!(report["command"], "issue show", "{args:?}");
        assert_eq!(report["error"]["code"], "INVALID_ARGUMENT", "{args:?}");
        assert!(output.stderr.is_empty(), "{args:?}");
    }

    for bad_selector in ["-1", "-1.5"] {
        let args = [
            "issue",
            "show",
            bad_selector,
            "--workspace-id",
            uuid,
            "--database",
            "fixture_db",
            "--port",
            "3307",
            "--contract",
            "2",
            "--json",
        ];
        let output = sandbox.run(&args);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(output.stderr.is_empty(), "{args:?}");
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["schema_version"], 2, "{args:?}");
        assert_eq!(report["command"], "issue show", "{args:?}");
        assert_eq!(report["error"]["code"], "INVALID_ARGUMENT", "{args:?}");
    }

    for bad_selector in [
        "1",
        "#0",
        "#-1",
        "bad-selector",
        "01900000-0000-7000-8000-00000000000A",
        "01900000-0000-4000-8000-00000000000a",
    ] {
        let args = [
            "--json",
            "--contract",
            "2",
            "issue",
            "show",
            bad_selector,
            "--workspace-id",
            uuid,
            "--database",
            "fixture_db",
            "--port",
            "3307",
        ];
        let output = sandbox.run(&args);
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(output.status.code(), Some(2), "{bad_selector} {report}");
        assert_eq!(report["schema_version"], 2);
        assert_eq!(report["command"], "issue show");
        assert_eq!(report["error"]["code"], "INVALID_ARGUMENT");
        assert!(!report.to_string().contains(bad_selector), "{bad_selector}");
        assert!(output.stderr.is_empty());
    }
    sandbox.assert_empty();
}

#[test]
fn dolt_issue_queries_require_one_explicit_scope_and_preserve_v1_help() {
    let sandbox = Sandbox::new();
    let default = sandbox.run(&["--help"]);
    let v1 = sandbox.run(&["--contract", "1", "--help"]);
    assert_eq!(default.status.code(), Some(0));
    assert_eq!(default.stdout, v1.stdout);
    assert!(
        !String::from_utf8(default.stdout)
            .unwrap()
            .contains("Inspect Issues")
    );

    for view in ["list", "ready", "blocked"] {
        let command = format!("issue {view}");
        let leaf = sandbox.run(&["issue", view, "--help"]);
        assert_eq!(leaf.status.code(), Some(0));
        let help = String::from_utf8(leaf.stdout).unwrap();
        assert!(help.contains(&format!("Usage: hive {command}")));
        for option in ["--all", "--area", "--project", "--limit", "--cursor"] {
            assert!(help.contains(option), "{help}");
        }

        let v1_list = ["--json", "issue", view, "--all"];
        assert_json_failure(
            &sandbox.run(&v1_list),
            &v1_list,
            &command,
            "INVALID_ARGUMENT",
            2,
        );

        let target = [
            "--contract",
            "2",
            "--json",
            "issue",
            view,
            "--workspace-id",
            "01900000-0000-7000-8000-000000000001",
            "--database",
            "fixture_db",
            "--port",
            "3307",
        ];
        let uuid = "01900000-0000-7000-8000-00000000000a";
        for (options, code) in [
            ([].as_slice(), "INVALID_ARGUMENT"),
            (&["--area", "root"], "INVALID_ARGUMENT"),
            (
                &["--area", "01900000-0000-7000-8000-00000000000A"],
                "INVALID_ARGUMENT",
            ),
            (&["--project", "project-slug"], "INVALID_ARGUMENT"),
            (&["--all", "--area", uuid], "AMBIGUOUS_SCOPE"),
            (&["--area", uuid, "--project", uuid], "AMBIGUOUS_SCOPE"),
            (&["--all", "--project", uuid], "AMBIGUOUS_SCOPE"),
            (&["--all", "--limit", "0"], "INVALID_ARGUMENT"),
            (&["--all", "--limit", "101"], "INVALID_ARGUMENT"),
            (&["--all", "--status", "TODO"], "INVALID_ARGUMENT"),
            (&["--all", "--type", "Task"], "INVALID_ARGUMENT"),
            (&["--all", "--priority", "-1"], "INVALID_ARGUMENT"),
            (&["--all", "--project-root", "."], "INVALID_ARGUMENT"),
        ] {
            let args: Vec<_> = target.iter().chain(options).copied().collect();
            let output = sandbox.run(&args);
            let report: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(output.status.code(), Some(2), "{args:?} {report}");
            assert_eq!(report["schema_version"], 2, "{args:?}");
            assert_eq!(report["command"], command, "{args:?}");
            assert_eq!(report["error"]["code"], code, "{args:?}");
            assert!(output.stderr.is_empty(), "{args:?}");
        }
    }
}

#[test]
fn malformed_dolt_list_project_preserves_late_v2_and_json_options() {
    let sandbox = Sandbox::new();
    let target = [
        "issue",
        "list",
        "--workspace-id",
        "01900000-0000-7000-8000-000000000001",
        "--database",
        "fixture_db",
        "--port",
        "3307",
    ];
    let malformed = "01900000-0000-7000-8000-00000000000A";
    let selection = ["--project", malformed, "--contract", "2", "--json"];
    for args in [
        [target.as_slice(), &selection].concat(),
        [selection.as_slice(), &target].concat(),
    ] {
        let output = sandbox.run(&args);
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(output.status.code(), Some(2), "{args:?} {report}");
        assert_eq!(report["schema_version"], 2, "{args:?}");
        assert_eq!(report["command"], "issue list", "{args:?}");
        assert_eq!(report["error"]["code"], "INVALID_ARGUMENT", "{args:?}");
        assert!(
            report["error"]["message"]
                .as_str()
                .unwrap()
                .contains("UUIDv7")
        );
        assert!(!report.to_string().contains(malformed));
        assert!(output.stderr.is_empty(), "{args:?}");
    }

    let args = [target.as_slice(), &["--project=--json", "--contract", "2"]].concat();
    let output = sandbox.run(&args);
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8(output.stderr).unwrap().contains("UUIDv7"));
}

#[test]
fn malformed_dolt_config_scope_preserves_late_v2_and_json_options() {
    let sandbox = Sandbox::new();
    let command = ["config", "show"];
    let assert_scope_error = |args: &[&str], flag: &str| {
        let output = sandbox.run(args);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(output.stderr.is_empty(), "{args:?}");
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["schema_version"], 2, "{args:?}");
        assert_eq!(report["ok"], false, "{args:?}");
        assert_eq!(report["command"], "config show", "{args:?}");
        assert_eq!(report["error"]["code"], "INVALID_ARGUMENT", "{args:?}");
        let message = report["error"]["message"].as_str().unwrap();
        assert!(
            message.contains(flag) && message.contains("UUIDv7"),
            "{args:?}"
        );
    };
    for (flag, value) in [
        ("--area", "-1"),
        ("--area", "123"),
        ("--project", "-1"),
        ("--project", "01900000-0000-7000-8000-00000000000A"),
    ] {
        let args = [
            command.as_slice(),
            &[flag, value, "--contract", "2", "--json"],
        ]
        .concat();
        assert_scope_error(&args, flag);
    }

    for (flag, json_literal, contract_literal) in [
        ("--area", "--area=--json", "--area=--contract"),
        ("--project", "--project=--json", "--project=--contract"),
    ] {
        let args = [command.as_slice(), &[json_literal, "--contract", "2"]].concat();
        let output = sandbox.run(&args);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(output.stdout.is_empty(), "{args:?}");
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(stderr.contains("UUIDv7"), "{args:?}");
        assert!(!stderr.contains("--json"), "{args:?}");

        let args = [
            command.as_slice(),
            &[contract_literal, "--contract", "2", "--json"],
        ]
        .concat();
        assert_scope_error(&args, flag);
    }
}

#[test]
fn bot_metadata_has_explicit_v2_selection_and_preserves_v1_parsing() {
    let sandbox = Sandbox::new();
    for (command, args) in [
        ("bot list", &["bot", "list"][..]),
        (
            "bot list",
            &["bot", "list", "--limit", "500", "--after", "legacy-key"][..],
        ),
        ("bot show", &["bot", "show", BOT]),
        ("memory show", &["memory", "show", BOT]),
    ] {
        let mut flags = vec!["--json"];
        flags.extend(args);
        assert_json_failure(&sandbox.run(&flags), &flags, command, "NOT_IMPLEMENTED", 7);
    }

    for (command, args) in [
        ("bot list", &["bot", "list", "--limit", "501"][..]),
        ("bot list", &["bot", "list", "--after="]),
        ("bot show", &["bot", "show", "Invalid_Bot"]),
    ] {
        let mut flags = vec!["--contract", "1", "--json"];
        flags.extend(args);
        assert_json_failure(&sandbox.run(&flags), &flags, command, "INVALID_ARGUMENT", 2);
    }

    for (command, args) in [
        ("bot list", &["bot", "list", "--status", "retired"][..]),
        (
            "bot show",
            &["bot", "show", BOT, "--database", "fixture_db"],
        ),
    ] {
        let mut flags = vec!["--json"];
        flags.extend(args);
        assert_json_failure(&sandbox.run(&flags), &flags, command, "INVALID_ARGUMENT", 2);
    }

    let target = [
        "--database",
        "fixture_db",
        "--port",
        "3307",
        "--workspace-id",
        "01900000-0000-7000-8000-000000000001",
    ];
    for (args, explicit_target) in [
        (&["list"][..], false),
        (&["list", "--status", "Active"], true),
        (&["list", "--limit", "0"], true),
        (&["list", "--limit", "101"], true),
        (&["list", "--after", "legacy-key"], true),
        (&["list", "--workspace", "fixture"], true),
        (&["list", "--project", "project-fixture"], true),
        (&["list", "--at", REVISION], true),
        (&["list", "--authority"], true),
        (&["show", BOT], true),
        (&["show", "01900000-0000-4000-8000-00000000000a"], true),
        (&["show", "01900000-0000-7000-8000-00000000000A"], true),
    ] {
        let mut flags = vec!["--contract", "2", "--json", "bot"];
        flags.extend(args);
        if explicit_target {
            flags.extend(target);
        }
        let output = sandbox.run(&flags);
        let result: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(output.status.code(), Some(2), "{flags:?} {result}");
        assert!(output.stderr.is_empty());
        assert_eq!(result["schema_version"], 2);
        assert_eq!(result["command"], format!("bot {}", args[0]));
        assert_eq!(result["error"]["code"], "INVALID_ARGUMENT");
    }
    sandbox.assert_empty();
}

#[test]
fn v1_bot_help_matches_the_reviewed_cli() {
    let sandbox = Sandbox::new();
    // Captured from e4749287044a71ec752f8539c51e77dfbbfe5ba0.
    let expected = concat!(
        "List or inspect registered Manager Bots\n",
        "\n",
        "Usage: hive bot <COMMAND>\n",
        "\n",
        "Commands:\n",
        "  list  List registered Manager Bots\n",
        "  show  Show a registered Bot's identity and routing metadata\n",
        "  help  Print this message or the help of the given subcommand(s)\n",
        "\n",
        "Options:\n",
        "  -h, --help  Print help\n",
        "List registered Manager Bots\n",
        "\n",
        "Usage: hive bot list [OPTIONS]\n",
        "\n",
        "Options:\n",
        "      --limit <LIMIT>        [default: 50]\n",
        "      --after <KEY>          \n",
        "      --contract <VERSION>   Select the CLI contract version [possible values: 1, 2]\n",
        "      --workspace <PATH>     Select the Hive workspace\n",
        "      --project <ID>         Select the project or reserved scope\n",
        "      --project-root <PATH>  Select the Git project root\n",
        "      --format <FORMAT>      Set the output format [possible values: text, json]\n",
        "      --json                 Use JSON output\n",
        "      --at <COMMIT>          Read a snapshot at a full local commit object ID\n",
        "      --authority            Check the selected workspace's authority for this read\n",
        "  -h, --help                 Print help\n",
        "Show a registered Bot's identity and routing metadata\n",
        "\n",
        "Usage: hive bot show [OPTIONS] <BOT-ID>\n",
        "\n",
        "Arguments:\n",
        "  <BOT-ID>  \n",
        "\n",
        "Options:\n",
        "      --contract <VERSION>   Select the CLI contract version [possible values: 1, 2]\n",
        "      --workspace <PATH>     Select the Hive workspace\n",
        "      --project <ID>         Select the project or reserved scope\n",
        "      --project-root <PATH>  Select the Git project root\n",
        "      --format <FORMAT>      Set the output format [possible values: text, json]\n",
        "      --json                 Use JSON output\n",
        "      --at <COMMIT>          Read a snapshot at a full local commit object ID\n",
        "      --authority            Check the selected workspace's authority for this read\n",
        "  -h, --help                 Print help\n",
    );
    let mut actual = Vec::new();
    for args in [
        &["bot", "--help"][..],
        &["bot", "list", "--help"],
        &["bot", "show", "--help"],
    ] {
        let output = sandbox.run(args);
        assert!(output.status.success(), "{args:?}");
        assert!(output.stderr.is_empty(), "{args:?}");
        actual.extend(output.stdout);
    }
    assert_eq!(String::from_utf8(actual).unwrap(), expected);
    sandbox.assert_empty();
}

#[test]
fn malformed_dolt_bot_arguments_preserve_trailing_contract_and_format() {
    let sandbox = Sandbox::new();
    let target = [
        "--database",
        "fixture_db",
        "--port",
        "3307",
        "--workspace-id",
        "01900000-0000-7000-8000-000000000001",
    ];
    let check = |args: &[&str], leaf: &str, code, exit| {
        let output = sandbox.run(args);
        let report: Value = serde_json::from_slice(&output.stdout)
            .unwrap_or_else(|error| panic!("{args:?}: {error}\n{output:?}"));
        assert_eq!(output.status.code(), Some(exit), "{args:?}: {report}");
        assert!(output.stderr.is_empty(), "{args:?}");
        assert_eq!(report["schema_version"], 2, "{args:?}");
        assert_eq!(report["command"], format!("bot {leaf}"));
        assert_eq!(report["error"]["code"], code);
        assert!(report.get("context").is_none(), "{args:?}");
        report
    };
    for (options, code, exit) in [
        (
            &["show", "01900000-0000-7000-8000-00000000000A"][..],
            "INVALID_ARGUMENT",
            2,
        ),
        (&["list", "--limit", "malformed"], "INVALID_ARGUMENT", 2),
        (&["list", "--limit", "-1"], "INVALID_ARGUMENT", 2),
        (&["list", "--limit=-1"], "INVALID_ARGUMENT", 2),
        (&["list", "--limit", "101"], "INVALID_ARGUMENT", 2),
        (&["list", "--limit"], "INVALID_ARGUMENT", 2),
        (
            &["list", "--project", "INVALID_PROJECT"],
            "INVALID_ARGUMENT",
            2,
        ),
        (&["list", "--cursor", "malformed"], "STALE_CURSOR", 5),
        (&["list", "--cursor"], "INVALID_ARGUMENT", 2),
        (&["list", "--cursor", "-1"], "INVALID_ARGUMENT", 2),
        (&["list", "--cursor=-1"], "STALE_CURSOR", 5),
        (&["list", "--status", "-1"], "INVALID_ARGUMENT", 2),
        (&["list", "--status=-1"], "INVALID_ARGUMENT", 2),
        (&["list", "--after="], "INVALID_ARGUMENT", 2),
    ] {
        let (leaf, options) = options.split_first().unwrap();
        let args = [
            &["bot", leaf][..],
            &target,
            options,
            &["--contract", "2", "--json"],
        ]
        .concat();
        check(&args, leaf, code, exit);
    }

    let command = ["bot", "list"];
    let trailing = ["--contract", "2", "--json"];
    for (flag, value) in [
        ("--database", "fixture_db"),
        ("--workspace-id", "01900000-0000-7000-8000-000000000001"),
        ("--port", "3307"),
        ("--status", "active"),
        ("--cursor", "cursor-fixture"),
        ("--limit", "25"),
        ("--after", "legacy-key"),
    ] {
        let other = target
            .as_chunks::<2>()
            .0
            .iter()
            .filter(|pair| pair[0] != flag)
            .flatten()
            .copied()
            .collect::<Vec<_>>();
        for invalid in [&[flag][..], &[flag, "-1"]] {
            let args = [&command[..], &other, invalid, &trailing].concat();
            check(&args, "list", "INVALID_ARGUMENT", 2);
        }
        let duplicate = [flag, value, flag, value];
        let args = [&command[..], &other, &duplicate, &trailing].concat();
        let report = check(&args, "list", "INVALID_ARGUMENT", 2);
        assert_eq!(
            report["error"]["message"], "Arguments conflict.",
            "{args:?}"
        );
    }

    for flag in ["--project", "--at", "--workspace", "--project-root"] {
        let empty = format!("{flag}=");
        for invalid in [&[flag][..], &[flag, "-1"], &[empty.as_str()]] {
            let root = [invalid, &trailing, &command, &target].concat();
            let leaf = [&command[..], &target, invalid, &trailing].concat();
            check(&root, "list", "INVALID_ARGUMENT", 2);
            check(&leaf, "list", "INVALID_ARGUMENT", 2);
        }
    }

    for at in [
        &["--at", "BAD"][..],
        &["--at=BAD"],
        &["--at", "-1"],
        &["--at=-1"],
    ] {
        for (leaf, command) in [
            ("list", &["bot", "list"][..]),
            (
                "show",
                &["bot", "show", "01900000-0000-7000-8000-00000000000a"],
            ),
        ] {
            let trailing = ["--contract", "2", "--json"];
            check(
                &[at, command, &target, &trailing].concat(),
                leaf,
                "INVALID_ARGUMENT",
                2,
            );
            check(
                &[command, &target, at, &trailing].concat(),
                leaf,
                "INVALID_ARGUMENT",
                2,
            );
        }
    }

    for (literal, exit) in [
        ("--project=--json", 2),
        ("--cursor=--json", 5),
        ("--status=--json", 2),
        ("--at=--json", 2),
        ("--after=--json", 2),
        ("--database=--json", 2),
        ("--workspace-id=--json", 2),
    ] {
        let command = ["bot", "list"];
        let trailing = ["--contract", "2"];
        let args = [&command[..], &target, &[literal], &trailing].concat();
        let output = sandbox.run(&args);
        assert_eq!(output.status.code(), Some(exit), "{args:?}");
        assert!(output.stdout.is_empty(), "{args:?}");
        assert!(!output.stderr.is_empty(), "{args:?}");
        if matches!(literal, "--at=--json" | "--project=--json") {
            let args = [&[literal][..], &command, &target, &trailing].concat();
            let output = sandbox.run(&args);
            assert_eq!(output.status.code(), Some(exit), "{args:?}");
            assert!(output.stdout.is_empty(), "{args:?}");
            assert!(!output.stderr.is_empty(), "{args:?}");
        }
    }
    sandbox.assert_empty();
}

#[test]
fn dolt_area_reads_require_contract_2_and_an_explicit_target() {
    let sandbox = Sandbox::new();
    let help = sandbox.run(&["--help"]);
    assert_eq!(help.status.code(), Some(0));
    assert_eq!(
        help.stdout,
        sandbox.run(&["--contract", "1", "--help"]).stdout
    );
    assert!(
        !String::from_utf8(help.stdout)
            .unwrap()
            .contains("Inspect Areas")
    );

    let list_help = sandbox.run(&["--contract", "2", "area", "list", "--help"]);
    assert_eq!(list_help.status.code(), Some(0));
    let help = String::from_utf8(list_help.stdout).unwrap();
    for option in ["--parent", "--limit", "--cursor", "--workspace-id"] {
        assert!(help.contains(option), "{help}");
    }
    let tree_help = sandbox.run(&["--contract", "2", "area", "tree", "--help"]);
    assert_eq!(tree_help.status.code(), Some(0));
    assert!(
        String::from_utf8(tree_help.stdout)
            .unwrap()
            .contains("--root")
    );
    let args = ["--json", "area", "tree"];
    assert_json_failure(
        &sandbox.run(&args),
        &args,
        "area tree",
        "INVALID_ARGUMENT",
        2,
    );

    let args = ["--json", "area", "list"];
    assert_json_failure(
        &sandbox.run(&args),
        &args,
        "area list",
        "INVALID_ARGUMENT",
        2,
    );
    let args = ["--contract", "2", "--json", "area", "list"];
    let output = sandbox.run(&args);
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stderr.is_empty());
    assert_eq!(report["schema_version"], 2);
    assert_eq!(report["command"], "area list");
    assert_eq!(report["error"]["code"], "INVALID_ARGUMENT");

    let target = [
        "--contract",
        "2",
        "--json",
        "area",
        "list",
        "--workspace-id",
        "01900000-0000-7000-8000-000000000001",
        "--database",
        "fixture_db",
        "--port",
        "3307",
    ];
    for options in [
        &["--project", "fixture"][..],
        &["--authority"][..],
        &["--parent", "unsafe'path/fixture"][..],
        &["--limit", "0"][..],
        &["--limit", "101"][..],
    ] {
        let args: Vec<_> = target.into_iter().chain(options.iter().copied()).collect();
        let output = sandbox.run(&args);
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(output.status.code(), Some(2), "{report}");
        assert!(output.stderr.is_empty());
        assert_eq!(report["schema_version"], 2);
        assert_eq!(report["command"], "area list");
        assert_eq!(report["error"]["code"], "INVALID_ARGUMENT");
        assert!(!report.to_string().contains("unsafe'path/fixture"));
    }
    sandbox.assert_empty();
}

#[test]
fn malformed_dolt_area_options_preserve_v2_json_at_root_and_leaf() {
    let sandbox = Sandbox::new();
    let target = [
        "--workspace-id",
        "01900000-0000-7000-8000-000000000001",
        "--database",
        "fixture_db",
        "--port",
        "3307",
    ];
    let selectors = ["--contract", "2", "--json"];
    let project = ["--project", "BAD!"];
    let cursor = ["--cursor", "-1"];
    let root = ["--root", "-1"];
    let cases = [
        ("area list", &["area", "list"][..], &cursor[..], false),
        ("area list", &["area", "list"][..], &project[..], false),
        ("area list", &["area", "list"][..], &project[..], true),
        ("area tree", &["area", "tree"][..], &root[..], false),
        ("area tree", &["area", "tree"][..], &project[..], false),
        ("area tree", &["area", "tree"][..], &project[..], true),
        (
            "area show",
            &["area", "show", target[1]][..],
            &project[..],
            false,
        ),
        (
            "area show",
            &["area", "show", target[1]][..],
            &project[..],
            true,
        ),
    ];
    for (name, command, invalid, at_root) in cases {
        let command = [command, &target].concat();
        let invalid_command = if at_root {
            [invalid, &command].concat()
        } else {
            [&command[..], invalid].concat()
        };
        let early = [&selectors[..], &invalid_command].concat();
        let late = [&invalid_command[..], &selectors].concat();
        let output = sandbox.run(&early);
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(output.status.code(), Some(2), "{early:?}");
        assert_eq!(report["schema_version"], 2, "{early:?}");
        assert_eq!(report["command"], name, "{early:?}");
        assert_eq!(report["error"]["code"], "INVALID_ARGUMENT", "{early:?}");
        assert!(!report.to_string().contains(invalid[1]), "{early:?}");
        assert!(output.stderr.is_empty(), "{early:?}");

        let trailing = sandbox.run(&late);
        assert_eq!(trailing.status.code(), output.status.code(), "{late:?}");
        assert_eq!(trailing.stdout, output.stdout, "{late:?}");
        assert!(trailing.stderr.is_empty(), "{late:?}");
    }
    sandbox.assert_empty();
}

#[test]
fn dolt_resource_reads_preserve_json_errors_and_option_literals() {
    let sandbox = Sandbox::new();
    let id = "01900000-0000-7000-8000-000000000001";
    let target = [
        "--workspace-id",
        id,
        "--database",
        "fixture_db",
        "--port",
        "3307",
    ];
    let selectors = ["--contract", "2", "--json"];
    let help = sandbox.run(&["--contract", "2", "resource", "show", "--help"]);
    assert!(help.status.success(), "{help:?}");
    assert!(String::from_utf8_lossy(&help.stdout).contains("hive resource show"));
    for (command, invalid, at_root, code) in [
        (
            &["list", "--all"][..],
            &["--project", "BAD!"][..],
            true,
            "INVALID_ARGUMENT",
        ),
        (
            &["list", "--all"][..],
            &["--at", "BAD!"][..],
            false,
            "INVALID_ARGUMENT",
        ),
        (
            &["list", "--all"][..],
            &["--cursor", "-1"][..],
            false,
            "INVALID_ARGUMENT",
        ),
        (
            &["list", "--all"][..],
            &["--limit", "-1"][..],
            false,
            "INVALID_ARGUMENT",
        ),
        (
            &["list", "--all"][..],
            &["--workspace", "-1"][..],
            true,
            "INVALID_ARGUMENT",
        ),
        (
            &["list", "--all"][..],
            &["--all"][..],
            false,
            "INVALID_ARGUMENT",
        ),
        (
            &["list"][..],
            &["--all=false"][..],
            false,
            "INVALID_ARGUMENT",
        ),
        (
            &["show", id][..],
            &["--area", id][..],
            false,
            "INVALID_ARGUMENT",
        ),
        (
            &["list", "--all"][..],
            &["--issue", id][..],
            false,
            "AMBIGUOUS_SCOPE",
        ),
        (&["list"][..], &[][..], false, "INVALID_ARGUMENT"),
        (
            &["list", "--issue", "#1"][..],
            &[][..],
            false,
            "INVALID_ARGUMENT",
        ),
        (&["show", "BAD!"][..], &[][..], false, "INVALID_ARGUMENT"),
        (
            &["show", id][..],
            &["--at=--json"][..],
            true,
            "INVALID_ARGUMENT",
        ),
    ] {
        let name = format!("resource {}", command[0]);
        let command = [&["resource"][..], command, &target].concat();
        let invalid_command = if at_root {
            [invalid, &command].concat()
        } else {
            [&command[..], invalid].concat()
        };
        let early = sandbox.run(&[&selectors[..], &invalid_command].concat());
        let late = sandbox.run(&[&invalid_command[..], &selectors].concat());
        let report: Value = serde_json::from_slice(&early.stdout)
            .unwrap_or_else(|error| panic!("{invalid_command:?}: {error}: {early:?}"));
        assert_eq!(report["schema_version"], 2);
        assert_eq!(report["command"], name);
        assert_eq!(report["error"]["code"], code);
        assert!(!report.to_string().contains("BAD!"));
        assert!(early.stderr.is_empty());
        assert_eq!(late.status, early.status, "{invalid_command:?}");
        assert_eq!(late.stdout, early.stdout, "{invalid_command:?}: {late:?}");
        assert!(late.stderr.is_empty());
    }

    for args in [
        &["--project=--json", "resource", "list", "--contract", "2"][..],
        &[
            "resource",
            "list",
            "--cursor=--json",
            "--at",
            "BAD!",
            "--contract",
            "2",
        ],
        &["--contract", "2", "resource", "show", "--", "--json"],
    ] {
        let output = sandbox.run(args);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(output.stdout.is_empty(), "{args:?}");
        assert!(!output.stderr.is_empty(), "{args:?}");
    }

    let v1 = sandbox.run(&["--json", "resource", "list"]);
    let unknown = sandbox.run(&["--json", "unrecognized", "list"]);
    assert_eq!(v1.status, unknown.status);
    assert_eq!(v1.stdout, unknown.stdout);
    assert_eq!(v1.stderr, unknown.stderr);
    sandbox.assert_empty();
}

#[test]
fn malformed_resource_json_switch_preserves_independent_output_selection() {
    let sandbox = Sandbox::new();
    let command = ["--contract", "2", "resource", "list"];
    let early = sandbox.run(&[&command[..], &["--format", "json", "--json=false"]].concat());
    assert_eq!(early.status.code(), Some(2), "{early:?}");
    assert!(early.stderr.is_empty(), "{early:?}");
    let report: Value = serde_json::from_slice(&early.stdout).unwrap();
    assert_eq!(report["schema_version"], 2);
    assert_eq!(report["command"], "resource list");
    assert_eq!(report["error"]["code"], "INVALID_ARGUMENT");
    assert_eq!(
        report["error"]["message"],
        "An argument has a missing or invalid value."
    );

    for options in [
        &["--json=false", "--format", "json"][..],
        &["--json=false", "--json"],
        &["--json", "--json=false"],
    ] {
        let output = sandbox.run(&[&command[..], options].concat());
        assert_eq!(output.status, early.status, "{options:?}");
        assert_eq!(output.stdout, early.stdout, "{options:?}");
        assert!(output.stderr.is_empty(), "{options:?}");
    }

    for args in [
        [&command[..], &["--json=false"]].concat(),
        [&command[..], &["--json=true"]].concat(),
        [&command[..], &["--json="]].concat(),
        [&command[..], &["--json=--json"]].concat(),
        [&command[..], &["--json=--format=json"]].concat(),
        [&command[..], &["--json=false", "--cursor=--json"]].concat(),
        vec![
            "--contract",
            "2",
            "resource",
            "show",
            "--json=false",
            "--",
            "--json",
        ],
    ] {
        let output = sandbox.run(&args);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(output.stdout.is_empty(), "{args:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("Usage: hive resource"),
            "{args:?}"
        );
    }
    sandbox.assert_empty();
}

#[test]
fn dolt_area_option_recovery_preserves_literals_and_v1_errors() {
    let sandbox = Sandbox::new();
    for args in [
        &["--project=--json", "area", "list", "--contract", "2"][..],
        &[
            "area",
            "list",
            "--cursor=--json",
            "--project",
            "BAD!",
            "--contract",
            "2",
        ],
        &[
            "area",
            "show",
            "--project=--json",
            "--contract",
            "2",
            "01900000-0000-7000-8000-000000000001",
        ],
        &[
            "--contract",
            "2",
            "area",
            "show",
            "--workspace-id",
            "01900000-0000-7000-8000-000000000001",
            "--database",
            "fixture_db",
            "--port",
            "3307",
            "--",
            "--json",
        ],
    ] {
        let output = sandbox.run(args);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(output.stdout.is_empty(), "{args:?}");
        assert!(!output.stderr.is_empty(), "{args:?}");
    }

    for invalid in [
        &["--project=--contract", "area", "list"][..],
        &["area", "list", "--project=--contract"],
        &["area", "list", "--cursor", "-1"],
    ] {
        let default = ["--json"]
            .iter()
            .chain(invalid)
            .copied()
            .collect::<Vec<_>>();
        let v1 = ["--contract", "1"]
            .iter()
            .chain(&default)
            .copied()
            .collect::<Vec<_>>();
        let output = sandbox.run(&default);
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(output.status.code(), Some(2), "{default:?}");
        assert_eq!(report["schema_version"], 1, "{default:?}");
        assert_eq!(report["error"]["code"], "INVALID_ARGUMENT", "{default:?}");
        let explicit = sandbox.run(&v1);
        assert_eq!(explicit.status.code(), output.status.code(), "{v1:?}");
        assert_eq!(explicit.stdout, output.stdout, "{v1:?}");
        assert_eq!(explicit.stderr, output.stderr, "{v1:?}");
    }
    sandbox.assert_empty();
}

#[test]
fn directive_preserves_v1_help_and_reports_late_v2_known_option_errors() {
    let sandbox = Sandbox::new();
    for operation in ["list", "show"] {
        let help = sandbox.run(&["directive", operation, "--help"]);
        assert!(help.status.success());
        let help = String::from_utf8(help.stdout).unwrap();
        for option in [
            "--workspace-scope",
            "--area",
            "--workspace-id",
            "--database",
            "--port",
            "--cursor",
        ] {
            assert!(!help.contains(option), "{help}");
        }
    }
    for args in [
        vec!["directive", "list", "--status", "ACTIVE"],
        vec!["directive", "list", "--project", "BAD_SCOPE"],
        vec!["directive", "list", "--limit", "-1"],
        vec![
            "directive",
            "list",
            "--workspace-scope",
            "--area",
            "malformed",
        ],
        vec![
            "directive",
            "list",
            "--database",
            "first",
            "--database",
            "second",
        ],
        vec!["directive", "list", "--limit"],
        vec!["directive", "list", "--workspace-scope=false"],
        vec![
            "directive",
            "list",
            "--workspace-scope",
            "--workspace-scope",
        ],
        vec!["directive", "list", "--authority=false"],
        vec!["directive", "list", "--at", "bad-commit"],
        vec!["directive", "show", "BAD_ID"],
        vec!["directive", "show", "--workspace-scope"],
    ] {
        let mut command = args.clone();
        command.extend(["--contract", "2", "--json"]);
        let output = sandbox.run(&command);
        assert_eq!(output.status.code(), Some(2), "{output:?}");
        assert!(output.stderr.is_empty(), "{output:?}");
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["schema_version"], 2);
        assert_eq!(value["command"], format!("directive {}", args[1]));
        assert_eq!(value["error"]["code"], "INVALID_ARGUMENT");
    }

    for args in [
        &["--contract", "2", "directive", "show", "--", "--json"][..],
        &["--contract", "2", "directive", "list", "--status=--json"],
        &["--contract", "2", "directive", "list", "--limit=--json"],
        &["--contract", "2", "directive", "list", "--project=--json"],
        &[
            "--contract",
            "2",
            "directive",
            "list",
            "--workspace-scope=--json",
        ],
    ] {
        let output = sandbox.run(args);
        assert_eq!(output.status.code(), Some(2), "{output:?}");
        assert!(output.stdout.is_empty(), "{output:?}");
    }
    sandbox.assert_empty();
}

#[test]
fn unknown_directive_long_option_preserves_v2_errors_and_literal_boundaries() {
    let sandbox = Sandbox::new();
    let directive = "01900000-0000-7000-8000-000000000004";
    for leaf in ["list", "show"] {
        let mut command = vec!["directive", leaf];
        if leaf == "show" {
            command.push(directive);
        }
        for options in [
            &["--unknown", "--contract=2", "--json"][..],
            &["--unknown=--contract=1", "--contract", "2", "--format=json"],
            &["--unknown=--help", "--contract=2", "--json"],
            &["--contract=2", "--unknown=--json", "--format=json"],
        ] {
            let args = [&command[..], options].concat();
            let output = sandbox.run(&args);
            assert_eq!(output.status.code(), Some(2), "{args:?}");
            assert!(output.stderr.is_empty(), "{args:?}");
            let report: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(
                report,
                serde_json::json!({
                    "schema_version": 2, "ok": false, "command": format!("directive {leaf}"),
                    "error": {
                        "code": "INVALID_ARGUMENT",
                        "message": "An argument or command is not recognized.",
                    },
                    "warnings": [],
                }),
                "{args:?}",
            );
        }

        for options in [
            &["--unknown=--json", "--contract=2"][..],
            &["--unknown", "--contract=2", "--", "--json"],
            &["--", "--unknown", "--contract=2", "--json"],
            &["--unknown", "detached", "--contract=2", "--json"],
        ] {
            let args = [&command[..], options].concat();
            let output = sandbox.run(&args);
            assert_eq!(output.status.code(), Some(2), "{args:?}");
            assert!(output.stdout.is_empty(), "{args:?}");
            assert!(!output.stderr.is_empty(), "{args:?}");
        }
    }
    let args = ["directive", "show", "--unknown", "--contract=2", "--json"];
    let output = sandbox.run(&args);
    assert_eq!(output.status.code(), Some(2), "{args:?}");
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["schema_version"], 2);
    assert_eq!(report["command"], "directive show");
    assert_eq!(
        report["error"]["message"],
        "An argument or command is not recognized.",
    );

    let args = ["--json", "directive", "list", "--unknown=--contract=2"];
    let output = sandbox.run(&args);
    assert_json_failure(&output, &args, "directive list", "INVALID_ARGUMENT", 2);
    sandbox.assert_empty();
}

#[test]
fn artifact_contract_selection_preserves_legacy_help_and_v2_error_boundaries() {
    let sandbox = Sandbox::new();
    let legacy = sandbox.run(&["artifact", "list", "--help"]);
    assert!(legacy.status.success());
    let help = String::from_utf8(legacy.stdout).unwrap();
    assert!(help.contains("--task"));
    assert!(help.contains("--standalone"));
    assert!(!help.contains("--scope"));
    let v2 = sandbox.run(&["artifact", "list", "--contract", "2", "--help"]);
    assert!(v2.status.success());
    assert!(String::from_utf8(v2.stdout).unwrap().contains("--scope"));

    for (leaf, options) in [
        ("list", &["--limit", "-7"][..]),
        ("list", &["--scope", "unknown:bad"][..]),
        ("list", &["--scope", "one", "--scope", "two"][..]),
        ("list", &["--standalone"][..]),
        ("list", &["--scope"][..]),
        ("show", &["bad", "--authority"][..]),
        ("show", &["bad", "--approval", "fixture"][..]),
        ("show", &["--workspace", "fixture"][..]),
        ("show", &["not-a-uuid", "--file", "fixture"][..]),
    ] {
        let selectors = ["--contract", "2", "--json"];
        let command = [&["artifact", leaf][..], options].concat();
        let early = sandbox.run(&[&selectors[..], &command].concat());
        let late = sandbox.run(&[&command[..], &selectors].concat());
        assert_eq!(early.status.code(), Some(2), "{command:?}");
        assert_eq!(early.status.code(), late.status.code(), "{command:?}");
        assert_eq!(early.stdout, late.stdout, "{command:?}");
        assert!(
            early.stderr.is_empty() && late.stderr.is_empty(),
            "{command:?}"
        );
        let report: Value = serde_json::from_slice(&early.stdout).unwrap();
        assert_eq!(report["schema_version"], 2);
        assert_eq!(report["command"], format!("artifact {leaf}"));
        assert_eq!(report["error"]["code"], "INVALID_ARGUMENT");
    }

    for args in [
        &["artifact", "list", "--scope=--json", "--contract", "2"][..],
        &["artifact", "show", "--project=--json", "--contract", "2"][..],
        &["artifact", "show", "--contract", "2", "--", "--json"][..],
    ] {
        let output = sandbox.run(args);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(output.stdout.is_empty(), "{args:?}");
        assert!(!output.stderr.is_empty(), "{args:?}");
    }
    sandbox.assert_empty();
}

#[test]
fn unknown_artifact_long_option_preserves_v2_errors_and_literal_boundaries() {
    let sandbox = Sandbox::new();
    let artifact = "01900000-0000-7000-8000-000000000004";
    for leaf in ["list", "show"] {
        let mut command = vec!["artifact", leaf];
        if leaf == "show" {
            command.push(artifact);
        }
        for options in [
            &["--unknown", "--contract=2", "--json"][..],
            &["--unknown=--contract=1", "--contract", "2", "--format=json"],
            &["--unknown=--help", "--contract=2", "--json"],
            &["--contract=2", "--unknown=--json", "--format=json"],
        ] {
            let args = [&command[..], options].concat();
            let output = sandbox.run(&args);
            assert_eq!(output.status.code(), Some(2), "{args:?}");
            assert!(output.stderr.is_empty(), "{args:?}");
            let report: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(
                report,
                serde_json::json!({
                    "schema_version": 2, "ok": false, "command": format!("artifact {leaf}"),
                    "error": {
                        "code": "INVALID_ARGUMENT",
                        "message": "An argument or command is not recognized.",
                    },
                    "warnings": [],
                }),
                "{args:?}",
            );
        }
        for options in [
            &["--unknown=--json", "--contract=2"][..],
            &["--unknown", "--contract=2", "--", "--json"],
            &["--", "--unknown", "--contract=2", "--json"],
            &["--unknown", "detached", "--contract=2", "--json"],
        ] {
            let args = [&command[..], options].concat();
            let output = sandbox.run(&args);
            assert_eq!(output.status.code(), Some(2), "{args:?}");
            assert!(output.stdout.is_empty(), "{args:?}");
            assert!(!output.stderr.is_empty(), "{args:?}");
        }

        let args = [&["--json"][..], &command, &["--unknown=--contract=2"]].concat();
        let output = sandbox.run(&args);
        let explicit_v1 = sandbox.run(&[&["--contract", "1"][..], &args].concat());
        assert_json_failure(
            &output,
            &args,
            &format!("artifact {leaf}"),
            "INVALID_ARGUMENT",
            2,
        );
        assert_eq!(output.status.code(), explicit_v1.status.code(), "{args:?}");
        assert_eq!(output.stdout, explicit_v1.stdout, "{args:?}");
        assert_eq!(output.stderr, explicit_v1.stderr, "{args:?}");
    }
    sandbox.assert_empty();
}

#[test]
fn project_reads_parse_late_v2_flags_and_preserve_literal_values() {
    let sandbox = Sandbox::new();
    let target = [
        "--database",
        "fixture_db",
        "--port",
        "3307",
        "--workspace-id",
        "01900000-0000-7000-8000-000000000001",
    ];
    for (index, (action, options)) in [
        ("list", &["--status", "ACTIVE"][..]),
        ("list", &["--status"]),
        ("list", &["--status=--json"]),
        ("list", &["--limit", "-1"]),
        ("list", &["--limit", "101"]),
        ("show", &["project/fixture"]),
        ("show", &["-1"]),
        ("show", &[]),
    ]
    .into_iter()
    .enumerate()
    {
        let mut args = vec!["project", action];
        args.extend(target);
        args.extend(options);
        args.extend([
            "--contract=2",
            if index % 2 == 0 {
                "--json"
            } else {
                "--format=json"
            },
        ]);
        let output = sandbox.run(&args);
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(output.stderr.is_empty(), "{args:?}");
        assert_eq!(report["schema_version"], 2, "{args:?}");
        assert_eq!(report["command"], format!("project {action}"), "{args:?}");
        assert_eq!(report["error"]["code"], "INVALID_ARGUMENT", "{args:?}");
        assert!(report.get("context").is_none(), "{args:?}");
    }
    let literal_contract = sandbox.run(&["--json", "project", "list", "--after=--contract=2"]);
    assert_json_failure(
        &literal_contract,
        &["--json", "project", "list", "--after=--contract=2"],
        "project list",
        "NOT_IMPLEMENTED",
        7,
    );

    for options in [&["list", "--status=--json"][..], &["show", "--", "--json"]] {
        let mut args = vec!["--contract", "2"];
        args.push("project");
        args.push(options[0]);
        args.extend(target);
        args.extend_from_slice(&options[1..]);
        let output = sandbox.run(&args);
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        assert!(!output.stderr.is_empty());
    }
    let output = sandbox.run(&["project", "list", "--help", "--contract", "2"]);
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("--status"));
    assert!(output.stderr.is_empty());
    sandbox.assert_empty();
}

#[test]
fn unknown_project_long_option_keeps_original_error_with_late_v2_json() {
    let sandbox = Sandbox::new();
    for (command, args) in [
        (
            "project list",
            &["project", "list", "--unknown", "--contract=2", "--json"][..],
        ),
        (
            "project list",
            &[
                "project",
                "list",
                "--unknown",
                "--limit",
                "0",
                "--contract=2",
                "--json",
            ],
        ),
        (
            "project list",
            &[
                "project",
                "list",
                "--unknown=--contract=1",
                "--contract=2",
                "--json",
            ],
        ),
        (
            "project list",
            &[
                "project",
                "list",
                "--unknown=--help",
                "--contract=2",
                "--json",
            ],
        ),
        (
            "project show",
            &[
                "project",
                "show",
                "--unknown=--json",
                "01900000-0000-7000-8000-000000000003",
                "--contract",
                "2",
                "--format=json",
            ],
        ),
    ] {
        let output = sandbox.run(args);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(output.stderr.is_empty(), "{args:?}");
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(
            report,
            serde_json::json!({
                "schema_version": 2, "ok": false, "command": command,
                "error": {
                    "code": "INVALID_ARGUMENT",
                    "message": "An argument or command is not recognized.",
                },
                "warnings": [],
            }),
            "{args:?}",
        );
    }
    sandbox.assert_empty();
}

#[test]
fn unknown_project_long_option_recovery_keeps_literals_and_legacy_boundaries() {
    let sandbox = Sandbox::new();
    for args in [
        &["project", "list", "--unknown=--json", "--contract=2"][..],
        &[
            "project",
            "list",
            "--status=--json",
            "--unknown",
            "--contract=2",
        ],
        &[
            "project",
            "list",
            "--unknown",
            "--contract=2",
            "--",
            "--json",
        ],
        &[
            "project",
            "show",
            "--unknown",
            "--contract=2",
            "--",
            "--json",
        ],
        &[
            "project",
            "list",
            "--",
            "--unknown",
            "--contract=2",
            "--json",
        ],
        &[
            "project",
            "list",
            "--unknown",
            "value",
            "--contract=2",
            "--json",
        ],
        &["--unknown", "project", "list", "--contract=2", "--json"],
    ] {
        let output = sandbox.run(args);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(output.stdout.is_empty(), "{args:?}");
        assert!(
            output
                .stderr
                .starts_with(b"error: An argument or command is not recognized.\n"),
            "{args:?}",
        );
    }
    for args in [
        &["--json", "project", "list", "--unknown=--contract=2"][..],
        &[
            "--json",
            "project",
            "show",
            "--unknown",
            "--",
            "--contract=2",
        ],
    ] {
        let output = sandbox.run(args);
        let explicit_v1 = sandbox.run(&[&["--contract", "1"][..], args].concat());
        assert_json_failure(
            &output,
            args,
            &format!("project {}", args[2]),
            "INVALID_ARGUMENT",
            2,
        );
        assert_eq!(output.status.code(), explicit_v1.status.code(), "{args:?}");
        assert_eq!(output.stdout, explicit_v1.stdout, "{args:?}");
        assert_eq!(output.stderr, explicit_v1.stderr, "{args:?}");
    }
    sandbox.assert_empty();
}

#[test]
fn duplicate_and_missing_config_scope_options_preserve_v2_parse_errors() {
    let sandbox = Sandbox::new();
    let command = ["config", "show"];
    let selectors = ["--contract", "2", "--json"];
    let area = "01900000-0000-7000-8000-000000000001";
    for (invalid, message) in [
        (&["--area", area, "--area", area][..], "Arguments conflict."),
        (
            &["--resource", ".", "--resource", "."],
            "Arguments conflict.",
        ),
        (&["--area"], "An argument has a missing or invalid value."),
        (
            &["--resource"],
            "An argument has a missing or invalid value.",
        ),
        (
            &["--resource="],
            "An argument has a missing or invalid value.",
        ),
        (
            &["--area="],
            "CLI --area must be a canonical lowercase UUIDv7.",
        ),
    ] {
        let early = sandbox.run(&[&selectors[..], &command, invalid].concat());
        let late = sandbox.run(&[&command[..], invalid, &selectors].concat());
        for output in [&early, &late] {
            assert_eq!(output.status.code(), Some(2), "{invalid:?}");
            assert!(output.stderr.is_empty(), "{invalid:?}");
            let report: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(report["schema_version"], 2, "{invalid:?}");
            assert_eq!(report["command"], "config show", "{invalid:?}");
            assert_eq!(report["error"]["code"], "INVALID_ARGUMENT", "{invalid:?}");
            assert_eq!(report["error"]["message"], message, "{invalid:?}");
        }
        assert_eq!(early.stdout, late.stdout, "{invalid:?}");
    }

    for args in [
        &["--area=--json", "--area"][..],
        &["--resource=--json", "--resource"],
        &["--resource=", "--area=--json"],
        &["--resource=", "--area=--contract"],
    ] {
        let output = sandbox.run(&[&command[..], args, &["--contract", "2"]].concat());
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(output.stdout.is_empty(), "{args:?}");
        assert!(!output.stderr.is_empty(), "{args:?}");
    }
}

#[test]
fn conversation_show_rejects_default_selection_and_preserves_late_v2_json() {
    let sandbox = Sandbox::new();
    let uuid = "01900000-0000-7000-8000-000000000001";
    let target = [
        "--workspace-id",
        uuid,
        "--database",
        "fixture",
        "--port",
        "3306",
    ];
    let selectors = ["--bot", uuid, "--id", uuid];
    let late = ["--contract", "2", "--json"];
    let command = ["conversation", "show"];
    let default = sandbox.0.join(".local/share/hive/v2/userdata");
    fs::create_dir_all(&default).unwrap();
    fs::write(
        default.join("hive-workspace.yml"),
        format!(
            "version: 2\nworkspace_id: {uuid}\nschema_version: 1\npolicy_revision: {REVISION}\n"
        ),
    )
    .unwrap();

    for args in [
        [&command[..], &target, &selectors, &late].concat(),
        [&command[..], &["--workspace", "."], &selectors, &late].concat(),
        [
            &command[..],
            &["--workspace", "."],
            &target,
            &["--bot", uuid],
            &late,
        ]
        .concat(),
        [
            &command[..],
            &["--workspace", "."],
            &target,
            &["--bot=--json", "--id", uuid],
            &late,
        ]
        .concat(),
        [
            &command[..],
            &["--workspace", "."],
            &target,
            &["--bot", uuid, "--id", "private-invalid-selector"],
            &late,
        ]
        .concat(),
    ] {
        let output = sandbox.run(&args);
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(output.status.code(), Some(2), "{args:?} {report}");
        assert!(output.stderr.is_empty());
        assert_eq!(report["schema_version"], 2);
        assert_eq!(report["command"], "conversation show");
        assert_eq!(report["ok"], false);
        assert_eq!(report["error"]["code"], "INVALID_ARGUMENT");
        assert!(report.get("data").is_none());
        assert!(
            !String::from_utf8(output.stdout)
                .unwrap()
                .contains("private-invalid-selector")
        );
    }

    let literal = [
        &command[..],
        &["--workspace", "."],
        &target,
        &["--bot=--json", "--id", uuid, "--contract", "2"],
    ]
    .concat();
    let output = sandbox.run(&literal);
    assert!(output.stdout.is_empty());
    assert_eq!(output.status.code(), Some(2));

    let legacy = [&command[..], &selectors, &["--json"]].concat();
    let output = sandbox.run(&legacy);
    assert_json_failure(&output, &legacy, "conversation show", "INVALID_ARGUMENT", 2);
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["schema_version"], 1);
}

#[test]
fn conversation_show_recovers_v2_json_for_duplicate_and_hyphenated_selectors() {
    let sandbox = Sandbox::new();
    let uuid = "01900000-0000-7000-8000-000000000001";
    let command = [
        "conversation",
        "show",
        "--workspace",
        ".",
        "--workspace-id",
        uuid,
        "--database",
        "fixture",
        "--port",
        "3306",
    ];
    let late = ["--contract", "2", "--json"];
    for selectors in [
        &["--bot", uuid, "--bot", uuid, "--id", uuid][..],
        &[
            "--bot",
            uuid,
            "--id",
            uuid,
            "--id",
            "private-invalid-selector",
        ],
        &["--bot", "-invalid", "--id", uuid],
        &["--bot", uuid, "--id", "-invalid"],
        &["--bot", uuid, "--bot=--json", "--id", uuid],
        &["--bot", uuid, "--id", uuid, "--id=--contract"],
        &["--bot=-invalid", "--id", uuid],
        &["--bot", uuid, "--id=-invalid"],
        &["--bot", uuid, "--id"],
        &["--id", uuid, "--bot"],
    ] {
        let args = [&command[..], selectors, &late].concat();
        let output = sandbox.run(&args);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(output.stderr.is_empty(), "{args:?}");
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["schema_version"], 2, "{args:?}");
        assert_eq!(report["command"], "conversation show");
        assert_eq!(report["ok"], false);
        assert_eq!(report["error"]["code"], "INVALID_ARGUMENT");
        assert!(report.get("data").is_none());
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(!stdout.contains("-invalid"));
    }

    for selectors in [
        &[
            "--bot",
            uuid,
            "--bot=--json",
            "--id",
            uuid,
            "--contract",
            "2",
        ][..],
        &[
            "--bot",
            "123",
            "--id",
            uuid,
            "--id=--json",
            "--contract",
            "2",
        ],
        &[
            "--bot",
            "-invalid",
            "--id",
            uuid,
            "--",
            "--contract",
            "2",
            "--json",
        ],
        &[
            "--bot",
            uuid,
            "--id",
            uuid,
            "--id",
            uuid,
            "--",
            "--contract",
            "2",
            "--json",
        ],
        &[
            "--bot",
            uuid,
            "--id",
            uuid,
            "--id",
            "--",
            "--contract",
            "2",
            "--json",
        ],
    ] {
        let args = [&command[..], selectors].concat();
        let output = sandbox.run(&args);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(output.stdout.is_empty(), "{args:?}");
        assert!(!String::from_utf8_lossy(&output.stderr).contains("-invalid"));
    }
    sandbox.assert_empty();
}

#[test]
fn conversation_show_recovers_output_for_target_and_scope_errors() {
    let sandbox = Sandbox::new();
    let uuid = "01900000-0000-7000-8000-000000000001";
    fs::write(
        sandbox.0.join("hive-workspace.yml"),
        format!(
            "version: 2\nworkspace_id: {uuid}\nschema_version: 1\npolicy_revision: {REVISION}\n"
        ),
    )
    .unwrap();
    let command = ["conversation", "show"];
    let output_options = ["--contract", "2", "--json"];
    let target = [
        ["--workspace", "."],
        ["--workspace-id", uuid],
        ["--database", "fixture"],
        ["--port", "3306"],
        ["--bot", uuid],
        ["--id", uuid],
    ];
    let check = |args: &[&str]| {
        let output = sandbox.run(args);
        let report: Value = serde_json::from_slice(&output.stdout)
            .unwrap_or_else(|error| panic!("{args:?}: {error}\n{output:?}"));
        assert!(output.status.code().is_some_and(|code| code != 0));
        assert!(output.stderr.is_empty(), "{args:?}");
        assert_eq!(report["schema_version"], 2, "{args:?}");
        assert_eq!(report["command"], "conversation show", "{args:?}");
        assert_eq!(report["ok"], false, "{args:?}");
        assert!(report.get("data").is_none(), "{args:?}");
        let message = report["error"]["message"].as_str().unwrap();
        for private in ["INVALID", "-hidden", "private-selector", "other-copy"] {
            assert!(!message.contains(private), "{report}");
        }
        (output.status.code(), report)
    };

    for (flag, valid, duplicate, invalid, root_allowed) in [
        ("--port", "3306", "3306", "private-selector", false),
        (
            "--database",
            "fixture",
            "fixture",
            "private-selector!",
            false,
        ),
        ("--workspace-id", uuid, uuid, "private-selector", false),
        ("--bot", uuid, uuid, "private-selector", false),
        ("--id", uuid, uuid, "private-selector", false),
        (
            "--workspace",
            ".",
            "other-copy",
            "https://private-selector",
            true,
        ),
        (
            "--project",
            "example-project",
            "other-project",
            "INVALID",
            true,
        ),
        (
            "--project-root",
            ".",
            "other-copy",
            "private-selector",
            true,
        ),
        (
            "--at",
            REVISION,
            "2222222222222222222222222222222222222222",
            "INVALID",
            true,
        ),
    ] {
        let other = target
            .iter()
            .filter(|pair| pair[0] != flag)
            .flatten()
            .copied()
            .collect::<Vec<_>>();
        let empty = format!("{flag}=");
        for (values, expected_message) in [
            (
                &[flag, valid, flag, duplicate][..],
                (!root_allowed).then_some("Arguments conflict."),
            ),
            (&[flag, ""], None),
            (&[empty.as_str()], None),
            (&[flag, "-1"], None),
            (
                &[flag, "-hidden"],
                Some("An argument has a missing or invalid value."),
            ),
            (
                &[flag, invalid],
                matches!(flag, "--project" | "--at")
                    .then_some("An argument has a missing or invalid value."),
            ),
        ] {
            for at_root in [false, true] {
                if at_root && !root_allowed {
                    continue;
                }
                let (root, leaf) = if at_root {
                    (values, &[][..])
                } else {
                    (&[][..], values)
                };
                let early = [&output_options[..], root, &command, &other, leaf].concat();
                let late = [root, &command, &other, leaf, &output_options].concat();
                let early = check(&early);
                let late = check(&late);
                assert_eq!(early, late, "{flag} {values:?} at_root={at_root}");
                if let Some(message) = expected_message {
                    assert_eq!(late.0, Some(2));
                    assert_eq!(late.1["error"]["code"], "INVALID_ARGUMENT");
                    assert_eq!(late.1["error"]["message"], message, "{flag} {values:?}");
                }
            }
        }

        for at_root in [false, true] {
            if at_root && !root_allowed {
                continue;
            }
            let literal = format!("{flag}=--json");
            let literal = [literal.as_str()];
            let (root, leaf) = if at_root {
                (&literal[..], &[][..])
            } else {
                (&[][..], &literal[..])
            };
            let args = [
                root,
                &command,
                &other,
                &["--port", "3306", "--port", "3306"],
                leaf,
                &["--contract", "2"],
            ]
            .concat();
            let output = sandbox.run(&args);
            assert_eq!(output.status.code(), Some(2), "{args:?}");
            assert!(output.stdout.is_empty(), "{args:?}");
            assert!(!output.stderr.is_empty(), "{args:?}");
        }
    }

    let full = target.into_iter().flatten().collect::<Vec<_>>();
    for authority in [
        "--authority",
        "--authority=",
        "--authority=INVALID",
        "--authority=-1",
    ] {
        let early = [&output_options[..], &command, &full, &[authority]].concat();
        let late = [&command[..], &full, &[authority], &output_options].concat();
        assert_eq!(check(&early), check(&late), "{authority}");
    }
    let legacy = [&command[..], &full, &["--project", "INVALID", "--json"]].concat();
    let explicit_v1 = [&["--contract", "1"][..], &legacy].concat();
    let implicit = sandbox.run(&legacy);
    let explicit = sandbox.run(&explicit_v1);
    assert_eq!(implicit.status, explicit.status);
    assert_eq!(implicit.stdout, explicit.stdout);
    assert_eq!(implicit.stderr, explicit.stderr);
}

#[test]
fn soul_inspection_requires_contract_two_and_an_explicit_root() {
    let sandbox = Sandbox::new();
    for (contract, expected) in [
        ("1", "Dolt Bot options require --contract 2."),
        ("2", "Soul inspection requires an explicit --workspace."),
    ] {
        let args = [
            "--contract",
            contract,
            "--json",
            "bot",
            "show",
            "01900000-0000-7000-8000-00000000000a",
            "--asset-commit",
            REVISION,
        ];
        let output = sandbox.run(&args);
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(output.status.code(), Some(2), "{report}");
        assert_eq!(report["schema_version"], contract.parse::<u32>().unwrap());
        assert_eq!(report["error"]["code"], "INVALID_ARGUMENT");
        assert_eq!(report["error"]["message"], expected);
        assert!(output.stderr.is_empty());
    }
    sandbox.assert_empty();
}
