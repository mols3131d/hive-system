use std::path::{Path, PathBuf};

use super::{
    AREA_GENERAL, Command, POLICY_PIN, Value, fs, git, git_text, id, init_git, json, sha256_hex,
};

const APPROVAL: &str = "https://forge.example/synthetic/approvals/7";
const REFERENCE: &str = "https://forge.example/synthetic/provenance/7";
const BODY: &str = "\r\n# Synthetic **title**\r\n\r\nCafé's synthetic body.  \r\n";
const CREATED: &str = "2024-02-01T12:00:00Z";
const UPDATED: &str = "2024-02-02T12:00:00.123456Z";

#[test]
#[ignore = "requires installed Dolt 2.7.6 and native Git"]
fn stages_reviewed_directives_and_standalone_artifacts_in_an_inactive_copy() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    let home = root.path().join("home");
    let config = root.path().join("config");
    let state = root.path().join("state");
    let target = root.path().join("inactive");
    fs::create_dir(&home).unwrap();
    let dolt = PathBuf::from(std::env::var_os("HIVE_DOLT_BIN").expect("pinned Dolt"));
    let search_path = std::env::join_paths(
        std::iter::once(dolt.parent().unwrap().to_owned())
            .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    let hive =
        std::env::var_os("HIVE_STAGE_BIN").unwrap_or_else(|| env!("CARGO_BIN_EXE_hive").into());
    init_git(
        &source,
        &home,
        "Synthetic Git Committer",
        "fixture@example.invalid",
    );
    fs::write(
        source.join("AGENTS.md"),
        format!("hive-system@{POLICY_PIN}\n"),
    )
    .unwrap();
    fs::write(
        source.join("hive.yml"),
        "version: 1\ncoordination:\n  mode: local\n",
    )
    .unwrap();
    let entries = [
        ("directives/_general/older.md", "status: superseded"),
        ("directives/_general/active.md", "status: active"),
        ("directives/_general/proposal.md", "status: proposed"),
        ("directives/_general/withdrawn.md", "status: withdrawn"),
        ("directives/_general/unapproved.md", "status: active"),
        ("artifacts/report.md", "kind: report\r\nstatus: closed"),
        ("artifacts/unclassified.md", "status: parked"),
        (
            "tasks/_general/backlog/work/artifacts/report.md",
            "kind: report",
        ),
    ];
    for (locator, fields) in entries {
        let path = source.join(locator);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            path,
            format!("---\r\ndescription: Synthetic fixture\r\n{fields}\r\n---\r\n{BODY}"),
        )
        .unwrap();
    }
    git(&source, &home, &["add", "."]);
    git(
        &source,
        &home,
        &["commit", "-qm", "Synthetic Directive and Artifact staging"],
    );
    let commit = git_text(&source, &home, &["rev-parse", "HEAD"]);
    let index = fs::read(source.join(".git/index")).unwrap();
    let workspace = id(1);
    let area = id(AREA_GENERAL);
    let directive = |status, approval, supersession| {
        json!({
            "kind": "directive_history", "status": status, "area_id": area, "issuer_ref": "human:synthetic-issuer",
            "source_ref": REFERENCE, "created_at": CREATED, "updated_at": UPDATED,
            "approval_and_validity": approval, "supersession": supersession,
        })
    };
    let artifact = json!({"kind": "standalone_artifact", "artifact_kind": "report", "author_ref": "bot:synthetic-author",
        "source_ref": REFERENCE, "created_at": CREATED, "substantive_updated_at": UPDATED,
        "links": "none", "archive": "none"});
    let none = || json!({"state": "reviewed"});
    let reviews = [
        directive(
            "superseded",
            none(),
            json!({"state": "reviewed", "replacement": "directives/_general/active"}),
        ),
        directive(
            "active",
            json!({"state": "reviewed", "approval_ref": APPROVAL,
            "valid_from": UPDATED, "expires_at": "2024-02-04T12:00:00Z"}),
            none(),
        ),
        json!({"kind": "proposed_directive", "area_id": area, "issuer_ref": "human:synthetic-issuer",
            "source_ref": REFERENCE, "created_at": CREATED, "updated_at": UPDATED,
            "supersession": "none", "approval_and_validity": "none"}),
        directive(
            "withdrawn",
            json!({"state": "reviewed", "approval_ref": APPROVAL, "expires_at": UPDATED}),
            none(),
        ),
        directive("active", none(), none()),
        artifact.clone(),
        artifact.clone(),
        artifact,
    ];
    let reviewed_records = entries.into_iter().zip(reviews).enumerate().map(|(i, ((path, _), provenance))| {
        let blob = git_text(&source, &home, &["rev-parse", &format!("{commit}:{path}")]);
        json!({"source_path": path, "source_git_blob_sha": blob, "target_id": id(0x700 + i as u32), "provenance": provenance})
    }).collect::<Vec<_>>();
    let mapping = json!({"version": 1, "source_commit": commit, "workspace_id": workspace, "policy_pin": POLICY_PIN,
        "directive_observed_at": "2024-02-03T12:00:00Z", "reviewed_records": reviewed_records,
        "scopes": [{"scope": "_general", "decision": {"kind": "area", "area": {"id": area, "slug": "general", "name": "Synthetic Area"}}}],
    });
    let mapping_path = root.path().join("mapping.json");
    let stage = |mode, destination: &Path, mapping: &Value, status| {
        fs::write(&mapping_path, serde_json::to_vec(mapping).unwrap()).unwrap();
        let result = Command::new(&hive)
            .current_dir(&home)
            .env_clear()
            .env("PATH", &search_path)
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", &config)
            .env("XDG_STATE_HOME", &state)
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
                &commit,
                "--workspace-id",
                &workspace,
                "--mapping-file",
            ])
            .arg(&mapping_path)
            .arg("--target")
            .arg(destination)
            .args([
                "--policy-revision",
                "c66b3c50150e18426485d134de92c6b311dac125",
                "--mode",
                mode,
            ])
            .output()
            .unwrap();
        let stdout = String::from_utf8(result.stdout).unwrap();
        assert_eq!(
            result.status.code(),
            Some(status),
            "{stdout} {:?}",
            result.stderr
        );
        assert!(result.stderr.is_empty());
        for private in [
            REFERENCE,
            APPROVAL,
            "Café",
            "Synthetic title",
            "human:synthetic-issuer",
            "bot:synthetic-author",
            "directives/_general/active.md",
            source.to_str().unwrap(),
        ] {
            assert!(!stdout.contains(private), "{private}");
        }
        serde_json::from_str::<Value>(&stdout).unwrap()
    };

    let dry = stage("dry-run", &target, &mapping, 0);
    assert_eq!(dry["data"]["checks"]["transaction"], "rolled_back");
    assert_eq!(dry["data"]["checks"]["target_directive_rows"], 0);
    assert_eq!(dry["data"]["checks"]["target_artifact_rows"], 0);
    assert!(dry["data"]["target"]["confirmed_dolt_commit"].is_null());
    assert!(!target.exists());
    let mut bad = mapping.clone();
    bad["reviewed_records"][0]["source_git_blob_sha"] = json!("0".repeat(commit.len()));
    let rejected = stage("staged-import", &target, &bad, 2);
    assert_eq!(rejected["error"]["code"], "INVALID_CONFIG");
    assert!(!target.exists());

    let staged = stage("staged-import", &target, &mapping, 0);
    let data = &staged["data"];
    assert_eq!(data["checks"]["published_inactive"], true);
    assert_eq!(data["checks"]["committed_record_validation"], "passed");
    assert_eq!(data["checks"]["target_directive_rows"], 4);
    assert_eq!(data["checks"]["target_artifact_rows"], 2);
    assert_eq!(data["checks"]["target_artifact_links"], 0);
    assert_eq!(
        data["counts"]["directives"],
        json!({"inventoried": 5, "imported": 4, "held": 1, "excluded_non_record": 0})
    );
    assert_eq!(data["counts"]["artifacts_standalone"]["imported"], 2);
    assert_eq!(data["counts"]["artifacts_standalone"]["held"], 0);
    assert_eq!(data["counts"]["artifacts_work_scoped"]["held"], 1);
    for (entry, _) in entries
        .into_iter()
        .take(4)
        .chain(entries.into_iter().skip(5).take(2))
    {
        let path_hash = sha256_hex(entry.as_bytes());
        let planned = data["id_map"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["source_path_sha256"] == path_hash)
            .unwrap();
        assert_eq!(
            planned["outcome"]["target_body_sha256"],
            sha256_hex(BODY.as_bytes())
        );
        assert_eq!(
            planned["source_raw_sha256"],
            sha256_hex(&fs::read(source.join(entry)).unwrap())
        );
    }

    let confirmed = data["target"]["confirmed_dolt_commit"].as_str().unwrap();
    let query = |query: &str| {
        let output = Command::new(&dolt)
            .current_dir(target.join("db"))
            .env_clear()
            .env("HOME", &home)
            .env("DOLT_DISABLE_EVENT_FLUSH", "true")
            .args(["sql", "-r", "json", "-q", query])
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        serde_json::from_slice::<Value>(&output.stdout).unwrap()["rows"].clone()
    };
    let rows = query(&format!(
        "SELECT id, status, area_id, supersedes_id, approval_ref, title, body FROM directives AS OF '{confirmed}' ORDER BY id"
    ));
    assert_eq!(rows[0]["status"], "superseded");
    assert_eq!(rows[1]["supersedes_id"], id(0x700));
    assert_eq!(rows[1]["status"], "active");
    assert_eq!(rows[1]["area_id"], area);
    assert_eq!(rows[1]["approval_ref"], APPROVAL);
    assert_eq!(rows[1]["title"], "Synthetic title");
    assert_eq!(rows[1]["body"], BODY);
    assert_eq!(rows[2]["status"], "proposed");
    assert_eq!(rows[3]["status"], "withdrawn");
    let artifact = query(&format!("SELECT id, kind, body, source_ref, author_ref,
        DATE_FORMAT(substantive_updated_at, '%Y-%m-%dT%H:%i:%s.%fZ') AS retention FROM artifacts AS OF '{confirmed}' ORDER BY id"));
    assert_eq!(
        artifact,
        json!([0x705, 0x706].map(|number| json!({
            "id": id(number), "kind": "report", "body": BODY,
            "source_ref": REFERENCE, "author_ref": "bot:synthetic-author", "retention": UPDATED
        })))
    );
    stage("staged-import", &target, &mapping, 5);
    assert!(
        target
            .join("conversations")
            .read_dir()
            .unwrap()
            .next()
            .is_none()
    );
    assert!(!config.exists());
    assert!(!state.exists());
    assert_eq!(fs::read(source.join(".git/index")).unwrap(), index);
    assert_eq!(git_text(&source, &home, &["rev-parse", "HEAD"]), commit);
    assert!(git(&source, &home, &["status", "--porcelain"]).is_empty());
}
