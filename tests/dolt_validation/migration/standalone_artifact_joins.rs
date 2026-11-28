use std::path::Path;
use std::process::Command;

use super::{
    POLICY_PIN, Value, fs, git, git_text, id, init_git, json, migration_cli, reviewed_row,
    sha256_hex,
};

const MULTI: &str = "artifacts/multi.md";
const ZERO: &str = "artifacts/zero.md";
const HELD: &str = "artifacts/held.md";
const STALE: &str = "artifacts/stale.md";
const ALPHA: &str = "tasks/app/backlog/alpha/README.md";
const BETA: &str = "tasks/_general/backlog/beta/README.md";
const README: &str = "tasks/app/README.md";
const URL: &str = "https://forge.example/synthetic/application";
const REF: &str = "https://forge.example/synthetic/multi-artifact";
const CREATED: &str = "2024-01-01T10:00:00Z";
const UPDATED: &str = "2024-01-03T11:00:00.000001Z";
const BODY: &str = "\r\n# Synthetic café evidence\r\n\r\nPrivate fixture body.  \r\n";

#[test]
fn standalone_artifact_plan_holds_entire_incomplete_link_set() {
    exercise(false);
}

#[test]
#[ignore = "requires installed Hive and pinned Git and Dolt"]
fn installed_cli_stages_standalone_artifact_complete_sets_and_rolls_back() {
    exercise(true);
}

fn exercise(installed: bool) {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path();
    let source = root.join("source");
    let home = root.join("home");
    let config = root.join("config");
    let mapping_file = root.join("mapping.json");
    fs::create_dir(&home).unwrap();
    init_git(
        &source,
        &home,
        "Synthetic Committer",
        "fixture@example.invalid",
    );
    let task_body = format!("# Synthetic task\n\nCreated: {CREATED}\nUpdated: {UPDATED}\n");
    let task_text = format!("---\ndescription: Synthetic task\n---\n{task_body}");
    let artifact =
        format!("---\r\ndescription: Synthetic evidence\r\nkind: report\r\n---\r\n{BODY}");
    let files = [
        ("AGENTS.md", format!("hive-system@{POLICY_PIN}\n")),
        (
            "hive.yml",
            "version: 1\ncoordination:\n  mode: local\n".into(),
        ),
        (README, format!("# Synthetic scope\n\n{URL}\n")),
        (ALPHA, task_text.clone()),
        (BETA, task_text),
        (MULTI, artifact.clone()),
        (ZERO, artifact.clone()),
        (HELD, artifact.clone()),
        (STALE, artifact.clone()),
    ];
    for (path, text) in &files {
        let path = source.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
    git(&source, &home, &["add", "."]);
    git(
        &source,
        &home,
        &["commit", "-qm", "Synthetic standalone Artifact joins"],
    );
    let commit = git_text(&source, &home, &["rev-parse", "HEAD"]);
    let index = fs::read(source.join(".git/index")).unwrap();
    let oid = |path| git_text(&source, &home, &["rev-parse", &format!("{commit}:{path}")]);
    let range = |value: &str| {
        let start = task_body.find(value).unwrap();
        json!({"start": start, "end": start + value.len()})
    };
    let area_a = id(0x810);
    let area_b = id(0x811);
    let project = id(0x812);
    let resource = id(0x813);
    let held_resource = id(0x814);
    let workspace = id(1);
    let task_review = |scope, work, path, area| {
        json!({"legacy_key": {"scope": scope,"work_id": work},
        "source_git_blob_sha": oid(path), "area_id": area, "relations": "no_edges",
        "created_at": range(CREATED), "updated_at": range(UPDATED)})
    };
    let issue = |scope, work, path, area| {
        json!({"legacy_key": {"scope": scope,"work_id": work},
        "source_path": path, "source_git_blob_sha": oid(path), "area_id": area})
    };
    let links = json!({"area_ids": [area_b, area_a], "project_ids": [project],
        "resource_ids": [resource], "issues": [issue("app", "alpha", ALPHA, &area_a), issue("_general", "beta", BETA, &area_b)]});
    let mut held = links.clone();
    held["resource_ids"]
        .as_array_mut()
        .unwrap()
        .push(json!(held_resource));
    let mut stale = links.clone();
    stale["issues"][0]["source_path"] = json!(BETA);
    let review = |path, target_id, links| {
        json!({"source_path": path, "source_git_blob_sha": oid(path), "target_id": id(target_id),
        "provenance": {"kind": "standalone_artifact", "artifact_kind": "report", "author_ref": "bot:synthetic-author",
            "source_ref": REF, "created_at": CREATED, "substantive_updated_at": UPDATED, "archive": "none", "links": links}})
    };
    let mapping = json!({
        "version": 1, "source_commit": commit, "workspace_id": workspace, "policy_pin": POLICY_PIN,
        "scopes": [
            {"scope": "app", "decision": {"kind": "area", "area": {"id": area_a,"slug": "app","name":"Synthetic App"},
                "project_evidence": [{"path": README,"canonical_ref": URL}]}},
            {"scope": "_general", "decision": {"kind": "area", "area": {"id": area_b,"slug": "general","name":"Synthetic General"}}}
        ],
        "task_reviews": [task_review("app", "alpha", ALPHA, &area_a), task_review("_general", "beta", BETA, &area_b)],
        "resource_bindings": {"version":1, "bindings": [
            {"scope":"app", "area_id":area_a, "resource":{"id":resource,"kind":"git-repo","name":"Synthetic Repo"},
                "evidence_path":README,"evidence_blob_oid":oid(README)},
            {"scope":"app", "area_id":area_a, "resource":{"id":held_resource,"kind":"git-repo","name":"Stale Repo"},
                "evidence_path":README,"evidence_blob_oid":"0".repeat(40)}
        ]},
        "project_membership": {"projects": [{"id":project, "name":"Synthetic Delivery", "status":"active"}], "memberships":[]},
        "reviewed_records": [review(MULTI, 0x901, links), review(ZERO, 0x902, json!("none")),
            review(HELD, 0x903, held), review(STALE, 0x904, stale)]
    });
    let plan = |value: &Value, status| {
        fs::write(&mapping_file, serde_json::to_vec(value).unwrap()).unwrap();
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
                mapping_file.to_str().unwrap(),
                "--include-directives-artifacts",
                "--tasks",
            ],
            status,
        )
    };
    let report = plan(&mapping, 0);
    assert_eq!(
        report["data"]["directive_artifact_intake"]["counts"]["artifacts_standalone"],
        json!({"inventoried":4,"prepared":2,"held":2,"excluded":0})
    );
    let row = reviewed_row(&report, MULTI);
    assert_eq!(
        row["outcome"]["target_body_sha256"],
        sha256_hex(BODY.as_bytes())
    );
    assert_eq!(
        row["outcome"]["provenance"]["links"]["area_ids"],
        json!([area_a, area_b])
    );
    assert_eq!(
        row["outcome"]["provenance"]["links"]["project_ids"],
        json!([project])
    );
    assert_eq!(
        row["outcome"]["provenance"]["links"]["resource_ids"],
        json!([resource])
    );
    assert_eq!(
        row["outcome"]["provenance"]["links"]["issues"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert!(
        reviewed_row(&report, ZERO)["outcome"]["provenance"]
            .get("links")
            .is_none()
    );
    let check_hold = |report: &Value, path: &str| {
        let state = &reviewed_row(report, path)["outcome"];
        assert_eq!(state["reason_codes"], json!(["unresolved_artifact_links"]));
        assert!(state.get("target_id").is_none());
    };
    check_hold(&report, HELD);
    check_hold(&report, STALE);
    for link_set in [
        json!({}),
        json!({"area_ids":[area_a,area_a]}),
        json!({"area_ids":[area_a],"project_ids":[id(0xbad)]}),
        json!({"area_ids":[id(0xbad)]}),
    ] {
        let mut broken = mapping.clone();
        broken["reviewed_records"][0]["provenance"]["links"] = link_set;
        check_hold(&plan(&broken, 0), MULTI);
    }
    for link_set in [
        json!(null),
        json!({"none":null}),
        json!([]),
        json!({"issues":[[]]}),
    ] {
        let mut malformed = mapping.clone();
        malformed["reviewed_records"][0]["provenance"]["links"] = link_set;
        assert_eq!(plan(&malformed, 2)["error"]["code"], "INVALID_CONFIG");
    }
    if installed {
        verify_installed(root, &mapping);
    }
    assert_eq!(git_text(&source, &home, &["rev-parse", "HEAD"]), commit);
    assert_eq!(fs::read(source.join(".git/index")).unwrap(), index);
    assert!(git(&source, &home, &["status", "--porcelain"]).is_empty());
    assert!(!config.exists());
    for (path, original) in files {
        assert_eq!(fs::read(source.join(path)).unwrap(), original.as_bytes());
    }
}

fn verify_installed(root: &Path, mapping: &Value) {
    let home = root.join("home");
    let source = root.join("source");
    let config = root.join("config");
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
        for private in [
            URL,
            REF,
            "Synthetic café evidence",
            "bot:synthetic-author",
            ALPHA,
            MULTI,
        ] {
            assert!(!stdout.contains(private), "{private}");
        }
        serde_json::from_str::<Value>(&stdout).unwrap()["data"].clone()
    };
    let dry = stage("dry-run");
    assert_eq!(dry["checks"]["transaction"], "rolled_back");
    assert_eq!(dry["checks"]["target_artifact_rows"], 0);
    assert_eq!(dry["checks"]["target_artifact_links"], 0);
    assert_eq!(dry["counts"]["artifacts_standalone"]["imported"], 2);
    assert!(!target.exists());
    let staged = stage("staged-import");
    assert_eq!(staged["checks"]["published_inactive"], true);
    assert_eq!(staged["checks"]["committed_record_validation"], "passed");
    assert_eq!(staged["checks"]["target_artifact_rows"], 2);
    assert_eq!(staged["checks"]["target_artifact_links"], 6);
    assert_eq!(
        staged["counts"]["artifacts_standalone"],
        json!({"inventoried":4,"imported":2,"held":2,"excluded_non_record":0})
    );
    let entries = staged["id_map"].as_array().unwrap();
    let by_path = |path: &str| {
        entries
            .iter()
            .find(|row| row["source_path_sha256"] == sha256_hex(path.as_bytes()))
            .unwrap()
    };
    let mut issue_ids = [
        by_path(ALPHA)["issue_id"].as_str().unwrap(),
        by_path(BETA)["issue_id"].as_str().unwrap(),
    ];
    issue_ids.sort_unstable();
    assert_eq!(
        by_path(MULTI)["outcome"]["provenance"]["links"]["issue_ids"],
        json!(issue_ids)
    );
    assert_eq!(
        by_path(MULTI)["outcome"]["target_body_sha256"],
        sha256_hex(BODY.as_bytes())
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
    assert_eq!(query(&format!("SELECT id, body, SHA2(body, 256) AS digest FROM artifacts AS OF '{confirmed}' ORDER BY id")),
        json!([0x901, 0x902].map(|id_number| json!({"id": id(id_number), "body": BODY, "digest": sha256_hex(BODY.as_bytes())}))));
    for (table, column, ids) in [
        ("artifact_areas", "area_id", vec![id(0x810), id(0x811)]),
        ("artifact_projects", "project_id", vec![id(0x812)]),
        ("artifact_resources", "resource_id", vec![id(0x813)]),
        (
            "artifact_issues",
            "issue_id",
            issue_ids.map(str::to_owned).to_vec(),
        ),
    ] {
        assert_eq!(
            query(&format!(
                "SELECT artifact_id, {column} FROM {table} AS OF '{confirmed}' ORDER BY artifact_id, {column}"
            )),
            json!(
                ids.iter()
                    .map(|id_value| json!({"artifact_id": id(0x901), (column): id_value}))
                    .collect::<Vec<_>>()
            )
        );
    }
    assert!(!state.exists());
    assert!(!data_home.exists());
}
