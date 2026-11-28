use std::path::Path;
use std::process::Command;

use super::{
    AREA_GENERAL, POLICY_PIN, Value, fs, git, git_text, id, init_git, json, migration_cli,
    reviewed_row, sha256_hex,
};

const PARENT: &str = "tasks/_general/backlog/parent/README.md";
const OTHER: &str = "tasks/_general/backlog/other/README.md";
const ARTIFACT: &str = "tasks/_general/backlog/parent/artifacts/report.md";
const WITH_AREA: &str = "tasks/_general/backlog/parent/artifacts/area.md";
const MISMATCH: &str = "tasks/_general/backlog/parent/artifacts/stale.md";
const OLD: &str = "directives/_general/old.md";
const REPLACEMENT: &str = "directives/_general/withdrawn.md";
const HELD: &str = "tasks/_general/backlog/held/artifacts/report.md";
const MISSING: &str = "tasks/_general/backlog/missing/artifacts/report.md";
const STANDALONE: &str = "artifacts/standalone.md";
const REFERENCE: &str = "https://forge.example/synthetic/artifact/7";
const CREATED: &str = "2024-01-02T12:00:00Z";
const UPDATED: &str = "2024-01-03t12:00:00.000001+09:00";
const MALFORMED_DATE: &str = "2024-01-03X12:00:00Z";

#[test]
fn prepares_only_verified_work_scoped_artifact_bindings_under_both_pins() {
    exercise(false);
}

#[test]
#[ignore = "requires an isolated installed Hive and pinned Dolt and Git"]
fn installed_cli_stages_verified_work_artifact_links_and_rolls_back() {
    exercise(true);
}

fn exercise(stage: bool) {
    for pin in ["0fd0f8dbe632d14094a93599cb1b5f653789ac9c", POLICY_PIN] {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let home = root.path().join("home");
        let config = root.path().join("config");
        fs::create_dir(&home).unwrap();
        init_git(
            &source,
            &home,
            "Synthetic Git Committer",
            "fixture@example.invalid",
        );
        fs::write(source.join("AGENTS.md"), format!("hive-system@{pin}\n")).unwrap();
        if pin == POLICY_PIN {
            fs::write(
                source.join("hive.yml"),
                "version: 1\ncoordination:\n  mode: local\n",
            )
            .unwrap();
        }
        let body = format!(
            "\r\n# Synthetic café evidence\r\n\r\nCreated: {CREATED}\r\nSubstantive update: {UPDATED}\r\nOther example: {MALFORMED_DATE}\r\n"
        );
        let task = format!("---\r\ndescription: Synthetic Task\r\n---\r\n{body}");
        let artifact = format!(
            "---\r\ndescription: Synthetic Artifact\r\nkind: report\r\nstatus: parked\r\n---\r\n{body}"
        );
        for (entries, raw) in [
            (
                [PARENT, OTHER, "tasks/_general/backlog/held/README.md"].as_slice(),
                &task,
            ),
            (
                [ARTIFACT, WITH_AREA, MISMATCH, HELD, MISSING, STANDALONE].as_slice(),
                &artifact,
            ),
        ] {
            for path in entries {
                let path = source.join(path);
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(path, raw).unwrap();
            }
        }
        fs::create_dir_all(source.join("directives/_general")).unwrap();
        for (path, status) in [(OLD, "superseded"), (REPLACEMENT, "withdrawn")] {
            fs::write(
                source.join(path),
                format!("---\ndescription: Synthetic history\nstatus: {status}\n---\n{body}"),
            )
            .unwrap();
        }
        git(&source, &home, &["add", "."]);
        git(
            &source,
            &home,
            &["commit", "-qm", "Synthetic reviewed Task and Artifact"],
        );
        let commit = git_text(&source, &home, &["rev-parse", "HEAD"]);
        let index = fs::read(source.join(".git/index")).unwrap();
        let blob =
            |path: &str| git_text(&source, &home, &["rev-parse", &format!("{commit}:{path}")]);
        let workspace = id(1);
        let area = id(AREA_GENERAL);
        let range = |value: &str| {
            let start = body.find(value).unwrap();
            json!({"start": start, "end": start + value.len()})
        };
        let key = |work_id: &str| json!({"scope": "_general", "work_id": work_id});
        let task_review = |work_id, path| {
            json!({"legacy_key": key(work_id), "source_git_blob_sha": blob(path),
            "area_id": area, "relations": "no_edges", "created_at": range(CREATED), "updated_at": range(UPDATED)})
        };
        let parent_review = |work_id: &str, path: &str, oid: String| {
            json!({"legacy_key": key(work_id),
            "workspace_id": workspace, "area_id": area, "source_path": path, "source_git_blob_sha": oid})
        };
        let work_review = |source_path, target_id, parent_task| {
            json!({
                "source_path": source_path, "source_git_blob_sha": blob(source_path), "target_id": id(target_id),
                "provenance": {"kind": "work_scoped_artifact", "artifact_kind": "report", "source_ref": REFERENCE,
                    "author_ref": "bot:synthetic-author", "created_at": range(CREATED), "substantive_updated_at": range(UPDATED),
                    "parent_task": parent_task, "issue_link": "retain", "archive": "none"}
            })
        };
        let mut area_review = work_review(
            WITH_AREA,
            0x905,
            parent_review("parent", PARENT, blob(PARENT)),
        );
        area_review["provenance"]["area_link"] = json!(area);
        let history = |path, status, id_number, supersession| {
            json!({
                "source_path": path, "source_git_blob_sha": blob(path), "target_id": id(id_number),
                "provenance": {"kind": "directive_history", "status": status, "area_id": area,
                    "source_ref": REFERENCE, "issuer_ref": "human:synthetic-issuer", "created_at": CREATED,
                    "updated_at": CREATED, "supersession": supersession, "approval_and_validity": {"state": "reviewed"}}
            })
        };
        let baseline = json!({
            "version": 1, "source_commit": commit, "workspace_id": workspace, "policy_pin": pin,
            "scopes": [{"scope": "_general", "decision": {"kind": "area", "area": {"id": area, "slug": "general", "name": "Synthetic Area"}}}],
            "task_reviews": [task_review("parent", PARENT), task_review("other", OTHER)],
            "reviewed_records": [
                work_review(ARTIFACT, 0x901, parent_review("parent", PARENT, blob(PARENT))),
                area_review,
                work_review(MISMATCH, 0x906, parent_review("other", OTHER, blob(OTHER))),
                history(OLD, "superseded", 0x907, json!({"state":"reviewed", "replacement":"directives/_general/withdrawn"})),
                history(REPLACEMENT, "withdrawn", 0x908, json!({"state":"reviewed"})),
                work_review(HELD, 0x902, parent_review("held", "tasks/_general/backlog/held/README.md", blob("tasks/_general/backlog/held/README.md"))),
                work_review(MISSING, 0x903, parent_review("missing", "tasks/_general/backlog/missing/README.md", blob(PARENT))),
                {"source_path": STANDALONE, "source_git_blob_sha": blob(STANDALONE), "target_id": id(0x904),
                 "provenance": {"kind": "standalone_artifact", "artifact_kind": "report", "source_ref": REFERENCE,
                    "author_ref": "bot:synthetic-author", "created_at": CREATED, "substantive_updated_at": "2024-01-03T03:00:00.000001Z",
                    "links": "none", "archive": "none"}}
            ],
        });
        let file = root.path().join("mapping.json");
        let run = |mapping: &Value, tasks: bool, status| {
            fs::write(&file, serde_json::to_vec(mapping).unwrap()).unwrap();
            let mut args = vec![
                "--source",
                source.to_str().unwrap(),
                "--source-commit",
                &commit,
                "--workspace-id",
                &workspace,
                "--mapping-file",
                file.to_str().unwrap(),
                "--include-directives-artifacts",
            ];
            if tasks {
                args.push("--tasks");
            }
            migration_cli(&home, &config, "migration-plan", &args, status)
        };
        let bind = |field: &str| format!("/reviewed_records/0/provenance/{field}");
        let report = run(&baseline, true, 0);
        let just_artifacts = run(&baseline, false, 0);
        assert_eq!(
            report["data"]["directive_artifact_intake"],
            just_artifacts["data"]["directive_artifact_intake"]
        );
        assert!(just_artifacts["data"].get("task_intake").is_none());
        assert_eq!(report["data"]["task_intake"]["counts"]["prepared"], 2);
        assert_eq!(
            report["data"]["directive_artifact_intake"]["counts"]["artifacts_work_scoped"],
            json!({"inventoried": 5, "prepared": 2, "held": 3, "excluded": 0})
        );
        let row = reviewed_row(&report, ARTIFACT);
        let prepared = &row["outcome"];
        assert_eq!(prepared["status"], "prepared");
        assert_eq!(prepared["target_id"], id(0x901));
        assert_eq!(row["source_git_blob_sha"], blob(ARTIFACT));
        assert_eq!(row["source_raw_sha256"], sha256_hex(artifact.as_bytes()));
        assert_eq!(prepared["target_body_sha256"], sha256_hex(body.as_bytes()));
        assert_eq!(
            prepared["provenance"]["substantive_updated_at"],
            json!({"value": UPDATED, "body_range": range(UPDATED)})
        );
        assert_eq!(
            prepared["provenance"]["issue_link"],
            json!({"decision": "retain", "legacy_key": key("parent"),
            "area_id": area, "workspace_id": workspace, "source_path_sha256": sha256_hex(PARENT.as_bytes()),
            "source_git_blob_sha": blob(PARENT)})
        );
        assert_eq!(
            reviewed_row(&report, STANDALONE)["outcome"]["provenance"]["scope"],
            "standalone"
        );
        assert!(
            reviewed_row(&report, STANDALONE)["outcome"]["provenance"]
                .get("issue_link")
                .is_none()
        );
        let assert_hold = |report: &Value, path, reason| {
            let outcome = &reviewed_row(report, path)["outcome"];
            assert_eq!(outcome["status"], "held");
            assert_eq!(outcome["reason_codes"], json!([reason]));
            assert!(outcome.get("target_id").is_none());
        };
        for path in [HELD, MISSING, MISMATCH] {
            assert_hold(&report, path, "parent_task_quarantined");
        }
        assert_eq!(blob(PARENT), blob(OTHER));
        for (field, value, reason) in [
            (
                "parent_task",
                parent_review("other", OTHER, blob(OTHER)),
                "parent_task_quarantined",
            ),
            (
                "parent_task/workspace_id",
                json!(id(2)),
                "parent_task_quarantined",
            ),
            (
                "parent_task/area_id",
                json!(id(3)),
                "parent_task_quarantined",
            ),
            (
                "parent_task/source_path",
                json!(OTHER),
                "parent_task_quarantined",
            ),
            (
                "parent_task/source_git_blob_sha",
                json!(blob(ARTIFACT)),
                "parent_task_quarantined",
            ),
            ("issue_link", Value::Null, "unresolved_artifact_links"),
            (
                "issue_link",
                json!("unresolved"),
                "unresolved_artifact_links",
            ),
            (
                "substantive_updated_at",
                Value::Null,
                "incomplete_historical_provenance",
            ),
            (
                "created_at",
                range(MALFORMED_DATE),
                "incomplete_historical_provenance",
            ),
            (
                "created_at",
                json!({"start": 0, "end": 4}),
                "incomplete_historical_provenance",
            ),
        ] {
            let mut broken = baseline.clone();
            *broken.pointer_mut(&bind(field)).unwrap() = value;
            assert_hold(&run(&broken, false, 0), ARTIFACT, reason);
        }
        for (field, value) in [
            ("issue_link", json!({"retain": null})),
            ("parent_task", json!([])),
            ("parent_task/legacy_key", json!(["_general", "parent"])),
            ("created_at", json!([0, 1])),
        ] {
            let mut malformed = baseline.clone();
            *malformed.pointer_mut(&bind(field)).unwrap() = value;
            assert_eq!(run(&malformed, false, 2)["error"]["code"], "INVALID_CONFIG");
        }
        let mut other_area = baseline.clone();
        other_area["reviewed_records"][0]["provenance"]["area_link"] = json!(id(2));
        assert_hold(
            &run(&other_area, false, 0),
            ARTIFACT,
            "unresolved_artifact_links",
        );
        let mut malformed = baseline.clone();
        malformed["reviewed_records"] = Value::Null;
        assert_eq!(run(&malformed, false, 2)["error"]["code"], "INVALID_CONFIG");

        assert_eq!(
            reviewed_row(&report, WITH_AREA)["outcome"]["provenance"]["issue_link"]["joined_area_id"],
            area
        );
        if stage && pin == POLICY_PIN {
            verify_installed_stage(root.path(), &baseline, &body);
        }
        for private in [
            "Synthetic café evidence",
            REFERENCE,
            "bot:synthetic-author",
            MALFORMED_DATE,
        ] {
            assert!(!report.to_string().contains(private));
        }
        assert_eq!(fs::read(source.join(".git/index")).unwrap(), index);
        assert_eq!(git_text(&source, &home, &["rev-parse", "HEAD"]), commit);
        assert!(git(&source, &home, &["status", "--porcelain"]).is_empty());
        assert!(!config.exists());
    }
}

fn verify_installed_stage(root: &Path, mapping: &Value, body: &str) {
    let source = root.join("source");
    let home = root.join("home");
    let config = root.join("config");
    let state = root.join("state");
    let data_home = root.join("data");
    let mapping_file = root.join("stage-review.json");
    let target = root.join("inactive");
    let hive = std::env::var_os("HIVE_STAGING_INSTALLED_BIN").expect("isolated installed Hive");
    let dolt = std::env::var_os("HIVE_DOLT_BIN").expect("pinned Dolt");
    let search_path = std::env::var_os("PATH").unwrap();
    let commit = mapping["source_commit"].as_str().unwrap();
    let workspace = mapping["workspace_id"].as_str().unwrap();
    let area = id(AREA_GENERAL);
    fs::write(&mapping_file, serde_json::to_vec(mapping).unwrap()).unwrap();
    let stage = |mode, with_tasks: bool| {
        let mut command = Command::new(&hive);
        command
            .current_dir(&home)
            .env_clear()
            .env("HOME", &home)
            .env("PATH", &search_path)
            .env("XDG_CONFIG_HOME", &config)
            .env("XDG_STATE_HOME", &state)
            .env("XDG_DATA_HOME", &data_home)
            .args([
                "--contract",
                "2",
                "--json",
                "workspace",
                "migration-stage",
                "--source",
            ])
            .arg(&source)
            .args([
                "--source-commit",
                commit,
                "--workspace-id",
                workspace,
                "--mapping-file",
            ])
            .arg(&mapping_file)
            .arg("--target")
            .arg(&target)
            .args([
                "--policy-revision",
                "c66b3c50150e18426485d134de92c6b311dac125",
                "--mode",
                mode,
            ]);
        if with_tasks {
            command.arg("--tasks");
        }
        let output = command.output().unwrap();
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(output.status.success(), "{stdout} {:?}", output.stderr);
        assert!(output.stderr.is_empty());
        for private in [
            "Synthetic café evidence",
            REFERENCE,
            "bot:synthetic-author",
            ARTIFACT,
            PARENT,
        ] {
            assert!(!stdout.contains(private), "{private}");
        }
        serde_json::from_str::<Value>(&stdout).unwrap()["data"].clone()
    };
    // A Task must be eligible for this transaction, even when its review could prepare it.
    let no_tasks = stage("dry-run", false);
    assert_eq!(no_tasks["counts"]["artifacts_work_scoped"]["imported"], 0);
    assert_eq!(no_tasks["counts"]["artifacts_standalone"]["imported"], 1);
    assert!(!target.exists());
    let dry = stage("dry-run", true);
    assert_eq!(dry["checks"]["transaction"], "rolled_back");
    assert_eq!(dry["checks"]["target_artifact_rows"], 0);
    assert_eq!(dry["checks"]["target_artifact_links"], 0);
    assert_eq!(dry["checks"]["target_issue_rows"], 0);
    assert_eq!(dry["counts"]["artifacts_work_scoped"]["imported"], 2);
    assert!(dry["target"]["confirmed_dolt_commit"].is_null());
    assert!(!target.exists());

    let result = stage("staged-import", true);
    assert_eq!(result["checks"]["published_inactive"], true);
    assert_eq!(result["checks"]["committed_record_validation"], "passed");
    assert_eq!(result["checks"]["target_issue_rows"], 2);
    assert_eq!(result["checks"]["target_directive_rows"], 2);
    assert_eq!(result["checks"]["target_artifact_rows"], 3);
    assert_eq!(result["checks"]["target_artifact_links"], 3);
    assert_eq!(
        result["counts"]["artifacts_work_scoped"],
        json!({"inventoried": 5, "imported": 2, "held": 3, "excluded_non_record": 0})
    );
    let id_map = result["id_map"].as_array().unwrap();
    let by_source = |path: &str| {
        id_map
            .iter()
            .find(|row| row["source_path_sha256"] == sha256_hex(path.as_bytes()))
            .unwrap()
    };
    let owner = by_source(PARENT)["issue_id"].as_str().unwrap();
    assert_ne!(owner, by_source(OTHER)["issue_id"].as_str().unwrap());
    for path in [ARTIFACT, WITH_AREA] {
        let row = by_source(path);
        let link = &row["outcome"]["provenance"]["issue_link"];
        assert_eq!(link["issue_id"], owner);
        assert_eq!(link["legacy_key_sha256"], sha256_hex(b"_general\0parent"));
        assert!(link.get("legacy_key").is_none());
        assert_eq!(
            row["outcome"]["target_body_sha256"],
            sha256_hex(body.as_bytes())
        );
        assert_eq!(
            row["source_raw_sha256"],
            sha256_hex(&fs::read(source.join(path)).unwrap())
        );
    }
    for path in [HELD, MISSING, MISMATCH] {
        let locator = sha256_hex(path.as_bytes());
        let hold = result["holds"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["source_path_sha256"] == locator)
            .unwrap();
        assert_eq!(
            hold["outcome"]["reason_codes"],
            json!(["parent_task_quarantined"])
        );
        assert!(
            id_map
                .iter()
                .all(|row| row["source_path_sha256"] != locator)
        );
    }

    let confirmed = result["target"]["confirmed_dolt_commit"].as_str().unwrap();
    let query = |sql: &str| {
        let output = Command::new(&dolt)
            .current_dir(target.join("db"))
            .env_clear()
            .env("HOME", &home)
            .env("DOLT_DISABLE_EVENT_FLUSH", "true")
            .args(["sql", "-r", "json", "-q", sql])
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        serde_json::from_slice::<Value>(&output.stdout).unwrap()["rows"].clone()
    };
    assert_eq!(
        query(&format!(
            "SELECT artifact_id, issue_id FROM artifact_issues AS OF '{confirmed}' ORDER BY artifact_id"
        )),
        json!([{"artifact_id": id(0x901), "issue_id": owner}, {"artifact_id": id(0x905), "issue_id": owner}])
    );
    assert_eq!(
        query(&format!(
            "SELECT artifact_id, area_id FROM artifact_areas AS OF '{confirmed}'"
        )),
        json!([{"artifact_id": id(0x905), "area_id": area}])
    );
    let artifacts = query(&format!("SELECT id, body, SHA2(body, 256) AS digest,
        DATE_FORMAT(substantive_updated_at, '%Y-%m-%dT%H:%i:%s.%fZ') AS updated_at FROM artifacts AS OF '{confirmed}' ORDER BY id"));
    assert_eq!(
        artifacts,
        json!(
            [0x901, 0x904, 0x905].map(|record| json!({"id": id(record), "body": body,
        "digest": sha256_hex(body.as_bytes()), "updated_at": "2024-01-03T03:00:00.000001Z"}))
        )
    );
    assert_eq!(
        query(&format!(
            "SELECT id, status, supersedes_id FROM directives AS OF '{confirmed}' ORDER BY id"
        )),
        json!([{"id": id(0x907), "status": "superseded"}, {"id": id(0x908), "status": "withdrawn", "supersedes_id": id(0x907)}])
    );
    assert!(!config.exists());
    assert!(!state.exists());
    assert!(!data_home.exists());
}
