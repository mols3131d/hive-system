use std::io::{Seek, Write};

use super::*;
use crate::config_git::migration;
use crate::config_v2::verify_descriptor;

const WORKSPACE_ID: &str = "01900000-0000-7000-8000-000000000001";
const SOURCE_POLICY_PIN: &str = "0e584830a7eff314d85b7af8c6f8cd69e735730a";
const TARGET_POLICY_PIN: &str = "c66b3c50150e18426485d134de92c6b311dac125";
const SCOPE_ROWS: &str = "
INSERT INTO areas (id, slug, name, revision)
VALUES ('01900000-0000-7000-8000-000000000010', 'general', 'General',
        '01900000-0000-7000-8000-000000000020');
INSERT INTO areas (id, parent_id, slug, name, revision)
VALUES ('01900000-0000-7000-8000-000000000011', '01900000-0000-7000-8000-000000000010',
        'app', 'Synthetic App', '01900000-0000-7000-8000-000000000021');
";
const COUNTS: &str = "SELECT (SELECT COUNT(*) FROM areas) AS areas,
    (SELECT COUNT(*) FROM issues) AS issues,
    (SELECT COUNT(*) FROM issue_claims) AS claims,
    (SELECT next_value FROM workspace_counters WHERE name = 'issue_number') AS next_number";

#[test]
fn inactive_admission_rejects_overlap_occupied_symlink_and_locked_target() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    let output = root.path().join("output");
    let alias = root.path().join("alias");
    fs::create_dir(&source).unwrap();
    fs::create_dir(&output).unwrap();
    std::os::unix::fs::symlink(&output, &alias).unwrap();

    for path in [
        source.clone(),
        source.join("nested"),
        root.path().to_owned(),
        alias,
    ] {
        assert!(matches!(
            InactiveTarget::inspect(&source, &path),
            Err(HiveError {
                code: ErrorCode::InvalidArgument,
                ..
            })
        ));
    }
    fs::write(output.join("existing"), "keep").unwrap();
    assert!(matches!(
        InactiveTarget::inspect(&source, &output),
        Err(HiveError {
            code: ErrorCode::AlreadyExists,
            ..
        })
    ));
    fs::remove_file(output.join("existing")).unwrap();
    let (_held, _) = InactiveTarget::inspect(&source, &output).unwrap();
    assert!(matches!(
        InactiveTarget::inspect(&source, &output),
        Err(HiveError {
            code: ErrorCode::ConcurrentChange,
            ..
        })
    ));
}

#[test]
#[ignore = "requires installed Dolt 2.7.6 and native Git"]
fn scope_transaction_rollback_failure_and_committed_inactive_publication() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    let output = root.path().join("output");
    fs::create_dir(&source).unwrap();
    fs::create_dir(&output).unwrap();
    let output_inode = fs::metadata(&output).unwrap().ino();
    let source = ScopedDirectory::open_absolute(&source).unwrap();
    let (target, _) = InactiveTarget::inspect(&source.path, &output).unwrap();
    let (_stage, stage) = target.stage(WORKSPACE_ID).unwrap();
    let scratch = create_private_directory(&fd_path(&stage).join("scratch")).unwrap();
    let installed = Installed::check(&scratch).unwrap();
    let git = |args: &[&str]| installed.git(&source.path, args).unwrap();
    git(&["init", "--initial-branch=main", "--template="]);
    fs::write(
        source.path.join("AGENTS.md"),
        format!("# Synthetic v1 workspace\n\n[Policy](https://github.com/mols3131d/hive-system/blob/{SOURCE_POLICY_PIN}/docs/policies/workspace/README.md)\n"),
    ).unwrap();
    fs::write(
        source.path.join("hive.yml"),
        "version: 1\ncoordination:\n  mode: local\n",
    )
    .unwrap();
    for (scope, name) in [("_general", "General"), ("app", "Synthetic App")] {
        let dir = source.path.join("tasks").join(scope);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("README.md"),
            format!("---\ndescription: {name}\n---\n# {name}\n"),
        )
        .unwrap();
    }
    git(&["add", "."]);
    git(&["commit", "-m", "Synthetic scope fixture"]);
    let head = git(&["rev-parse", "HEAD"]);
    let revision = head.trim();
    let index_before = fs::read(source.path.join(".git/index")).unwrap();
    let before = migration::preflight(&source.path, revision).unwrap();
    assert_eq!(before["source"]["policy_pin"], SOURCE_POLICY_PIN);
    let verify_originals = || {
        assert_eq!(
            migration::preflight(&source.path, revision).unwrap(),
            before
        );
        assert_eq!(
            fs::read(source.path.join(".git/index")).unwrap(),
            index_before
        );
        assert_eq!(fs::metadata(&output).unwrap().ino(), output_inode);
        assert!(fs::read_dir(&output).unwrap().next().is_none());
    };

    let payload = fd_path(&stage).join("workspace");
    let payload_dir = create_private_directory(&payload).unwrap();
    lock_target(&payload_dir).unwrap();
    let db = payload.join("db");
    let assets = payload.join("assets");
    create_private_directory(&db).unwrap();
    create_private_directory(&assets).unwrap();
    create_private_directory(&payload.join("conversations")).unwrap();
    let (base, _) = installed.initialize_db(&db, WORKSPACE_ID).unwrap();
    let (asset_commit, _) = installed.initialize_assets(&assets).unwrap();
    let counts = |areas| json!({"areas": areas, "issues": 0, "claims": 0, "next_number": 1});
    let query = |sql| installed.query(&db, sql).unwrap();
    let run_sql = |sql: String| {
        let mut input = tempfile::tempfile_in(fd_path(&scratch)).unwrap();
        input.write_all(sql.as_bytes()).unwrap();
        input.rewind().unwrap();
        installed.dolt(&db, &["sql", "-r", "json"], Some(input))
    };
    let verify_rollback = || {
        installed.verify_db(&db, WORKSPACE_ID, Some(&base)).unwrap();
        assert_eq!(query(COUNTS), [counts(0)]);
        assert_eq!(
            installed
                .git(&assets, &["rev-parse", "HEAD"])
                .unwrap()
                .trim(),
            asset_commit
        );
        verify_originals();
    };

    let within = run_sql(format!(
        "START TRANSACTION; {SCOPE_ROWS} {COUNTS}; ROLLBACK;"
    ))
    .unwrap();
    let results = serde_json::Deserializer::from_str(&within)
        .into_iter::<Value>()
        .map(Result::unwrap)
        .filter(|result| result.get("rows").is_some())
        .collect::<Vec<_>>();
    assert_eq!(results, [json!({"rows": [counts(2)]})]);
    verify_rollback();

    // A second copy of the same inserts fails on a primary key before COMMIT.
    let failure = run_sql(format!(
        "START TRANSACTION; {SCOPE_ROWS} {SCOPE_ROWS} COMMIT;"
    ))
    .unwrap_err();
    assert!(matches!(failure.code, ErrorCode::IoError));
    verify_rollback();

    run_sql(format!("START TRANSACTION; {SCOPE_ROWS} COMMIT;")).unwrap();
    installed
        .dolt(
            &db,
            &[
                "commit",
                "-a",
                "--author",
                "Hive <bootstrap@hive.invalid>",
                "-m",
                "Stage synthetic scopes",
            ],
            None,
        )
        .unwrap();
    let confirmed = verify_db(&installed, &db, WORKSPACE_ID).unwrap();
    assert_ne!(confirmed, base);
    assert_eq!(
        query("SELECT COUNT(*) AS commits FROM dolt_log"),
        [json!({"commits": 3})]
    );
    verify_staged_db(&installed, &db, WORKSPACE_ID, &confirmed).unwrap();
    assert_eq!(query(COUNTS), [counts(2)]);
    assert_eq!(
        query(&format!(
            "SELECT slug, parent_id FROM areas AS OF '{confirmed}' ORDER BY slug"
        )),
        [
            json!({"slug": "app", "parent_id": "01900000-0000-7000-8000-000000000010"}),
            json!({"slug": "general"})
        ],
    );
    installed
        .git(&assets, &["fsck", "--full", "--no-reflogs", &asset_commit])
        .unwrap();
    assert!(
        installed
            .git(&assets, &["ls-tree", asset_commit.as_str()])
            .unwrap()
            .is_empty()
    );
    assert!(
        installed
            .git(&assets, &["status", "--porcelain", "--untracked-files=all"])
            .unwrap()
            .is_empty()
    );
    let queens = validate_restored_records(&installed, &db, &confirmed, WORKSPACE_ID, |path| {
        installed.read_committed_soul(&assets, &asset_commit, path)
    })
    .unwrap();
    assert!(queens.is_empty());

    verify_bootstrap_layout(&payload_dir).unwrap();
    let descriptor = format!(
        "version: 2\nworkspace_id: {WORKSPACE_ID}\nschema_version: {STORAGE_SCHEMA_VERSION}\npolicy_revision: {TARGET_POLICY_PIN}\nbootstrap_layout: {BOOTSTRAP_LAYOUT}\n"
    );
    verify_descriptor(descriptor.as_bytes()).unwrap();
    private_file(&payload.join(MARKER))
        .unwrap()
        .write_all(descriptor.as_bytes())
        .unwrap();
    sync_tree_with_links(&payload_dir, Path::new(""), &HashSet::new()).unwrap();
    verify_originals();
    target.publish(WORKSPACE_ID, &stage, &payload_dir).unwrap();
    target.verify_inactive(WORKSPACE_ID).unwrap();
    assert_eq!(
        migration::preflight(&source.path, revision).unwrap(),
        before
    );
    let published = ScopedDirectory::open_absolute(&output).unwrap();
    verify_bootstrap_layout(published.directory_file()).unwrap();
    assert_eq!(fs::read_to_string(output.join(MARKER)).unwrap(), descriptor);
    assert_eq!(
        verify_db(&installed, &output.join("db"), WORKSPACE_ID).unwrap(),
        confirmed
    );
    assert_eq!(
        installed
            .git(&output.join("assets"), &["rev-parse", "HEAD"])
            .unwrap()
            .trim(),
        asset_commit
    );
    assert!(
        fs::read_dir(output.join("conversations"))
            .unwrap()
            .next()
            .is_none()
    );
}
