use std::path::{Path, PathBuf};

use super::{
    AREA_GENERAL, Command, POLICY_PIN, Value, fs, git, git_text, id, init_git, json, migration_cli,
    sha256_hex,
};

const OLDER_PIN: &str = "0fd0f8dbe632d14094a93599cb1b5f653789ac9c";
const BODY: &str = "\r\nExact synthetic body café.  \r\n";
const CONFLICT: &str = "# First synthetic H1\r\n\r\n# Second synthetic H1\r\n";
const SOURCE_TITLE: &str = "Original synthetic title";
const FALLBACK: &str = "Reviewed synthetic fallback café";
const OVERRIDE: &str = "  Reviewed synthetic override λ  ";

struct Record<'a> {
    path: &'a str,
    expected: Result<&'a str, &'a str>,
    body: &'a str,
}

fn fixture(root: &Path, pin: &str) -> (String, Value, Vec<Record<'static>>) {
    let home = root.join("home");
    let source = root.join("source");
    fs::create_dir(&home).unwrap();
    init_git(
        &source,
        &home,
        "Synthetic reviewer",
        "reviewer@example.invalid",
    );
    fs::write(source.join("AGENTS.md"), format!("hive-system@{pin}\n")).unwrap();
    if pin == POLICY_PIN {
        fs::write(
            source.join("hive.yml"),
            "version: 1\ncoordination:\n  mode: local\n",
        )
        .unwrap();
    }
    let long = "x".repeat(256);
    let proposed = json!({
        "kind":"proposed_directive", "area_id":id(AREA_GENERAL), "issuer_ref":"human:synthetic-issuer",
        "source_ref":"https://forge.example/synthetic/directive",
        "created_at":"2024-02-01T12:00:00Z", "updated_at":"2024-02-02T12:00:00Z",
        "supersession":"none", "approval_and_validity":"none",
    });
    let artifact = json!({
        "kind":"standalone_artifact", "artifact_kind":"report", "author_ref":"bot:synthetic-author",
        "source_ref":"https://forge.example/synthetic/artifact",
        "created_at":"2024-02-01T12:00:00Z", "substantive_updated_at":"2024-02-02T12:00:00Z",
        "links":"none", "archive":"none",
    });
    let missing_title = Err("missing_or_conflicting_title");
    let fallback = Some(("fallback", FALLBACK));
    let replacement = Some(("override", OVERRIDE));
    let records = [
        (
            "directives/_general/fallback.md",
            "status: proposed",
            long.as_str(),
            BODY,
            fallback,
            Ok(FALLBACK),
        ),
        (
            "directives/_general/unsuitable.md",
            "status: proposed\r\ntitle: ' '",
            long.as_str(),
            BODY,
            fallback,
            missing_title,
        ),
        (
            "directives/_general/unsafe.md",
            "status: proposed\r\ntitle: null",
            "Synthetic description",
            BODY,
            replacement,
            Err("unsupported_directive_metadata"),
        ),
        (
            "directives/_general/invalid-status.md",
            "status: unknown",
            "Synthetic description",
            BODY,
            replacement,
            Err("invalid_directive_status"),
        ),
        (
            "directives/_general/bad-date.md",
            "status: proposed",
            "Synthetic description",
            BODY,
            replacement,
            Err("incomplete_historical_provenance"),
        ),
        (
            "artifacts/override.md",
            "kind: report\r\ntitle: Original synthetic title",
            "Synthetic description",
            BODY,
            replacement,
            Ok(OVERRIDE),
        ),
        (
            "artifacts/source-default.md",
            "kind: report\r\ntitle: Original synthetic title",
            "Synthetic description",
            BODY,
            None,
            Ok(SOURCE_TITLE),
        ),
        (
            "artifacts/fallback-conflict.md",
            "kind: report",
            SOURCE_TITLE,
            CONFLICT,
            fallback,
            missing_title,
        ),
        (
            "artifacts/override-conflict.md",
            "kind: report",
            SOURCE_TITLE,
            CONFLICT,
            replacement,
            Ok(OVERRIDE),
        ),
        (
            "artifacts/fallback-conflicts-with-source.md",
            "kind: report",
            SOURCE_TITLE,
            BODY,
            fallback,
            missing_title,
        ),
        (
            "artifacts/invalid-override.md",
            "kind: report",
            SOURCE_TITLE,
            BODY,
            Some(("override", "Invalid\u{c}title")),
            missing_title,
        ),
        (
            "artifacts/bad-kind.md",
            "kind: invalid",
            SOURCE_TITLE,
            BODY,
            replacement,
            Err("unclassified_artifact_kind"),
        ),
        (
            "artifacts/bad-author.md",
            "kind: report",
            SOURCE_TITLE,
            BODY,
            replacement,
            Err("unverified_artifact_author"),
        ),
        (
            "artifacts/unresolved-links.md",
            "kind: report",
            SOURCE_TITLE,
            BODY,
            replacement,
            Err("unresolved_artifact_links"),
        ),
        (
            "artifacts/override-unsuitable.md",
            "kind: report\r\ntitle: ' '",
            long.as_str(),
            BODY,
            replacement,
            Ok(OVERRIDE),
        ),
        (
            "artifacts/blank-description.md",
            "kind: report",
            "''",
            BODY,
            replacement,
            Err("unsupported_artifact_metadata"),
        ),
        (
            "directives/_general/bad-area.md",
            "status: proposed",
            SOURCE_TITLE,
            BODY,
            replacement,
            Err("ambiguous_scope_or_collision"),
        ),
    ];
    let mut reviews = Vec::new();
    let mut expected = Vec::new();
    for (index, (path, fields, description, body, title, outcome)) in
        records.into_iter().enumerate()
    {
        let file = source.join(path);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(
            &file,
            format!("---\r\ndescription: {description}\r\n{fields}\r\n---\r\n{body}"),
        )
        .unwrap();
        let mut provenance = if path.starts_with("directives/") {
            proposed.clone()
        } else {
            artifact.clone()
        };
        match path {
            "directives/_general/bad-date.md" => {
                provenance["updated_at"] = json!("2024-02-02X12:00:00Z")
            }
            "artifacts/bad-author.md" => provenance["author_ref"] = json!("invalid actor"),
            "artifacts/unresolved-links.md" => provenance["links"] = json!("unresolved"),
            "directives/_general/bad-area.md" => provenance["area_id"] = json!(id(0x777)),
            _ => {}
        }
        let target_id = id(0x601 + index as u32);
        let mut review = json!({
            "source_path":path, "source_git_blob_sha":git_text(&source, &home, &["hash-object", path]),
            "target_id":target_id, "provenance":provenance,
        });
        if let Some((mode, value)) = title {
            review["title"] = json!({"mode":mode, "value":value});
        }
        reviews.push(review);
        expected.push(Record {
            path,
            expected: outcome,
            body,
        });
    }
    git(&source, &home, &["add", "."]);
    git(
        &source,
        &home,
        &["commit", "-qm", "Synthetic reviewed record titles"],
    );
    let commit = git_text(&source, &home, &["rev-parse", "HEAD"]);
    let mapping = json!({
        "version":1, "source_commit":commit, "policy_pin":pin, "workspace_id":id(1),
        "scopes":[{"scope":"_general","decision":{"kind":"area","area":{"id":id(AREA_GENERAL),"slug":"general","name":"General"}}}],
        "reviewed_records":reviews,
    });
    (commit, mapping, expected)
}

fn check_outcome(row: &Value, record: &Record<'_>) {
    let outcome = &row["outcome"];
    match record.expected {
        Ok(title) => {
            assert_eq!(outcome["status"], "prepared", "{}: {outcome}", record.path);
            assert_eq!(
                outcome["target_title_sha256"],
                sha256_hex(title.as_bytes()),
                "{}",
                record.path
            );
            assert_eq!(
                outcome["target_body_sha256"],
                sha256_hex(record.body.as_bytes())
            );
        }
        Err(reason) => {
            assert_eq!(outcome["status"], "held", "{}", record.path);
            assert_eq!(outcome["reason_codes"], json!([reason]), "{}", record.path);
        }
    }
}

fn assert_redacted(report: &str) {
    for private in [
        "Reviewed synthetic",
        SOURCE_TITLE,
        "synthetic body",
        "synthetic H1",
    ] {
        assert!(!report.contains(private), "report exposed fixture content");
    }
}

#[test]
fn cli_reviewed_record_titles_require_exact_binding_and_preserve_independent_holds() {
    for pin in [POLICY_PIN, OLDER_PIN] {
        let root = tempfile::tempdir().unwrap();
        let (commit, mapping, expected) = fixture(root.path(), pin);
        let manifest = root.path().join("mapping.json");
        let source = root.path().join("source");
        let workspace = id(1);
        let plan = |bytes: &str, status| {
            fs::write(&manifest, bytes).unwrap();
            let report = migration_cli(
                &root.path().join("home"),
                &root.path().join("config"),
                "migration-plan",
                &[
                    "--source",
                    source.to_str().unwrap(),
                    "--source-commit",
                    &commit,
                    "--workspace-id",
                    &workspace,
                    "--mapping-file",
                    manifest.to_str().unwrap(),
                    "--include-directives-artifacts",
                ],
                status,
            );
            assert_redacted(&report.to_string());
            report
        };
        let report = plan(&mapping.to_string(), 0);
        let rows = report["data"]["directive_artifact_intake"]["records"]
            .as_array()
            .unwrap();
        for record in &expected {
            check_outcome(
                rows.iter()
                    .find(|row| row["source_path"] == record.path)
                    .unwrap(),
                record,
            );
        }
        assert_eq!(
            mapping["reviewed_records"][5]["source_git_blob_sha"],
            mapping["reviewed_records"][6]["source_git_blob_sha"]
        );
        for variant in [
            Value::Null,
            json!(["fallback", FALLBACK]),
            json!({"mode":"fallback","value":7}),
            json!({"mode":"fallback"}),
            json!({"mode":"override","value":FALLBACK,"extra":true}),
            json!({"mode":"other","value":FALLBACK}),
        ] {
            let mut invalid = mapping.clone();
            invalid["reviewed_records"][0]["title"] = variant;
            assert_eq!(
                plan(&invalid.to_string(), 2)["error"]["code"],
                "INVALID_CONFIG"
            );
        }
        let duplicate = mapping.to_string().replacen(
            r#""mode":"fallback""#,
            r#""mode":"override","mode":"fallback""#,
            1,
        );
        assert_eq!(plan(&duplicate, 2)["error"]["code"], "INVALID_CONFIG");
        for (field, value) in [
            ("source_path", json!("directives/_general/missing.md")),
            ("source_git_blob_sha", json!("0".repeat(commit.len()))),
        ] {
            let mut invalid = mapping.clone();
            invalid["reviewed_records"][0][field] = value;
            assert_eq!(
                plan(&invalid.to_string(), 2)["error"]["code"],
                "INVALID_CONFIG"
            );
        }
    }
}

#[test]
#[ignore = "requires installed Hive and pinned Dolt and Git"]
fn stages_reviewed_record_titles_with_exact_committed_bytes_and_digests() {
    let dolt = PathBuf::from(std::env::var_os("HIVE_DOLT_BIN").unwrap());
    let hive = std::env::var_os("HIVE_STAGE_BIN").unwrap();
    let path = std::env::var_os("PATH").unwrap();
    for pin in [POLICY_PIN, OLDER_PIN] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let (commit, mapping, expected) = fixture(root, pin);
        let manifest = root.join("mapping.json");
        let source = root.join("source");
        let home = root.join("home");
        let target = root.join("inactive");
        let index = fs::read(source.join(".git/index")).unwrap();
        fs::write(&manifest, mapping.to_string()).unwrap();
        let result = Command::new(&hive)
            .current_dir(&home)
            .env_clear()
            .env("HOME", &home)
            .env("PATH", &path)
            .env("XDG_CONFIG_HOME", root.join("config"))
            .env("XDG_STATE_HOME", root.join("state"))
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
                &id(1),
                "--mapping-file",
            ])
            .arg(&manifest)
            .args([
                "--policy-revision",
                "c66b3c50150e18426485d134de92c6b311dac125",
                "--mode",
                "staged-import",
                "--target",
            ])
            .arg(&target)
            .output()
            .unwrap();
        let stdout = String::from_utf8(result.stdout).unwrap();
        assert_redacted(&stdout);
        assert!(result.status.success(), "{pin}: {stdout}");
        assert!(result.stderr.is_empty());
        let report: Value = serde_json::from_str(&stdout).unwrap();
        let data = &report["data"];
        assert_eq!(data["checks"]["committed_record_validation"], "passed");
        let confirmed = data["target"]["confirmed_dolt_commit"].as_str().unwrap();
        for table in ["directives", "artifacts"] {
            let output = Command::new(&dolt).current_dir(target.join("db"))
                .env_clear().env("HOME", &home).env("PATH", &path).env("DOLT_DISABLE_VERSION_CHECKS","true")
                .args(["sql","-r","json","-q",&format!("SELECT id, title, body, LOWER(SHA2(CAST(title AS BINARY), 256)) AS title_sha256 FROM {table} AS OF '{confirmed}' ORDER BY id")])
                .output().unwrap();
            assert!(output.status.success());
            let actual: Value = serde_json::from_slice(&output.stdout).unwrap();
            let rows = actual["rows"].as_array().unwrap();
            let mut count = 0;
            for (index, record) in expected
                .iter()
                .enumerate()
                .filter(|(_, record)| record.path.starts_with(table))
            {
                let section = if record.expected.is_ok() {
                    "id_map"
                } else {
                    "holds"
                };
                let digest = sha256_hex(record.path.as_bytes());
                let reported = data[section]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|row| row["source_path_sha256"] == digest)
                    .unwrap();
                check_outcome(reported, record);
                if let Ok(title) = record.expected {
                    let target_id = &mapping["reviewed_records"][index]["target_id"];
                    let row = rows.iter().find(|row| row["id"] == *target_id).unwrap();
                    assert_eq!(row["title"], title);
                    assert_eq!(row["body"], record.body);
                    assert_eq!(
                        row["title_sha256"],
                        reported["outcome"]["target_title_sha256"]
                    );
                    count += 1;
                }
            }
            assert_eq!(rows.len(), count);
        }
        assert_eq!(fs::read(source.join(".git/index")).unwrap(), index);
        assert_eq!(git_text(&source, &home, &["rev-parse", "HEAD"]), commit);
    }
}
