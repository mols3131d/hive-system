#![cfg(unix)]

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

const POLICY: &str = "0e584830a7eff314d85b7af8c6f8cd69e735730a";
const LEGACY_POLICY: &str = "0fd0f8dbe632d14094a93599cb1b5f653789ac9c";
const PROJECT: &str = "fixture-project";
const PROJECT_URL: &str = "https://forge.example/fixture/project";
const BINDING: &str = ".local/hive/config.yml";
const LOCAL_CONFIG: &str = "version: 1\ncoordination:\n  mode: local\n";
const USER_CONFIG: &str = "workspace: ../../workspace\noutput:\n  format: json\n";

struct Sandbox(PathBuf);

impl Sandbox {
    fn new() -> Self {
        static NEXT_ID: AtomicUsize = AtomicUsize::new(0);
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "hive-config-{}-{timestamp}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        for directory in ["home", "xdg", "invocation"] {
            fs::create_dir(root.join(directory)).unwrap();
        }
        Self(root.canonicalize().unwrap())
    }

    fn command(&self, program: &str, cwd: &Path) -> Command {
        let mut command = Command::new(program);
        command
            .env_clear()
            .env("HOME", self.0.join("home"))
            .env("XDG_CONFIG_HOME", self.0.join("xdg"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("PATH", std::env::var_os("PATH").unwrap())
            .current_dir(cwd);
        command
    }

    fn hive(&self, cwd: &Path, args: &[&str]) -> Command {
        let mut command = self.command(env!("CARGO_BIN_EXE_hive"), cwd);
        command.args(args);
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        self.run_in(&self.0.join("invocation"), args)
    }

    fn run_in(&self, cwd: &Path, args: &[&str]) -> Output {
        self.hive(cwd, args).output().unwrap()
    }

    fn git(&self, cwd: &Path, args: &[&str]) -> String {
        let output = self.command("git", cwd).args(args).output().unwrap();
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    fn init_repo(&self, name: &str) -> PathBuf {
        let root = self.0.join(name);
        fs::create_dir(&root).unwrap();
        self.git(&root, &["init", "-q", "--initial-branch=main"]);
        self.git(&root, &["config", "user.name", name]);
        self.git(
            &root,
            &["config", "user.email", &format!("{name}@example.test")],
        );
        root
    }

    fn commit(&self, root: &Path) {
        self.git(root, &["add", "."]);
        // Keep fixture commits from detaching background maintenance that races with snapshots.
        self.git(
            root,
            &[
                "-c",
                "maintenance.auto=false",
                "commit",
                "-qm",
                "Fixture",
                "--allow-empty",
            ],
        );
    }

    fn workspace(&self, name: &str, project_url: &str) -> PathBuf {
        let root = self.init_repo(name);
        write_policy(&root, POLICY);
        write(&root.join("hive.yml"), LOCAL_CONFIG);
        write(
            &root.join("tasks").join(PROJECT).join("README.md"),
            format!("# Project fixture\n\nProject: [Fixture repository]({project_url})\n"),
        );
        self.commit(&root);
        root
    }

    fn project(&self, name: &str, origin: &str) -> PathBuf {
        let root = self.init_repo(name);
        self.git(&root, &["remote", "add", "origin", origin]);
        write(&root.join("src/README.md"), "Tracked project file\n");
        self.commit(&root);
        root
    }

    fn binding(&self, root: &Path, yaml: &str) {
        write(&root.join(".git/info/exclude"), format!("/{BINDING}\n"));
        write(&root.join(BINDING), yaml);
    }

    fn user_config(&self, yaml: &str) {
        write(&self.0.join("xdg/hive/config.yml"), yaml);
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn write(path: &Path, contents: impl AsRef<[u8]>) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

fn write_policy(root: &Path, revision: &str) {
    write(
        &root.join("AGENTS.md"),
        format!(
            "# Workspace fixture\n\n[Workspace policy](https://github.com/mols3131d/hive-system/blob/{revision}/docs/policies/workspace/README.md)\n"
        ),
    );
}

fn json_result(output: &Output, command: &str, exit: i32) -> Value {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(exit),
        "hive {command}\nstdout: {stdout}\nstderr: {stderr}"
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!("hive {command} printed invalid JSON: {error}\nstdout: {stdout}\nstderr: {stderr}")
    });
    assert_eq!(result["schema_version"], 1);
    assert_eq!(result["ok"], exit == 0);
    assert_eq!(result["command"], command);
    assert!(result["warnings"].is_array());
    if exit == 0 {
        assert!(result.get("error").is_none());
        assert!(result["data"].is_object());
    } else {
        assert!(result.get("data").is_none());
        assert!(
            result["error"]["message"]
                .as_str()
                .is_some_and(|message| !message.is_empty())
        );
    }
    result
}

fn assert_error(output: &Output, command: &str, code: &str, exit: i32) {
    assert_eq!(json_result(output, command, exit)["error"]["code"], code);
}

fn assert_redacted(output: &Output, private_values: &[&str]) {
    for stream in [&output.stdout, &output.stderr] {
        let text = String::from_utf8_lossy(stream);
        for value in private_values {
            assert!(
                !text.contains(value),
                "diagnostic exposed a private fixture value"
            );
        }
    }
}

fn snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn visit(root: &Path, dir: &Path, entries: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                visit(root, &path, entries);
            } else {
                entries.insert(
                    path.strip_prefix(root).unwrap().to_owned(),
                    fs::read(&path).unwrap(),
                );
            }
        }
    }

    let mut entries = BTreeMap::new();
    visit(root, root, &mut entries);
    entries
}

fn assert_unchanged(root: &Path, before: &BTreeMap<PathBuf, Vec<u8>>) {
    let after = snapshot(root);
    assert!(
        before.keys().eq(after.keys()),
        "file paths changed in {}",
        root.display()
    );
    for (path, contents) in before {
        assert!(
            &after[path] == contents,
            "contents changed in {}",
            root.join(path).display()
        );
    }
}

#[test]
fn user_defaults_use_absolute_xdg_or_home_and_relative_path_at_source() {
    let fixture = Sandbox::new();
    let workspace = fixture.workspace("workspace", PROJECT_URL);
    let fallback = fixture.workspace("fallback-workspace", PROJECT_URL);
    fixture.user_config(USER_CONFIG);
    let fallback_source = fixture.0.join("home/.config/hive/config.yml");
    write(
        &fallback_source,
        "workspace: ../../../fallback-workspace\noutput:\n  format: json\n",
    );

    let context = json_result(&fixture.run(&["context"]), "context", 0);
    assert_eq!(
        context["data"]["workspace_root"],
        workspace.to_str().unwrap()
    );
    assert_eq!(context["data"]["policy_revision"], POLICY);
    assert_eq!(context["data"]["output_format"], "json");
    assert_eq!(context["context"]["view"], "working-tree");
    assert_eq!(context["context"]["freshness"], "local");

    let cwd = fixture.0.join("invocation");
    write(&cwd.join("relative-xdg/hive/config.yml"), "malformed: [\n");
    let output = fixture
        .hive(&cwd, &["config", "show", "--scope", "user"])
        .env("XDG_CONFIG_HOME", "relative-xdg")
        .output()
        .unwrap();
    let user = json_result(&output, "config show", 0);
    assert_eq!(user["data"]["source"], fallback_source.to_str().unwrap());
    let declared_workspace = Path::new(user["data"]["workspace"].as_str().unwrap());
    assert!(declared_workspace.is_absolute());
    assert_eq!(declared_workspace.canonicalize().unwrap(), fallback);

    let output = fixture.run(&["context", "--format", "text"]);
    assert!(output.status.success());
    assert!(serde_json::from_slice::<Value>(&output.stdout).is_err());
    assert!(String::from_utf8_lossy(&output.stdout).contains(workspace.to_str().unwrap()));
}

#[test]
fn absent_user_config_and_help_work_without_resolution() {
    let fixture = Sandbox::new();
    let output = fixture.run(&["config", "show", "--scope", "user", "--json"]);
    let user = json_result(&output, "config show", 0);
    assert_eq!(user["data"]["present"], false);
    assert!(user["data"]["workspace"].is_null());

    fixture.user_config("unsupported: !fixture broken\n");
    for args in [
        &["--json", "context", "--help"][..],
        &["config", "show", "--json", "--help"],
        &["--json", "--version"],
    ] {
        let output = fixture.run(args);
        assert!(output.status.success(), "hive {args:?}");
        assert!(output.stderr.is_empty(), "hive {args:?}");
        assert!(String::from_utf8_lossy(&output.stdout).contains("hive"));
        assert!(serde_json::from_slice::<Value>(&output.stdout).is_err());
    }
}

#[test]
fn yaml_owners_reject_incorrect_or_unsupported_values() {
    let fixture = Sandbox::new();
    let workspace = fixture.workspace("workspace", PROJECT_URL);
    let project = fixture.project("project", PROJECT_URL);
    let invalid_user = [
        "workspace: one\nworkspace: two\n",
        "output:\n  format: json\n  format: text\n",
        "workspace: true\n",
        "workspace: null\n",
        "output:\n  format: xml\n",
        "output:\n  format: !fixture json\n",
        "workspace: [unterminated\n",
        "workspace: one\n---\nworkspace: two\n",
        "credential: fixture-secret-canary\n",
    ];
    let user_validation = ["config", "validate", "--scope", "user", "--json"];
    for yaml in invalid_user {
        fixture.user_config(yaml);
        let output = fixture.run(&user_validation);
        let result = json_result(&output, "config validate", 2);
        assert_eq!(result["error"]["code"], "INVALID_CONFIG");
        let diagnostic = &result["error"]["details"]["diagnostics"][0];
        assert_eq!(diagnostic["code"], "INVALID_CONFIG");
        assert_eq!(diagnostic["severity"], "error");
        assert_eq!(diagnostic["path"], "hive/config.yml");
        assert!(diagnostic["message"].is_string());
        assert_redacted(&output, &["fixture-secret-canary"]);
    }

    fixture.user_config(&"#\n".repeat(64 * 1024));
    assert_error(
        &fixture.run(&user_validation),
        "config validate",
        "INVALID_CONFIG",
        2,
    );
    let user_source = fixture.0.join("xdg/hive/config.yml");
    fs::remove_file(&user_source).unwrap();
    symlink(fixture.0.join("absent.yml"), user_source).unwrap();
    assert_error(
        &fixture.run(&user_validation),
        "config validate",
        "INVALID_CONFIG",
        2,
    );

    for yaml in [
        "project_id: false\n",
        "project_id: ../unsafe\n",
        "workspace: ../../../workspace\n",
    ] {
        fixture.binding(&project, yaml);
        let output = fixture.run(&[
            "config",
            "validate",
            "--scope",
            "project",
            "--project-root",
            "../project",
            "--json",
        ]);
        assert_error(&output, "config validate", "INVALID_CONFIG", 2);
    }

    let workspace_validation = [
        "config",
        "validate",
        "--scope",
        "workspace",
        "--workspace",
        "../workspace",
        "--json",
    ];
    for yaml in [
        "version: '1'\ncoordination: {mode: local}\n",
        "version: 1\ncoordination: {mode: local, remote: origin}\n",
        "version: 1\ncoordination: {mode: remote}\n",
        "version: 1\ncoordination: {mode: local, ref: refs/heads/other}\n",
    ] {
        write(&workspace.join("hive.yml"), yaml);
        fixture.commit(&workspace);
        let output = fixture.run(&workspace_validation);
        assert_error(&output, "config validate", "INVALID_CONFIG", 2);
    }

    write(
        &workspace.join("hive.yml"),
        "version: 2\ncoordination: {mode: local}\n",
    );
    fixture.commit(&workspace);
    assert_error(
        &fixture.run(&workspace_validation),
        "config validate",
        "INCOMPATIBLE_POLICY",
        4,
    );
}

#[test]
fn source_scopes_do_not_require_unrelated_valid_configurations() {
    let fixture = Sandbox::new();
    let workspace = fixture.workspace("workspace", PROJECT_URL);
    let project = fixture.project("project", PROJECT_URL);
    fixture.binding(&project, "project_id: [broken\n");
    write(&workspace.join("hive.yml"), "coordination: [broken\n");
    fixture.user_config(USER_CONFIG);

    let output = fixture.run_in(
        &project,
        &["config", "validate", "--scope", "user", "--json"],
    );
    assert_eq!(
        json_result(&output, "config validate", 0)["data"]["scope"],
        "user"
    );

    fixture.user_config("workspace: [broken\n");
    fixture.binding(
        &project,
        "project_id: fixture-project\nworkspace: ../../../workspace\n",
    );
    let output = fixture.run_in(
        &project.join("src"),
        &["config", "show", "--scope", "project", "--json"],
    );
    let selected = json_result(&output, "config show", 0);
    assert_eq!(selected["data"]["scope"], "project");
    assert_eq!(
        selected["data"]["source"],
        project.join(BINDING).to_str().unwrap()
    );
    assert_eq!(selected["data"]["project_id"], PROJECT);
    let declared_workspace = Path::new(selected["data"]["workspace"].as_str().unwrap());
    assert!(declared_workspace.is_absolute());
    assert_eq!(declared_workspace.canonicalize().unwrap(), workspace);

    fixture.binding(&project, "project_id: [broken\n");
    write(&workspace.join("hive.yml"), LOCAL_CONFIG);
    let output = fixture.run_in(
        &project,
        &[
            "config",
            "validate",
            "--scope",
            "workspace",
            "--workspace",
            "../workspace",
            "--json",
        ],
    );
    assert_eq!(
        json_result(&output, "config validate", 0)["data"]["scope"],
        "workspace"
    );
}

#[test]
fn project_binding_resolves_only_a_verified_workspace_project_pair() {
    let fixture = Sandbox::new();
    let workspace = fixture.workspace("workspace", PROJECT_URL);
    let alternate = fixture.workspace("alternate", "https://forge.example/fixture/other");
    fixture.user_config(USER_CONFIG);
    let project = fixture.project("project", PROJECT_URL);
    fixture.git(&project, &["remote", "rename", "origin", "upstream"]);
    fixture.binding(&project, "project_id: fixture-project\n");

    let cwd = project.join("src");
    let output = fixture.run_in(&cwd, &["context"]);
    let context = json_result(&output, "context", 0);
    assert_eq!(
        context["data"]["workspace_root"],
        workspace.to_str().unwrap()
    );
    assert_eq!(context["data"]["project_id"], PROJECT);
    assert_eq!(context["data"]["project_root"], project.to_str().unwrap());
    assert!(context["data"]["sources"].is_object());

    let output = fixture.run_in(&cwd, &["context", "--workspace", "../../alternate"]);
    assert_error(&output, "context", "AMBIGUOUS_SCOPE", 2);

    let output = fixture.run_in(
        &cwd,
        &[
            "context",
            "--workspace",
            "../../alternate",
            "--project",
            PROJECT,
        ],
    );
    let context = json_result(&output, "context", 0);
    assert_eq!(
        context["data"]["workspace_root"],
        alternate.to_str().unwrap()
    );
    assert_eq!(context["data"]["project_id"], PROJECT);
    assert!(context["data"].get("project_root").is_none());

    fixture.git(
        &project,
        &[
            "remote",
            "set-url",
            "upstream",
            "https://forge.example/fixture/unrelated",
        ],
    );
    let output = fixture.run_in(&cwd, &["context"]);
    assert_error(&output, "context", "AMBIGUOUS_SCOPE", 2);
}

#[test]
fn an_existing_malformed_or_tracked_binding_cannot_be_bypassed() {
    let fixture = Sandbox::new();
    fixture.workspace("workspace", PROJECT_URL);
    let project = fixture.project("project", PROJECT_URL);

    let args = [
        "context",
        "--workspace",
        "../workspace",
        "--project",
        PROJECT,
        "--json",
    ];
    fixture.binding(&project, "project_id: fixture-project\n");
    assert_error(
        &fixture.run_in(&project, &args),
        "context",
        "INVALID_CONFIG",
        2,
    );
    fixture.user_config(USER_CONFIG);
    fixture.binding(&project, "project_id: !fixture fixture-project\n");
    let output = fixture.run_in(&project, &args);
    assert_error(&output, "context", "INVALID_CONFIG", 2);

    let binding_contents = "project_id: fixture-project\nworkspace: ../../../workspace\n";
    let outside_binding = fixture.0.join("outside-binding.yml");
    write(&outside_binding, binding_contents);
    let binding = project.join(BINDING);
    fs::remove_file(&binding).unwrap();
    symlink(outside_binding, &binding).unwrap();
    assert_error(
        &fixture.run_in(&project, &args),
        "context",
        "INVALID_CONFIG",
        2,
    );

    fs::remove_file(&binding).unwrap();
    write(&binding, binding_contents);
    fixture.git(&project, &["add", "--force", BINDING]);
    fixture.commit(&project);
    let output = fixture.run_in(&project, &args);
    assert_error(&output, "context", "INVALID_CONFIG", 2);
}

#[test]
fn binding_canonical_references_preserve_git_address_distinctions() {
    let fixture = Sandbox::new();
    let reference = "git@forge.example:operators/project.git";
    let workspace = fixture.workspace("workspace", reference);
    let project = fixture.project("project", reference);
    fixture.binding(
        &project,
        "project_id: fixture-project\nworkspace: ../../../workspace\n",
    );
    let output = fixture.run_in(&project, &["context", "--json"]);
    assert_eq!(
        json_result(&output, "context", 0)["data"]["workspace_root"],
        workspace.to_str().unwrap()
    );

    for other in [
        "other@forge.example:operators/project.git",
        "git@forge.example:/operators/project.git",
        "https://forge.example/operators/project.git",
    ] {
        fixture.git(&project, &["remote", "set-url", "origin", other]);
        assert_error(
            &fixture.run_in(&project, &["context", "--json"]),
            "context",
            "AMBIGUOUS_SCOPE",
            2,
        );
    }
    for (reference, different) in [
        (
            "https://forge.example/project.",
            "https://forge.example/project",
        ),
        ("/example/project copy", "/example/project"),
    ] {
        write(
            &workspace.join("tasks").join(PROJECT).join("README.md"),
            format!("# Project fixture\n\nProject: [Repository]({reference})\n"),
        );
        fixture.commit(&workspace);
        fixture.git(&project, &["remote", "set-url", "origin", different]);
        assert_error(
            &fixture.run_in(&project, &["context", "--json"]),
            "context",
            "AMBIGUOUS_SCOPE",
            2,
        );
    }
}

#[test]
fn project_binding_cannot_traverse_an_intermediate_symbolic_link() {
    let fixture = Sandbox::new();
    fixture.workspace("workspace", PROJECT_URL);
    fixture.user_config(USER_CONFIG);
    let project = fixture.project("project", PROJECT_URL);
    write(&project.join(".git/info/exclude"), format!("/{BINDING}\n"));
    let outside = fixture.0.join("outside");
    write(
        &outside.join("hive/config.yml"),
        "project_id: fixture-project\n",
    );
    symlink(outside, project.join(".local")).unwrap();
    assert_error(
        &fixture.run_in(&project, &["context", "--json"]),
        "context",
        "INVALID_CONFIG",
        2,
    );
}

#[test]
fn git_discovery_rejects_an_ancestor_with_a_different_git_directory() {
    let fixture = Sandbox::new();
    let parent = fixture.project("parent", PROJECT_URL);
    let nested = fixture.init_repo("parent/child");
    fixture.git(
        &nested,
        &["config", "core.worktree", parent.to_str().unwrap()],
    );
    assert_eq!(
        fixture.git(&nested, &["rev-parse", "--show-toplevel"]),
        parent.to_str().unwrap(),
    );
    assert_error(
        &fixture.run_in(&nested, &["config", "show", "--scope", "project", "--json"]),
        "config show",
        "INVALID_CONFIG",
        2,
    );
}

#[test]
fn native_git_resolution_isolated_from_inherited_overrides_and_project_writes() {
    let fixture = Sandbox::new();
    let workspace = fixture.workspace("workspace", PROJECT_URL);
    let project = fixture.project("project", PROJECT_URL);
    let unrelated = fixture.project("unrelated", "https://forge.example/fixture/unrelated");
    write(&project.join("untracked.md"), "Keep user changes\n");
    write(
        &project.join("src/README.md"),
        "Keep tracked user changes\n",
    );
    write(
        &project.join("project-hooks/pre-commit"),
        "#!/bin/sh\nexit 1\n",
    );
    fixture.git(&project, &["config", "core.hooksPath", "project-hooks"]);

    let injected_config = fixture.0.join("injected.gitconfig");
    write(
        &injected_config,
        "[url \"https://redirect.example/\"]\n  insteadOf = https://forge.example/\n",
    );
    let git_trace = fixture.0.join("inherited-git-trace");
    let project_before = snapshot(&project);
    let workspace_before = snapshot(&workspace);
    let output = fixture
        .hive(
            &fixture.0.join("invocation"),
            &[
                "--json",
                "context",
                "--workspace",
                "../workspace",
                "--project",
                PROJECT,
                "--project-root",
                "../project",
            ],
        )
        .env("GIT_DIR", unrelated.join(".git"))
        .env("GIT_WORK_TREE", &unrelated)
        .env("GIT_COMMON_DIR", unrelated.join(".git"))
        .env("GIT_CONFIG_GLOBAL", &injected_config)
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "remote.origin.url")
        .env(
            "GIT_CONFIG_VALUE_0",
            "https://unrelated.example/override-secret-canary",
        )
        .env("GIT_TRACE", &git_trace)
        .output()
        .unwrap();
    let context = json_result(&output, "context", 0);
    assert_eq!(
        context["data"]["workspace_root"],
        workspace.to_str().unwrap()
    );
    assert_eq!(context["data"]["project_root"], project.to_str().unwrap());
    assert_eq!(context["data"]["project_id"], PROJECT);
    assert_redacted(&output, &["override-secret-canary", "redirect.example"]);
    assert!(!git_trace.exists());

    let output = fixture.run(&[
        "config",
        "bind",
        "--workspace",
        "../workspace",
        "--project",
        PROJECT,
        "--project-root",
        "../project",
        "--json",
    ]);
    assert_error(&output, "config bind", "NOT_IMPLEMENTED", 7);
    assert_unchanged(&project, &project_before);
    assert_unchanged(&workspace, &workspace_before);
    assert!(!project.join(BINDING).exists());
}

#[test]
fn workspace_policy_is_reviewed_and_resolved_at_the_selected_view() {
    let fixture = Sandbox::new();
    let workspace = fixture.workspace("workspace", PROJECT_URL);
    let args = ["context", "--workspace", "../workspace", "--json"];

    fs::remove_file(workspace.join("hive.yml")).unwrap();
    write_policy(&workspace, LEGACY_POLICY);
    fixture.commit(&workspace);
    let legacy_revision = fixture.git(&workspace, &["rev-parse", "HEAD"]);
    let report = json_result(&fixture.run(&args), "context", 0);
    assert_eq!(report["data"]["policy_revision"], LEGACY_POLICY);
    assert!(report["data"]["coordination"].is_null());

    write_policy(&workspace, POLICY);
    write(&workspace.join("hive.yml"), LOCAL_CONFIG);
    fixture.commit(&workspace);
    let report = json_result(&fixture.run(&args), "context", 0);
    assert_eq!(report["data"]["policy_revision"], POLICY);
    assert_eq!(report["data"]["coordination"]["ref"], "refs/heads/main");

    let output = fixture.run(&[args.as_slice(), &["--at", &legacy_revision]].concat());
    let historic = json_result(&output, "context", 0);
    assert_eq!(historic["context"]["view"], "commit");
    assert_eq!(historic["context"]["revision"], legacy_revision.as_str());
    assert_eq!(historic["context"]["policy_revision"], LEGACY_POLICY);
    let text = fixture.run(&[
        "context",
        "--workspace",
        "../workspace",
        "--at",
        &legacy_revision,
        "--format",
        "text",
    ]);
    assert!(text.status.success());
    let text = String::from_utf8(text.stdout).unwrap();
    assert!(text.contains(&legacy_revision));
    assert!(text.contains("\"view\": \"commit\""));
    assert!(text.contains("\"freshness\": \"local\""));

    write(
        &workspace.join("hive.yml"),
        format!("{LOCAL_CONFIG}# Unreviewed change\n"),
    );
    assert_error(&fixture.run(&args), "context", "INVALID_CONFIG", 2);
    let project = fixture.project("project", PROJECT_URL);
    fixture.binding(
        &project,
        "project_id: fixture-project\nworkspace: ../../../workspace\n",
    );
    let historic = json_result(
        &fixture.run_in(&project, &["context", "--json", "--at", &legacy_revision]),
        "context",
        0,
    );
    assert_eq!(historic["context"]["policy_revision"], LEGACY_POLICY);
    assert_eq!(historic["data"]["project_id"], PROJECT);
    assert!(historic["data"]["coordination"].is_null());
    write(&workspace.join("hive.yml"), LOCAL_CONFIG);
    fixture.git(&workspace, &["rm", "--cached", "--", "hive.yml"]);
    assert_error(&fixture.run(&args), "context", "INVALID_CONFIG", 2);
    fixture.git(&workspace, &["add", "--", "hive.yml"]);

    write_policy(&workspace, "main");
    fixture.commit(&workspace);
    assert_error(&fixture.run(&args), "context", "POLICY_UNAVAILABLE", 7);
    write_policy(&workspace, "1111111111111111111111111111111111111111");
    fixture.commit(&workspace);
    assert_error(&fixture.run(&args), "context", "INCOMPATIBLE_POLICY", 4);
}

#[test]
fn remote_coordination_checks_local_metadata_without_contacting_the_remote() {
    let fixture = Sandbox::new();
    let workspace = fixture.workspace("workspace", PROJECT_URL);
    write(
        &workspace.join("hive.yml"),
        "version: 1\ncoordination:\n  mode: remote\n  remote: origin\n",
    );
    fixture.git(
        &workspace,
        &[
            "remote",
            "add",
            "origin",
            "ssh://git@forge.example/fixture/workspace.git",
        ],
    );
    let transport = workspace.join(".git/network-probe");
    let marker = workspace.join(".git/network-probe.called");
    write(&transport, "#!/bin/sh\n: > \"$0.called\"\nexit 1\n");
    fs::set_permissions(&transport, fs::Permissions::from_mode(0o700)).unwrap();
    let transport_command = format!("'{}'", transport.to_str().unwrap().replace('\'', "'\\''"));
    fixture.git(
        &workspace,
        &["config", "core.sshCommand", &transport_command],
    );
    fixture.commit(&workspace);

    let args = ["--workspace", "../workspace", "--json"];
    let before = snapshot(&workspace);
    for command in ["context", "doctor"] {
        let output = fixture.run(&[&[command], args.as_slice()].concat());
        let report = json_result(&output, command, 0);
        assert_eq!(
            report["data"]["workspace_root"],
            workspace.to_str().unwrap()
        );
        assert_eq!(report["data"]["coordination"]["mode"], "remote");
        assert_redacted(&output, &["git@forge.example/fixture/workspace.git"]);

        let output = fixture.run(&[&[command, "--authority"], args.as_slice()].concat());
        assert_error(&output, command, "NOT_IMPLEMENTED", 7);
    }
    assert!(!marker.exists(), "the remote transport was invoked");
    assert_unchanged(&workspace, &before);

    fixture.git(
        &workspace,
        &[
            "remote",
            "set-url",
            "--push",
            "origin",
            "https://fixture-secret-user:fixture-secret-password@forge.example/fixture/other.git",
        ],
    );
    let output = fixture.run(&[&["context"], args.as_slice()].concat());
    assert_error(&output, "context", "INVALID_CONFIG", 2);
    assert_redacted(
        &output,
        &[
            "fixture-secret-user",
            "fixture-secret-password",
            "forge.example",
        ],
    );
    assert!(
        !marker.exists(),
        "the invalid remote caused a transport invocation"
    );
}

const V2_DESCRIPTOR: &str = "version: 2
workspace_id: 0195f1b2-3c4d-7e8f-9a0b-1c2d3e4f5a6b
schema_version: 1
policy_revision: c66b3c50150e18426485d134de92c6b311dac125
";
const V2_SHOW: &[&str] = &["--contract", "2", "config", "show", "--scope", "workspace"];

fn show_v2(fixture: &Sandbox, extra: &[&str]) -> Command {
    fixture.hive(&fixture.0.join("invocation"), &[V2_SHOW, extra].concat())
}

fn v2_json(output: Output, exit: i32) -> Value {
    assert_eq!(
        output.status.code(),
        Some(exit),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["schema_version"], 2);
    assert_eq!(result["command"], "config show");
    assert_eq!(result["ok"], exit == 0);
    assert!(result.get("context").is_none());
    result
}

fn v2_error(output: Output, exit: i32, code: &str) {
    assert_redacted(&output, &["synthetic-private"]);
    let result = v2_json(output, exit);
    assert!(result.get("data").is_none());
    assert_eq!(result["error"]["code"], code);
}

#[test]
fn v2_descriptor_show_coexists_with_v1_and_keeps_targets_read_only() {
    let fixture = Sandbox::new();
    let v1 = fixture.workspace("workspace", PROJECT_URL);
    fixture.user_config(USER_CONFIG);

    let user = fixture.0.join("xdg/hive/v2/config.yml");
    let default = fixture.0.join("user-default");
    let explicit = fixture.0.join("explicit");
    write(
        &user,
        "version: 2\nworkspace: ../../../user-default\noutput: {format: json}\n",
    );
    write(&default.join("hive-workspace.yml"), V2_DESCRIPTOR);
    write(&explicit.join("hive-workspace.yml"), V2_DESCRIPTOR);
    write(
        &user.with_file_name("bindings.yml"),
        "synthetic-private: [\n",
    );
    write(
        &fixture.0.join("invocation/external.txt"),
        "synthetic project file",
    );
    let before = snapshot(&fixture.0);

    let legacy = json_result(
        &fixture.run(&["config", "show", "--scope", "workspace"]),
        "config show",
        0,
    );
    assert_eq!(legacy["data"]["workspace_root"], v1.to_str().unwrap());

    let state = fixture.0.join("state");
    let runtime = fixture.0.join("runtime");
    let selected = v2_json(
        show_v2(&fixture, &[])
            .env_remove("PATH")
            .env("XDG_STATE_HOME", &state)
            .env("XDG_RUNTIME_DIR", &runtime)
            .output()
            .unwrap(),
        0,
    );
    assert!(!state.exists());
    assert!(!runtime.exists());
    assert!(!default.join("db").exists());
    assert_eq!(
        selected["data"]["workspace_root"],
        default.to_str().unwrap()
    );
    assert_eq!(selected["data"]["user_config"], user.to_str().unwrap());
    assert_eq!(
        selected["data"]["sources"],
        serde_json::json!({"workspace": "user", "output_format": "user"})
    );

    let selected = v2_json(
        show_v2(&fixture, &["--workspace", "../explicit"])
            .output()
            .unwrap(),
        0,
    );
    assert_eq!(
        selected["data"]["workspace_root"],
        explicit.to_str().unwrap()
    );
    assert_eq!(
        selected["data"]["descriptor"],
        explicit.join("hive-workspace.yml").to_str().unwrap()
    );
    assert_eq!(
        selected["data"]["workspace_id"],
        "0195f1b2-3c4d-7e8f-9a0b-1c2d3e4f5a6b"
    );
    assert_eq!(
        selected["data"]["policy_revision"],
        "c66b3c50150e18426485d134de92c6b311dac125"
    );
    assert_eq!(selected["data"]["storage_schema_version"], 1);
    assert_eq!(
        selected["data"]["sources"],
        serde_json::json!({"workspace": "cli", "output_format": "user"})
    );
    assert_eq!(
        selected["data"]["verification"],
        serde_json::json!({
            "descriptor": "verified", "committed_identity": "unverified", "runtime": "unverified"
        })
    );

    for (target, exit, code) in [
        ("../synthetic-private-absent", 3, "NOT_FOUND"),
        ("../workspace", 4, "SCHEMA_INCOMPATIBLE"),
        ("https://synthetic-private.example", 2, "INVALID_CONFIG"),
        ("synthetic-private\n", 2, "INVALID_CONFIG"),
    ] {
        v2_error(
            show_v2(&fixture, &["--json", "--workspace", target])
                .output()
                .unwrap(),
            exit,
            code,
        );
    }
    v2_error(
        show_v2(
            &fixture,
            &["--json", "--workspace", "../explicit", "--authority"],
        )
        .output()
        .unwrap(),
        2,
        "INVALID_ARGUMENT",
    );
    assert_unchanged(&fixture.0, &before);

    symlink("absent", explicit.join("hive.yml")).unwrap();
    v2_error(
        show_v2(&fixture, &["--json", "--workspace", "../explicit"])
            .output()
            .unwrap(),
        4,
        "SCHEMA_INCOMPATIBLE",
    );
}

#[test]
fn v2_uses_absolute_xdg_or_home_only_when_needed() {
    let fixture = Sandbox::new();
    let home_default = fixture.0.join("home/.local/share/hive/v2/userdata");
    let xdg = fixture.0.join("xdg");
    let data_default = xdg.join("hive/v2/userdata");
    write(&home_default.join("hive-workspace.yml"), V2_DESCRIPTOR);
    write(&data_default.join("hive-workspace.yml"), V2_DESCRIPTOR);
    write(
        &fixture.0.join("invocation/relative/hive/v2/config.yml"),
        "bad: [",
    );
    write(
        &fixture.0.join("home/.config/hive/v2/config.yml"),
        "version: 2\n",
    );
    let before = snapshot(&fixture.0);
    for value in [None, Some(""), Some("relative")] {
        let mut command = show_v2(&fixture, &["--json"]);
        if let Some(value) = value {
            command
                .env("XDG_CONFIG_HOME", value)
                .env("XDG_DATA_HOME", value);
        } else {
            command
                .env_remove("XDG_CONFIG_HOME")
                .env_remove("XDG_DATA_HOME");
        }
        let report = v2_json(command.output().unwrap(), 0);
        assert_eq!(
            report["data"]["workspace_root"],
            home_default.to_str().unwrap()
        );
        assert_eq!(report["data"]["sources"]["workspace"], "xdg-default");
    }
    let selected = v2_json(
        show_v2(&fixture, &["--json"])
            .env_remove("HOME")
            .env("XDG_DATA_HOME", &xdg)
            .output()
            .unwrap(),
        0,
    );
    assert_eq!(
        selected["data"]["workspace_root"],
        data_default.to_str().unwrap()
    );

    for home in [None, Some(""), Some("relative")] {
        for config_home in ["relative", xdg.to_str().unwrap()] {
            let mut command = show_v2(&fixture, &["--json"]);
            command
                .env("XDG_CONFIG_HOME", config_home)
                .env("XDG_DATA_HOME", "relative");
            if let Some(home) = home {
                command.env("HOME", home);
            } else {
                command.env_remove("HOME");
            }
            v2_error(command.output().unwrap(), 2, "INVALID_CONFIG");
        }
    }
    v2_json(
        show_v2(
            &fixture,
            &["--json", "--workspace", "../xdg/hive/v2/userdata"],
        )
        .env_remove("HOME")
        .env_remove("XDG_CONFIG_HOME")
        .output()
        .unwrap(),
        0,
    );
    v2_error(
        show_v2(&fixture, &["--json", "--workspace", "~/synthetic-private"])
            .env_remove("HOME")
            .output()
            .unwrap(),
        2,
        "INVALID_CONFIG",
    );
    v2_json(
        show_v2(
            &fixture,
            &["--json", "--workspace", "~/.local/share/hive/v2/userdata"],
        )
        .output()
        .unwrap(),
        0,
    );
    assert_unchanged(&fixture.0, &before);
}

#[test]
fn v2_rejects_unsafe_config_descriptor_and_workspace_paths() {
    let fixture = Sandbox::new();
    let target = fixture.0.join("candidate");
    let marker = target.join("hive-workspace.yml");
    let outside = fixture.0.join("outside");
    write(&outside, V2_DESCRIPTOR);
    fs::create_dir(&target).unwrap();
    symlink(&outside, &marker).unwrap();
    v2_error(
        show_v2(&fixture, &["--json", "--workspace", "../candidate"])
            .output()
            .unwrap(),
        2,
        "INVALID_CONFIG",
    );
    fs::remove_file(&marker).unwrap();
    // Nonblocking opens must refuse FIFOs without waiting for a peer.
    assert!(
        fixture
            .command("mkfifo", &target)
            .arg(&marker)
            .status()
            .unwrap()
            .success()
    );
    v2_error(
        show_v2(&fixture, &["--json", "--workspace", "../candidate"])
            .output()
            .unwrap(),
        2,
        "INVALID_CONFIG",
    );
    fs::remove_file(&marker).unwrap();
    write(&marker, V2_DESCRIPTOR);
    fs::create_dir_all(target.join("nested/child")).unwrap();
    let report = v2_json(
        show_v2(
            &fixture,
            &["--json", "--workspace", "../candidate/./nested/child/../.."],
        )
        .output()
        .unwrap(),
        0,
    );
    assert_eq!(report["data"]["workspace_root"], target.to_str().unwrap());
    symlink(&target, fixture.0.join("synthetic-private-link")).unwrap();
    for path in [
        "../synthetic-private-link",
        "../synthetic-private-link/../candidate",
        "../outside/../candidate",
    ] {
        v2_error(
            show_v2(&fixture, &["--json", "--workspace", path])
                .output()
                .unwrap(),
            2,
            "INVALID_CONFIG",
        );
    }

    let user = fixture.0.join("xdg/hive/v2/config.yml");
    fs::create_dir_all(user.parent().unwrap()).unwrap();
    symlink(&outside, &user).unwrap();
    v2_error(
        show_v2(&fixture, &["--json"]).output().unwrap(),
        2,
        "INVALID_CONFIG",
    );
    v2_error(
        show_v2(
            &fixture,
            &[
                "--json",
                "--workspace",
                "../synthetic-private-absent/../candidate",
            ],
        )
        .output()
        .unwrap(),
        3,
        "NOT_FOUND",
    );
}

#[test]
fn v2_reports_strict_schema_errors_without_disclosing_inputs() {
    let fixture = Sandbox::new();
    let user = fixture.0.join("xdg/hive/v2/config.yml");
    for input in [
        "workspace: synthetic-private",
        "version: '2'",
        "version: 2.0",
        "version: 2\nworkspace: null",
        "version: 2\nworkspace: https://user:synthetic-private@example.invalid",
        "version: 2\nworkspace: ''",
        "version: 2\nworkspace: 123",
        "version: 2\noutput: null",
        "version: 2\noutput: {format: null}",
        "version: 2\noutput: {format: synthetic-private}",
        "version: 2\noutput: {synthetic-private: json}",
        "version: 2\nworkspace: &synthetic-private path",
        "version: 2\nworkspace: *synthetic-private",
        "version: 2\nworkspace: !synthetic-private path",
        "version: 2\nworkspace: !!str synthetic-private",
        "version: 2\nversion: 2",
        "version: 2\n<<: {workspace: synthetic-private}",
        "version: 2\n---\nworkspace: synthetic-private",
    ] {
        write(&user, input);
        v2_error(
            show_v2(&fixture, &["--json"]).output().unwrap(),
            2,
            "INVALID_CONFIG",
        );
    }
    write(&user, "version: -1");
    v2_error(
        show_v2(&fixture, &["--json"]).output().unwrap(),
        4,
        "SCHEMA_INCOMPATIBLE",
    );

    let marker = fixture.0.join("candidate/hive-workspace.yml");
    for (input, exit, code) in [
        (
            V2_DESCRIPTOR.replace("version: 2", "version: '2'"),
            2,
            "INVALID_CONFIG",
        ),
        (
            V2_DESCRIPTOR.replace("version: 2", "version: 1"),
            4,
            "SCHEMA_INCOMPATIBLE",
        ),
        (
            V2_DESCRIPTOR.replace("schema_version: 1", "schema_version: 2"),
            4,
            "SCHEMA_INCOMPATIBLE",
        ),
        (
            V2_DESCRIPTOR.replace("0195f1b2", "0195F1B2"),
            2,
            "INVALID_CONFIG",
        ),
        (
            V2_DESCRIPTOR.replace(
                "c66b3c50150e18426485d134de92c6b311dac125",
                "synthetic-private",
            ),
            2,
            "INVALID_CONFIG",
        ),
        (
            format!("{V2_DESCRIPTOR}synthetic-private: credential\n"),
            2,
            "INVALID_CONFIG",
        ),
        (
            format!("{V2_DESCRIPTOR}#{}", "x".repeat(64 * 1024)),
            2,
            "INVALID_CONFIG",
        ),
    ] {
        write(&marker, input);
        v2_error(
            show_v2(&fixture, &["--json", "--workspace", "../candidate"])
                .output()
                .unwrap(),
            exit,
            code,
        );
    }
    write(&marker, b"\xff");
    v2_error(
        show_v2(&fixture, &["--json", "--workspace", "../candidate"])
            .output()
            .unwrap(),
        2,
        "INVALID_CONFIG",
    );
}

#[test]
fn v2_rejects_tagged_field_names_and_accepts_quoted_tag_like_values() {
    let fixture = Sandbox::new();
    fixture.user_config("!!str output: {!!str format: json}\n");
    let legacy = json_result(
        &fixture.run(&["--json", "config", "show", "--scope", "user"]),
        "config show",
        0,
    );
    assert_eq!(legacy["data"]["output_format"], "json");

    let candidate = fixture.0.join("xdg/hive/v2/!!str candidate");
    let marker = candidate.join("hive-workspace.yml");
    let user = fixture.0.join("xdg/hive/v2/config.yml");
    write(&marker, V2_DESCRIPTOR);
    let config = "version: 2\nworkspace: '!!str candidate'\noutput: {format: json}\n";
    write(&user, config);
    let report = v2_json(show_v2(&fixture, &[]).output().unwrap(), 0);
    assert_eq!(
        report["data"]["workspace_root"],
        candidate.to_str().unwrap()
    );

    for (file, contents) in [
        (
            &marker,
            V2_DESCRIPTOR.replacen("version:", "!!str version:", 1),
        ),
        (&user, config.replacen("version:", "!!str version:", 1)),
        (&user, config.replacen("format:", "!!str format:", 1)),
        (
            &user,
            config.replacen("format:", "!<tag:yaml.org,2002:str> format:", 1),
        ),
        (&user, config.replacen("format:", "! format:", 1)),
    ] {
        write(file, contents);
        let output = show_v2(&fixture, &["--json"]).output().unwrap();
        assert_eq!(
            output.status.code(),
            Some(2),
            "tagged key was accepted in {file:?}"
        );
        assert_redacted(&output, &["!!str", "tag:yaml.org"]);
        v2_error(output, 2, "INVALID_CONFIG");
        write(
            file,
            if file == &marker {
                V2_DESCRIPTOR
            } else {
                config
            },
        );
    }
}

const V2_RESOLVED: &[&str] = &["--contract", "2", "config", "show", "--json"];
const RESOURCE: &str = "0195f1b2-4000-7000-8000-000000000010";
const AREA: &str = "0195f1b2-4000-7000-8000-000000000001";
const PROJECT_V2: &str = "0195f1b2-4000-7000-8000-000000000020";
const OVERRIDE: &str = "0195f1b2-4000-7000-8000-000000000021";

#[test]
fn v2_resolved_verification_target_is_all_or_nothing_and_scope_specific() {
    let fixture = Sandbox::new();
    let invocation = fixture.0.join("invocation");
    for options in [
        vec!["--workspace-id", RESOURCE],
        vec!["--database", "fixture_db", "--port", "1"],
        vec!["--database", "fixture_db", "--port"],
        vec![
            "--scope",
            "workspace",
            "--workspace-id",
            RESOURCE,
            "--database",
            "fixture_db",
            "--port",
            "1",
        ],
        vec![
            "--scope",
            "user",
            "--workspace-id",
            RESOURCE,
            "--database",
            "fixture_db",
            "--port",
            "1",
        ],
    ] {
        let output = fixture
            .hive(&invocation, &[V2_RESOLVED, &options].concat())
            .output()
            .unwrap();
        v2_error(output, 2, "INVALID_ARGUMENT");
    }
}

#[test]
fn v2_resolved_verification_compares_descriptor_before_requesting_credentials() {
    let fixture = Sandbox::new();
    let invocation = fixture.0.join("invocation");
    let workspace = fixture.0.join("workspace");
    write(&workspace.join("hive-workspace.yml"), V2_DESCRIPTOR);
    let run = |workspace_id| {
        let mut args = V2_RESOLVED.to_vec();
        args.extend([
            "--workspace",
            workspace.to_str().unwrap(),
            "--workspace-id",
            workspace_id,
            "--database",
            "fixture_db",
            "--port",
            "1",
        ]);
        fixture
            .hive(&invocation, &args)
            .env_remove("HIVE_DOLT_USER")
            .env_remove("HIVE_DOLT_PASSWORD")
            .output()
            .unwrap()
    };
    v2_error(run(RESOURCE), 7, "AUTHORITY_UNVERIFIED");
    let matched = v2_json(run("0195f1b2-3c4d-7e8f-9a0b-1c2d3e4f5a6b"), 2);
    assert_eq!(matched["error"]["code"], "INVALID_ARGUMENT");
    assert_eq!(matched["error"]["input"]["kind"], "dolt_target");
    assert_eq!(matched["error"]["input"]["source"], "cli");
    assert_eq!(
        matched["error"]["message"],
        "Set HIVE_DOLT_USER for the selected Dolt reader."
    );
}

fn v2_binding(path: &str, workspace: &str) -> String {
    format!("  - path: {path}\n    workspace: {workspace}\n    resource_id: {RESOURCE}\n")
}

#[test]
fn v2_resolved_selects_canonical_roots_and_reports_sources_offline() {
    let fixture = Sandbox::new();
    let external = fixture.project("external", PROJECT_URL);
    write(&external.join("src/untracked.txt"), "synthetic local file");
    write(&external.join(".git/hooks/pre-commit"), "synthetic hook");
    write(
        &fixture.0.join("invocation/notes/deep/example"),
        "non-Git resource",
    );
    write(
        &fixture.0.join("invocation/notes-other/example"),
        "sibling resource",
    );
    write(
        &fixture.0.join("invocation/notes/bindings.yml"),
        "synthetic-project-input: [",
    );
    for workspace in ["parent", "child", "user", "xdg/hive/v2/userdata"] {
        write(
            &fixture.0.join(workspace).join("hive-workspace.yml"),
            V2_DESCRIPTOR,
        );
    }
    let user = fixture.0.join("xdg/hive/v2/config.yml");
    let bindings = user.with_file_name("bindings.yml");
    write(
        &user,
        "version: 2\nworkspace: ../../../user\noutput: {format: json}\n",
    );
    let mut yaml = String::from("version: 2\nbindings:\n");
    yaml.push_str(&v2_binding("../../../invocation", "../../../parent"));
    yaml.push_str(&v2_binding(
        "../../../invocation/notes/./deep/..",
        "../../../child",
    ));
    yaml.push_str(&format!(
        "    area_id: {AREA}\n    project_id: {PROJECT_V2}\n"
    ));
    yaml.push_str(&v2_binding("../../../external", "../../../child"));
    write(&bindings, yaml);
    let before = snapshot(&fixture.0);
    let inspect = |cwd: &Path, extra: &[&str]| {
        let output = fixture
            .hive(
                cwd,
                &[&["--contract", "2", "config", "show"][..], extra].concat(),
            )
            .env("PATH", fixture.0.join("no-executables"))
            .env("XDG_DATA_HOME", fixture.0.join("xdg"))
            .env("XDG_STATE_HOME", fixture.0.join("state"))
            .env("XDG_RUNTIME_DIR", fixture.0.join("runtime"))
            .output()
            .unwrap();
        v2_json(output, 0)["data"].clone()
    };
    let deep = fixture.0.join("invocation/notes/deep");
    let report = inspect(&deep, &[]);
    assert_eq!(report["scope"], "resolved");
    assert_eq!(
        report["workspace_root"],
        fixture.0.join("child").to_str().unwrap()
    );
    assert_eq!(
        report["resource_root"],
        deep.parent().unwrap().to_str().unwrap()
    );
    assert_eq!(report["resource_id"], RESOURCE);
    assert_eq!(report["area_id"], AREA);
    assert_eq!(report["project_id"], PROJECT_V2);
    assert_eq!(report["binding"], bindings.to_str().unwrap());
    assert_eq!(report["user_config"], user.to_str().unwrap());
    assert_eq!(
        report["sources"],
        serde_json::json!({
            "workspace": "binding", "area": "binding", "project": "binding",
            "resource": "binding", "output_format": "user",
        })
    );
    assert_eq!(
        report["verification"],
        serde_json::json!({
            "descriptor": "verified", "committed_identity": "unverified",
            "runtime": "unverified", "scope_membership": "unverified",
        })
    );

    let sibling = fixture.0.join("invocation/notes-other");
    for report in [
        inspect(&sibling, &[]),
        inspect(&deep, &["--resource", "../.."]),
    ] {
        assert_eq!(
            report["workspace_root"],
            fixture.0.join("parent").to_str().unwrap()
        );
        assert!(report.get("area_id").is_none());
    }
    assert_eq!(
        inspect(&external.join("src"), &[])["sources"]["workspace"],
        "binding"
    );
    assert_eq!(inspect(&fixture.0, &[])["sources"]["workspace"], "user");

    let report = inspect(
        &deep,
        &[
            "--json",
            "--workspace",
            "../../../child",
            "--area",
            OVERRIDE,
            "--project",
            OVERRIDE,
        ],
    );
    assert_eq!(report["area_id"], OVERRIDE);
    assert_eq!(report["project_id"], OVERRIDE);
    assert_eq!(
        report["sources"],
        serde_json::json!({
            "workspace": "cli", "area": "cli", "project": "cli",
            "resource": "binding", "output_format": "cli",
        })
    );
    assert!(report.get("user_config").is_none());

    let report = inspect(
        &deep,
        &["--json", "--workspace", "../../../user", "--area", OVERRIDE],
    );
    assert_eq!(
        report["workspace_root"],
        fixture.0.join("user").to_str().unwrap()
    );
    assert_eq!(report["area_id"], OVERRIDE);
    assert_eq!(
        report["sources"],
        serde_json::json!({
            "workspace": "cli", "area": "cli", "output_format": "cli",
        })
    );
    for field in ["binding", "resource_id", "resource_root", "project_id"] {
        assert!(report.get(field).is_none(), "{field}");
    }
    assert!(!fixture.0.join("state").exists());
    assert!(!fixture.0.join("runtime").exists());
    assert_unchanged(&fixture.0, &before);

    // A foreign workspace's descriptor and scope are irrelevant to --workspace.
    write(
        &fixture.0.join("child/hive-workspace.yml"),
        "synthetic-private: [",
    );
    let report = inspect(&deep, &["--json", "--workspace", "../../../user"]);
    assert!(report.get("binding").is_none());

    write(&user, "version: 2\n");
    assert_eq!(
        inspect(&fixture.0, &["--json"])["sources"]["workspace"],
        "xdg-default"
    );

    // A supplied root is exact, even if an ancestor would apply implicitly.
    v2_error(
        fixture.run_in(&deep, &[V2_RESOLVED, &["--resource", "."]].concat()),
        3,
        "NOT_FOUND",
    );
}

#[test]
fn v2_binding_without_workspace_uses_only_the_user_default() {
    let fixture = Sandbox::new();
    let user = fixture.0.join("xdg/hive/v2/config.yml");
    write(
        &fixture.0.join("workspace/hive-workspace.yml"),
        V2_DESCRIPTOR,
    );
    write(
        &fixture.0.join("xdg/hive/v2/userdata/hive-workspace.yml"),
        V2_DESCRIPTOR,
    );
    write(
        &user.with_file_name("bindings.yml"),
        format!(
            "version: 2\nbindings:\n  - path: ../../../invocation\n    resource_id: {RESOURCE}\n"
        ),
    );
    write(&user, "version: 2\nworkspace: ../../../workspace\n");
    let report = v2_json(fixture.run(V2_RESOLVED), 0);
    assert_eq!(report["data"]["sources"]["workspace"], "binding");
    assert_eq!(report["data"]["user_config"], user.to_str().unwrap());
    assert_eq!(
        report["data"]["workspace_root"],
        fixture.0.join("workspace").to_str().unwrap()
    );
    write(&user, "version: 2\n");
    let output = fixture
        .hive(&fixture.0.join("invocation"), V2_RESOLVED)
        .env("XDG_DATA_HOME", fixture.0.join("xdg"))
        .output()
        .unwrap();
    v2_error(output, 2, "INVALID_CONFIG");
}

#[test]
fn v2_bindings_use_only_an_absolute_xdg_config_home_or_the_home_fallback() {
    let fixture = Sandbox::new();
    write(
        &fixture.0.join("workspace/hive-workspace.yml"),
        V2_DESCRIPTOR,
    );
    let home_bindings = fixture.0.join("home/.config/hive/v2/bindings.yml");
    write(
        &home_bindings,
        format!(
            "version: 2\nbindings:\n{}",
            v2_binding("../../../../invocation", "../../../../workspace")
        ),
    );
    write(
        &fixture.0.join("invocation/relative/hive/v2/bindings.yml"),
        "synthetic-private: [",
    );
    let before = snapshot(&fixture.0);
    for override_value in [None, Some(""), Some("relative")] {
        let mut command = fixture.hive(&fixture.0.join("invocation"), V2_RESOLVED);
        if let Some(value) = override_value {
            command.env("XDG_CONFIG_HOME", value);
        } else {
            command.env_remove("XDG_CONFIG_HOME");
        }
        let report = v2_json(command.output().unwrap(), 0);
        assert_eq!(report["data"]["binding"], home_bindings.to_str().unwrap());
    }
    assert_unchanged(&fixture.0, &before);
}

#[test]
fn v2_bindings_refuse_invalid_selected_inputs_and_unsafe_files() {
    let fixture = Sandbox::new();
    let user = fixture.0.join("xdg/hive/v2/config.yml");
    let bindings = user.with_file_name("bindings.yml");
    let workspace = fixture.0.join("workspace");
    write(&workspace.join("hive-workspace.yml"), V2_DESCRIPTOR);
    write(
        &user,
        "version: 2\nworkspace: ../../../workspace\noutput: {format: json}\n",
    );
    write(
        &fixture.0.join("invocation/child/example"),
        "synthetic resource file",
    );
    let binding = v2_binding("../../../invocation", "../../../workspace");
    let valid = format!("version: 2\nbindings:\n{binding}");
    let duplicate = v2_binding("../../../invocation/child/..", "../../../workspace");
    for (yaml, exit, code) in [
        (format!("{valid}{duplicate}"), 2, "INVALID_CONFIG"),
        (
            format!("{valid}    synthetic-private: forbidden\n"),
            2,
            "INVALID_CONFIG",
        ),
        (
            format!("{valid}    resource_id: {RESOURCE}\n"),
            2,
            "INVALID_CONFIG",
        ),
        (format!("{valid}    area_id: null\n"), 2, "INVALID_CONFIG"),
        (
            format!("{valid}    area_id: synthetic-private\n"),
            2,
            "INVALID_CONFIG",
        ),
        (
            valid.replace("version: 2", "version: 1"),
            4,
            "SCHEMA_INCOMPATIBLE",
        ),
        (
            valid.replace("- path:", "- &synthetic-private path:"),
            2,
            "INVALID_CONFIG",
        ),
        (
            valid.replace("- path:", "- !!map\n    path:"),
            2,
            "INVALID_CONFIG",
        ),
        (
            valid.replace("../../../workspace", "../../../absent"),
            3,
            "NOT_FOUND",
        ),
        (
            format!("{valid}    workspace_id: {OVERRIDE}\n"),
            7,
            "AUTHORITY_UNVERIFIED",
        ),
        (format!("{valid}---\n{{}}\n"), 2, "INVALID_CONFIG"),
    ] {
        write(&bindings, yaml);
        let output = fixture.run(&["--contract", "2", "config", "show"]);
        assert!(String::from_utf8_lossy(&output.stdout).contains("binding"));
        v2_error(output, exit, code);
    }

    let broken_child = v2_binding("../../../invocation/child", "../../../absent");
    write(&bindings, format!("{valid}{broken_child}"));
    v2_error(
        fixture.run_in(&fixture.0.join("invocation/child"), V2_RESOLVED),
        3,
        "NOT_FOUND",
    );
    for bytes in [vec![0xff], vec![b' '; 64 * 1024 + 1]] {
        write(&bindings, bytes);
        v2_error(fixture.run(V2_RESOLVED), 2, "INVALID_CONFIG");
    }
    fs::remove_file(&bindings).unwrap();
    symlink(&user, &bindings).unwrap();
    v2_error(fixture.run(V2_RESOLVED), 2, "INVALID_CONFIG");

    // Invalid explicit inputs win even with a malformed binding file.
    for (extra, exit, code) in [
        (&["--workspace", "../absent"][..], 3, "NOT_FOUND"),
        (&["--area", "synthetic-private"][..], 2, "INVALID_ARGUMENT"),
        (
            &["--project", "SYNTHETIC-PRIVATE"][..],
            2,
            "INVALID_ARGUMENT",
        ),
    ] {
        v2_error(fixture.run(&[V2_RESOLVED, extra].concat()), exit, code);
    }
}

#[test]
fn v2_binding_selection_opens_only_the_winning_resource_path() {
    let fixture = Sandbox::new();
    let invocation = fixture.0.join("invocation");
    let child = invocation.join("child");
    fs::create_dir(&child).unwrap();
    symlink(&child, invocation.join("shortcut")).unwrap();
    write(
        &fixture.0.join("workspace/hive-workspace.yml"),
        V2_DESCRIPTOR,
    );
    let yaml = fixture.0.join("xdg/hive/v2/bindings.yml");
    let binding = |path: &str| v2_binding(path, "../../../workspace");
    let parent = binding("../../../invocation");
    let unrelated = binding("../../../unrelated-missing");
    write(&yaml, format!("version: 2\nbindings:\n{unrelated}{parent}"));
    for scope in [&[][..], &["--resource", ".."]] {
        let report = v2_json(fixture.run_in(&child, &[V2_RESOLVED, scope].concat()), 0);
        assert_eq!(
            report["data"]["resource_root"],
            invocation.to_str().unwrap()
        );
    }

    let duplicate = binding("../../../unrelated-missing/another/..");
    write(
        &yaml,
        format!("version: 2\nbindings:\n{parent}{unrelated}{duplicate}"),
    );
    v2_error(fixture.run(V2_RESOLVED), 2, "INVALID_CONFIG");

    for (component, exit, code) in [
        ("missing", 3, "NOT_FOUND"),
        ("shortcut", 2, "INVALID_CONFIG"),
    ] {
        let winner = binding(&format!("../../../invocation/{component}/../child"));
        write(&yaml, format!("version: 2\nbindings:\n{parent}{winner}"));
        for scope in [&[][..], &["--resource", "."]] {
            let output = fixture.run_in(&child, &[V2_RESOLVED, scope].concat());
            v2_error(output, exit, code);
        }
    }
}

#[test]
fn v2_explicit_unbound_resource_precedes_malformed_user_config() {
    let fixture = Sandbox::new();
    write(
        &fixture.0.join("xdg/hive/v2/config.yml"),
        "synthetic-private: [",
    );
    let message = "The CLI Resource directory has no exact machine-local binding.";
    let output = fixture.run(&["--contract", "2", "config", "show", "--resource", "."]);
    assert_eq!(output.status.code(), Some(3));
    assert!(output.stdout.is_empty());
    assert_eq!(output.stderr, format!("error: {message}\n").as_bytes());

    let output = fixture.run(&[V2_RESOLVED, &["--resource", "."]].concat());
    let report = v2_json(output, 3);
    assert_eq!(report["error"]["code"], "NOT_FOUND");
    assert_eq!(report["error"]["message"], message);
}

#[test]
fn v2_exact_resource_validates_its_workspace_before_output_preferences() {
    let fixture = Sandbox::new();
    let user = fixture.0.join("xdg/hive/v2/config.yml");
    let bindings = user.with_file_name("bindings.yml");
    for name in ["workspace", "foreign"] {
        write(
            &fixture.0.join(name).join("hive-workspace.yml"),
            V2_DESCRIPTOR,
        );
    }
    let exact = ["--contract", "2", "config", "show", "--resource", "."];
    let pair = [&exact[..], &["--workspace", "../workspace"]].concat();
    let header = "version: 2\nbindings:\n";
    let same_path = v2_binding("../../../invocation", "../../../workspace");
    let wrong_id = format!("{same_path}    workspace_id: {OVERRIDE}\n");
    let malformed = "synthetic-private: [";
    for (binding, args, exit, code) in [
        (
            v2_binding("../../../invocation", "../../../foreign"),
            pair.as_slice(),
            3,
            "NOT_FOUND",
        ),
        (wrong_id.clone(), pair.as_slice(), 7, "AUTHORITY_UNVERIFIED"),
        (wrong_id, exact.as_slice(), 7, "AUTHORITY_UNVERIFIED"),
    ] {
        write(&bindings, format!("{header}{binding}"));
        write(&user, "version: 2\n");
        let baseline = fixture.run(args);
        assert_eq!(baseline.status.code(), Some(exit));
        assert!(baseline.stdout.is_empty());
        write(&user, malformed);
        let output = fixture.run(args);
        assert_eq!(output.status.code(), baseline.status.code());
        assert_eq!(output.stdout, baseline.stdout);
        assert_eq!(output.stderr, baseline.stderr);
        let output = fixture.run(&[args, &["--json"]].concat());
        v2_error(output, exit, code);
    }

    // A matching workspace ID alone still needs the configured workspace path.
    let id = "0195f1b2-3c4d-7e8f-9a0b-1c2d3e4f5a6b";
    write(
        &bindings,
        format!(
            "{header}  - path: ../../../invocation\n    resource_id: {RESOURCE}\n    workspace_id: {id}\n"
        ),
    );
    let json_pair = [&pair[..], &["--json"]].concat();
    v2_error(fixture.run(&json_pair), 2, "INVALID_CONFIG");
    write(
        &user,
        "version: 2\nworkspace: ../../../workspace\noutput: {format: json}\n",
    );
    let report = v2_json(fixture.run(&pair), 0);
    assert_eq!(report["data"]["user_config"], user.to_str().unwrap());
    assert_eq!(report["data"]["sources"]["output_format"], "user");
    write(&bindings, format!("{header}{same_path}"));
    let report = v2_json(fixture.run(&pair), 0);
    assert_eq!(report["data"]["sources"]["output_format"], "user");
}

#[test]
fn v2_workspace_scope_preserves_descriptor_failure_diagnostics() {
    let fixture = Sandbox::new();
    for (selection, exit, code, message) in [
        (
            &["--workspace", "../absent"][..],
            3,
            "NOT_FOUND",
            "Workspace directory does not exist",
        ),
        (
            &["--authority"][..],
            2,
            "INVALID_ARGUMENT",
            "Workspace descriptor inspection accepts only workspace and output selection.",
        ),
    ] {
        let output = show_v2(&fixture, selection).output().unwrap();
        assert_eq!(output.status.code(), Some(exit));
        assert!(output.stdout.is_empty());
        assert_eq!(output.stderr, format!("error: {message}\n").as_bytes());
        let output = show_v2(&fixture, &[selection, &["--json"]].concat())
            .output()
            .unwrap();
        let report = v2_json(output, exit);
        assert_eq!(report["error"]["code"], code);
        assert_eq!(report["error"]["message"], message);
    }
}

const STATUS_WORKSPACE_ID: &str = "0195f1b2-3c4d-7e8f-9a0b-1c2d3e4f5a6b";

fn status_report(mut command: Command, args: &[&str], exit: i32) -> Value {
    let output = command.args(args).output().unwrap();
    assert_redacted(&output, &["synthetic-private"]);
    assert_eq!(
        output.status.code(),
        Some(exit),
        "{} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["schema_version"], 2);
    assert_eq!(report["command"], "workspace status");
    assert_eq!(report["ok"], exit == 0);
    report
}

#[test]
fn workspace_status_resolves_offline_without_creating_state() {
    let fixture = Sandbox::new();
    let root = fixture.0.join("workspace");
    write(&root.join("hive-workspace.yml"), V2_DESCRIPTOR);
    write(
        &fixture.0.join("xdg/hive/v2/bindings.yml"),
        format!(
            "version: 2\nbindings:\n{}",
            v2_binding("../../../invocation", "../../../workspace")
        ),
    );
    let before = snapshot(&fixture.0);
    let command = || {
        fixture.hive(
            &fixture.0.join("invocation"),
            &["workspace", "status", "--contract", "2", "--json"],
        )
    };
    let report = status_report(command(), &[], 0);
    assert_eq!(report["data"]["workspace_source"], "binding");
    assert_eq!(report["data"]["workspace_root"], root.to_str().unwrap());
    assert_eq!(report["data"]["active_root"], "unregistered");
    assert_eq!(report["data"]["state"], "unknown");
    assert_eq!(report["data"]["runtime"]["process"], "unrecorded");
    assert!(report["data"]["committed_identity"].is_null());

    let target = [
        "--workspace-id",
        STATUS_WORKSPACE_ID,
        "--database",
        "fixture_db",
        "--port",
        "3307",
    ];
    let report = status_report(command(), &target, 2);
    assert_eq!(report["error"]["code"], "INVALID_ARGUMENT");
    assert!(
        report["error"]["message"]
            .as_str()
            .unwrap()
            .contains("HIVE_DOLT_USER")
    );
    let report = status_report(command(), &["--port", "3307"], 2);
    assert_eq!(report["error"]["code"], "INVALID_ARGUMENT");
    let report = status_report(command(), &invalid_database_target(), 2);
    assert_eq!(report["error"]["code"], "INVALID_ARGUMENT");
    assert!(
        report["error"]["message"]
            .as_str()
            .unwrap()
            .contains("database name")
    );
    let report = status_report(command(), &database_target(AREA, "fixture_db", "0"), 2);
    assert_eq!(report["error"]["code"], "INVALID_ARGUMENT");
    assert!(
        report["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Dolt port")
    );
    let late_options = fixture.hive(
        &fixture.0.join("invocation"),
        &[
            "workspace",
            "status",
            "--unknown",
            "--contract",
            "2",
            "--json",
        ],
    );
    let report = status_report(late_options, &[], 2);
    assert_eq!(report["error"]["code"], "INVALID_ARGUMENT");
    let report = status_report(command(), &["--workspace", "../missing"], 3);
    assert_eq!(report["error"]["code"], "NOT_FOUND");
    assert_eq!(snapshot(&fixture.0), before);
}

#[test]
fn workspace_status_explicit_root_does_not_require_a_binding_workspace() {
    let fixture = Sandbox::new();
    let root = fixture.0.join("workspace");
    write(&root.join("hive-workspace.yml"), V2_DESCRIPTOR);
    write(
        &fixture.0.join("xdg/hive/v2/bindings.yml"),
        format!(
            "version: 2\nbindings:\n  - path: ../../../invocation\n    resource_id: {RESOURCE}\n"
        ),
    );
    let before = snapshot(&fixture.0);
    let explicit = ["--workspace", "../workspace"];
    let command = fixture.hive(
        &fixture.0.join("invocation"),
        &["--contract", "2", "--json", "workspace", "status"],
    );
    let report = status_report(command, &explicit, 0);
    assert_eq!(report["data"]["workspace_source"], "cli");
    assert_eq!(report["data"]["workspace_root"], root.to_str().unwrap());

    let output = fixture.run(&[V2_RESOLVED, &explicit].concat());
    let report = v2_json(output, 2);
    assert_eq!(report["error"]["code"], "INVALID_CONFIG");
    assert!(
        report["error"]["message"]
            .as_str()
            .unwrap()
            .contains("binding without a workspace")
    );
    assert_eq!(snapshot(&fixture.0), before);
}

fn invalid_database_target() -> [&'static str; 6] {
    database_target(STATUS_WORKSPACE_ID, "invalid.name", "3307")
}

fn database_target<'a>(workspace_id: &'a str, database: &'a str, port: &'a str) -> [&'a str; 6] {
    [
        "--workspace-id",
        workspace_id,
        "--database",
        database,
        "--port",
        port,
    ]
}

#[test]
fn workspace_status_checks_registration_and_scopes_state_to_its_uuid() {
    let fixture = Sandbox::new();
    let root = fixture.0.join("workspace");
    write(&root.join("hive-workspace.yml"), V2_DESCRIPTOR);
    write(&fixture.0.join("copy/hive-workspace.yml"), V2_DESCRIPTOR);
    let workspaces = fixture.0.join("home/.local/state/hive/v2/workspaces");
    let active = workspaces.join(STATUS_WORKSPACE_ID);
    let registration = active.join("active-root.json");
    let fields = serde_json::json!({
        "version": 2, "workspace_id": STATUS_WORKSPACE_ID, "workspace_root": root,
    });
    write(&registration, fields.to_string());
    write(
        &workspaces.join(AREA).join("server.json"),
        "synthetic-private",
    );
    let command = || {
        let mut command = fixture.hive(
            &fixture.0.join("invocation"),
            &["--contract", "2", "--json", "workspace", "status"],
        );
        command.env("XDG_STATE_HOME", "relative-state");
        command
    };
    let before = snapshot(&fixture.0);
    let report = status_report(command(), &["--workspace", "../workspace"], 0);
    assert_eq!(report["data"]["active_root"], "matched");
    let report = status_report(command(), &["--workspace", "../copy"], 7);
    assert_eq!(report["error"]["code"], "AUTHORITY_UNVERIFIED");
    assert_eq!(snapshot(&fixture.0), before);

    write(&active.join("pending-request.json"), "synthetic-private");
    let report = status_report(command(), &["--workspace", "../workspace"], 0);
    assert_eq!(report["data"]["state"], "recovery_required");

    let mut mixed = fields.clone();
    mixed["workspace_id"] = AREA.into();
    write(&registration, mixed.to_string());
    let report = status_report(command(), &["--workspace", "../workspace"], 7);
    assert_eq!(report["error"]["code"], "AUTHORITY_UNVERIFIED");

    write(
        &registration,
        r#"{"version":2,"version":2,"synthetic-private":"secret"}"#,
    );
    let report = status_report(command(), &["--workspace", "../workspace"], 2);
    assert_eq!(report["error"]["code"], "INVALID_CONFIG");

    fs::remove_file(&registration).unwrap();
    symlink(workspaces.join(AREA).join("server.json"), &registration).unwrap();
    let report = status_report(command(), &["--workspace", "../workspace"], 2);
    assert_eq!(report["error"]["code"], "INVALID_CONFIG");

    let other_state = fixture.0.join("other-state");
    fs::create_dir_all(other_state.join("hive")).unwrap();
    symlink(workspaces.parent().unwrap(), other_state.join("hive/v2")).unwrap();
    let mut linked = command();
    linked.env("XDG_STATE_HOME", other_state);
    let report = status_report(linked, &["--workspace", "../workspace"], 2);
    assert_eq!(report["error"]["code"], "INVALID_CONFIG");
}

#[cfg(target_os = "linux")]
#[test]
fn workspace_commands_reject_state_at_or_below_the_selected_root() {
    let fixture = Sandbox::new();
    let xdg_state = fixture.0.join("state");
    let workspaces = xdg_state.join("hive/v2/workspaces");
    let uuid_state = workspaces.join(STATUS_WORKSPACE_ID);
    let unused_dolt = fixture.0.join("unused-dolt");
    fs::create_dir_all(&uuid_state).unwrap();
    fs::set_permissions(&uuid_state, fs::Permissions::from_mode(0o700)).unwrap();
    for root in [&workspaces, &uuid_state] {
        write(&root.join("hive-workspace.yml"), V2_DESCRIPTOR);
        let before = snapshot(&fixture.0);
        for args in [
            &["status"][..],
            &["stop"][..],
            &[
                "start",
                "--dolt-bin",
                unused_dolt.to_str().unwrap(),
                "--port",
                "3307",
            ][..],
        ] {
            let mut command = fixture.hive(
                &fixture.0.join("invocation"),
                &[
                    "--contract",
                    "2",
                    "--json",
                    "--workspace",
                    root.to_str().unwrap(),
                    "workspace",
                ],
            );
            let output = command
                .env("XDG_STATE_HOME", &xdg_state)
                .args(args)
                .output()
                .unwrap();
            assert_eq!(output.status.code(), Some(2));
            assert!(output.stderr.is_empty());
            let report: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(report["command"], format!("workspace {}", args[0]));
            assert_eq!(report["error"]["code"], "INVALID_CONFIG");
            assert!(
                report["error"]["message"]
                    .as_str()
                    .unwrap()
                    .contains("outside the selected workspace")
            );
            assert_eq!(snapshot(&fixture.0), before);
        }
    }
}

#[cfg(target_os = "linux")]
#[test]
fn workspace_status_checks_process_start_and_endpoint_independently() {
    let fixture = Sandbox::new();
    let root = fixture.0.join("workspace");
    write(&root.join("hive-workspace.yml"), V2_DESCRIPTOR);
    let state = fixture.0.join("local-state");
    let active = state.join("hive/v2/workspaces").join(STATUS_WORKSPACE_ID);
    let registration = active.join("active-root.json");
    write(
        &registration,
        serde_json::json!({
            "version": 2, "workspace_id": STATUS_WORKSPACE_ID, "workspace_root": root,
        })
        .to_string(),
    );
    let stat = fs::read_to_string("/proc/self/stat").unwrap();
    let start_ticks: u64 = stat
        .rsplit_once(") ")
        .unwrap()
        .1
        .split_ascii_whitespace()
        .nth(19)
        .unwrap()
        .parse()
        .unwrap();
    let boot_id = fs::read_to_string("/proc/sys/kernel/random/boot_id").unwrap();
    let mut server = serde_json::json!({
        "version": 2, "workspace_id": STATUS_WORKSPACE_ID, "workspace_root": root,
        "pid": std::process::id(), "boot_id": boot_id.trim_end(), "start_ticks": start_ticks,
        "nonce": AREA, "endpoint": { "database": "fixture_db", "port": 3307 },
    });
    let server_path = active.join("server.json");
    write(&server_path, server.to_string());
    let command = || {
        let mut command = fixture.hive(
            &fixture.0.join("invocation"),
            &[
                "--contract",
                "2",
                "workspace",
                "status",
                "--json",
                "--workspace",
                "../workspace",
            ],
        );
        command.env("XDG_STATE_HOME", &state);
        command
    };
    let before = snapshot(&fixture.0);
    let report = status_report(command(), &[], 0);
    assert_eq!(report["data"]["runtime"]["process"], "pid_start_verified");
    assert_eq!(report["data"]["runtime"]["endpoint"], "recorded");
    assert_eq!(
        report["data"]["runtime"]["endpoint_ownership"],
        "unverified"
    );
    assert_eq!(report["data"]["state"], "unknown");

    let report = status_report(command(), &invalid_database_target(), 2);
    assert_eq!(report["error"]["code"], "INVALID_ARGUMENT");
    assert!(
        report["error"]["message"]
            .as_str()
            .unwrap()
            .contains("database name")
    );

    let target = [
        "--workspace-id",
        STATUS_WORKSPACE_ID,
        "--database",
        "fixture_db",
        "--port",
        "3308",
    ];
    let report = status_report(command(), &target, 7);
    assert_eq!(report["error"]["code"], "AUTHORITY_UNVERIFIED");
    let target = [
        "--workspace-id",
        AREA,
        "--database",
        "fixture_db",
        "--port",
        "3307",
    ];
    let report = status_report(command(), &target, 7);
    assert_eq!(report["error"]["code"], "AUTHORITY_UNVERIFIED");
    assert_eq!(snapshot(&fixture.0), before);

    server["start_ticks"] = (start_ticks + 1).into();
    write(&server_path, server.to_string());
    let report = status_report(command(), &[], 7);
    assert_eq!(report["error"]["code"], "AUTHORITY_UNVERIFIED");
    for (target, syntax_error) in [
        (
            database_target("synthetic-private", "fixture_db", "3307"),
            "lowercase UUID",
        ),
        (
            database_target(STATUS_WORKSPACE_ID, "fixture_db", "0"),
            "Dolt port",
        ),
    ] {
        let report = status_report(command(), &target, 2);
        assert_eq!(report["error"]["code"], "INVALID_ARGUMENT");
        assert!(
            report["error"]["message"]
                .as_str()
                .unwrap()
                .contains(syntax_error)
        );
    }
}

#[test]
fn context_resolve_binds_the_committed_role_policy_to_the_descriptor() {
    let fixture = Sandbox::new();
    let policy = fixture.init_repo("policy");
    let role_path = policy.join("docs/architecture/dolt-mvp-contract.md");
    write(
        &role_path,
        "# Synthetic contract\n\n### Bot and curated memory\n\n| Drone | Synthetic role |\n\n#### Continuity boundaries and proposed MVP scope\n",
    );
    fixture.commit(&policy);
    let pin = fixture.git(&policy, &["rev-parse", "HEAD"]);
    let workspace = fixture.0.join("v2-workspace");
    let descriptor = workspace.join("hive-workspace.yml");
    let actor = "fixture-actor";
    let id = "0195f1b2-3c4d-7e8f-9a0b-1c2d3e4f5a6b";
    let descriptor_at = |pin: &str| {
        format!("version: 2\nworkspace_id: {id}\nschema_version: 1\npolicy_revision: {pin}\n")
    };
    write(&descriptor, descriptor_at(&pin));
    let common = [
        "--contract",
        "2",
        "context",
        "resolve",
        "--workspace",
        workspace.to_str().unwrap(),
        "--database",
        "fixture_db",
        "--port",
        "3307",
        "--workspace-id",
        id,
        "--policy-root",
        policy.to_str().unwrap(),
        "--actor",
        actor,
        "--role",
        "drone",
        "--issue",
        "#1",
        "--json",
    ];
    let expect = |args: &[&str], exit, code| {
        let output = fixture.run(args);
        assert_eq!(
            output.status.code(),
            Some(exit),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["schema_version"], 2);
        assert_eq!(report["ok"], false);
        assert_eq!(report["command"], "context resolve");
        assert_eq!(report["error"]["code"], code);
        assert!(report.get("data").is_none());
        report
    };

    expect(
        &[
            "context",
            "resolve",
            "--role",
            "invalid-role",
            "--contract",
            "2",
            "--json",
        ],
        2,
        "INVALID_ARGUMENT",
    );
    // The committed document remains the selected policy even with a dirty replacement.
    write(&role_path, "synthetic-private working replacement");
    let report = expect(&common, 7, "AUTHORITY_UNVERIFIED");
    assert_eq!(report["context"]["policy_revision"], pin);
    assert!(!report.to_string().contains("synthetic-private"));
    let before = snapshot(&fixture.0);
    for candidate in [pin.as_str(), &"a".repeat(64)] {
        let report = expect(
            &[common.as_slice(), &["--asset-commit", candidate]].concat(),
            7,
            "AUTHORITY_UNVERIFIED",
        );
        assert!(report["context"].get("asset_commit").is_none());
    }
    for candidate in ["a123", &"A".repeat(40), &"z".repeat(40), &"a".repeat(65)] {
        expect(
            &[common.as_slice(), &["--asset-commit", candidate]].concat(),
            2,
            "INVALID_ARGUMENT",
        );
    }
    expect(
        &[
            "context",
            "resolve",
            "--asset-commit",
            "--contract",
            "2",
            "--json",
        ],
        2,
        "INVALID_ARGUMENT",
    );
    let mut invalid_target = common;
    let port = invalid_target
        .iter()
        .position(|arg| *arg == "--port")
        .unwrap();
    invalid_target[port + 1] = "0";
    let report = expect(&invalid_target, 2, "INVALID_ARGUMENT");
    assert!(
        report["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Dolt port")
    );
    assert_eq!(snapshot(&fixture.0), before);
    let limited = expect(
        &[common.as_slice(), &["--budget", "1"]].concat(),
        2,
        "CONTEXT_LIMIT",
    );
    assert_eq!(
        limited["error"]["details"],
        serde_json::json!({"kind": "role", "id": "Drone"})
    );
    expect(
        &[common.as_slice(), &["--bot", id]].concat(),
        2,
        "INVALID_ARGUMENT",
    );

    // A checkout at another commit cannot satisfy the pin even if the old object exists.
    fixture.commit(&policy);
    let report = expect(&common, 4, "INCOMPATIBLE_POLICY");
    assert!(report.get("context").is_none());
    let missing_role = fixture.git(&policy, &["rev-parse", "HEAD"]);
    write(&descriptor, descriptor_at(&missing_role));
    expect(&common, 7, "POLICY_UNAVAILABLE");
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn context_resolve_accepts_only_bounded_exact_evidence_inputs() {
    let fixture = Sandbox::new();
    let policy = fixture.init_repo("policy");
    write(
        &policy.join("docs/architecture/dolt-mvp-contract.md"),
        "# Fixture\n\n### Bot and curated memory\n\n| Drone | Fixture role |\n\n#### Continuity boundaries and proposed MVP scope\n",
    );
    fixture.commit(&policy);
    let pin = fixture.git(&policy, &["rev-parse", "HEAD"]);
    let root = fixture.0.join("workspace");
    let workspace = "01900000-0000-7000-8000-000000000001";
    let area = "01900000-0000-7000-8000-000000000002";
    let issue = "01900000-0000-7000-8000-000000000003";
    let artifact = "01900000-0000-7000-8000-000000000004";
    let invocation = "01900000-0000-7000-8000-000000000005";
    write(
        &root.join("hive-workspace.yml"),
        format!(
            "version: 2\nworkspace_id: {workspace}\nschema_version: 1\npolicy_revision: {pin}\n"
        ),
    );
    let path = fixture.0.join("evidence.json");
    let file = path.to_str().unwrap();
    let common = [
        "context",
        "resolve",
        "--workspace",
        root.to_str().unwrap(),
        "--database",
        "fixture_db",
        "--port",
        "3307",
        "--workspace-id",
        workspace,
        "--policy-root",
        policy.to_str().unwrap(),
        "--actor",
        "fixture-actor",
        "--role",
        "drone",
        "--issue",
        issue,
        "--area",
        area,
    ];
    let expect = |extra: &[&str], exit, code| {
        let args = [common.as_slice(), extra, &["--contract", "2", "--json"]].concat();
        let output = fixture.run(&args);
        let report: Value = serde_json::from_slice(&output.stdout)
            .unwrap_or_else(|_| panic!("{}", String::from_utf8_lossy(&output.stderr)));
        assert_eq!(output.status.code(), Some(exit), "{report}");
        assert!(output.stderr.is_empty(), "{args:?}");
        assert_eq!(report["command"], "context resolve");
        assert_eq!(report["schema_version"], 2);
        assert_eq!(report["error"]["code"], code);
        assert!(report.get("data").is_none());
        report
    };
    let declaration = serde_json::json!({
        "version": 1, "evidence_id": "fixture-id", "revision": "r1", "source_ref": "fixture",
        "issuer_ref": "fixture-issuer", "workspace_id": workspace, "actor_ref": "fixture-actor",
        "area_id": area, "issue_id": issue, "operations": ["disclose_artifact_body"],
        "record": {"kind": "artifact", "id": artifact, "revision": "artifact-r1"},
        "invocation_id": invocation, "db_commit": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "valid_from": "2000-01-01T00:00:00Z", "expires_at": "2099-01-01T00:00:00Z",
        "withdrawn": false
    });
    let receipt = serde_json::json!({"directive_id": area, "request_id": invocation});
    let selected = [
        "--include-artifact",
        artifact,
        "--evidence-file",
        file,
        "--invocation-id",
        invocation,
    ];
    expect(&["--include-artifact", artifact], 7, "AUTHORITY_UNVERIFIED");
    expect(
        &[
            "--include-artifact",
            artifact,
            "--include-artifact",
            artifact,
        ],
        2,
        "INVALID_ARGUMENT",
    );
    expect(
        &["--evidence-file", file, "--evidence-file", file],
        2,
        "INVALID_ARGUMENT",
    );
    let ids: Vec<_> = (1..=26)
        .map(|id| format!("01900000-0000-7000-8000-00000000{id:04x}"))
        .collect();
    let excessive: Vec<_> = ids
        .iter()
        .flat_map(|id| ["--include-artifact", id.as_str()])
        .collect();
    expect(&excessive, 2, "CONTEXT_LIMIT");

    write(
        &path,
        serde_json::json!({"version": 1, "directive_receipts": [receipt]}).to_string(),
    );
    expect(&selected, 7, "AUTHORITY_UNVERIFIED");
    write(
        &path,
        serde_json::json!({"version": 1, "directive_receipts": [receipt, receipt]}).to_string(),
    );
    expect(&["--evidence-file", file], 2, "INVALID_ARGUMENT");
    write(
        &path,
        serde_json::json!({"version": 1, "handoff": receipt}).to_string(),
    );
    expect(
        &["--invocation-id", invocation, "--evidence-file", file],
        2,
        "INVALID_ARGUMENT",
    );

    let valid = serde_json::json!({"version": 1, "directive_receipts": [receipt], "artifacts": [declaration]});
    write(&path, valid.to_string());
    let report = expect(&selected, 7, "AUTHORITY_UNVERIFIED");
    assert!(
        report["error"]["message"]
            .as_str()
            .unwrap()
            .contains("verified pairing")
    );
    expect(
        &["--include-artifact", artifact, "--evidence-file", file],
        7,
        "AUTHORITY_UNVERIFIED",
    );
    expect(
        &["--evidence-file", file, "--invocation-id", invocation],
        6,
        "UNAUTHORIZED",
    );
    for (field, value, exit, code) in [
        ("actor_ref", "other-actor", 6, "UNAUTHORIZED"),
        ("area_id", workspace, 6, "UNAUTHORIZED"),
        ("issue_id", workspace, 6, "UNAUTHORIZED"),
        ("invocation_id", workspace, 6, "UNAUTHORIZED"),
        (
            "expires_at",
            "2001-01-01T00:00:00Z",
            7,
            "AUTHORITY_UNVERIFIED",
        ),
        ("unexpected", "synthetic-private", 2, "INVALID_ARGUMENT"),
    ] {
        let mut changed = valid.clone();
        changed["artifacts"][0][field] = value.into();
        write(&path, changed.to_string());
        let report = expect(&selected, exit, code);
        assert!(!report.to_string().contains("synthetic-private"));
    }
    for (object, replacements, message) in [
        (
            &receipt,
            [
                serde_json::json!([area, invocation]).to_string(),
                format!(
                    r#"{{"directive_id":"{area}","directive_id":"{area}","request_id":"{invocation}"}}"#
                ),
                format!(
                    r#"{{"directive_id":"{area}","request_id":"{invocation}","synthetic-private":true}}"#
                ),
            ],
            "The context evidence file is not a valid versioned JSON object.",
        ),
        (
            &declaration["record"],
            [
                serde_json::json!(["artifact", artifact, "artifact-r1"]).to_string(),
                format!(
                    r#"{{"kind":"artifact","kind":"artifact","id":"{artifact}","revision":"artifact-r1"}}"#
                ),
                format!(
                    r#"{{"kind":"artifact","id":"{artifact}","revision":"artifact-r1","synthetic-private":true}}"#
                ),
            ],
            "The workflow read declaration is malformed.",
        ),
    ] {
        for replacement in replacements {
            write(
                &path,
                valid.to_string().replace(&object.to_string(), &replacement),
            );
            let report = expect(&selected, 2, "INVALID_ARGUMENT");
            assert_eq!(
                report,
                serde_json::json!({
                    "schema_version": 2,
                    "ok": false,
                    "command": "context resolve",
                    "context": {"policy_revision": pin},
                    "error": {"code": "INVALID_ARGUMENT", "message": message},
                    "warnings": [],
                })
            );
        }
    }

    for content in [
        "{\"version\":1,\"version\":1}",
        "{\"version\":1,\"unexpected\":true}",
    ] {
        write(&path, content);
        expect(&["--evidence-file", file], 2, "INVALID_ARGUMENT");
    }
    let content = "{\"version\":1}";
    write(
        &path,
        format!("{content}{}", " ".repeat(256 * 1024 - content.len())),
    );
    expect(&["--evidence-file", file], 7, "AUTHORITY_UNVERIFIED");
    write(
        &path,
        format!("{content}{}", " ".repeat(256 * 1024 + 1 - content.len())),
    );
    expect(&["--evidence-file", file], 2, "CONTEXT_LIMIT");

    let link = fixture.0.join("linked-evidence.json");
    symlink(&path, &link).unwrap();
    expect(
        &["--evidence-file", link.to_str().unwrap()],
        2,
        "INVALID_ARGUMENT",
    );
    let parent_link = fixture.0.join("linked-directory");
    symlink(&fixture.0, &parent_link).unwrap();
    expect(
        &[
            "--evidence-file",
            parent_link.join("evidence.json").to_str().unwrap(),
        ],
        2,
        "INVALID_ARGUMENT",
    );
    let traversal = fixture.0.join("invocation/../evidence.json");
    expect(
        &["--evidence-file", traversal.to_str().unwrap()],
        2,
        "INVALID_ARGUMENT",
    );
    fs::remove_file(&path).unwrap();
    expect(&["--evidence-file", file], 7, "AUTHORITY_UNVERIFIED");
}
