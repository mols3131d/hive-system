use std::path::Path;
use std::process::Command;

use super::{
    AREA_GENERAL, POLICY_PIN, Value, fs, git, git_text, id, init_git, json, migration_cli,
    reviewed_row, sha256_hex,
};

const DONE: &str = "tasks/_general/archive/done/README.md";
const CANCELLED: &str = "tasks/_general/archive/cancelled/README.md";
const HELD: &str = "tasks/_general/archive/held/README.md";
const DONE_ARTIFACT: &str = "tasks/_general/archive/done/artifacts/report.md";
const CANCELLED_ARTIFACT: &str = "tasks/_general/archive/cancelled/artifacts/report.md";
const HELD_ARTIFACT: &str = "tasks/_general/archive/held/artifacts/report.md";
const NO_REVIEW: &str = "tasks/_general/archive/done/artifacts/no-review.md";
const WRONG_RANGE: &str = "tasks/_general/archive/done/artifacts/wrong-range.md";
const EARLY_ARCHIVE: &str = "tasks/_general/archive/done/artifacts/early-archive.md";
const ROOT_ARTIFACT: &str = "artifacts/standalone.md";
const CREATED: &str = "2024-02-01T00:00:00Z";
const TASK_UPDATED: &str = "2024-02-02T00:00:00Z";
const ARTIFACT_UPDATED: &str = "2024-02-04T21:00:00.000001+09:00";
const DONE_CLOSED: &str = "2024-02-05T12:34:56.123456+09:00";
const CANCELLED_CLOSED: &str = "2024-02-03T23:00:00-05:00";
const DONE_UTC: &str = "2024-02-05T03:34:56.123456Z";
const CANCELLED_UTC: &str = "2024-02-04T04:00:00.000000Z";
const UPDATED_UTC: &str = "2024-02-04T12:00:00.000001Z";
const ARCHIVED_AT: &str = "2024-02-06T10:00:00-05:00";
const ARCHIVED_UTC: &str = "2024-02-06T15:00:00.000000Z";
const TOO_EARLY: &str = "2024-02-04T15:00:00Z";
const CLOSER: &str = "human:synthetic-closer";
const RESOLUTION: &str = "Synthetic private Task resolution.";
const ARCHIVE_REASON: &str = "Synthetic retained café evidence.";
const SOURCE_REF: &str = "https://forge.example/synthetic/terminal-artifact";

#[test]
fn terminal_artifact_retention_requires_exact_verified_closure_under_both_pins() {
    exercise(false);
}

#[test]
#[ignore = "requires isolated installed Hive and pinned Git and Dolt"]
fn installed_cli_preserves_terminal_parent_artifact_retention() {
    exercise(true);
}

fn range_at(start: usize, evidence: &str) -> Value {
    json!({"start": start, "end": start + evidence.len()})
}

fn range(body: &str, evidence: &str) -> Value {
    range_at(body.find(evidence).unwrap(), evidence)
}

fn exercise(installed: bool) {
    for pin in ["0fd0f8dbe632d14094a93599cb1b5f653789ac9c", POLICY_PIN] {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path();
        let source = root.join("source");
        let home = root.join("home");
        let config = root.join("config");
        let mapping_path = root.join("mapping.json");
        fs::create_dir(&home).unwrap();
        init_git(
            &source,
            &home,
            "Synthetic Committer",
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
        let task_body = |closed| {
            format!(
                "# Synthetic history\n\nCreated: {CREATED}\nUpdated: {TASK_UPDATED}\nClosed: {closed}\nRepeated illustration: {closed}\nCloser: {CLOSER}\n{RESOLUTION}\n"
            )
        };
        let artifact_body = format!(
            "\r\n# Synthetic Artifact\r\n\r\nCreated: {CREATED}\r\nUpdated: {ARTIFACT_UPDATED}\r\nArchived: {ARCHIVED_AT}\r\nEarlier archive example: {TOO_EARLY}\r\n{ARCHIVE_REASON}\r\n"
        );
        let artifact_text = format!(
            "---\r\ndescription: Synthetic Artifact\r\nkind: report\r\n---\r\n{artifact_body}"
        );
        let parents = [
            ("done", DONE, "done", DONE_CLOSED),
            ("cancelled", CANCELLED, "cancelled", CANCELLED_CLOSED),
            ("held", HELD, "done", DONE_CLOSED),
        ];
        for (_, path, status, closed) in parents {
            let path = source.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(
                path,
                format!(
                    "---\ndescription: Synthetic Task\nstatus: {status}\n---\n{}",
                    task_body(closed)
                ),
            )
            .unwrap();
        }
        for path in [
            DONE_ARTIFACT,
            CANCELLED_ARTIFACT,
            HELD_ARTIFACT,
            NO_REVIEW,
            WRONG_RANGE,
            EARLY_ARCHIVE,
            ROOT_ARTIFACT,
        ] {
            let path = source.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, &artifact_text).unwrap();
        }
        git(&source, &home, &["add", "."]);
        git(
            &source,
            &home,
            &["commit", "-qm", "Synthetic terminal Artifact retention"],
        );
        let commit = git_text(&source, &home, &["rev-parse", "HEAD"]);
        let index = fs::read(source.join(".git/index")).unwrap();
        let oid = |path| git_text(&source, &home, &["rev-parse", &format!("{commit}:{path}")]);
        let area = id(AREA_GENERAL);
        let workspace = id(1);
        let key = |work_id: &str| json!({"scope": "_general", "work_id": work_id});
        let task_reviews = parents.map(|(name, path, _, closed)| {
            let body = task_body(closed);
            let mut closure = json!({"closed_at": range(&body, closed), "resolution_note": range(&body, RESOLUTION)});
            if name != "held" { closure["closed_by_ref"] = range(&body, CLOSER); }
            json!({"legacy_key": key(name), "area_id": area, "source_git_blob_sha": oid(path),
                "created_at": range(&body, CREATED), "updated_at": range(&body, TASK_UPDATED),
                "relations":{"parent":null,"prerequisites":[],"children":[]}, "closure": closure})
        });
        let review = |path, id_number, parent_path, work_id, closed| {
            json!({"source_path": path, "source_git_blob_sha": oid(path), "target_id": id(id_number),
                "provenance": {"kind":"work_scoped_artifact", "artifact_kind":"report", "source_ref":SOURCE_REF,
                    "author_ref":"bot:synthetic-author", "created_at":range(&artifact_body, CREATED),
                    "substantive_updated_at":range(&artifact_body, ARTIFACT_UPDATED), "issue_link":"retain", "archive":"none",
                    "parent_task":{"legacy_key":key(work_id), "workspace_id":workspace, "area_id":area,
                        "source_path":parent_path, "source_git_blob_sha":oid(parent_path)},
                    "retention":{"parent_closed_at":range(&task_body(closed), closed)}}})
        };
        let archive = |date| json!({"archived_at": range(&artifact_body, date), "archive_reason": range(&artifact_body, ARCHIVE_REASON)});
        let mut completed = review(DONE_ARTIFACT, 0x901, DONE, "done", DONE_CLOSED);
        completed["provenance"]["archive"] = archive(ARCHIVED_AT);
        completed["provenance"]["area_link"] = json!(area);
        let mut missing = review(NO_REVIEW, 0x904, DONE, "done", DONE_CLOSED);
        missing["provenance"]
            .as_object_mut()
            .unwrap()
            .remove("retention");
        let mut conflicting = review(WRONG_RANGE, 0x905, DONE, "done", DONE_CLOSED);
        conflicting["provenance"]["retention"]["parent_closed_at"] = range_at(
            task_body(DONE_CLOSED).rfind(DONE_CLOSED).unwrap(),
            DONE_CLOSED,
        );
        let mut earlier = review(EARLY_ARCHIVE, 0x906, DONE, "done", DONE_CLOSED);
        earlier["provenance"]["archive"] = archive(TOO_EARLY);
        let mapping = json!({"version":1, "source_commit":commit, "policy_pin":pin, "workspace_id":workspace,
            "scopes":[{"scope":"_general", "decision":{"kind":"area", "area":{"id":area,"slug":"general","name":"Synthetic Area"}}}],
            "task_reviews":task_reviews, "reviewed_records":[completed,
                review(CANCELLED_ARTIFACT, 0x902, CANCELLED, "cancelled", CANCELLED_CLOSED),
                review(HELD_ARTIFACT, 0x903, HELD, "held", DONE_CLOSED), missing, conflicting, earlier,
                {"source_path":ROOT_ARTIFACT, "source_git_blob_sha":oid(ROOT_ARTIFACT), "target_id":id(0x907),
                 "provenance":{"kind":"standalone_artifact", "artifact_kind":"report", "source_ref":SOURCE_REF,
                    "author_ref":"bot:synthetic-author", "links":"none", "archive":"none", "created_at":CREATED,
                    "substantive_updated_at":UPDATED_UTC}}]});
        let plan = |value: &Value, status| {
            fs::write(&mapping_path, serde_json::to_vec(value).unwrap()).unwrap();
            migration_cli(
                &home,
                &config,
                "migration-plan",
                &[
                    "--source",
                    source.to_str().unwrap(),
                    "--source-commit",
                    &commit,
                    "--workspace-id",
                    &workspace,
                    "--mapping-file",
                    mapping_path.to_str().unwrap(),
                    "--include-directives-artifacts",
                    "--tasks",
                ],
                status,
            )
        };
        let report = plan(&mapping, 0);
        assert_eq!(report["data"]["task_intake"]["counts"]["prepared"], 2);
        assert_eq!(
            report["data"]["directive_artifact_intake"]["counts"]["artifacts_work_scoped"],
            json!({"inventoried":6,"prepared":2,"held":4,"excluded":0})
        );
        for (path, closed, updated) in [
            (DONE_ARTIFACT, DONE_CLOSED, DONE_UTC),
            (CANCELLED_ARTIFACT, CANCELLED_CLOSED, UPDATED_UTC),
        ] {
            let row = reviewed_row(&report, path);
            assert_eq!(row["outcome"]["status"], "prepared");
            assert_eq!(
                row["outcome"]["target_body_sha256"],
                sha256_hex(artifact_body.as_bytes())
            );
            assert_eq!(
                row["outcome"]["provenance"]["substantive_updated_at"]["value"],
                ARTIFACT_UPDATED
            );
            assert_eq!(
                row["outcome"]["provenance"]["retention"],
                json!({
                "parent_closed_at":{"value":closed,"body_range":range(&task_body(closed), closed)},
                "effective_substantive_updated_at":updated})
            );
        }
        for (path, reason) in [
            (HELD_ARTIFACT, "parent_task_quarantined"),
            (NO_REVIEW, "unverified_artifact_retention"),
            (WRONG_RANGE, "unverified_artifact_retention"),
            (EARLY_ARCHIVE, "unverified_artifact_retention"),
        ] {
            let outcome = &reviewed_row(&report, path)["outcome"];
            assert_eq!(outcome["reason_codes"], json!([reason]));
            assert!(outcome.get("target_id").is_none());
        }
        assert_redacted(&report.to_string());
        let mut malformed = mapping.clone();
        malformed["reviewed_records"][0]["provenance"]["retention"]["parent_closed_at"] =
            json!([0, 1]);
        assert_eq!(plan(&malformed, 2)["error"]["code"], "INVALID_CONFIG");
        if installed && pin == POLICY_PIN {
            verify_installed(root, &mapping, &artifact_body);
        }
        assert_eq!(git_text(&source, &home, &["rev-parse", "HEAD"]), commit);
        assert_eq!(fs::read(source.join(".git/index")).unwrap(), index);
        assert!(git(&source, &home, &["status", "--porcelain"]).is_empty());
        assert_eq!(
            fs::read(source.join(DONE_ARTIFACT)).unwrap(),
            artifact_text.as_bytes()
        );
        assert!(!config.exists());
    }
}

fn assert_redacted(report: &str) {
    for private in [
        CLOSER,
        RESOLUTION,
        ARCHIVE_REASON,
        SOURCE_REF,
        "bot:synthetic-author",
        "Synthetic Artifact",
    ] {
        assert!(!report.contains(private), "{private}");
    }
}

fn verify_installed(root: &Path, mapping: &Value, body: &str) {
    let home = root.join("home");
    let state = root.join("state");
    let data_home = root.join("data");
    let target = root.join("inactive");
    let mapping_path = root.join("stage-mapping.json");
    fs::write(&mapping_path, serde_json::to_vec(mapping).unwrap()).unwrap();
    let hive = std::env::var_os("HIVE_STAGING_INSTALLED_BIN").expect("isolated installed Hive");
    let dolt = std::env::var_os("HIVE_DOLT_BIN").expect("pinned Dolt");
    let search_path = std::env::var_os("PATH").unwrap();
    let stage = |mode| {
        let output = Command::new(&hive)
            .current_dir(&home)
            .env_clear()
            .env("HOME", &home)
            .env("PATH", &search_path)
            .env("XDG_CONFIG_HOME", root.join("config"))
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
            .arg(root.join("source"))
            .args([
                "--source-commit",
                mapping["source_commit"].as_str().unwrap(),
                "--workspace-id",
                mapping["workspace_id"].as_str().unwrap(),
                "--mapping-file",
            ])
            .arg(&mapping_path)
            .arg("--target")
            .arg(&target)
            .args([
                "--policy-revision",
                "c66b3c50150e18426485d134de92c6b311dac125",
                "--tasks",
                "--mode",
                mode,
            ])
            .output()
            .unwrap();
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(output.status.success(), "{stdout} {:?}", output.stderr);
        assert!(output.stderr.is_empty());
        assert_redacted(&stdout);
        serde_json::from_str::<Value>(&stdout).unwrap()["data"].clone()
    };
    let dry = stage("dry-run");
    assert_eq!(dry["checks"]["transaction"], "rolled_back");
    assert_eq!(dry["counts"]["artifacts_work_scoped"]["imported"], 2);
    assert_eq!(dry["checks"]["target_artifact_rows"], 0);
    assert_eq!(dry["checks"]["target_artifact_links"], 0);
    assert!(!target.exists());
    let staged = stage("staged-import");
    assert_eq!(staged["checks"]["published_inactive"], true);
    assert_eq!(staged["checks"]["committed_record_validation"], "passed");
    assert_eq!(staged["checks"]["target_artifact_rows"], 3);
    assert_eq!(staged["checks"]["target_artifact_links"], 3);
    assert_eq!(
        staged["counts"]["artifacts_work_scoped"],
        json!({"inventoried":6,"imported":2,"held":4,"excluded_non_record":0})
    );
    let held_locator = sha256_hex(HELD_ARTIFACT.as_bytes());
    let held = staged["holds"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["source_path_sha256"] == held_locator)
        .unwrap();
    assert_eq!(
        held["outcome"]["reason_codes"],
        json!(["parent_task_quarantined"])
    );
    let confirmed = staged["target"]["confirmed_dolt_commit"].as_str().unwrap();
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
    let task_rows = query(&format!(
        "SELECT id, status, DATE_FORMAT(closed_at, '%Y-%m-%dT%H:%i:%s.%fZ') AS closed_at FROM issues AS OF '{confirmed}' ORDER BY status"
    ));
    assert_eq!(task_rows[0]["status"], "cancelled");
    assert_eq!(task_rows[0]["closed_at"], CANCELLED_UTC);
    assert_eq!(task_rows[1]["status"], "done");
    assert_eq!(task_rows[1]["closed_at"], DONE_UTC);
    assert_eq!(
        query(&format!(
            "SELECT artifact_id, issue_id FROM artifact_issues AS OF '{confirmed}' ORDER BY artifact_id"
        )),
        json!([{"artifact_id":id(0x901),"issue_id":task_rows[1]["id"]},{"artifact_id":id(0x902),"issue_id":task_rows[0]["id"]}])
    );
    let artifacts = query(&format!("SELECT id, body, SHA2(body, 256) AS digest,
        DATE_FORMAT(substantive_updated_at, '%Y-%m-%dT%H:%i:%s.%fZ') AS updated_at,
        DATE_FORMAT(archived_at, '%Y-%m-%dT%H:%i:%s.%fZ') AS archived_at, archive_reason FROM artifacts AS OF '{confirmed}' ORDER BY id"));
    assert_eq!(
        artifacts,
        json!([
            {"id":id(0x901),"body":body,"digest":sha256_hex(body.as_bytes()),"updated_at":DONE_UTC,
                "archived_at":ARCHIVED_UTC,"archive_reason":ARCHIVE_REASON},
            {"id":id(0x902),"body":body,"digest":sha256_hex(body.as_bytes()),"updated_at":UPDATED_UTC},
            {"id":id(0x907),"body":body,"digest":sha256_hex(body.as_bytes()),"updated_at":UPDATED_UTC},
        ])
    );
    assert!(!state.exists());
    assert!(!data_home.exists());
}
