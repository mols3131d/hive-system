use super::{
    AREA_APP, AREA_GENERAL, AREA_USERDATA, POLICY_PIN, Value, fs, git, git_text, id, init_source,
    json, migration_cli, sha256_hex,
};

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

    let json = report.to_string();
    for private in [
        "synthetic-private-description",
        "Synthetic private heading",
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
