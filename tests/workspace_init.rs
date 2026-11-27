#![cfg(target_os = "linux")]

use std::fs::{self, DirBuilder};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt, symlink};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::Value;

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("hive-init-test-{}-{nonce}", std::process::id()));
        DirBuilder::new().mode(0o700).create(&path).unwrap();
        Self(path)
    }

    fn dir(&self, name: &str) -> PathBuf {
        let path = self.0.join(name);
        fs::create_dir(&path).unwrap();
        path
    }

    fn hive(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_hive"));
        command.current_dir(&self.0).env("PATH", self.0.join("bin"));
        command
    }

    fn init(&self, target: &Path) -> Command {
        let mut command = self.hive();
        command
            .args(["--contract", "2", "--json", "workspace", "init", "--target"])
            .arg(target);
        command
    }

    fn stages(&self) -> Vec<PathBuf> {
        fs::read_dir(&self.0)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with(".hive-init-incomplete-")
            })
            .collect()
    }

    fn fake_dolt(&self, body: &str) {
        let bin = self.dir("bin");
        fs::write(bin.join("dolt"), format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(bin.join("dolt"), fs::Permissions::from_mode(0o700)).unwrap();
    }

    fn fake_git_version(&self) {
        let git = self.0.join("bin/git");
        fs::write(
            &git,
            "#!/bin/sh\n[ \"$3\" = --version ] && echo 'git version 2.56.1'\n",
        )
        .unwrap();
        fs::set_permissions(git, fs::Permissions::from_mode(0o700)).unwrap();
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn response(output: Output, status: i32) -> Value {
    let body = String::from_utf8(output.stdout).unwrap();
    assert_eq!(output.status.code(), Some(status), "{body}");
    assert!(output.stderr.is_empty(), "{:?}", output.stderr);
    serde_json::from_str(&body).unwrap()
}

#[test]
fn doctor_checks_an_explicit_empty_target_and_leaves_it_empty() {
    let fixture = Fixture::new();
    let target = fixture.dir("empty");
    let xdg = fixture.0.join("state");
    fixture.fake_dolt(
        "[ \"$#\" = 1 ] && [ \"$1\" = version ] && [ \"$DOLT_ROOT_PATH\" = \"$HOME\" ] && \
         [ \"$DOLT_DISABLE_EVENT_FLUSH\" = true ] && echo 'dolt version 2.7.6'",
    );
    fixture.fake_git_version();
    let doctor = || {
        let mut command = fixture.hive();
        command
            .args([
                "--contract",
                "2",
                "--json",
                "workspace",
                "doctor",
                "--target",
            ])
            .arg(&target)
            .env("XDG_STATE_HOME", &xdg);
        command
    };
    let result = response(doctor().output().unwrap(), 0);
    assert_eq!(result["command"], "workspace doctor");
    let data = &result["data"];
    assert_eq!(data["versions"]["hive"], env!("CARGO_PKG_VERSION"));
    assert_eq!(data["versions"]["git"], "2.56.1");
    assert_eq!(data["versions"]["dolt"], "2.7.6");
    assert!(data["available_blocks"].as_u64().unwrap() > 0);
    assert_eq!(
        data["deferred"]["committed_identity"],
        "workspace_uninitialized"
    );
    assert!(fs::read_dir(&target).unwrap().next().is_none());
    assert!(fixture.stages().is_empty());
    assert!(!xdg.exists());

    fs::write(
        fixture.0.join("bin/dolt"),
        "#!/bin/sh\necho 'dolt version 2.7.60'\n",
    )
    .unwrap();
    let result = response(doctor().output().unwrap(), 4);
    assert_eq!(result["error"]["code"], "SCHEMA_INCOMPATIBLE");
    assert!(fs::read_dir(&target).unwrap().next().is_none());

    fs::write(target.join(".keep"), "untouched").unwrap();
    let result = response(doctor().output().unwrap(), 5);
    assert_eq!(result["error"]["code"], "ALREADY_EXISTS");
    assert_eq!(
        fs::read_to_string(target.join(".keep")).unwrap(),
        "untouched"
    );
}

#[test]
fn doctor_uses_status_for_initialized_inspection_and_requires_explicit_selection() {
    let fixture = Fixture::new();
    let target = fixture.dir("initialized");
    let state = fixture.0.join("state");
    let descriptor = "version: 2\nworkspace_id: 01900000-0000-7000-8000-000000000001\nschema_version: 1\npolicy_revision: c66b3c50150e18426485d134de92c6b311dac125\n";
    fs::write(target.join("hive-workspace.yml"), descriptor).unwrap();
    fixture.fake_dolt("[ \"$1\" = version ] && echo 'dolt version 2.7.6'");
    fixture.fake_git_version();
    let doctor = || {
        let mut command = fixture.hive();
        command
            .args(["--contract", "2", "--json", "workspace", "doctor"])
            .env("XDG_STATE_HOME", &state);
        command
    };
    let result = response(doctor().output().unwrap(), 2);
    assert_eq!(result["error"]["code"], "INVALID_ARGUMENT");
    let result = response(
        doctor().arg("--workspace").arg(&target).output().unwrap(),
        0,
    );
    let data = &result["data"];
    assert_eq!(data["initialized"], true);
    assert_eq!(data["sources"]["workspace"], "cli");
    assert_eq!(data["workspace_id"], "01900000-0000-7000-8000-000000000001");
    assert_eq!(data["runtime"]["endpoint_ownership"], "unverified");
    assert_eq!(
        data["deferred"]["committed_identity"],
        "authenticated_endpoint_not_supplied"
    );
    let wrong_identity = response(
        doctor()
            .arg("--workspace")
            .arg(&target)
            .args([
                "--workspace-id",
                "01900000-0000-7000-8000-000000000002",
                "--database",
                "fixture_db",
                "--port",
                "3307",
            ])
            .output()
            .unwrap(),
        7,
    );
    assert_eq!(wrong_identity["error"]["code"], "AUTHORITY_UNVERIFIED");
    assert!(!state.exists());
    assert_eq!(
        fs::read_to_string(target.join("hive-workspace.yml")).unwrap(),
        descriptor
    );
}

#[test]
fn refuses_implicit_unsafe_and_existing_destinations_before_tools() {
    let fixture = Fixture::new();
    let empty = fixture.dir("empty");
    let used = fixture.dir("used");
    fs::write(used.join(".hidden"), "keep").unwrap();
    let linked = fixture.0.join("linked");
    symlink(&empty, &linked).unwrap();
    let nested = fixture.dir("nested");
    let legacy = fixture.dir("legacy");
    fs::write(
        legacy.join("hive.yml"),
        "version: 1\ncoordination: {mode: local}\n",
    )
    .unwrap();
    fs::create_dir(legacy.join("empty")).unwrap();
    let linked_ancestor = fixture.0.join("alias");
    symlink(&fixture.0, &linked_ancestor).unwrap();
    fs::set_permissions(&nested, fs::Permissions::from_mode(0o777)).unwrap();
    fs::create_dir(nested.join("empty")).unwrap();

    for (path, status, code) in [
        (fixture.0.join("missing"), 3, "NOT_FOUND"),
        (used.clone(), 5, "ALREADY_EXISTS"),
        (linked, 2, "INVALID_ARGUMENT"),
        (linked_ancestor.join("empty"), 2, "INVALID_ARGUMENT"),
        (nested.join("../empty"), 2, "INVALID_ARGUMENT"),
        (nested.join("empty"), 2, "INVALID_ARGUMENT"),
        (legacy.join("empty"), 2, "INVALID_ARGUMENT"),
    ] {
        let error = response(fixture.init(&path).output().unwrap(), status);
        assert_eq!(error["error"]["code"], code, "{error}");
    }

    for contract in [vec![], vec!["--contract", "1"]] {
        let error = response(
            fixture
                .hive()
                .args(contract)
                .args(["--json", "workspace", "init", "--target"])
                .arg(&empty)
                .output()
                .unwrap(),
            2,
        );
        assert_eq!(error["schema_version"], 1);
        assert_eq!(error["error"]["code"], "INVALID_ARGUMENT");
    }
    let error = response(
        fixture
            .hive()
            .args([
                "--contract",
                "2",
                "--json",
                "workspace",
                "init",
                "--workspace",
            ])
            .arg(legacy)
            .output()
            .unwrap(),
        2,
    );
    assert_eq!(error["error"]["code"], "INVALID_ARGUMENT");
    assert!(fixture.stages().is_empty());
    assert_eq!(fs::read_dir(empty).unwrap().count(), 0);
    assert_eq!(fs::read_to_string(used.join(".hidden")).unwrap(), "keep");
}

#[test]
fn rejects_git_components_and_allows_unrelated_empty_git_placeholders() {
    let fixture = Fixture::new();
    let placeholder = fixture.dir(".git");
    let sibling = fixture.dir("empty");

    let rejected = response(fixture.init(&placeholder).output().unwrap(), 2);
    assert_eq!(rejected["error"]["code"], "INVALID_ARGUMENT");
    let sibling = response(fixture.init(&sibling).output().unwrap(), 7);
    assert_eq!(sibling["error"]["code"], "RUNTIME_UNAVAILABLE");

    let nested = placeholder.join("empty");
    fs::create_dir(&nested).unwrap();
    let rejected = response(fixture.init(&nested).output().unwrap(), 2);
    assert_eq!(rejected["error"]["code"], "INVALID_ARGUMENT");
    assert!(fixture.stages().is_empty());
}

#[test]
fn config_readers_refuse_incomplete_staging_even_with_a_valid_descriptor() {
    let fixture = Fixture::new();
    let stage = fixture.dir(".hive-init-incomplete-synthetic");
    let payload = stage.join("workspace");
    fs::create_dir(&payload).unwrap();
    fs::write(
        payload.join("hive-workspace.yml"),
        "version: 2\nworkspace_id: 01900000-0000-7000-8000-000000000001\nschema_version: 1\npolicy_revision: c66b3c50150e18426485d134de92c6b311dac125\n",
    )
    .unwrap();

    for scope in ["workspace", "resolved"] {
        let error = response(
            fixture
                .hive()
                .args([
                    "--contract",
                    "2",
                    "--json",
                    "config",
                    "show",
                    "--scope",
                    scope,
                    "--workspace",
                ])
                .arg(&payload)
                .output()
                .unwrap(),
            2,
        );
        assert_eq!(error["error"]["code"], "INVALID_CONFIG");
        assert!(
            error["error"]["message"]
                .as_str()
                .unwrap()
                .contains("Incomplete initialization staging")
        );
    }
}

#[test]
fn config_and_conversation_readers_refuse_the_publication_sentinel() {
    let fixture = Fixture::new();
    let target = fixture.dir("workspace");
    let uuid = "01900000-0000-7000-8000-000000000001";
    fs::write(
        target.join("hive-workspace.yml"),
        format!("version: 2\nworkspace_id: {uuid}\nschema_version: 1\npolicy_revision: c66b3c50150e18426485d134de92c6b311dac125\n"),
    )
    .unwrap();
    let sentinel = target.join(".hive-init-publication-incomplete");
    symlink(fixture.0.join("missing"), &sentinel).unwrap();

    for scope in ["workspace", "resolved"] {
        let error = response(
            fixture
                .hive()
                .args([
                    "--contract",
                    "2",
                    "--json",
                    "config",
                    "show",
                    "--scope",
                    scope,
                    "--workspace",
                ])
                .arg(&target)
                .output()
                .unwrap(),
            2,
        );
        assert_eq!(error["error"]["code"], "INVALID_CONFIG");
        let message = if scope == "resolved" {
            "CLI workspace: Workspace initialization is incomplete."
        } else {
            "Workspace initialization is incomplete."
        };
        assert_eq!(error["error"]["message"], message);
    }
    let error = response(
        fixture
            .hive()
            .env_clear()
            .args([
                "--contract",
                "2",
                "--json",
                "conversation",
                "show",
                "--workspace",
            ])
            .arg(&target)
            .args([
                "--workspace-id",
                uuid,
                "--database",
                "fixture",
                "--port",
                "1",
                "--bot",
                uuid,
                "--id",
                uuid,
            ])
            .output()
            .unwrap(),
        2,
    );
    assert_eq!(error["error"]["code"], "INVALID_CONFIG");
    assert_eq!(
        error["error"]["message"],
        "Workspace initialization is incomplete."
    );
    assert!(error["context"].is_null());
    assert!(error["data"].is_null());
    assert_eq!(fs::read_link(sentinel).unwrap(), fixture.0.join("missing"));
}

#[test]
fn bootstrap_layout_refuses_missing_empty_and_symlinked_native_markers() {
    let fixture = Fixture::new();
    let target = fixture.dir("workspace");
    fs::write(
        target.join("hive-workspace.yml"),
        "version: 2\nworkspace_id: 01900000-0000-7000-8000-000000000001\nschema_version: 1\npolicy_revision: c66b3c50150e18426485d134de92c6b311dac125\nbootstrap_layout: dolt-native-v1\n",
    )
    .unwrap();
    let show = || {
        fixture
            .hive()
            .args([
                "--contract",
                "2",
                "--json",
                "config",
                "show",
                "--scope",
                "workspace",
                "--workspace",
            ])
            .arg(&target)
            .output()
            .unwrap()
    };
    let assert_missing_layout = || {
        let error = response(show(), 2);
        assert_eq!(error["error"]["code"], "INVALID_CONFIG");
        assert_eq!(
            error["error"]["message"],
            "The bootstrap Dolt repository layout is missing or unsafe."
        );
    };
    assert_missing_layout();
    let dolt = target.join("db/.dolt");
    fs::create_dir_all(dolt.join("noms")).unwrap();
    fs::write(dolt.join("repo_state.json"), b"fixture state").unwrap();
    let manifest = dolt.join("noms/manifest");
    fs::write(&manifest, b"").unwrap();
    assert_missing_layout();
    fs::remove_file(&manifest).unwrap();
    let other = fixture.0.join("manifest");
    fs::write(&other, b"fixture manifest").unwrap();
    symlink(&other, &manifest).unwrap();
    assert_missing_layout();
    fs::remove_file(&manifest).unwrap();
    fs::rename(&other, &manifest).unwrap();
    let assert_missing_assets = || {
        let error = response(show(), 2);
        assert_eq!(error["error"]["code"], "INVALID_CONFIG");
        assert_eq!(
            error["error"]["message"],
            "The bootstrap Git asset repository layout is missing or unsafe."
        );
    };
    assert_missing_assets();
    let assets = target.join("assets");
    let git = assets.join(".git");
    fs::create_dir(&assets).unwrap();
    let elsewhere = fixture.dir("external-git");
    fs::write(elsewhere.join("HEAD"), b"ref: refs/heads/main\n").unwrap();
    fs::create_dir(elsewhere.join("objects")).unwrap();
    fs::create_dir(elsewhere.join("refs")).unwrap();
    symlink(&elsewhere, &git).unwrap();
    assert_missing_assets();
    fs::remove_file(&git).unwrap();
    fs::rename(elsewhere, git).unwrap();
    assert_eq!(
        response(show(), 0)["data"]["verification"]["descriptor"],
        "verified"
    );
}

#[test]
fn refuses_unsafe_dolt_markers_without_following_them() {
    let fixture = Fixture::new();
    let elsewhere = fixture.dir("elsewhere");
    let ancestor = fixture.dir("ancestor");
    let target = ancestor.join("empty");
    fs::create_dir(&target).unwrap();
    let marker = ancestor.join(".dolt");

    symlink(&elsewhere, &marker).unwrap();
    let error = response(fixture.init(&target).output().unwrap(), 2);
    assert_eq!(error["error"]["code"], "INVALID_ARGUMENT");

    fs::remove_file(&marker).unwrap();
    fs::create_dir(&marker).unwrap();
    for name in ["repo_state.json", "noms"] {
        let child = marker.join(name);
        symlink(elsewhere.join("missing"), &child).unwrap();
        let error = response(fixture.init(&target).output().unwrap(), 2);
        assert_eq!(error["error"]["code"], "INVALID_ARGUMENT");
        assert!(
            error["error"]["message"]
                .as_str()
                .unwrap()
                .contains("Dolt repository")
        );
        assert!(child.symlink_metadata().unwrap().is_symlink());
        fs::remove_file(child).unwrap();
    }
    assert_eq!(fs::read_dir(ancestor).unwrap().count(), 2);
    assert_eq!(fs::read_dir(target).unwrap().count(), 0);
    assert_eq!(fs::read_dir(elsewhere).unwrap().count(), 0);
}

#[test]
fn reports_missing_unusable_and_incompatible_engines() {
    let fixture = Fixture::new();
    let target = fixture.dir("empty");
    let missing = response(fixture.init(&target).output().unwrap(), 7);
    assert_eq!(missing["error"]["code"], "RUNTIME_UNAVAILABLE");
    assert!(
        missing["error"]["message"]
            .as_str()
            .unwrap()
            .contains("dolt executable was not found")
    );

    fixture.fake_dolt("printf 'dolt version 2.7.5\\n'");
    let missing_git = response(fixture.init(&target).output().unwrap(), 7);
    assert!(
        missing_git["error"]["message"]
            .as_str()
            .unwrap()
            .contains("git executable was not found")
    );
    fixture.fake_git_version();
    let bad_version = response(fixture.init(&target).output().unwrap(), 4);
    assert_eq!(bad_version["error"]["code"], "SCHEMA_INCOMPATIBLE");
    fs::set_permissions(
        fixture.0.join("bin/dolt"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let unusable = response(fixture.init(&target).output().unwrap(), 7);
    assert!(
        unusable["error"]["message"]
            .as_str()
            .unwrap()
            .contains("present but unusable")
    );
    assert!(fixture.stages().is_empty());
    assert_eq!(fs::read_dir(target).unwrap().count(), 0);
}

#[test]
fn failing_child_leaves_only_private_marked_staging() {
    let fixture = Fixture::new();
    let target = fixture.dir("empty");
    fixture.fake_dolt(
        "case $1 in version) echo 'dolt version 2.7.6';; init) /bin/mkdir .dolt; echo private-child-output >&2; exit 17;; esac",
    );
    fixture.fake_git_version();
    let error = response(fixture.init(&target).output().unwrap(), 7);
    assert_eq!(error["error"]["code"], "IO_ERROR");
    assert!(
        error["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Dolt failed during init")
    );
    assert!(!error.to_string().contains("private-child-output"));
    assert_eq!(fs::read_dir(target).unwrap().count(), 0);
    let stages = fixture.stages();
    assert_eq!(stages.len(), 1);
    assert_eq!(
        stages[0].metadata().unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert!(!stages[0].join("scratch").exists());
    assert!(!stages[0].join("workspace/hive-workspace.yml").exists());
}

#[test]
fn killed_parent_leaves_target_empty_and_stops_the_engine() {
    let fixture = Fixture::new();
    let target = fixture.dir("empty");
    fixture.fake_dolt(
        "case $1 in version) echo 'dolt version 2.7.6';; init) echo $$ > \"$HOME/ready\"; exec /bin/sleep 60;; esac",
    );
    fixture.fake_git_version();
    let mut child = fixture.init(&target).stdout(Stdio::null()).spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let pid = loop {
        if let [stage] = fixture.stages().as_slice()
            && let Ok(ready) = fs::read_to_string(stage.join("scratch/ready"))
            && let Ok(pid) = ready.trim().parse::<u32>()
        {
            break pid;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("Dolt did not reach its interrupted init step");
        }
        thread::sleep(Duration::from_millis(10));
    };
    child.kill().unwrap();
    child.wait().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if fs::read_to_string(format!("/proc/{pid}/status")).map_or(true, |status| {
            status.lines().any(|line| line.starts_with("State:\tZ"))
        }) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "Dolt remained running after Hive exited"
        );
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(fs::read_dir(target).unwrap().count(), 0);
    assert!(
        !fixture.stages()[0]
            .join("workspace/hive-workspace.yml")
            .exists()
    );
}

#[test]
#[ignore = "requires installed Dolt 2.7.6 and Git"]
fn publishes_verified_workspace_with_isolated_children() {
    let fixture = Fixture::new();
    let bin = fixture.dir("bin");
    let unrelated_git = fixture.dir("unrelated-git");
    let template = fixture.dir("template");
    fs::create_dir(template.join("hooks")).unwrap();
    let hook = template.join("hooks/pre-commit");
    fs::write(&hook, "#!/bin/sh\nexit 42\n").unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o700)).unwrap();
    let xdg = fixture.dir("xdg");
    let home = fixture.dir("operator-home");
    let target = home.join("work space");
    fs::create_dir(&target).unwrap();
    let original_inode = target.metadata().unwrap().ino();
    fs::create_dir(home.join(".dolt")).unwrap();
    let global = home.join(".dolt/config_global.json");
    let global_contents = b"{\"user.name\": \"Synthetic operator\"}\n";
    fs::write(&global, global_contents).unwrap();
    let dolt = PathBuf::from(std::env::var_os("HIVE_DOLT_BIN").unwrap());
    assert!(dolt.is_absolute());
    symlink(&dolt, bin.join("dolt")).unwrap();
    let git = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join("git"))
        .find(|path| path.is_file())
        .unwrap();
    symlink(&git, bin.join("git")).unwrap();

    let data = response(
        fixture
            .init(Path::new("operator-home/work space"))
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", &xdg)
            .env("XDG_DATA_HOME", &xdg)
            .env("XDG_STATE_HOME", &xdg)
            .env("DOLT_ROOT_PATH", &xdg)
            .env("GIT_DIR", &unrelated_git)
            .env("GIT_TEMPLATE_DIR", &template)
            .env("GIT_CONFIG_COUNT", "1")
            .env("GIT_CONFIG_KEY_0", "commit.gpgsign")
            .env("GIT_CONFIG_VALUE_0", "invalid")
            .output()
            .unwrap(),
        0,
    );
    let data = &data["data"];
    assert_eq!(data["dolt_version"], "2.7.6");
    assert_eq!(target.metadata().unwrap().ino(), original_inode);
    let descriptor = response(
        fixture
            .hive()
            .args([
                "config",
                "show",
                "--contract",
                "2",
                "--json",
                "--scope",
                "workspace",
                "--workspace",
            ])
            .arg(&target)
            .output()
            .unwrap(),
        0,
    );
    assert_eq!(data["workspace_id"], descriptor["data"]["workspace_id"]);

    let asset_head = Command::new(git)
        .env_clear()
        .current_dir(target.join("assets"))
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    assert!(asset_head.status.success());
    assert_eq!(
        data["asset_commit"],
        String::from_utf8(asset_head.stdout).unwrap().trim()
    );

    let db_head = Command::new(dolt)
        .env_clear()
        .current_dir(target.join("db"))
        .env("HOME", fixture.dir("probe-home"))
        .env("DOLT_DISABLE_VERSION_CHECKS", "true")
        .env("DOLT_DISABLE_EVENT_FLUSH", "true")
        .args([
            "sql",
            "-r",
            "json",
            "-q",
            "SELECT DOLT_HASHOF('HEAD') AS revision, workspace_id FROM workspace_meta AS OF 'HEAD'",
        ])
        .output()
        .unwrap();
    assert!(db_head.status.success(), "{:?}", db_head.stderr);
    let db_head: Value = serde_json::from_slice(&db_head.stdout).unwrap();
    assert_eq!(data["db_commit"], db_head["rows"][0]["revision"]);
    assert_eq!(data["workspace_id"], db_head["rows"][0]["workspace_id"]);
    assert_eq!(
        target
            .join("conversations")
            .metadata()
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        fs::read_dir(target.join("conversations")).unwrap().count(),
        0
    );
    assert_eq!(fs::read_dir(unrelated_git).unwrap().count(), 0);
    assert!(!target.join("assets/.git/hooks/pre-commit").exists());
    assert_eq!(fs::read_dir(xdg).unwrap().count(), 0);
    assert_eq!(fs::read(&global).unwrap(), global_contents);
    assert_eq!(fs::read_dir(home.join(".dolt")).unwrap().count(), 1);
    assert_eq!(fs::read_dir(home).unwrap().count(), 2);
    assert!(fixture.stages().is_empty());
    let repeat = response(fixture.init(&target).output().unwrap(), 5);
    assert_eq!(repeat["error"]["code"], "ALREADY_EXISTS");
}

#[test]
#[ignore = "requires installed Dolt 2.7.6"]
fn refuses_targets_inside_a_pinned_dolt_repository() {
    let fixture = Fixture::new();
    let repo = fixture.dir("repo");
    let target = repo.join("empty");
    fs::create_dir(&target).unwrap();
    let home = fixture.dir("dolt-home");
    let dolt = PathBuf::from(std::env::var_os("HIVE_DOLT_BIN").unwrap());
    assert!(dolt.is_absolute());
    let output = Command::new(dolt)
        .env_clear()
        .current_dir(&repo)
        .env("HOME", &home)
        .env("DOLT_ROOT_PATH", &home)
        .env("DOLT_DISABLE_VERSION_CHECKS", "true")
        .env("DOLT_DISABLE_EVENT_FLUSH", "true")
        .args([
            "init",
            "--initial-branch",
            "main",
            "--name",
            "Fixture",
            "--email",
            "fixture@example.invalid",
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let state = repo.join(".dolt/repo_state.json");
    let state_before = fs::read(&state).unwrap();
    assert!(repo.join(".dolt/noms").is_dir());

    let error = response(fixture.init(&target).output().unwrap(), 2);
    assert_eq!(error["error"]["code"], "INVALID_ARGUMENT");
    assert!(
        error["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Dolt repository")
    );
    assert_eq!(fs::read(state).unwrap(), state_before);
    assert_eq!(fs::read_dir(repo).unwrap().count(), 2);
    assert_eq!(fs::read_dir(target).unwrap().count(), 0);
}

#[test]
#[ignore = "requires installed Dolt 2.7.6 and Git"]
fn moving_parent_cannot_redirect_real_dolt_home_or_publish_the_workspace() {
    let fixture = Fixture::new();
    let parent = fixture.dir("parent");
    let target = parent.join("empty");
    fs::create_dir(&target).unwrap();
    let ready = fixture.0.join("ready");
    let release = fixture.0.join("release");
    let dolt = std::env::var("HIVE_DOLT_BIN").unwrap();
    assert!(Path::new(&dolt).is_absolute());
    let quote = |value: &str| format!("'{}'", value.replace('\'', "'\"'\"'"));
    let git = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join("git"))
        .find(|path| path.is_file())
        .unwrap();

    let unrelated = fs::File::open(&fixture.0).unwrap();
    // SAFETY: Duplicate a valid descriptor into a range distinct from stdio
    // and the scratch descriptor. It remains CLOEXEC in this test process.
    let fd = unsafe { libc::fcntl(unrelated.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 90) };
    assert!(fd >= 90);
    // SAFETY: fcntl returned a new owned descriptor.
    let inherited = unsafe { OwnedFd::from_raw_fd(fd) };
    fixture.fake_dolt(&format!(
        r#"
set -eu
if [ "$1" = init ]; then
    [ ! -e "/proc/$$/fd/{fd}" ] || exit 90
    printf pinned > "$HOME/witness"
    : > {ready}
    attempts=0
    until [ -f {release} ]; do
        attempts=$((attempts + 1))
        [ "$attempts" -lt 500 ] || exit 91
        /bin/sleep .01
    done
    [ "$DOLT_ROOT_PATH" = "$HOME" ] && [ "$TMPDIR" = "$HOME" ] || exit 92
    [ "$XDG_CONFIG_HOME" = "$HOME" ] && [ "$XDG_DATA_HOME" = "$HOME" ] &&
        [ "$XDG_CACHE_HOME" = "$HOME" ] && [ "$XDG_STATE_HOME" = "$HOME" ] &&
        [ "$XDG_RUNTIME_DIR" = "$HOME" ] || exit 93
    [ "$(/bin/cat "$HOME/witness")" = pinned ] || exit 94
fi
exec {dolt} "$@"
"#,
        ready = quote(ready.to_str().unwrap()),
        release = quote(release.to_str().unwrap()),
        dolt = quote(&dolt),
    ));
    symlink(git, fixture.0.join("bin/git")).unwrap();

    let mut command = fixture.init(&target);
    // SAFETY: Only an async-signal-safe fcntl runs before exec. The fixture
    // retains ownership of this descriptor until the child has started.
    unsafe {
        command.pre_exec(move || {
            if libc::fcntl(fd, libc::F_SETFD, 0) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.stdout(Stdio::piped()).spawn().unwrap();
    drop(inherited);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !ready.exists() {
        if Instant::now() >= deadline {
            child.kill().unwrap();
            let output = child.wait_with_output().unwrap();
            panic!("Dolt did not reach its suspended init step: {output:?}");
        }
        thread::sleep(Duration::from_millis(10));
    }
    let stage = fs::read_dir(&parent)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .find(|name| {
            name.as_encoded_bytes()
                .starts_with(b".hive-init-incomplete-")
        })
        .unwrap();
    let moved = fixture.0.join("moved-parent");
    fs::rename(&parent, &moved).unwrap();
    fs::create_dir_all(&target).unwrap();
    let redirected = parent.join(&stage).join("scratch");
    fs::create_dir_all(&redirected).unwrap();
    fs::write(redirected.join("witness"), b"replacement").unwrap();
    fs::write(release, b"go").unwrap();

    let error = response(child.wait_with_output().unwrap(), 5);
    assert_eq!(error["error"]["code"], "CONCURRENT_CHANGE");
    assert!(
        error["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Incomplete staging")
    );
    assert_eq!(
        fs::read(redirected.join("witness")).unwrap(),
        b"replacement"
    );
    assert_eq!(fs::read_dir(redirected).unwrap().count(), 1);
    assert_eq!(fs::read_dir(target).unwrap().count(), 0);
    assert_eq!(fs::read_dir(moved.join("empty")).unwrap().count(), 0);
    assert!(
        moved
            .join(&stage)
            .join("workspace/db/.dolt/repo_state.json")
            .is_file()
    );
    let staged = moved.join(stage);
    assert!(staged.join("workspace/hive-workspace.yml").is_file());
    assert!(!staged.join("scratch").exists());
}

#[test]
#[ignore = "requires installed Dolt 2.7.6 and Git"]
fn moved_dolt_verification_refuses_replaced_storage_inside_the_pinned_directory() {
    let fixture = Fixture::new();
    let donor = fixture.dir("donor");
    let target = fixture.dir("target");
    let changed = fixture.0.join("storage-swapped");
    let moved = fixture.0.join("original-noms");
    let dolt = PathBuf::from(std::env::var_os("HIVE_DOLT_BIN").unwrap());
    assert!(dolt.is_absolute());
    let git = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join("git"))
        .find(|path| path.is_file())
        .unwrap();

    // A real committed donor retains a valid native layout and schema but
    // has a different workspace identity and committed main revision.
    let donor_path = std::env::join_paths([dolt.parent().unwrap(), git.parent().unwrap()]).unwrap();
    response(
        fixture
            .init(&donor)
            .env("PATH", donor_path)
            .output()
            .unwrap(),
        0,
    );
    let quote = |path: &Path| format!("'{}'", path.to_str().unwrap().replace('\'', "'\"'\"'"));
    fixture.fake_dolt(&format!(
        r#"
set -eu
if [ "$1" = sql ] && [ "$(/bin/pwd -P)" = {target_db} ] && [ ! -e {changed} ]; then
    /bin/mv .dolt/noms {moved}
    /bin/mv {donor_noms} .dolt/noms
    : > {changed}
fi
exec {dolt} "$@"
"#,
        target_db = quote(&target.join("db")),
        changed = quote(&changed),
        moved = quote(&moved),
        donor_noms = quote(&donor.join("db/.dolt/noms")),
        dolt = quote(&dolt),
    ));
    symlink(git, fixture.0.join("bin/git")).unwrap();

    let error = response(fixture.init(&target).output().unwrap(), 7);
    assert!(
        changed.is_file(),
        "must replace storage only after the DB has moved"
    );
    assert_eq!(error["error"]["code"], "AUTHORITY_UNVERIFIED");
    assert!(!target.join("hive-workspace.yml").exists());
    assert!(target.join("db/.dolt/noms/manifest").is_file());
    assert!(moved.join("manifest").is_file());
    let stages = fixture.stages();
    assert_eq!(stages.len(), 1);
    assert!(!stages[0].join("scratch").exists());
}
