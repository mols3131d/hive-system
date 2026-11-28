use super::{
    AREA_APP, AREA_GENERAL, AREA_USERDATA, POLICY_PIN, Value, fs, git, git_text, id, init_git,
    init_source, json, migration_cli, sha256_hex,
};

#[test]
fn cli_migration_plan_prepares_only_exact_reviewed_records_under_both_pins() {
    const LEGACY_PIN: &str = "0fd0f8dbe632d14094a93599cb1b5f653789ac9c";
    const PROPOSAL: &str = "directives/_general/proposal.md";
    const REPORT: &str = "artifacts/report.md";
    const COPY: &str = "artifacts/other.md";
    const UNCLASSIFIED: &str = "artifacts/unclassified.md";
    const ACTIVE: &str = "directives/_general/active.md";
    const WORK: &str = "tasks/_general/backlog/work/artifacts/report.md";
    const REFERENCE: &str = "https://forge.example/synthetic/review/123";
    const CREATED: &str = "2024-02-29T12:00:00Z";
    const UPDATED: &str = "2024-03-01T12:00:00.000001Z";
    const ISSUER: &str = "human:synthetic-issuer";
    const AUTHOR: &str = "bot:synthetic-author";

    for pin in [LEGACY_PIN, POLICY_PIN] {
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
        let proposal_body = format!(
            "\r\n# Synthetic **proposal** ###\r\n\r\n```sh\r\n# Synthetic shell comment\r\n```\r\n\r\nIssuer: {ISSUER}\r\nSource: {REFERENCE}\r\nCreated: {CREATED}\r\nUpdated: {UPDATED}\r\n"
        );
        let artifact_body =
            "\n# Synthetic heading\n\nRetain café and trailing spaces.  \nNo final newline";
        let proposal = format!(
            "---\r\ndescription: Synthetic direction\r\nstatus: proposed\r\n---\r\n{proposal_body}"
        );
        let artifact = format!(
            "---\ndescription: Synthetic evidence\ntitle: Reviewed synthetic artifact\nkind: review\nstatus: closed\n---\n{artifact_body}"
        );
        let active = proposal.replace("status: proposed", "status: active");
        let unclassified = artifact.replace("kind: review\n", "");
        let files = [
            (PROPOSAL, &proposal),
            (REPORT, &artifact),
            (COPY, &artifact),
            (UNCLASSIFIED, &unclassified),
            (ACTIVE, &active),
            (WORK, &artifact),
        ];
        for (path, contents) in files {
            let path = source.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, contents).unwrap();
        }
        git(&source, &home, &["add", "."]);
        git(
            &source,
            &home,
            &["commit", "-qm", "Reviewed synthetic records"],
        );
        let git_index = fs::read(source.join(".git/index")).unwrap();
        let commit = git_text(&source, &home, &["rev-parse", "HEAD"]);
        let blob = |path| git_text(&source, &home, &["rev-parse", &format!("{commit}:{path}")]);
        let workspace = id(1);
        let area_id = id(AREA_GENERAL);
        let mapping = json!({
            "version": 1, "source_commit": commit, "policy_pin": pin, "workspace_id": workspace,
            "scopes": [{"scope": "_general", "decision": {
                "kind": "area", "area": {"id": area_id, "slug": "general", "name": "Synthetic general"}
            }}],
            "reviewed_records": [
                {"source_path": PROPOSAL, "source_git_blob_sha": blob(PROPOSAL), "target_id": id(0x411),
                 "provenance": {"kind": "proposed_directive", "area_id": area_id, "source_ref": REFERENCE,
                     "issuer_ref": ISSUER, "created_at": CREATED, "updated_at": UPDATED,
                     "supersession": "none", "approval_and_validity": "none"}},
                {"source_path": REPORT, "source_git_blob_sha": blob(REPORT), "target_id": id(0x511),
                 "provenance": {"kind": "standalone_artifact", "artifact_kind": "review", "source_ref": REFERENCE,
                     "author_ref": AUTHOR, "created_at": CREATED, "substantive_updated_at": UPDATED,
                     "links": "none", "archive": "none"}}
            ]
        });
        let mapping_file = root.path().join("scope.json");
        let args = [
            "--source",
            source.to_str().unwrap(),
            "--source-commit",
            &commit,
            "--workspace-id",
            &workspace,
            "--mapping-file",
            mapping_file.to_str().unwrap(),
            "--include-directives-artifacts",
        ];
        let run_raw = |input: &str, status| {
            fs::write(&mapping_file, input).unwrap();
            migration_cli(&home, &config, "migration-plan", &args, status)
        };
        let run = |mapping: &Value, status| run_raw(&mapping.to_string(), status);
        let report = run(&mapping, 0);
        assert_eq!(report, run(&mapping, 0));
        let data = &report["data"];
        assert_eq!(
            data["mapping_sha256"],
            sha256_hex(&fs::read(&mapping_file).unwrap())
        );
        assert_eq!(data["source"]["source_commit"], commit);
        assert_eq!(data["source"]["policy_pin"], pin);
        assert_eq!(data["target"]["workspace_id"], workspace);
        let intake = &data["directive_artifact_intake"];
        assert_eq!(
            intake["counts"],
            json!({
                "directives": {"inventoried": 2, "held": 1, "excluded": 0, "prepared": 1},
                "artifacts_standalone": {"inventoried": 3, "held": 2, "excluded": 0, "prepared": 1},
                "artifacts_work_scoped": {"inventoried": 1, "held": 1, "excluded": 0, "prepared": 0},
            })
        );
        for (path, target, raw, body, title) in [
            (
                PROPOSAL,
                0x411,
                proposal.as_str(),
                proposal_body.as_str(),
                "Synthetic proposal",
            ),
            (
                REPORT,
                0x511,
                artifact.as_str(),
                artifact_body,
                "Reviewed synthetic artifact",
            ),
        ] {
            let row = reviewed_row(&report, path);
            assert_eq!(row["source_git_blob_sha"], blob(path));
            assert_eq!(row["source_raw_sha256"], sha256_hex(raw.as_bytes()));
            let outcome = &row["outcome"];
            assert_eq!(outcome["status"], "prepared");
            assert_eq!(outcome["target_id"], id(target));
            assert_eq!(outcome["target_title_sha256"], sha256_hex(title.as_bytes()));
            assert_eq!(outcome["target_body_sha256"], sha256_hex(body.as_bytes()));
            assert_eq!(
                outcome["provenance"]["source_ref_sha256"],
                sha256_hex(REFERENCE.as_bytes())
            );
            assert_eq!(outcome["provenance"]["created_at"], CREATED);
        }
        let proposal_plan = &reviewed_row(&report, PROPOSAL)["outcome"];
        assert_eq!(
            proposal_plan["candidate"],
            json!({"record_type": "directive", "status": "proposed",
            "scope_kind": "area", "area_id": area_id, "include_descendants": false})
        );
        assert_eq!(
            proposal_plan["provenance"]["issuer_ref_sha256"],
            sha256_hex(ISSUER.as_bytes())
        );
        assert_eq!(proposal_plan["provenance"]["updated_at"], UPDATED);
        for field in ["supersedes_id", "approval_ref", "valid_from", "expires_at"] {
            assert_eq!(proposal_plan["provenance"].get(field), Some(&Value::Null));
        }
        let artifact_plan = &reviewed_row(&report, REPORT)["outcome"];
        assert_eq!(
            artifact_plan["candidate"],
            json!({"record_type": "artifact", "kind": "review", "legacy_status": "closed"})
        );
        assert_eq!(artifact_plan["provenance"]["scope"], "standalone");
        assert_eq!(
            artifact_plan["provenance"]["author_ref_sha256"],
            sha256_hex(AUTHOR.as_bytes())
        );
        assert_eq!(
            artifact_plan["provenance"]["substantive_updated_at"],
            UPDATED
        );
        assert_eq!(reviewed_row(&report, COPY)["outcome"]["status"], "held");
        let output = report.to_string();
        for private in [
            REFERENCE,
            ISSUER,
            AUTHOR,
            "Retain café and trailing spaces",
            "Reviewed synthetic artifact",
            "Synthetic proposal",
            mapping_file.to_str().unwrap(),
        ] {
            assert!(!output.contains(private), "{private}");
        }

        for (index, field, value, path, reason) in [
            (
                0,
                "area_id",
                json!(id(2)),
                PROPOSAL,
                "ambiguous_scope_or_collision",
            ),
            (
                0,
                "supersession",
                json!("unresolved"),
                PROPOSAL,
                "unresolved_supersession_graph",
            ),
            (
                0,
                "approval_and_validity",
                json!("unresolved"),
                PROPOSAL,
                "unverified_directive_validity",
            ),
            (
                0,
                "updated_at",
                json!("2024-02-28T12:00:00Z"),
                PROPOSAL,
                "incomplete_historical_provenance",
            ),
            (
                1,
                "source_ref",
                json!("file:///synthetic/path"),
                REPORT,
                "incomplete_historical_provenance",
            ),
            (
                1,
                "substantive_updated_at",
                json!("2024-03-01T12:00:00.0000001Z"),
                REPORT,
                "incomplete_historical_provenance",
            ),
            (
                1,
                "author_ref",
                json!("fixture@example.invalid"),
                REPORT,
                "unverified_artifact_author",
            ),
            (
                1,
                "artifact_kind",
                json!("report"),
                REPORT,
                "unclassified_artifact_kind",
            ),
            (
                1,
                "links",
                json!("unresolved"),
                REPORT,
                "unresolved_artifact_links",
            ),
            (
                1,
                "archive",
                json!("unresolved"),
                REPORT,
                "unverified_artifact_retention",
            ),
        ] {
            let mut changed = mapping.clone();
            changed["reviewed_records"][index]["provenance"][field] = value;
            let held = run(&changed, 0);
            assert_eq!(
                reviewed_row(&held, path)["outcome"]["reason_codes"],
                json!([reason])
            );
            assert!(
                reviewed_row(&held, path)["outcome"]
                    .get("target_id")
                    .is_none()
            );
        }
        let mut changed = mapping.clone();
        for (index, path) in [(0, ACTIVE), (1, WORK)] {
            changed["reviewed_records"][index]["source_path"] = json!(path);
            changed["reviewed_records"][index]["source_git_blob_sha"] = json!(blob(path));
        }
        let held = run(&changed, 0);
        for path in [ACTIVE, WORK] {
            assert_eq!(reviewed_row(&held, path)["outcome"]["status"], "held");
        }
        changed["reviewed_records"][1]["source_path"] = json!(UNCLASSIFIED);
        changed["reviewed_records"][1]["source_git_blob_sha"] = json!(blob(UNCLASSIFIED));
        assert_eq!(
            reviewed_row(&run(&changed, 0), UNCLASSIFIED)["outcome"]["reason_codes"],
            json!(["unclassified_artifact_kind"])
        );
        for (field, value) in [
            ("source_commit", json!(blob(PROPOSAL))),
            ("workspace_id", json!(id(2))),
        ] {
            let mut changed = mapping.clone();
            changed[field] = value;
            assert_eq!(run(&changed, 2)["error"]["code"], "INVALID_CONFIG");
        }
        for (field, value) in [
            ("source_git_blob_sha", json!(blob(REPORT))),
            ("target_id", json!(id(0x511))),
            ("source_path", json!("directives/_general/missing.md")),
        ] {
            let mut changed = mapping.clone();
            changed["reviewed_records"][0][field] = value;
            let failure = run(&changed, 2);
            assert_eq!(failure["error"]["code"], "INVALID_CONFIG");
            assert!(failure.get("data").is_none());
        }

        let assert_invalid_format = |input: &str| {
            let failure = run_raw(input, 2);
            assert_eq!(failure["error"]["code"], "INVALID_CONFIG");
            assert_eq!(
                failure["error"]["message"],
                "The scope mapping must match the versioned JSON format."
            );
            assert!(failure.get("data").is_none());
        };
        for (index, field, value) in [
            (0, "supersession", json!({"none": null})),
            (0, "approval_and_validity", json!({"none": null})),
            (1, "links", json!({"none": null})),
            (1, "archive", json!({"none": null})),
            (1, "artifact_kind", json!({"review": null})),
            (1, "created_at", json!(2024)),
            (1, "unexpected", json!("synthetic")),
        ] {
            let mut changed = mapping.clone();
            changed["reviewed_records"][index]["provenance"][field] = value;
            assert_invalid_format(&changed.to_string());
        }
        let duplicate = mapping.to_string().replace(
            r#""supersession":"none""#,
            r#""supersession":"unresolved","supersession":"none""#,
        );
        assert_invalid_format(&duplicate);

        for (index, provenance_fields) in [
            (
                0,
                [
                    "area_id",
                    "source_ref",
                    "issuer_ref",
                    "created_at",
                    "updated_at",
                    "supersession",
                    "approval_and_validity",
                ],
            ),
            (
                1,
                [
                    "artifact_kind",
                    "source_ref",
                    "author_ref",
                    "created_at",
                    "substantive_updated_at",
                    "links",
                    "archive",
                ],
            ),
        ] {
            let mut changed = mapping.clone();
            let record = &mut changed["reviewed_records"][index];
            *record = json!([
                record["source_path"],
                record["source_git_blob_sha"],
                record["target_id"],
                record["provenance"],
            ]);
            assert_invalid_format(&changed.to_string());

            let mut changed = mapping.clone();
            let provenance = &mut changed["reviewed_records"][index]["provenance"];
            *provenance = Value::Array(
                std::iter::once("kind")
                    .chain(provenance_fields)
                    .map(|field| provenance[field].clone())
                    .collect(),
            );
            assert_invalid_format(&changed.to_string());
        }

        let mut lowercase = mapping.clone();
        for (index, path, field, date) in [
            (0, PROPOSAL, "created_at", CREATED),
            (0, PROPOSAL, "updated_at", UPDATED),
            (1, REPORT, "created_at", CREATED),
            (1, REPORT, "substantive_updated_at", UPDATED),
        ] {
            for separator in ["X", "\n"] {
                let mut changed = mapping.clone();
                changed["reviewed_records"][index]["provenance"][field] =
                    json!(date.replace('T', separator));
                let result = run(&changed, 0);
                let outcome = &reviewed_row(&result, path)["outcome"];
                assert_eq!(outcome["status"], "held", "{field} with {separator:?}");
                assert_eq!(
                    outcome["reason_codes"],
                    json!(["incomplete_historical_provenance"])
                );
                assert!(outcome.get("target_id").is_none());
            }
            lowercase["reviewed_records"][index]["provenance"][field] =
                json!(date.replace('T', "t"));
        }
        let result = run(&lowercase, 0);
        for (index, path, updated_field) in [
            (0, PROPOSAL, "updated_at"),
            (1, REPORT, "substantive_updated_at"),
        ] {
            let outcome = &reviewed_row(&result, path)["outcome"];
            assert_eq!(outcome["status"], "prepared");
            for field in ["created_at", updated_field] {
                assert_eq!(
                    outcome["provenance"][field],
                    lowercase["reviewed_records"][index]["provenance"][field]
                );
            }
        }

        let mut changed = mapping.clone();
        for value in [Value::Null, json!({})] {
            changed["reviewed_records"] = value;
            assert_invalid_format(&changed.to_string());
        }
        changed.as_object_mut().unwrap().remove("reviewed_records");
        let omitted = run(&changed, 0);
        changed["reviewed_records"] = json!([]);
        let empty = run(&changed, 0);
        for result in [&omitted, &empty] {
            for path in [PROPOSAL, REPORT] {
                assert_eq!(reviewed_row(result, path)["outcome"]["status"], "held");
            }
        }
        assert_eq!(report, run(&mapping, 0));
        let failure = migration_cli(&home, &config, "migration-plan", &args[..args.len() - 1], 2);
        assert_eq!(failure["error"]["code"], "INVALID_CONFIG");
        assert_eq!(fs::read(source.join(".git/index")).unwrap(), git_index);
        assert_eq!(git_text(&source, &home, &["rev-parse", "HEAD"]), commit);
        assert!(git(&source, &home, &["status", "--porcelain"]).is_empty());
        assert!(!config.exists());
    }
}

fn reviewed_row<'a>(report: &'a Value, path: &str) -> &'a Value {
    report["data"]["directive_artifact_intake"]["records"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["source_path"] == path)
        .unwrap()
}

#[test]
fn cli_migration_plan_inventories_directives_and_artifacts_read_only() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    fs::create_dir(&home).unwrap();
    let config = root.path().join("config");
    let source = root.path().join("source");
    init_source(&source, &home);

    let proposed = b"---\r\ndescription: 'synthetic-private-description'\r\nstatus: proposed\r\n---\r\n# Synthetic private heading\r\n\r\nSynthetic private direction.\r\n";
    let active = b"---\ndescription: Synthetic fixture\nstatus: active\n---\n\nApproval: synthetic-unverified-approval\nExpires: 2000-01-01\n";
    let artifact = b"---\ndescription: Synthetic fixture\nkind: work-note\nstatus: closed\n---\n\nAuthor: synthetic-unverified-author\nSource: https://forge.example/synthetic/private\nCreated: 2026-01-01\n";
    let files: &[(&str, &[u8])] = &[
        (
            "directives/example/README.md",
            b"Project: https://forge.example/synthetic/example\n",
        ),
        ("directives/example/direction.md", proposed),
        ("directives/_general/proposal/README.md", proposed),
        ("directives/_general/proposal/context.md", active),
        ("directives/_general/active.md", active),
        (
            "directives/_general/superseded.md",
            b"---\ndescription: Synthetic fixture\nstatus: superseded\n---\nReplaced by: synthetic-unverified-replacement\n",
        ),
        ("directives/_general/duplicate.md", proposed),
        ("directives/_general/duplicate/README.md", active),
        ("directives/_general/no-entry/support.md", proposed),
        (
            "directives/_general/invalid.md",
            b"---\ndescription: Synthetic fixture\nstatus: parked\n---\n",
        ),
        ("directives/_hive/legacy.md", proposed),
        ("directives/external-team/external.md", active),
        ("directives/unmapped/held.md", proposed),
        ("artifacts/README.md", artifact),
        ("artifacts/standalone.md", artifact),
        ("artifacts/pack/README.md", artifact),
        ("artifacts/pack/member.md", artifact),
        ("artifacts/pack/output.bin", b"\x00\xF0"),
        (
            "artifacts/elsewhere.url",
            b"https://forge.example/synthetic/private\n",
        ),
        (
            "artifacts/elsewhere.md",
            b"version https://git-lfs.github.com/spec/v1\noid sha256:synthetic-pointer\nsize 100\n",
        ),
        ("artifacts/binary.md", b"\x00\xF0"),
        ("artifacts/nested/orphan.md", artifact),
        (
            "artifacts/unknown-kind.md",
            b"---\ndescription: Synthetic fixture\nkind: not-a-kind\n---\n",
        ),
        (
            "artifacts/invalid-status.md",
            b"---\ndescription: Synthetic fixture\nkind: report\nstatus: done\n---\n",
        ),
        (
            "artifacts/duplicate-field.md",
            b"---\ndescription: Synthetic fixture\nkind: report\nkind: review\n---\n",
        ),
        (
            "artifacts/conflicting-titles.md",
            b"---\ndescription: Synthetic fixture\nkind: report\n---\n# One\n# Two\n",
        ),
        (
            "tasks/_general/archive/close-checklist/artifacts/report.md",
            artifact,
        ),
        (
            "tasks/_general/active/missing-task/artifacts/report.md",
            artifact,
        ),
        (
            "tasks/external-team/backlog/work/artifacts/report.md",
            artifact,
        ),
        ("tasks/_hive/archive/work/artifacts/report.md", artifact),
        ("tasks/_general/active/work/reports/ignored.md", artifact),
    ];
    let write = |path: &str, bytes: &[u8]| {
        let file = source.join(path);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, bytes).unwrap();
    };
    for (path, bytes) in files {
        write(path, bytes);
    }
    let malformed_labels = [
        (
            "directives/_general/map-status.md",
            "status: {active: null}",
            "invalid_directive_status",
        ),
        (
            "directives/_general/null-status.md",
            "status: null",
            "invalid_directive_status",
        ),
        (
            "directives/_general/null-kind.md",
            "status: proposed\nkind: null",
            "unsupported_directive_metadata",
        ),
        (
            "artifacts/map-kind.md",
            "kind: {report: null}",
            "unclassified_artifact_kind",
        ),
        (
            "artifacts/map-status.md",
            "kind: report\nstatus: {closed: null}",
            "invalid_artifact_status",
        ),
        (
            "artifacts/null-kind.md",
            "kind: null",
            "unclassified_artifact_kind",
        ),
        (
            "artifacts/null-status.md",
            "kind: report\nstatus: null",
            "invalid_artifact_status",
        ),
        (
            "artifacts/null-title.md",
            "kind: report\ntitle: null",
            "unsupported_artifact_metadata",
        ),
        (
            "artifacts/null-tags.md",
            "kind: report\ntags: null",
            "unsupported_artifact_metadata",
        ),
    ];
    for (path, fields, _) in malformed_labels {
        write(
            path,
            format!("---\ndescription: Synthetic fixture\n{fields}\n---\n").as_bytes(),
        );
    }
    let heading_cases = [
        (
            "artifacts/fenced-comment.md",
            "# Synthetic title\n\n```sh\n# Synthetic shell comment\n```\n",
            true,
        ),
        (
            "artifacts/setext-title.md",
            "Intro paragraph\n\nSynthetic **title**\n===\n\n> # Nested heading\n\n# Synthetic title\n",
            true,
        ),
        (
            "artifacts/fenced-heading-only.md",
            "```md\n# Synthetic shell comment\n```\n",
            false,
        ),
    ];
    for (path, body, _) in heading_cases {
        write(
            path,
            format!(
                "---\ndescription: {}\nkind: 'report'\n---\n{body}",
                "a".repeat(256)
            )
            .as_bytes(),
        );
    }
    write("artifacts/oversize.md", &vec![b'a'; 256 * 1024 + 1]);
    write(
        "artifacts/oversize-header.md",
        format!(
            "---\ndescription: {}\nkind: report\n---\n",
            "a".repeat(64 * 1024)
        )
        .as_bytes(),
    );
    git(&source, &home, &["add", "."]);
    git(
        &source,
        &home,
        &["commit", "-qm", "Synthetic Directive and Artifact intake"],
    );
    let commit = git_text(&source, &home, &["rev-parse", "HEAD"]);
    let workspace = id(1);
    let mapping = root.path().join("scope.json");
    fs::write(&mapping, json!({
        "version": 1,
        "source_commit": commit,
        "policy_pin": POLICY_PIN,
        "workspace_id": workspace,
        "scopes": [
            {"scope": "_general", "decision": {
                "kind": "area", "area": {"id": id(AREA_GENERAL), "slug": "general", "name": "Synthetic General"}
            }},
            {"scope": "_hive-userdata", "decision": {
                "kind": "area", "area": {"id": id(AREA_USERDATA), "slug": "workspace-operations", "name": "Synthetic Userdata"}
            }},
            {"scope": "example", "decision": {
                "kind": "area", "area": {"id": id(AREA_APP), "slug": "application", "name": "Synthetic Area"},
                "project_evidence": [{"path": "directives/example/README.md", "canonical_ref": "https://forge.example/synthetic/example"}]
            }},
            {"scope": "external-team", "decision": {"kind": "external"}}
        ]
    }).to_string()).unwrap();
    let args = [
        "--source",
        source.to_str().unwrap(),
        "--source-commit",
        &commit,
        "--workspace-id",
        &workspace,
        "--mapping-file",
        mapping.to_str().unwrap(),
        "--include-directives-artifacts",
    ];
    let git_files = [
        ".git/index",
        ".git/HEAD",
        ".git/refs/heads/main",
        ".git/config",
    ];
    let original_git_files = git_files.map(|path| fs::read(source.join(path)).unwrap());
    let mapping_bytes = fs::read(&mapping).unwrap();
    let run = |args: &[&str], status| migration_cli(&home, &config, "migration-plan", args, status);
    let previous = run(&args[..args.len() - 1], 0);
    assert!(previous["data"].get("directive_artifact_intake").is_none());

    let report = run(&args, 0);
    assert_eq!(report, run(&args, 0));
    let intake = &report["data"]["directive_artifact_intake"];
    assert_eq!(intake["coverage"], "partial");
    let records = intake["records"].as_array().unwrap();
    assert!(
        records
            .windows(2)
            .all(|pair| pair[0]["source_path"].as_str() < pair[1]["source_path"].as_str())
    );
    for (surface, counts) in intake["counts"].as_object().unwrap() {
        let records = records
            .iter()
            .filter(|row| row["surface"] == *surface)
            .collect::<Vec<_>>();
        let held = records
            .iter()
            .filter(|row| row["outcome"]["status"] == "held")
            .count();
        assert_eq!(
            *counts,
            json!({"inventoried": records.len(), "held": held, "excluded": records.len() - held})
        );
    }
    let row = |path: &str| {
        records
            .iter()
            .find(|row| row["source_path"] == path)
            .unwrap()
    };
    let outcome = |path| &row(path)["outcome"];
    let held = |path, reasons: &[&str]| {
        assert_eq!(outcome(path)["status"], "held", "{path}");
        assert_eq!(outcome(path)["reason_codes"], json!(reasons), "{path}");
    };
    let excluded = |path, reason| {
        assert_eq!(
            *outcome(path),
            json!({"status": "excluded", "reason": reason}),
            "{path}"
        );
    };

    let scoped_directive = |status, area| {
        json!({
            "record_type": "directive", "status": status, "scope_kind": "area",
            "area_id": id(area), "include_descendants": false
        })
    };
    let bundle = "directives/_general/proposal/README.md";
    held(bundle, &["incomplete_historical_provenance"]);
    assert_eq!(
        outcome(bundle)["candidate"],
        scoped_directive("proposed", AREA_GENERAL)
    );
    assert_eq!(row(bundle)["source_raw_sha256"], sha256_hex(proposed));
    assert_eq!(
        row(bundle)["source_git_blob_sha"],
        git_text(
            &source,
            &home,
            &["rev-parse", &format!("{commit}:{bundle}")]
        )
    );
    assert_eq!(
        outcome("directives/example/direction.md")["candidate"],
        scoped_directive("proposed", AREA_APP)
    );
    held(
        "directives/_general/active.md",
        &[
            "incomplete_historical_provenance",
            "missing_directive_approval",
            "unverified_directive_validity",
        ],
    );
    assert_eq!(
        outcome("directives/_general/active.md")["candidate"],
        scoped_directive("active", AREA_GENERAL)
    );
    assert_eq!(
        outcome("directives/_hive-userdata/unapproved-active.md")["candidate"],
        scoped_directive("active", AREA_USERDATA)
    );
    held(
        "directives/_general/superseded.md",
        &[
            "incomplete_historical_provenance",
            "unresolved_supersession_graph",
        ],
    );
    held(
        "directives/_general/invalid.md",
        &["invalid_directive_status"],
    );
    held(
        "directives/_general/duplicate.md",
        &["duplicate_legacy_directive"],
    );
    held(
        "directives/_general/duplicate/README.md",
        &["duplicate_legacy_directive"],
    );
    held(
        "directives/_general/no-entry/support.md",
        &["unsupported_directive_shape"],
    );
    held("directives/_hive/legacy.md", &["unsplit_legacy_scope"]);
    held(
        "directives/unmapped/held.md",
        &["ambiguous_scope_or_collision"],
    );
    excluded(
        "directives/external-team/external.md",
        "external_backend_owned",
    );
    excluded(
        "directives/_general/proposal/context.md",
        "directive_bundle_support",
    );
    excluded("directives/README.md", "navigation_only");
    excluded("directives/example/README.md", "navigation_only");
    excluded("artifacts/README.md", "navigation_only");

    let artifact_holds = [
        "incomplete_historical_provenance",
        "unverified_artifact_author",
        "unverified_artifact_retention",
    ];
    for path in ["artifacts/standalone.md", "artifacts/pack/README.md"] {
        held(path, &artifact_holds);
        assert_eq!(
            outcome(path)["candidate"],
            json!({
                "record_type": "artifact", "kind": "work-note", "legacy_status": "closed",
            })
        );
        assert_eq!(row(path)["source_raw_sha256"], sha256_hex(artifact));
        assert_eq!(row(path)["parent_task_path"], Value::Null);
    }
    let work = "tasks/_general/archive/close-checklist/artifacts/report.md";
    held(
        work,
        &[artifact_holds.as_slice(), &["unresolved_parent_task"]].concat(),
    );
    assert_eq!(
        row(work)["parent_task_path"],
        "tasks/_general/archive/close-checklist"
    );
    assert_eq!(
        outcome(work)["candidate"],
        outcome("artifacts/standalone.md")["candidate"]
    );
    held(
        "tasks/_general/active/missing-task/artifacts/report.md",
        &[
            artifact_holds.as_slice(),
            &["missing_parent_task_entrypoint"],
        ]
        .concat(),
    );
    held(
        "tasks/_hive/archive/work/artifacts/report.md",
        &["unsplit_legacy_scope"],
    );
    excluded(
        "tasks/external-team/backlog/work/artifacts/report.md",
        "external_backend_owned",
    );
    assert!(
        !records
            .iter()
            .any(|row| row["source_path"] == "tasks/_general/active/work/reports/ignored.md")
    );

    for (path, reason) in [
        (
            "artifacts/unclassified-note.md",
            "unclassified_artifact_kind",
        ),
        ("artifacts/unknown-kind.md", "unclassified_artifact_kind"),
        ("artifacts/invalid-status.md", "invalid_artifact_status"),
        (
            "artifacts/duplicate-field.md",
            "unsupported_artifact_metadata",
        ),
        (
            "artifacts/conflicting-titles.md",
            "missing_or_conflicting_title",
        ),
        ("artifacts/oversize.md", "record_markdown_too_large"),
        (
            "artifacts/oversize-header.md",
            "unsupported_artifact_metadata",
        ),
        (
            "artifacts/pack/member.md",
            "unresolved_artifact_bundle_member",
        ),
        ("artifacts/nested/orphan.md", "unsupported_artifact_shape"),
        (
            "artifacts/elsewhere.url",
            "unresolved_binary_or_external_owner",
        ),
        (
            "artifacts/elsewhere.md",
            "unresolved_binary_or_external_owner",
        ),
        ("artifacts/binary.md", "unresolved_binary_or_external_owner"),
        (
            "artifacts/pack/output.bin",
            "unresolved_binary_or_external_owner",
        ),
    ] {
        held(path, &[reason]);
        assert!(outcome(path).get("candidate").is_none());
    }
    for (path, _, reason) in malformed_labels {
        held(path, &[reason]);
        assert!(outcome(path).get("candidate").is_none());
    }
    for (path, _, has_heading) in heading_cases {
        if has_heading {
            held(path, &artifact_holds);
            assert_eq!(
                outcome(path)["candidate"],
                json!({"record_type": "artifact", "kind": "report", "legacy_status": null})
            );
        } else {
            held(path, &["missing_or_conflicting_title"]);
            assert!(outcome(path).get("candidate").is_none());
        }
    }

    let json = report.to_string();
    for private in [
        "synthetic-private-description",
        "Synthetic private heading",
        "Synthetic shell comment",
        "Synthetic private direction.",
        "synthetic-unverified-approval",
        "synthetic-unverified-replacement",
        "synthetic-unverified-author",
        "2000-01-01",
        "2026-01-01",
        "https://forge.example/synthetic/private",
        "https://forge.example/synthetic/example",
        "synthetic-pointer",
        "Synthetic General",
        mapping.to_str().unwrap(),
    ] {
        assert!(!json.contains(private), "{private}");
    }
    for (path, bytes) in files {
        assert_eq!(fs::read(source.join(path)).unwrap(), *bytes);
    }
    assert_eq!(
        git_files.map(|path| fs::read(source.join(path)).unwrap()),
        original_git_files
    );
    assert_eq!(fs::read(&mapping).unwrap(), mapping_bytes);
    assert_eq!(git_text(&source, &home, &["rev-parse", "HEAD"]), commit);
    assert!(git(&source, &home, &["status", "--porcelain"]).is_empty());
    assert!(!config.exists());

    write(bundle, b"Uncommitted Directive");
    let error = run(&args, 5);
    assert_eq!(error["error"]["code"], "DIRTY_CONFLICT");
    assert!(error.get("data").is_none());
}

#[test]
fn cli_migration_plan_redacts_nonportable_directive_and_artifact_locators() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    fs::create_dir(&home).unwrap();
    let config = root.path().join("config");
    let source = root.path().join("source");
    init_source(&source, &home);

    let cases = [
        (
            "directives/Synthetic Scope/README.md",
            "excluded",
            "navigation_only",
        ),
        (
            "directives/Synthetic Scope/record.md",
            "held",
            "ambiguous_scope_or_collision",
        ),
        (
            "directives/_general/Synthetic Bundle/README.md",
            "held",
            "unsupported_directive_shape",
        ),
        (
            "directives/_general/record.private.md",
            "held",
            "unsupported_directive_shape",
        ),
        (
            "directives/_general/bundle/approval@synthetic.invalid.md",
            "excluded",
            "directive_bundle_support",
        ),
        (
            "directives/_general/bundle/Synthetic Nested/context.md",
            "excluded",
            "directive_bundle_support",
        ),
        (
            "artifacts/Synthetic Artifact/README.md",
            "held",
            "unclassified_artifact_kind",
        ),
        (
            "artifacts/Synthetic.File.bin",
            "held",
            "unresolved_binary_or_external_owner",
        ),
        (
            "tasks/Synthetic Scope/backlog/work/artifacts/report.md",
            "held",
            "ambiguous_scope_or_collision",
        ),
        (
            "tasks/external-team/backlog/Synthetic Work/artifacts/report.md",
            "excluded",
            "external_backend_owned",
        ),
        (
            "tasks/external-team/backlog/Synthetic Work/artifacts/other.md",
            "excluded",
            "external_backend_owned",
        ),
        (
            "tasks/external-team/backlog/normal-work/artifacts/Synthetic Name.md",
            "excluded",
            "external_backend_owned",
        ),
    ];
    let body = b"---\ndescription: Synthetic fixture\nstatus: proposed\n---\n";
    for path in cases.iter().map(|(path, _, _)| *path).chain([
        "directives/_general/bundle/README.md",
        "artifacts/bundle/output.tar.gz",
        "bots/Synthetic Bot/memory/_hive-userdata/MEMORY.md",
    ]) {
        let file = source.join(path);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, body).unwrap();
    }
    git(&source, &home, &["add", "."]);
    git(
        &source,
        &home,
        &["commit", "-qm", "Synthetic nonportable locators"],
    );
    let commit = git_text(&source, &home, &["rev-parse", "HEAD"]);
    let workspace = id(1);
    let mapping = root.path().join("scope.json");
    fs::write(&mapping, json!({
        "version": 1,
        "source_commit": commit,
        "policy_pin": POLICY_PIN,
        "workspace_id": workspace,
        "scopes": [
            {"scope": "_general", "decision": {
                "kind": "area", "area": {"id": id(AREA_GENERAL), "slug": "general", "name": "General"}
            }},
            {"scope": "_hive-userdata", "decision": {
                "kind": "area", "area": {"id": id(AREA_USERDATA), "slug": "workspace-operations", "name": "Synthetic Userdata"}
            }},
            {"scope": "external-team", "decision": {"kind": "external"}}
        ]
    }).to_string()).unwrap();
    let args = [
        "--source",
        source.to_str().unwrap(),
        "--source-commit",
        &commit,
        "--workspace-id",
        &workspace,
        "--mapping-file",
        mapping.to_str().unwrap(),
        "--include-directives-artifacts",
    ];
    let run = |args| migration_cli(&home, &config, "migration-plan", args, 0);
    let report = run(&args);
    assert_eq!(report, run(&args));
    let scopes = report["data"]["scopes"].as_array().unwrap();
    assert_eq!(
        report["data"]["scopes"],
        run(&args[..args.len() - 1])["data"]["scopes"]
    );
    let scope_digest = sha256_hex(b"Synthetic Scope");
    let scope = scopes
        .iter()
        .find(|row| row["scope_sha256"] == scope_digest)
        .unwrap();
    assert_eq!(
        *scope,
        json!({
            "scope_sha256": scope_digest,
            "source_paths": [],
            "source_path_sha256s": [
                sha256_hex(b"directives/Synthetic Scope"), sha256_hex(b"tasks/Synthetic Scope"),
            ],
            "outcome": {"status": "held", "reason": "ambiguous_scope_or_collision"}
        })
    );
    let userdata = scopes
        .iter()
        .find(|row| row["scope"] == "_hive-userdata")
        .unwrap();
    assert_eq!(
        userdata["source_paths"],
        json!(["directives/_hive-userdata", "tasks/_hive-userdata"])
    );
    assert_eq!(
        userdata["source_path_sha256s"],
        json!([sha256_hex(b"bots/Synthetic Bot/memory/_hive-userdata")])
    );
    assert_eq!(
        userdata["outcome"],
        json!({"status": "held", "reason": "ambiguous_scope_or_collision"})
    );

    let records = report["data"]["directive_artifact_intake"]["records"]
        .as_array()
        .unwrap();
    let blocked = records
        .iter()
        .find(|row| row["source_path"] == "directives/_hive-userdata/unapproved-active.md")
        .unwrap();
    assert_eq!(
        blocked["outcome"],
        json!({"status": "held", "reason_codes": ["ambiguous_scope_or_collision"]})
    );
    let by_digest = |path: &str| {
        let digest = sha256_hex(path.as_bytes());
        records
            .iter()
            .find(|row| row["source_path_sha256"] == digest)
            .unwrap()
    };
    for (path, status, reason) in cases {
        let row = by_digest(path);
        assert!(row.get("source_path").is_none());
        assert_eq!(row["outcome"]["status"], status);
        if status == "held" {
            assert_eq!(row["outcome"]["reason_codes"], json!([reason]));
        } else {
            assert_eq!(row["outcome"]["reason"], reason);
        }
    }
    for (parent, filenames) in [
        ("tasks/Synthetic Scope/backlog/work", &["report.md"][..]),
        (
            "tasks/external-team/backlog/Synthetic Work",
            &["report.md", "other.md"][..],
        ),
    ] {
        for filename in filenames {
            let row = by_digest(&format!("{parent}/artifacts/{filename}"));
            assert!(row.get("parent_task_path").is_none());
            assert_eq!(
                row["parent_task_path_sha256"],
                sha256_hex(parent.as_bytes())
            );
        }
    }
    let normal_parent =
        by_digest("tasks/external-team/backlog/normal-work/artifacts/Synthetic Name.md");
    assert_eq!(
        normal_parent["parent_task_path"],
        "tasks/external-team/backlog/normal-work"
    );
    assert!(normal_parent.get("parent_task_path_sha256").is_none());

    for path in [
        "directives/_general/bundle/README.md",
        "artifacts/bundle/output.tar.gz",
    ] {
        let row = records
            .iter()
            .find(|row| row["source_path"] == path)
            .unwrap();
        assert!(row.get("source_path_sha256").is_none());
        assert_eq!(row["parent_task_path"], Value::Null);
    }
    let output = report.to_string();
    for private in [
        "Synthetic Scope",
        "Synthetic Bundle",
        "record.private.md",
        "approval@synthetic.invalid",
        "Synthetic Nested",
        "Synthetic Artifact",
        "Synthetic.File",
        "Synthetic Work",
        "Synthetic Name",
        "Synthetic Bot",
    ] {
        assert!(!output.contains(private), "{private}");
    }
    assert!(git(&source, &home, &["status", "--porcelain"]).is_empty());
    assert!(!config.exists());
}
