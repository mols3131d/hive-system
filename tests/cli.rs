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
fn dolt_issue_list_requires_one_explicit_scope_and_preserves_v1_help() {
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

    let leaf = sandbox.run(&["issue", "list", "--help"]);
    assert_eq!(leaf.status.code(), Some(0));
    let help = String::from_utf8(leaf.stdout).unwrap();
    assert!(help.contains("Usage: hive issue list"));
    for option in ["--all", "--area", "--project", "--limit", "--cursor"] {
        assert!(help.contains(option), "{help}");
    }

    let v1_list = ["--json", "issue", "list", "--all"];
    assert_json_failure(
        &sandbox.run(&v1_list),
        &v1_list,
        "issue list",
        "INVALID_ARGUMENT",
        2,
    );

    let target = [
        "--contract",
        "2",
        "--json",
        "issue",
        "list",
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
        assert_eq!(report["command"], "issue list", "{args:?}");
        assert_eq!(report["error"]["code"], code, "{args:?}");
        assert!(output.stderr.is_empty(), "{args:?}");
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
