use super::{
    AREA_GENERAL, POLICY_PIN, Value, fs, git, git_text, id, init_git, json, migration_cli,
    reviewed_row, sha256_hex,
};

const OLD: &str = "directives/_general/old/README.md";
const OLD_KEY: &str = "directives/_general/old";
const MIDDLE: &str = "directives/_general/middle.md";
const MIDDLE_KEY: &str = "directives/_general/middle";
const ACTIVE: &str = "directives/_general/current.md";
const ACTIVE_KEY: &str = "directives/_general/current";
const WITHDRAWN: &str = "directives/_general/withdrawn.md";
const WITHDRAWN_KEY: &str = "directives/_general/withdrawn";
const APPROVAL: &str = "https://forge.example/synthetic/approval/7";
const SOURCE: &str = "https://forge.example/synthetic/evidence/7";
const UPDATED: &str = "2024-02-02T12:00:00Z";
const OBSERVED: &str = "2024-02-03T12:00:00.000001Z";
const EXPIRES: &str = "2024-02-04T12:00:00Z";

#[test]
fn cli_migration_prepares_and_holds_reviewed_directive_history_under_both_pins() {
    for pin in ["0fd0f8dbe632d14094a93599cb1b5f653789ac9c", POLICY_PIN] {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let config = root.path().join("config");
        let source = root.path().join("source");
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
        let records = [
            (OLD, "superseded"),
            (MIDDLE, "superseded"),
            (ACTIVE, "active"),
            (WITHDRAWN, "withdrawn"),
        ];
        let body = format!("# Synthetic direction\r\n\r\nSource: {SOURCE}\r\n");
        let text = |status| {
            format!("---\r\ndescription: Synthetic record\r\nstatus: {status}\r\n---\r\n{body}")
        };
        for (path, status) in records {
            let path = source.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text(status)).unwrap();
        }
        git(&source, &home, &["add", "."]);
        git(
            &source,
            &home,
            &["commit", "-qm", "Synthetic Directive history"],
        );
        let commit = git_text(&source, &home, &["rev-parse", "HEAD"]);
        let index = fs::read(source.join(".git/index")).unwrap();
        let workspace = id(1);
        let area = id(AREA_GENERAL);
        let reviews: Vec<_> = records.into_iter().enumerate().map(|(index, (path, status))| {
            let blob = git_text(&source, &home, &["rev-parse", &format!("{commit}:{path}")]);
            json!({
                "source_path": path, "source_git_blob_sha": blob, "target_id": id(0x810 + index as u32),
                "provenance": {"kind": "directive_history", "status": status, "area_id": area,
                    "source_ref": SOURCE, "issuer_ref": "human:synthetic-issuer", "created_at": "2024-02-01T12:00:00Z",
                    "updated_at": UPDATED, "approval_and_validity": {"state": "reviewed"},
                    "supersession": {"state": "reviewed"}}
            })
        }).collect();
        let mut mapping = json!({
            "version": 1, "source_commit": commit, "policy_pin": pin, "workspace_id": workspace,
            "scopes": [{"scope": "_general", "decision": {"kind": "area",
                "area": {"id": area, "slug": "general", "name": "Synthetic general"}}}],
            "directive_observed_at": OBSERVED, "reviewed_records": reviews,
        });
        let provenance = |index, key| format!("/reviewed_records/{index}/provenance/{key}");
        mapping["reviewed_records"][0]["provenance"]["supersession"]["replacement"] =
            json!(MIDDLE_KEY);
        mapping["reviewed_records"][1]["provenance"]["supersession"]["replacement"] =
            json!(ACTIVE_KEY);
        mapping["reviewed_records"][2]["provenance"]["supersession"]["replaces"] =
            json!(MIDDLE_KEY);
        *mapping
            .pointer_mut(&provenance(2, "approval_and_validity"))
            .unwrap() = json!({
            "state": "reviewed", "approval_ref": APPROVAL, "valid_from": OBSERVED, "expires_at": EXPIRES
        });
        *mapping
            .pointer_mut(&provenance(3, "approval_and_validity"))
            .unwrap() = json!({
            "state": "reviewed", "approval_ref": APPROVAL, "expires_at": UPDATED
        });

        let file = root.path().join("mapping.json");
        let run = |mapping: &Value, status| {
            fs::write(&file, serde_json::to_vec(mapping).unwrap()).unwrap();
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
                    file.to_str().unwrap(),
                    "--include-directives-artifacts",
                ],
                status,
            )
        };
        let output = run(&mapping, 0);
        assert_eq!(output, run(&mapping, 0));
        assert_eq!(output["data"]["directive_observed_at"], OBSERVED);
        assert_eq!(
            output["data"]["directive_artifact_intake"]["counts"]["directives"],
            json!({"inventoried": 4, "prepared": 4, "held": 0, "excluded": 0})
        );
        for (index, (path, status)) in records.into_iter().enumerate() {
            let row = reviewed_row(&output, path);
            let outcome = &row["outcome"];
            assert_eq!(outcome["candidate"]["status"], status);
            assert_eq!(outcome["candidate"]["area_id"], area);
            assert_eq!(outcome["candidate"]["include_descendants"], false);
            assert_eq!(outcome["status"], "prepared");
            assert_eq!(outcome["target_id"], id(0x810 + index as u32));
            assert_eq!(
                row["source_raw_sha256"],
                sha256_hex(text(status).as_bytes())
            );
            assert_eq!(outcome["target_body_sha256"], sha256_hex(body.as_bytes()));
            let supersedes = match index {
                1 => json!(id(0x810)),
                2 => json!(id(0x811)),
                _ => Value::Null,
            };
            assert_eq!(outcome["provenance"]["supersedes_id"], supersedes);
            if index >= 2 {
                assert_eq!(
                    outcome["provenance"]["approval_ref_sha256"],
                    sha256_hex(APPROVAL.as_bytes())
                );
            }
        }
        assert!(!output.to_string().contains(APPROVAL));
        assert!(!output.to_string().contains(SOURCE));

        let assert_hold = |report: &Value, path: &str, reason: &str| {
            let outcome = &reviewed_row(report, path)["outcome"];
            assert_eq!(outcome["status"], "held", "{path}");
            assert!(
                outcome["reason_codes"]
                    .as_array()
                    .unwrap()
                    .contains(&json!(reason)),
                "{path} {outcome}"
            );
            assert!(outcome.get("target_id").is_none());
        };
        for (field, value, reason) in [
            (
                "expires_at",
                json!(OBSERVED),
                "unverified_directive_validity",
            ),
            ("approval_ref", Value::Null, "missing_directive_approval"),
            (
                "valid_from",
                json!(EXPIRES),
                "unverified_directive_validity",
            ),
        ] {
            let mut changed = mapping.clone();
            changed["reviewed_records"][2]["provenance"]["approval_and_validity"][field] = value;
            let result = run(&changed, 0);
            assert_hold(&result, ACTIVE, reason);
            for path in [OLD, MIDDLE, ACTIVE] {
                assert_hold(&result, path, "invalid_supersession_chain");
            }
        }
        let mut expired = mapping.clone();
        expired["directive_observed_at"] = json!(EXPIRES);
        let result = run(&expired, 0);
        assert_hold(&result, ACTIVE, "incomplete_historical_provenance");
        assert_hold(&result, OLD, "invalid_supersession_chain");
        assert_eq!(
            reviewed_row(&result, WITHDRAWN)["outcome"]["status"],
            "prepared"
        );
        let mut early = mapping.clone();
        early["directive_observed_at"] = json!("2024-02-03T12:00:00Z");
        assert_hold(&run(&early, 0), ACTIVE, "unverified_directive_validity");
        let mut unobserved = mapping.clone();
        unobserved
            .as_object_mut()
            .unwrap()
            .remove("directive_observed_at");
        assert_hold(
            &run(&unobserved, 0),
            ACTIVE,
            "unverified_directive_validity",
        );
        early["directive_observed_at"] = json!("2024-02-03T12:00:00");
        assert_eq!(run(&early, 2)["error"]["code"], "INVALID_CONFIG");

        for (index, key, value, expected) in [
            (
                1,
                "replacement",
                json!("directives/_general/missing"),
                vec![OLD, MIDDLE, ACTIVE],
            ),
            (
                1,
                "replacement",
                json!(MIDDLE_KEY),
                vec![OLD, MIDDLE, ACTIVE],
            ),
            (1, "replacement", json!(OLD_KEY), vec![OLD, MIDDLE, ACTIVE]),
            (
                0,
                "replacement",
                json!(ACTIVE_KEY),
                vec![MIDDLE, OLD, ACTIVE],
            ),
            (
                2,
                "replaces",
                json!(WITHDRAWN_KEY),
                vec![OLD, MIDDLE, ACTIVE, WITHDRAWN],
            ),
        ] {
            let mut changed = mapping.clone();
            changed["reviewed_records"][index]["provenance"]["supersession"][key] = value;
            let result = run(&changed, 0);
            for path in expected {
                assert_hold(&result, path, "invalid_supersession_chain");
            }
        }
        let mut unreviewed = mapping.clone();
        unreviewed["reviewed_records"]
            .as_array_mut()
            .unwrap()
            .remove(2);
        let result = run(&unreviewed, 0);
        assert_hold(&result, OLD, "invalid_supersession_chain");
        assert_hold(&result, MIDDLE, "invalid_supersession_chain");
        let mut withdrawn = mapping.clone();
        withdrawn["reviewed_records"][1]["provenance"]["supersession"]["replacement"] =
            json!(WITHDRAWN_KEY);
        withdrawn["reviewed_records"][2]["provenance"]["supersession"]
            .as_object_mut()
            .unwrap()
            .remove("replaces");
        let result = run(&withdrawn, 0);
        assert_eq!(
            reviewed_row(&result, WITHDRAWN)["outcome"]["provenance"]["supersedes_id"],
            id(0x811)
        );

        assert_eq!(fs::read(source.join(".git/index")).unwrap(), index);
        assert_eq!(git_text(&source, &home, &["rev-parse", "HEAD"]), commit);
        assert!(git(&source, &home, &["status", "--porcelain"]).is_empty());
        assert!(!config.exists());
    }
}
