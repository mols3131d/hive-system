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

    fn doctor_target(&self, target: &Path) -> Command {
        let mut command = self.hive();
        command
            .args([
                "--contract",
                "2",
                "--json",
                "workspace",
                "doctor",
                "--target",
            ])
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
        "[ \"$(/usr/bin/stat -Lc %a \"$HOME\")\" = 700 ] && \
         [ \"$#\" = 1 ] && [ \"$1\" = version ] && [ \"$DOLT_ROOT_PATH\" = \"$HOME\" ] && \
         [ \"$DOLT_DISABLE_EVENT_FLUSH\" = true ] && echo 'dolt version 2.7.6'",
    );
    fixture.fake_git_version();
    let doctor = || {
        let mut command = fixture.doctor_target(&target);
        command.env("XDG_STATE_HOME", &xdg);
        command
    };
    let mut permissive = doctor();
    // SAFETY: umask is async-signal-safe and runs only in the forked child.
    unsafe {
        permissive.pre_exec(|| {
            libc::umask(0);
            Ok(())
        });
    }
    let result = response(permissive.output().unwrap(), 0);
    assert_eq!(result["command"], "workspace doctor");
    let data = &result["data"];
    assert_eq!(data["versions"]["hive"], env!("CARGO_PKG_VERSION"));
    assert_eq!(data["versions"]["git"], "2.56.1");
    assert_eq!(data["versions"]["dolt"], "2.7.6");
    assert_eq!(data["sources"]["output_format"], "cli");
    assert_eq!(data["directory_lock"], "acquired");
    assert!(data["available_blocks"].as_u64().unwrap() > 0);
    assert_eq!(
        data["deferred"]["committed_identity"],
        "workspace_uninitialized"
    );
    assert!(fs::read_dir(&target).unwrap().next().is_none());
    assert!(fixture.stages().is_empty());
    assert!(!xdg.exists());

    let mut restrictive = doctor();
    // SAFETY: umask is async-signal-safe and runs only in the forked child.
    unsafe {
        restrictive.pre_exec(|| {
            libc::umask(0o777);
            Ok(())
        });
    }
    response(restrictive.output().unwrap(), 0);

    let held = fs::File::open(&target).unwrap();
    // SAFETY: held owns the live directory descriptor for the duration of this check.
    assert_eq!(
        unsafe { libc::flock(held.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    let result = response(doctor().output().unwrap(), 5);
    assert_eq!(result["error"]["code"], "LOCK_BUSY");
    drop(held);

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
fn doctor_bounds_version_output_and_exit_time() {
    let fixture = Fixture::new();
    let target = fixture.dir("empty");
    let bin = fixture.0.join("bin");
    fixture.fake_dolt("exec /usr/bin/head -c 65536 /dev/zero");
    fixture.fake_git_version();
    let report = response(fixture.doctor_target(&target).output().unwrap(), 7);
    assert_eq!(report["error"]["code"], "RUNTIME_UNAVAILABLE");
    assert_eq!(
        report["error"]["message"],
        "Dolt version probe exceeded its output limit."
    );

    fs::write(bin.join("dolt"), "#!/bin/sh\necho 'dolt version 2.7.6'\n").unwrap();
    fs::write(
        bin.join("git"),
        "#!/bin/sh\nexec 1>&-\nexec /bin/sleep 30\n",
    )
    .unwrap();
    let started = Instant::now();
    let report = response(fixture.doctor_target(&target).output().unwrap(), 7);
    assert!(started.elapsed() < Duration::from_secs(10));
    assert_eq!(report["error"]["code"], "RUNTIME_UNAVAILABLE");
    assert_eq!(report["error"]["message"], "Git version probe timed out.");
    assert!(fs::read_dir(&target).unwrap().next().is_none());
}

#[test]
fn doctor_terminates_version_helpers_before_reaping_the_wrapper() {
    for (tail, reason) in [
        ("exit 0", "timed out"),
        (
            "/usr/bin/head -c 65536 /dev/zero",
            "exceeded its output limit",
        ),
    ] {
        let fixture = Fixture::new();
        let target = fixture.dir("empty");
        fixture.fake_dolt(&format!(
            r#"
            marker="${{0%/*}}/../helper"
            (
                [ -d "$HOME" ] || exit 1
                printf '%s\n' "${{HOME##*/}}" > "$marker.fd"
                exec /bin/sleep 8
            ) &
            printf '%s\n' "$!" > "$marker.pid"
            while [ ! -s "$marker.fd" ]; do :; done
            {tail}
            "#
        ));
        fixture.fake_git_version();
        let report = response(fixture.doctor_target(&target).output().unwrap(), 7);
        assert_eq!(report["error"]["code"], "RUNTIME_UNAVAILABLE");
        assert_eq!(
            report["error"]["message"],
            format!("Dolt version probe {reason}.")
        );
        let helper: u32 = fs::read_to_string(fixture.0.join("helper.pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let fd: u32 = fs::read_to_string(fixture.0.join("helper.fd"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let scratch_fd = PathBuf::from(format!("/proc/{helper}/fd/{fd}"));
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            match fs::symlink_metadata(&scratch_fd) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
                Err(error) => panic!("could not inspect the version helper's descriptor: {error}"),
                Ok(_) => assert!(
                    Instant::now() < deadline,
                    "version helper still holds scratch"
                ),
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
}

#[test]
fn doctor_refuses_version_probes_when_children_are_auto_reaped() {
    let fixture = Fixture::new();
    let target = fixture.dir("empty");
    fixture.fake_dolt("exit 42");
    fixture.fake_git_version();
    let mut doctor = fixture.doctor_target(&target);
    // SAFETY: signal sets a disposition only in the forked child, before exec.
    unsafe {
        doctor.pre_exec(|| {
            if libc::signal(libc::SIGCHLD, libc::SIG_IGN) == libc::SIG_ERR {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let report = response(doctor.output().unwrap(), 7);
    assert_eq!(
        report["error"]["message"],
        "Dolt version probe requires waitable children."
    );
}

#[test]
fn doctor_uses_status_for_initialized_inspection_and_requires_explicit_selection() {
    let fixture = Fixture::new();
    let target = fixture.dir("initialized");
    let state = fixture.0.join("state");
    let config = fixture.0.join("config");
    let descriptor = "version: 2\nworkspace_id: 01900000-0000-7000-8000-000000000001\nschema_version: 1\npolicy_revision: c66b3c50150e18426485d134de92c6b311dac125\n";
    fs::write(target.join("hive-workspace.yml"), descriptor).unwrap();
    fixture.fake_dolt("[ \"$1\" = version ] && echo 'dolt version 2.7.6'");
    fixture.fake_git_version();
    let doctor = || {
        let mut command = fixture.hive();
        command
            .args(["--contract", "2", "--json", "workspace", "doctor"])
            .env("XDG_STATE_HOME", &state)
            .env("XDG_CONFIG_HOME", &config);
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
    assert_eq!(data["sources"]["output_format"], "cli");
    assert_eq!(
        data["descriptor"],
        target.join("hive-workspace.yml").to_str().unwrap()
    );
    assert!(data.get("user_config").is_none());
    assert_eq!(
        data["state_directory"]["unavailable"],
        "state_directory_missing"
    );
    assert_eq!(
        data["working_set"]["unavailable"],
        "authenticated_endpoint_not_supplied"
    );
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
    let private_state = state.join("hive/v2/workspaces/01900000-0000-7000-8000-000000000001");
    fs::create_dir_all(&private_state).unwrap();
    fs::set_permissions(&private_state, fs::Permissions::from_mode(0o755)).unwrap();
    let insecure = response(
        doctor().arg("--workspace").arg(&target).output().unwrap(),
        0,
    );
    assert_eq!(
        insecure["data"]["state_directory"],
        serde_json::json!({
            "owned_by_caller": true, "mode": "0755", "private": false,
        })
    );
    fs::set_permissions(&private_state, fs::Permissions::from_mode(0o700)).unwrap();
    let user_path = config.join("hive/v2/config.yml");
    fs::create_dir_all(user_path.parent().unwrap()).unwrap();
    let configured_doctor = || {
        let mut command = fixture.hive();
        command
            .args(["--contract", "2", "workspace", "doctor", "--workspace"])
            .arg(&target)
            .env("XDG_CONFIG_HOME", &config)
            .env("XDG_STATE_HOME", &state);
        command
    };
    let default_output = configured_doctor().output().unwrap();
    assert!(default_output.status.success());
    let text = String::from_utf8(default_output.stdout).unwrap();
    let report: Value =
        serde_json::from_str(text.strip_prefix("workspace doctor\n").unwrap()).unwrap();
    assert_eq!(report["sources"]["output_format"], "default");
    assert!(report.get("user_config").is_none());
    let user_config = "version: 2\noutput: {format: json}\n";
    fs::write(&user_path, user_config).unwrap();
    let from_user = response(configured_doctor().output().unwrap(), 0);
    assert_eq!(from_user["data"]["sources"]["output_format"], "user");
    assert_eq!(
        from_user["data"]["user_config"],
        user_path.to_str().unwrap()
    );
    assert_eq!(from_user["data"]["state_directory"]["private"], true);
    assert_eq!(fs::read_dir(&private_state).unwrap().count(), 0);
    assert_eq!(fs::read_to_string(user_path).unwrap(), user_config);
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
    let state = fixture.0.join("state");
    let error = response(
        fixture
            .init(&target)
            .arg("--register-runtime")
            .env("XDG_STATE_HOME", &state)
            .output()
            .unwrap(),
        7,
    );
    assert_eq!(error["error"]["code"], "IO_ERROR");
    assert!(
        error["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Dolt failed during init")
    );
    assert!(!error.to_string().contains("private-child-output"));
    assert_eq!(fs::read_dir(target).unwrap().count(), 0);
    assert!(!state.exists());
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
fn registration_refuses_copied_descriptors_and_state_inside_the_target() {
    let fixture = Fixture::new();
    let copy = fixture.dir("copy");
    let descriptor = "version: 2\nworkspace_id: 01900000-0000-7000-8000-000000000001\nschema_version: 1\npolicy_revision: c66b3c50150e18426485d134de92c6b311dac125\n";
    fs::write(copy.join("hive-workspace.yml"), descriptor).unwrap();
    let state = fixture.0.join("state");
    let error = response(
        fixture
            .init(&copy)
            .arg("--register-runtime")
            .env("XDG_STATE_HOME", &state)
            .output()
            .unwrap(),
        5,
    );
    assert_eq!(error["error"]["code"], "ALREADY_EXISTS");
    assert_eq!(
        fs::read_to_string(copy.join("hive-workspace.yml")).unwrap(),
        descriptor
    );
    assert!(!state.exists());

    let empty = fixture.dir("empty");
    let error = response(
        fixture
            .init(&empty)
            .arg("--register-runtime")
            .env("XDG_STATE_HOME", &empty)
            .output()
            .unwrap(),
        7,
    );
    assert_eq!(error["error"]["code"], "AUTHORITY_UNVERIFIED");
    assert_eq!(fs::read_dir(&empty).unwrap().count(), 0);

    // The versioned XDG directory can be an ancestor of the target while
    // the record and UUID lock would still be published inside the target.
    let workspaces = fixture.0.join("state/hive/v2/workspaces");
    fs::create_dir_all(&workspaces).unwrap();
    let error = response(
        fixture
            .init(&workspaces)
            .arg("--register-runtime")
            .env("XDG_STATE_HOME", &state)
            .output()
            .unwrap(),
        7,
    );
    assert_eq!(error["error"]["code"], "AUTHORITY_UNVERIFIED");
    assert_eq!(fs::read_dir(workspaces).unwrap().count(), 0);
    assert!(fixture.stages().is_empty());
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
    assert_eq!(fs::read_dir(&xdg).unwrap().count(), 0);
    assert_eq!(fs::read(&global).unwrap(), global_contents);
    assert_eq!(fs::read_dir(home.join(".dolt")).unwrap().count(), 1);
    assert_eq!(fs::read_dir(home).unwrap().count(), 2);
    assert!(fixture.stages().is_empty());
    let repeat = response(fixture.init(&target).output().unwrap(), 5);
    assert_eq!(repeat["error"]["code"], "ALREADY_EXISTS");

    let registered = fixture.dir("registered");
    let data = response(
        fixture
            .init(&registered)
            .arg("--register-runtime")
            .env("XDG_STATE_HOME", &xdg)
            .output()
            .unwrap(),
        0,
    );
    assert_eq!(data["data"]["active_root"], "matched");
    let status = response(
        fixture
            .hive()
            .args([
                "--contract",
                "2",
                "--json",
                "workspace",
                "status",
                "--workspace",
            ])
            .arg(&registered)
            .env("XDG_CONFIG_HOME", &xdg)
            .env("XDG_STATE_HOME", &xdg)
            .output()
            .unwrap(),
        0,
    );
    assert_eq!(status["data"]["active_root"], "matched");
    assert_eq!(status["data"]["workspace_id"], data["data"]["workspace_id"]);
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

#[test]
fn backup_help_and_conversation_flag_are_contract_two_only() {
    let fixture = Fixture::new();
    let run = |args: &[&str]| fixture.hive().args(args).output().unwrap();
    for args in [
        &["--help"][..],
        &["backup", "--help"],
        &["--contract", "1", "backup", "create", "--help"],
        &["--contract", "1", "backup", "restore", "--help"],
    ] {
        let result = run(args);
        assert!(!String::from_utf8_lossy(&result.stdout).contains("Create a local"));
    }
    let v2_root = run(&["--contract", "2", "--help"]);
    assert!(v2_root.status.success());
    assert!(
        String::from_utf8_lossy(&v2_root.stdout).contains("Create a local v2 workspace backup")
    );
    let help = run(&["backup", "create", "--help", "--contract", "2"]);
    assert!(help.status.success(), "{help:?}");
    assert!(String::from_utf8_lossy(&help.stdout).contains("--include-conversations"));
    let refused = response(
        run(&[
            "--contract",
            "2",
            "--json",
            "--workspace",
            "missing",
            "backup",
            "create",
            "--destination",
            "backup",
            "--include-conversations",
        ]),
        7,
    );
    assert_eq!(refused["error"]["code"], "NOT_IMPLEMENTED");
    assert!(!fixture.0.join("backup").exists());
    let help = run(&["backup", "restore", "--help", "--contract", "2"]);
    assert!(help.status.success(), "{help:?}");
    assert!(String::from_utf8_lossy(&help.stdout).contains("--source <SNAPSHOT>"));
    let refused = response(
        run(&[
            "--contract",
            "2",
            "--json",
            "backup",
            "restore",
            "--source",
            "missing",
            "--target",
            "restored",
            "--include-conversations",
        ]),
        7,
    );
    assert_eq!(refused["error"]["code"], "NOT_IMPLEMENTED");
    assert!(!fixture.0.join("restored").exists());
}

#[test]
#[ignore = "requires installed Dolt 2.7.6 and Git"]
fn creates_and_restores_exact_local_backup_and_refuses_unverified_inputs() {
    use sha2::{Digest, Sha256};

    let fixture = Fixture::new();
    let root = fixture.dir("workspace");
    let xdg = fixture.dir("xdg");
    let home = fixture.dir("offline home");
    let bin = fixture.dir("bin");
    let dolt = PathBuf::from(std::env::var_os("HIVE_DOLT_BIN").unwrap());
    let git = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join("git"))
        .find(|path| path.is_file())
        .unwrap();
    symlink(&dolt, bin.join("dolt")).unwrap();
    symlink(&git, bin.join("git")).unwrap();
    let initialized = response(
        fixture
            .init(&root)
            .arg("--register-runtime")
            .env("XDG_STATE_HOME", &xdg)
            .output()
            .unwrap(),
        0,
    );
    let initial = &initialized["data"];
    let execute = |command: &mut Command| {
        let output = command.output().unwrap();
        assert!(output.status.success(), "{output:?}");
        output.stdout
    };
    let database = |path: &Path| {
        let mut command = Command::new(&dolt);
        command
            .current_dir(path)
            .env_clear()
            .env("HOME", &home)
            .env("DOLT_ROOT_PATH", &home)
            .env("DOLT_DISABLE_VERSION_CHECKS", "true")
            .env("DOLT_DISABLE_EVENT_FLUSH", "true");
        command
    };
    let assets = |path: &Path| {
        let mut command = Command::new(&git);
        command
            .current_dir(path)
            .env_clear()
            .env("HOME", &home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_AUTHOR_NAME", "Synthetic")
            .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
            .env("GIT_COMMITTER_NAME", "Synthetic")
            .env("GIT_COMMITTER_EMAIL", "test@example.invalid");
        command
    };
    let db = root.join("db");
    let asset_root = root.join("assets");
    let source_soul = b"# Synthetic Soul

Two lines.
";
    let soul = "bots/synthetic/SOUL.md";
    fs::create_dir_all(asset_root.join("bots/synthetic")).unwrap();
    let prior_soul = b"# Earlier Synthetic Soul\n";
    fs::write(asset_root.join(soul), prior_soul).unwrap();
    execute(assets(&asset_root).args(["add", soul]));
    execute(assets(&asset_root).args(["commit", "-m", "Add earlier synthetic Soul"]));
    fs::write(asset_root.join(soul), source_soul).unwrap();
    let linked_soul = "bots/synthetic/current.md";
    symlink("SOUL.md", asset_root.join(linked_soul)).unwrap();
    let linked_directory = "synthetic-assets";
    symlink("bots/synthetic", asset_root.join(linked_directory)).unwrap();
    let linked_chain = "bots/synthetic/linked.md";
    symlink(
        "../../synthetic-assets/current.md",
        asset_root.join(linked_chain),
    )
    .unwrap();
    execute(assets(&asset_root).args(["add", "."]));
    execute(assets(&asset_root).args(["commit", "-m", "Revise synthetic Soul"]));
    let previous = initial["db_commit"].as_str().unwrap();
    let changes = format!(
        "UPDATE workspace_counters SET next_value = 18446744073709551615 WHERE name = 'issue_number';
         INSERT INTO areas (id, slug, name, revision)
         VALUES ('01900000-0000-7000-8000-000000000007', 'synthetic', 'Synthetic Area',
           '01900000-0000-7000-8000-000000000008');
         INSERT INTO issues (id, number, area_id, title, type, status, revision, created_at, updated_at)
         VALUES ('01900000-0000-7000-8000-000000000009', 18446744073709551614,
           '01900000-0000-7000-8000-000000000007', 'Synthetic Issue', 'task', 'todo',
           '01900000-0000-7000-8000-00000000000a', UTC_TIMESTAMP(6), UTC_TIMESTAMP(6));
         INSERT INTO bots VALUES ('01900000-0000-7000-8000-000000000001', 'synthetic',
           'Synthetic Queen', '01900000-0000-7000-8000-000000000002', NULL);
         INSERT INTO resources VALUES ('01900000-0000-7000-8000-000000000003', 'reference',
           'Synthetic reference', NULL, '01900000-0000-7000-8000-000000000004');
         INSERT INTO operation_receipts VALUES ('01900000-0000-7000-8000-000000000005',
           'fixture', REPEAT('a', 64), 'synthetic', 'fixture', '{{}}', '{previous}', '{{}}',
           '2026-01-01 00:00:00');"
    );
    execute(database(&db).args(["sql", "-q", &changes]));
    execute(database(&db).args(["add", "."]));
    execute(database(&db).args([
        "commit",
        "--author",
        "Synthetic <test@example.invalid>",
        "-m",
        "Advance counter",
    ]));
    let history: Value = serde_json::from_slice(&execute(database(&db).args([
        "sql",
        "-r",
        "json",
        "-q",
        "SELECT DOLT_HASHOF('HEAD') AS revision",
    ])))
    .unwrap();
    let receipt_commit = history["rows"][0]["revision"].as_str().unwrap();
    execute(database(&db).args([
        "sql",
        "-q",
        "UPDATE operation_receipts
         SET request_id = '01900000-0000-7000-8000-000000000006'",
    ]));
    execute(database(&db).args(["add", "."]));
    execute(database(&db).args([
        "commit",
        "--author",
        "Synthetic <test@example.invalid>",
        "-m",
        "Remove synthetic receipt from current view",
    ]));
    fs::write(
        root.join("conversations/private.md"),
        "synthetic-private-marker",
    )
    .unwrap();

    let run_backup = |destination: &str| {
        fixture
            .hive()
            .env("XDG_STATE_HOME", &xdg)
            .args(["--contract", "2", "--json", "--workspace"])
            .arg(&root)
            .args(["backup", "create", "--destination", destination])
            .output()
            .unwrap()
    };
    let expect_failure = |destination: &str, status, code| {
        let result = response(run_backup(destination), status);
        assert_eq!(result["error"]["code"], code, "{result}");
        assert!(!fixture.0.join(destination).exists());
        assert!(!fs::read_dir(&fixture.0).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".hive-backup-")
        }));
    };
    let state = xdg
        .join("hive/v2/workspaces")
        .join(initial["workspace_id"].as_str().unwrap());
    // Reproduce the files Dolt 2.7.6 leaves after Hive stops its server.
    // The credentials directory from that launch is already absent.
    let server_config = db.join("config.yaml");
    let run = state.join("run-01900000-0000-4000-8000-000000000001");
    let stopped_config = format!(
        "log_level: error\nbehavior:\n  event_scheduler: \"OFF\"\nlistener:\n  host: 127.0.0.1\n  port: 38765\ndata_dir: {}\ncfg_dir: {}\nprivilege_file: {}\n",
        serde_json::json!(db),
        serde_json::json!(run),
        serde_json::json!(run.join("privileges.db")),
    );
    symlink(db.join(".dolt/config.json"), &server_config).unwrap();
    expect_failure("failed", 7, "AUTHORITY_UNVERIFIED");
    fs::remove_file(&server_config).unwrap();
    fs::write(&server_config, vec![b'#'; 64 * 1024 + 1]).unwrap();
    expect_failure("failed", 7, "AUTHORITY_UNVERIFIED");
    fs::write(
        &server_config,
        format!("{stopped_config}remotesapi: {{}}\n"),
    )
    .unwrap();
    expect_failure("failed", 7, "AUTHORITY_UNVERIFIED");
    fs::write(
        &server_config,
        stopped_config.replace("127.0.0.1", "0.0.0.0"),
    )
    .unwrap();
    expect_failure("failed", 7, "AUTHORITY_UNVERIFIED");
    fs::write(&server_config, &stopped_config).unwrap();

    // The source YAML can be valid while the selected state directory cannot
    // be represented as a UTF-8 path. Refuse that mismatch with typed JSON.
    use std::os::unix::ffi::OsStrExt as _;
    let non_utf8_state = fixture.0.join(std::ffi::OsStr::from_bytes(b"state-\xff"));
    fs::rename(&xdg, &non_utf8_state).unwrap();
    let output = fixture
        .hive()
        .env("XDG_STATE_HOME", &non_utf8_state)
        .args(["--contract", "2", "--json", "--workspace"])
        .arg(&root)
        .args(["backup", "create", "--destination", "non-utf8-backup"])
        .output()
        .unwrap();
    fs::rename(&non_utf8_state, &xdg).unwrap();
    let refused = response(output, 7);
    assert_eq!(refused["error"]["code"], "AUTHORITY_UNVERIFIED");
    assert!(!fixture.0.join("non-utf8-backup").exists());

    let native_config = db.join(".dolt/config.json");
    let mut config: Value = serde_json::from_slice(&fs::read(&native_config).unwrap()).unwrap();
    config["sqlserver.global.server_uuid"] = serde_json::json!("not-an-engine-uuid");
    fs::write(&native_config, config.to_string()).unwrap();
    expect_failure("failed", 7, "AUTHORITY_UNVERIFIED");
    config["sqlserver.global.server_uuid"] =
        serde_json::json!("00000000-0000-4000-8000-000000000002");
    config["sqlserver.global.foreign_key_checks"] = serde_json::json!("off");
    fs::write(&native_config, config.to_string()).unwrap();
    expect_failure("failed", 7, "AUTHORITY_UNVERIFIED");
    config
        .as_object_mut()
        .unwrap()
        .remove("sqlserver.global.foreign_key_checks");
    fs::write(&native_config, config.to_string()).unwrap();

    for marker in [
        "pending-request.json",
        "starting",
        "server.json",
        "run-unrecognized",
    ] {
        fs::write(state.join(marker), b"{}").unwrap();
        expect_failure("failed", 7, "AUTHORITY_UNVERIFIED");
        fs::remove_file(state.join(marker)).unwrap();
    }
    let proof = state.join("stopped-request.json");
    symlink(state.join("active-root.json"), &proof).unwrap();
    expect_failure("failed", 7, "AUTHORITY_UNVERIFIED");
    fs::remove_file(&proof).unwrap();
    let proof_bytes = b"synthetic orphaned recovery proof";
    fs::write(&proof, proof_bytes).unwrap();
    fs::set_permissions(&proof, fs::Permissions::from_mode(0o600)).unwrap();
    response(run_backup("with-stale-proof"), 0);
    assert_eq!(fs::read(&proof).unwrap(), proof_bytes);
    assert!(
        !fixture
            .0
            .join("with-stale-proof/stopped-request.json")
            .exists()
    );
    fs::remove_file(&proof).unwrap();

    let native_server = db.join(".dolt/sql-server.info");
    fs::write(&native_server, b"unknown").unwrap();
    expect_failure("failed", 7, "AUTHORITY_UNVERIFIED");
    fs::remove_file(native_server).unwrap();
    let lock = fs::File::open(state.join("lock")).unwrap();
    rustix::fs::flock(&lock, rustix::fs::FlockOperation::NonBlockingLockExclusive).unwrap();
    expect_failure("failed", 5, "LOCK_BUSY");
    drop(lock);
    let lock = fs::File::open(db.join(".dolt/noms/LOCK")).unwrap();
    rustix::fs::flock(&lock, rustix::fs::FlockOperation::NonBlockingLockExclusive).unwrap();
    expect_failure("failed", 5, "LOCK_BUSY");
    drop(lock);

    fs::write(asset_root.join(soul), "dirty").unwrap();
    expect_failure("failed", 5, "DIRTY_CONFLICT");
    fs::write(asset_root.join(soul), source_soul).unwrap();
    execute(database(&db).args(["sql", "-q", "UPDATE workspace_counters SET next_value = 43"]));
    expect_failure("failed", 5, "DIRTY_CONFLICT");
    execute(database(&db).args(["checkout", "--", "workspace_counters"]));
    execute(database(&db).args(["branch", "unsupported"]));
    expect_failure("failed", 7, "AUTHORITY_UNVERIFIED");
    execute(database(&db).args(["branch", "-d", "unsupported"]));

    // Inject failures at the native export boundary, after private staging is
    // prepared. A competing destination must survive the final rename.
    fs::remove_file(bin.join("dolt")).unwrap();
    let wrapper = bin.join("dolt");
    let fault = |body: &str| {
        fs::write(&wrapper, format!(
            "#!/bin/sh\nif [ \"$1\" = backup ] && [ \"$2\" = sync-url ]; then {body}; fi\nexec \"{}\" \"$@\"\n",
            dolt.display()
        )).unwrap();
        fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o700)).unwrap();
    };
    fault("exit 17");
    expect_failure("failed", 7, "IO_ERROR");
    let competing = fixture.0.join("contended");
    fault(&format!(
        "/bin/mkdir '{}'; printf competing > '{}/sentinel'",
        competing.display(),
        competing.display()
    ));
    let raced = response(run_backup("contended"), 5);
    assert_eq!(raced["error"]["code"], "ALREADY_EXISTS");
    assert_eq!(fs::read(competing.join("sentinel")).unwrap(), b"competing");
    fs::remove_file(&wrapper).unwrap();
    symlink(&dolt, &wrapper).unwrap();

    // Dolt can leave an empty native transfer directory in the source.
    let native_tmp = db.join(".dolt/tmp");
    fs::create_dir_all(&native_tmp).unwrap();
    fs::write(native_tmp.join("unexpected"), b"unverified temporary state").unwrap();
    expect_failure("failed", 7, "AUTHORITY_UNVERIFIED");
    fs::remove_file(native_tmp.join("unexpected")).unwrap();

    let result = response(run_backup("backup with spaces"), 0);
    let backup = fixture.0.join("backup with spaces");
    let manifest: Value =
        serde_json::from_slice(&fs::read(backup.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(result["data"]["manifest"], manifest);
    assert_eq!(manifest["format_version"], 1);
    assert_eq!(manifest["conversations_included"], false);
    assert_eq!(manifest["workspace_id"], initial["workspace_id"]);
    assert_eq!(fs::read_to_string(server_config).unwrap(), stopped_config);
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(native_config).unwrap()).unwrap(),
        config
    );
    assert!(!run.exists());
    assert_eq!(
        backup.metadata().unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(fs::read_dir(&backup).unwrap().count(), 3);
    let files = manifest["files"].as_array().unwrap();
    for entry in files {
        let relative = entry["path"].as_str().unwrap();
        assert!(!relative.contains("config") && !relative.contains("privileges"));
        let bytes = fs::read(backup.join(relative)).unwrap();
        assert_eq!(entry["bytes"], bytes.len());
        assert_eq!(entry["sha256"], format!("{:x}", Sha256::digest(bytes)));
    }
    assert_eq!(
        response(run_backup("backup with spaces"), 5)["error"]["code"],
        "ALREADY_EXISTS"
    );

    let run_restore = |source: &Path, target: &str| {
        fixture
            .hive()
            .env("XDG_STATE_HOME", fixture.0.join("unused-restore-state"))
            .env("XDG_CONFIG_HOME", fixture.0.join("unused-restore-config"))
            .args(["--contract", "2", "--json", "backup", "restore", "--source"])
            .arg(source)
            .args(["--target", target])
            .output()
            .unwrap()
    };

    // Also construct format-1 inputs directly, as the older backup command
    // could publish the checked-out Git modes without verifying their targets.
    let make_legacy_bundle = || {
        let bundle = fixture.0.join("legacy.bundle");
        execute(assets(&asset_root).args(["branch", "inspection"]));
        execute(
            assets(&asset_root)
                .args(["bundle", "create"])
                .arg(&bundle)
                .arg("refs/heads/inspection"),
        );
        execute(assets(&asset_root).args(["branch", "-D", "inspection"]));
        let bytes = fs::read(&bundle).unwrap();
        fs::remove_file(bundle).unwrap();
        bytes
    };
    let saved_assets = manifest["asset_git_commit"].as_str().unwrap();
    let backup_manifest = backup.join("manifest.json");
    let backup_bundle = backup.join("assets.bundle");
    let saved_manifest = fs::read(&backup_manifest).unwrap();
    let saved_bundle = fs::read(&backup_bundle).unwrap();
    let substitute_legacy_bundle = || {
        let revision = execute(assets(&asset_root).args(["rev-parse", "HEAD"]));
        let bytes = make_legacy_bundle();
        let mut replacement = manifest.clone();
        replacement["asset_git_commit"] = String::from_utf8(revision).unwrap().trim().into();
        let entry = replacement["files"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|entry| entry["path"] == "assets.bundle")
            .unwrap();
        entry["bytes"] = bytes.len().into();
        entry["sha256"] = format!("{:x}", Sha256::digest(&bytes)).into();
        fs::write(&backup_bundle, bytes).unwrap();
        fs::write(&backup_manifest, serde_json::to_vec(&replacement).unwrap()).unwrap();
    };
    for target in ["missing", "../conversations/private.md", ".git/HEAD"] {
        symlink(target, asset_root.join("unsafe")).unwrap();
        execute(assets(&asset_root).args(["add", "unsafe"]));
        execute(assets(&asset_root).args(["commit", "-m", "Add unsafe synthetic link"]));
        expect_failure("failed", 7, "AUTHORITY_UNVERIFIED");

        substitute_legacy_bundle();
        let refused = response(run_restore(&backup, "unsafe-restore"), 7);
        assert_eq!(refused["error"]["code"], "AUTHORITY_UNVERIFIED");
        assert!(!fixture.0.join("unsafe-restore").exists());
        execute(assets(&asset_root).args(["reset", "--hard", saved_assets]));
    }
    fs::write(&backup_bundle, &saved_bundle).unwrap();
    fs::write(&backup_manifest, &saved_manifest).unwrap();
    let restored = fixture.dir("restored");
    let result = response(run_restore(&backup, "restored"), 0);
    assert_eq!(result["data"]["active_root"], "inactive");
    assert_eq!(result["data"]["workspace_id"], manifest["workspace_id"]);
    assert_eq!(result["data"]["db_commit"], manifest["dolt_commit"]);
    assert_eq!(result["data"]["asset_commit"], manifest["asset_git_commit"]);
    assert_eq!(result["data"]["conversations_restored"], false);
    assert_eq!(restored.metadata().unwrap().mode() & 0o777, 0o700);
    assert_eq!(
        fs::read_dir(restored.join("conversations"))
            .unwrap()
            .count(),
        0
    );
    assert!(!restored.join("db/config.yaml").exists());
    assert!(!restored.join("db/.doltcfg").exists());
    assert!(!fixture.0.join("unused-restore-state").exists());
    assert!(!fixture.0.join("unused-restore-config").exists());
    let repo_state: Value =
        serde_json::from_slice(&fs::read(restored.join("db/.dolt/repo_state.json")).unwrap())
            .unwrap();
    assert_eq!(repo_state["remotes"], serde_json::json!({}));
    assert_eq!(repo_state["backups"], serde_json::json!({}));
    substitute_legacy_bundle();
    let legacy = response(run_restore(&backup, "new restored"), 0);
    assert_eq!(legacy["data"]["active_root"], "inactive");
    assert_eq!(legacy["data"]["asset_commit"], manifest["asset_git_commit"]);
    assert_eq!(
        fs::read_link(fixture.0.join("new restored/assets").join(linked_soul)).unwrap(),
        Path::new("SOUL.md")
    );
    fs::write(&backup_bundle, saved_bundle).unwrap();
    fs::write(&backup_manifest, saved_manifest).unwrap();
    let stale_target = fixture.0.join("stale-active-root");
    let stale_state = fixture
        .0
        .join("unused-restore-state/hive/v2/workspaces")
        .join(manifest["workspace_id"].as_str().unwrap());
    fs::create_dir_all(&stale_state).unwrap();
    let registration = serde_json::to_vec(&serde_json::json!({
        "version": 2,
        "workspace_id": manifest["workspace_id"],
        "workspace_root": stale_target,
    }))
    .unwrap();
    let registration_file = stale_state.join("active-root.json");
    fs::write(&registration_file, &registration).unwrap();
    let refused = response(run_restore(&backup, "stale-active-root"), 7);
    assert_eq!(refused["error"]["code"], "AUTHORITY_UNVERIFIED");
    assert!(!stale_target.exists());
    assert_eq!(fs::read(&registration_file).unwrap(), registration);

    // A failed check leaves a selected empty target untouched and no target
    // is published for invalid inputs when the target was absent.
    let manifest_path = backup.join("manifest.json");
    let empty = fixture.dir("still-empty");
    let original = fs::read(&manifest_path).unwrap();
    let mut invalid = manifest.clone();
    for (field, value) in [
        ("format_version", serde_json::json!(2)),
        ("conversations_included", serde_json::json!(true)),
        (
            "dolt_commit",
            serde_json::json!("00000000000000000000000000000000"),
        ),
        (
            "asset_git_commit",
            serde_json::json!("0000000000000000000000000000000000000001"),
        ),
    ] {
        invalid[field] = value;
        fs::write(&manifest_path, serde_json::to_vec(&invalid).unwrap()).unwrap();
        assert_eq!(
            response(run_restore(&backup, "still-empty"), 7)["ok"],
            false
        );
        assert_eq!(fs::read_dir(&empty).unwrap().count(), 0);
        invalid[field] = manifest[field].clone();
    }
    invalid["files"][0]["path"] = serde_json::json!("../assets.bundle");
    fs::write(&manifest_path, serde_json::to_vec(&invalid).unwrap()).unwrap();
    assert_eq!(
        response(run_restore(&backup, "bad-restore"), 7)["ok"],
        false
    );
    fs::write(&manifest_path, &original).unwrap();
    let bundle = backup.join("assets.bundle");
    let bundle_bytes = fs::read(&bundle).unwrap();
    let mut corrupt = bundle_bytes.clone();
    corrupt[0] ^= 1;
    fs::write(&bundle, corrupt).unwrap();
    assert_eq!(
        response(run_restore(&backup, "bad-restore"), 7)["ok"],
        false
    );
    fs::remove_file(&bundle).unwrap();
    let outside = fixture.0.join("external-bundle");
    fs::write(&outside, &bundle_bytes).unwrap();
    symlink(&outside, &bundle).unwrap();
    assert!(!run_restore(&backup, "bad-restore").status.success());
    fs::remove_file(&bundle).unwrap();
    fs::hard_link(&outside, &bundle).unwrap();
    assert!(!run_restore(&backup, "bad-restore").status.success());
    fs::remove_file(&bundle).unwrap();
    fs::write(&bundle, bundle_bytes).unwrap();

    // A prerequisite bundle must fail even when the manifest digest matches.
    let incomplete = fixture.0.join("incomplete.bundle");
    execute(
        assets(&asset_root)
            .args(["bundle", "create"])
            .arg(&incomplete)
            .arg("HEAD^..HEAD"),
    );
    let incomplete = fs::read(incomplete).unwrap();
    fs::write(&bundle, &incomplete).unwrap();
    let mut incomplete_manifest = manifest.clone();
    let entry = incomplete_manifest["files"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|entry| entry["path"] == "assets.bundle")
        .unwrap();
    entry["bytes"] = serde_json::json!(incomplete.len());
    entry["sha256"] = serde_json::json!(format!("{:x}", Sha256::digest(incomplete)));
    fs::write(
        &manifest_path,
        serde_json::to_vec(&incomplete_manifest).unwrap(),
    )
    .unwrap();
    assert_eq!(
        response(run_restore(&backup, "bad-restore"), 7)["error"]["code"],
        "IO_ERROR"
    );
    fs::write(&manifest_path, original).unwrap();
    fs::copy(&outside, &bundle).unwrap();

    // A competing write into an initially empty directory must survive and
    // prevent the validated payload from replacing the directory.
    let sentinel = empty.join("sentinel");
    fs::remove_file(&wrapper).unwrap();
    fs::write(&wrapper, format!(
        "#!/bin/sh\nif [ \"$1\" = fsck ]; then printf competing > '{}'; fi\nexec \"{}\" \"$@\"\n",
        sentinel.display(), dolt.display()
    )).unwrap();
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(!run_restore(&backup, "still-empty").status.success());
    assert_eq!(fs::read(&sentinel).unwrap(), b"competing");
    assert_eq!(fs::read_dir(&empty).unwrap().count(), 1);
    fs::remove_file(&wrapper).unwrap();
    symlink(&dolt, &wrapper).unwrap();
    assert!(!fixture.0.join("bad-restore").exists());
    assert!(!run_restore(&backup, "restored").status.success());
    assert!(
        !run_restore(&backup, "backup with spaces/nested")
            .status
            .success()
    );
    assert!(!run_restore(&backup, "workspace/nested").status.success());
    assert!(fixture.stages().is_empty());

    let verify = format!(
        "SELECT DOLT_HASHOF('HEAD') AS revision,
           (SELECT CAST(next_value AS CHAR) FROM workspace_counters) AS current_value,
           (SELECT CAST(number AS CHAR) FROM issues) AS issue_number,
           (SELECT next_value FROM workspace_counters AS OF '{previous}') AS prior_value,
           (SELECT request_id FROM operation_receipts) AS current_receipt,
           (SELECT request_id FROM operation_receipts AS OF '{receipt_commit}') AS past_receipt"
    );
    let rows: Value = serde_json::from_slice(&execute(
        database(&restored.join("db")).args(["sql", "-r", "json", "-q", &verify]),
    ))
    .unwrap();
    assert_eq!(rows["rows"][0]["revision"], manifest["dolt_commit"]);
    assert_eq!(rows["rows"][0]["current_value"], "18446744073709551615");
    assert_eq!(rows["rows"][0]["issue_number"], "18446744073709551614");
    assert_eq!(rows["rows"][0]["prior_value"], 1);
    assert_eq!(
        rows["rows"][0]["current_receipt"],
        "01900000-0000-7000-8000-000000000006"
    );
    assert_eq!(
        rows["rows"][0]["past_receipt"],
        "01900000-0000-7000-8000-000000000005"
    );

    let checked = restored.join("assets");
    for (path, target) in [
        (linked_soul, "SOUL.md"),
        (linked_directory, "bots/synthetic"),
        (linked_chain, "../../synthetic-assets/current.md"),
    ] {
        assert_eq!(
            fs::read_link(checked.join(path)).unwrap(),
            Path::new(target)
        );
    }
    assert_eq!(fs::read(checked.join(linked_chain)).unwrap(), source_soul);
    let revision = manifest["asset_git_commit"].as_str().unwrap();
    assert_eq!(
        execute(assets(&checked).args(["show", &format!("{revision}:{soul}")])),
        source_soul
    );
    assert_eq!(
        execute(assets(&checked).args(["show", &format!("{revision}^:{soul}")])),
        prior_soul
    );
    assert_eq!(
        execute(assets(&checked).args(["rev-parse", &format!("{revision}^^")])),
        format!(
            "{}
",
            initial["asset_commit"].as_str().unwrap()
        )
        .as_bytes()
    );
}
