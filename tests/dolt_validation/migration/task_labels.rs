use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::fs::PermissionsExt;

use super::{
    AREA_GENERAL, Command, POLICY_PIN, Value, fs, git, git_text, id, init_git, json, sha256_hex,
};

#[test]
#[ignore = "requires installed Hive and pinned Dolt and Git"]
fn installed_task_labels_preserve_distinct_spellings_in_the_initialized_target() {
    assert_staged_labels(false);
}

#[test]
#[ignore = "requires installed Hive and pinned Dolt and Git"]
fn installed_task_label_collisions_hold_graph_dependents_before_issue_allocation() {
    assert_staged_labels(true);
}

fn assert_staged_labels(case_insensitive: bool) {
    const POLICY: &str = "c66b3c50150e18426485d134de92c6b311dac125";
    const SHARED: &str = "Synthetic shared label";
    const INTERIOR: &str = "Synthetic  two spaces";
    const PLAIN: &str = "Synthetic Cafe label";
    const ACCENT: &str = "Synthetic Café label";
    const CASE: &str = "Synthetic CAFE label";
    const BODY: &str = "Created: 2026-04-01T10:00:00Z\nUpdated: 2026-04-02T10:00:00Z\n";
    let hive = std::env::var_os("HIVE_STAGING_INSTALLED_BIN").unwrap();
    let dolt = std::env::var_os("HIVE_DOLT_BIN").unwrap();
    let search_path = std::env::var_os("PATH").unwrap();
    for pin in [POLICY_PIN, "0fd0f8dbe632d14094a93599cb1b5f653789ac9c"] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let home = root.join("home");
        let source = root.join("source");
        let target = root.join("inactive");
        let manifest = root.join("reviewed.json");
        fs::create_dir(&home).unwrap();
        init_git(
            &source,
            &home,
            "Synthetic operator",
            "operator@example.invalid",
        );
        fs::write(source.join("AGENTS.md"), format!("hive-system@{pin}\n")).unwrap();
        if pin == POLICY_PIN {
            fs::write(
                source.join("hive.yml"),
                "version: 1\ncoordination:\n  mode: local\n",
            )
            .unwrap();
        }
        let trimmed = format!("\u{00a0}{SHARED}\u{2003}");
        let fixtures = [
            ("accent", vec![ACCENT]),
            ("case", vec![CASE]),
            ("chain", vec![]),
            ("dependent", vec![]),
            ("malformed", vec![SHARED, trimmed.as_str()]),
            ("plain", vec![PLAIN]),
            ("shared-first", vec![trimmed.as_str(), INTERIOR]),
            ("shared-second", vec![SHARED]),
        ];
        let workspace = id(1);
        let area = id(AREA_GENERAL);
        let key = |work_id| json!({"scope": "_general", "work_id": work_id});
        let source_path = |work_id| format!("tasks/_general/backlog/{work_id}/README.md");
        let range = |value: &str| {
            let start = BODY.find(value).unwrap();
            json!({"start": start, "end": start + value.len()})
        };
        let mut reviews = Vec::new();
        for (work_id, tags) in fixtures {
            let path = source_path(work_id);
            let entry = source.join(&path);
            fs::create_dir_all(entry.parent().unwrap()).unwrap();
            let tag_field = if tags.is_empty() {
                String::new()
            } else {
                format!("tags: {}\n", json!(tags))
            };
            fs::write(
                entry,
                format!("---\ndescription: Synthetic Task\n{tag_field}---\n{BODY}"),
            )
            .unwrap();
            let blob = git_text(&source, &home, &["hash-object", &path]);
            let relations = match work_id {
                "plain" | "dependent" | "chain" => {
                    let prerequisites = match work_id {
                        "dependent" => vec![key("plain")],
                        "chain" => vec![key("dependent")],
                        _ => vec![],
                    };
                    json!({"parent": null, "children": [], "prerequisites": prerequisites})
                }
                _ => json!("no_edges"),
            };
            reviews.push(json!({
                "legacy_key": key(work_id), "source_git_blob_sha": blob, "area_id": area,
                "created_at": range("2026-04-01T10:00:00Z"), "updated_at": range("2026-04-02T10:00:00Z"),
                "relations": relations,
            }));
        }
        git(&source, &home, &["add", "."]);
        git(
            &source,
            &home,
            &["commit", "-qm", "Synthetic reviewed Task labels"],
        );
        let commit = git_text(&source, &home, &["rev-parse", "HEAD"]);
        let original_index = fs::read(source.join(".git/index")).unwrap();
        fs::write(
            &manifest,
            json!({
                "version": 1, "source_commit": commit, "policy_pin": pin, "workspace_id": workspace,
                "scopes": [{"scope": "_general", "decision": {"kind": "area", "area": {
                    "id": area, "slug": "general", "name": "General"
                }}}],
                "task_reviews": reviews,
            })
            .to_string(),
        )
        .unwrap();

        let path = if case_insensitive {
            // Change the disposable database default before applying the
            // canonical schema, so all tables inherit the tested collation.
            let tools = root.join("tools");
            fs::create_dir(&tools).unwrap();
            let quoted_dolt = dolt.to_str().unwrap().replace('\'', "'\\''");
            let wrapper = tools.join("dolt");
            fs::write(&wrapper, format!(
                "#!/bin/sh\nif [ \"$1\" = init ]; then\n  '{quoted_dolt}' \"$@\" || exit $?\n  exec '{quoted_dolt}' sql -q 'ALTER DATABASE db COLLATE utf8mb4_0900_ai_ci'\nfi\nexec '{quoted_dolt}' \"$@\"\n"
            )).unwrap();
            fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o700)).unwrap();
            std::env::join_paths(std::iter::once(tools).chain(std::env::split_paths(&search_path)))
                .unwrap()
        } else {
            search_path.clone()
        };
        let stage = |mode| {
            let output = Command::new(&hive)
                .current_dir(&home)
                .env_clear()
                .env("HOME", &home)
                .env("XDG_CONFIG_HOME", root.join("config"))
                .env("XDG_DATA_HOME", root.join("data"))
                .env("XDG_STATE_HOME", root.join("state"))
                .env("TMPDIR", root)
                .env("PATH", &path)
                .args([
                    "--contract",
                    "2",
                    "--json",
                    "workspace",
                    "migration-stage",
                    "--tasks",
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
                .arg(&manifest)
                .args(["--policy-revision", POLICY, "--mode", mode, "--target"])
                .arg(&target)
                .output()
                .unwrap();
            let report = String::from_utf8(output.stdout).unwrap();
            for name in [SHARED, INTERIOR, PLAIN, ACCENT, CASE] {
                assert!(!report.contains(name), "label appeared in migration output");
            }
            assert!(output.stderr.is_empty());
            assert!(output.status.success(), "{pin} {mode}: {report}");
            serde_json::from_str::<Value>(&report).unwrap()["data"].clone()
        };
        let dry = stage("dry-run");
        assert!(!target.exists());
        let staged = stage("staged-import");
        let imported = if case_insensitive { 2 } else { 7 };
        assert_eq!(staged["counts"]["tasks"]["imported"], imported);
        assert_eq!(staged["counts"]["tasks"]["held"], 8 - imported);
        assert_eq!(staged["counts"], dry["counts"]);
        assert_eq!(staged["holds"], dry["holds"]);
        assert_eq!(dry["checks"]["transaction"], "rolled_back");
        assert_eq!(dry["target"]["issue_number_next_value"], 1);
        assert_eq!(
            dry["target"]["projected_issue_number_next_value"],
            imported + 1
        );
        assert_eq!(staged["target"]["issue_number_next_value"], imported + 1);
        let digest = |work_id| sha256_hex(source_path(work_id).as_bytes());
        let mut holds = vec![("malformed", "unsupported_task_metadata")];
        if case_insensitive {
            holds.extend([
                ("accent", "task_label_collation_collision"),
                ("case", "task_label_collation_collision"),
                ("plain", "task_label_collation_collision"),
                ("dependent", "unresolved_task_relations"),
                ("chain", "unresolved_task_relations"),
            ]);
        }
        let ids_for = |result: &Value| {
            result["id_map"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|row| row["kind"] == "task")
                .cloned()
                .collect::<Vec<_>>()
        };
        for result in [&dry, &staged] {
            let ids = ids_for(result);
            assert_eq!(ids.len(), imported as usize);
            for (index, row) in ids.iter().enumerate() {
                assert_eq!(row["issue_number"], index + 1);
            }
            for (work_id, reason) in &holds {
                let digest = digest(work_id);
                let row = result["holds"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|row| row["source_path_sha256"] == digest)
                    .unwrap();
                assert_eq!(row["reason_codes"], json!([reason]));
                assert!(row.get("issue_number").is_none());
                assert!(!ids.iter().any(|row| row["source_path_sha256"] == digest));
            }
        }
        let sql = |query| {
            let output = Command::new(&dolt)
                .env_clear()
                .env("HOME", &home)
                .env("PATH", &search_path)
                .env("DOLT_DISABLE_VERSION_CHECKS", "true")
                .current_dir(target.join("db"))
                .args(["sql", "-r", "json", "-q", query])
                .output()
                .unwrap();
            assert!(output.status.success());
            serde_json::from_slice::<Value>(&output.stdout).unwrap()["rows"].clone()
        };
        let collation = if case_insensitive {
            "utf8mb4_0900_ai_ci"
        } else {
            "utf8mb4_0900_bin"
        };
        assert_eq!(
            sql(
                "SELECT COLLATION_NAME AS name FROM information_schema.COLUMNS WHERE TABLE_SCHEMA=DATABASE() AND TABLE_NAME='labels' AND COLUMN_NAME='name'"
            ),
            json!([{"name": collation}]),
        );
        let names = sql("SELECT id, name FROM labels");
        let names: BTreeMap<_, _> = names
            .as_array()
            .unwrap()
            .iter()
            .map(|row| (row["id"].as_str().unwrap(), row["name"].as_str().unwrap()))
            .collect();
        let mut expected_names = BTreeSet::from([SHARED, INTERIOR]);
        if !case_insensitive {
            expected_names.extend([PLAIN, ACCENT, CASE]);
        }
        assert_eq!(
            names.values().copied().collect::<BTreeSet<_>>(),
            expected_names
        );
        assert_eq!(names.len(), expected_names.len());
        let stored_links =
            sql("SELECT issue_id, label_id FROM issue_labels ORDER BY issue_id, label_id");
        let ids = ids_for(&staged);
        let mut expected_links = BTreeSet::new();
        for (work_id, expected) in [
            ("shared-first", vec![SHARED, INTERIOR]),
            ("shared-second", vec![SHARED]),
            ("accent", vec![ACCENT]),
            ("case", vec![CASE]),
            ("plain", vec![PLAIN]),
        ] {
            if case_insensitive && expected[0] != SHARED {
                continue;
            }
            let row = ids
                .iter()
                .find(|row| row["source_path_sha256"] == digest(work_id))
                .unwrap();
            let label_ids = row["label_ids"].as_array().unwrap();
            let assigned_names: BTreeSet<_> = label_ids
                .iter()
                .map(|id| names[id.as_str().unwrap()])
                .collect();
            assert_eq!(assigned_names, expected.into_iter().collect());
            for label in label_ids {
                expected_links.insert((row["issue_id"].as_str().unwrap(), label.as_str().unwrap()));
            }
        }
        assert_eq!(
            stored_links,
            json!(
                expected_links
                    .into_iter()
                    .map(|(issue, label)| json!({"issue_id": issue, "label_id": label}))
                    .collect::<Vec<_>>()
            )
        );
        assert_eq!(fs::read(source.join(".git/index")).unwrap(), original_index);
        assert_eq!(git_text(&source, &home, &["rev-parse", "HEAD"]), commit);
    }
}
