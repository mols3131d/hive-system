use std::fmt::Write as _;
use std::io::Seek;
use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};
use time::OffsetDateTime;

use super::*;
use crate::cli::{MigrationStageArgs, MigrationStageMode};
use crate::config_git::migration::prepared_records::PreparedRecord;
use crate::config_git::migration::{
    Area, PreparedProjects, PreparedQueen, PreparedResources, ReviewedScopes, with_scope_mapping,
};
use crate::config_v2::{require_inactive_destination, verify_descriptor};
use crate::dolt::{INITIAL_SCHEMA, issue_writer::new_uuid_v7};
use crate::workspace_init::POLICY_REVISION;

#[path = "migration_directive_artifact.rs"]
mod directive_artifact;
mod projects;
mod resources;
mod tasks;

use projects::{IssueIds, StagedProjects};
use resources::StagedResources;
use tasks::{COUNTER, TaskImport};

const COUNTS: &str = "SELECT (SELECT COUNT(*) FROM areas) AS areas, \
    (SELECT COUNT(*) FROM bots) AS bots, (SELECT COUNT(*) FROM bot_memories) AS memories, \
    (SELECT COUNT(*) FROM issues) AS issues, (SELECT COUNT(*) FROM issue_claims) AS claims, \
    (SELECT COUNT(*) FROM issue_dependencies) AS dependencies, \
    (SELECT next_value FROM workspace_counters WHERE name = 'issue_number') AS next_number";

struct StagedPayload {
    stage: File,
    workspace: File,
    _cleanup: tempfile::TempDir,
}

pub(crate) fn run(args: &MigrationStageArgs) -> Result<Value, HiveError> {
    if !args.source.is_absolute()
        || args
            .source
            .components()
            .any(|part| part == Component::ParentDir)
        || !args.target.is_absolute()
    {
        return Err(invalid(
            "Select absolute source and target paths without parent traversal.",
        ));
    }
    if args.policy_revision != POLICY_REVISION {
        return Err(HiveError::new(
            ErrorCode::IncompatiblePolicy,
            "Select the reviewed v2 workspace initialization policy revision.",
        ));
    }
    let (target, _) = InactiveTarget::inspect(&args.source, &args.target)?;
    if target.original.is_some() {
        return Err(HiveError::new(
            ErrorCode::AlreadyExists,
            "The migration target must be absent.",
        ));
    }

    let (report, staged) = with_scope_mapping(
        &args.source,
        &args.source_commit,
        &args.workspace_id,
        &args.mapping_file,
        |reviewed| prepare(args, &target, &reviewed),
    )?;
    // The verified source callback rechecks source HEAD, identity, index and
    // worktree after all expensive preparation and before this rename.
    require_inactive_destination(&target.path)?;
    if let Some(staged) = staged {
        target.publish(&args.workspace_id, &staged.stage, &staged.workspace)?;
    } else {
        target.check_destination(&args.workspace_id)?;
    }
    Ok(report)
}

fn prepare(
    args: &MigrationStageArgs,
    target: &InactiveTarget,
    reviewed: &ReviewedScopes<'_>,
) -> Result<(Value, Option<StagedPayload>), HiveError> {
    let areas = reviewed.areas_in_parent_order()?;
    let queens = reviewed.prepared_queens();
    let intake = reviewed.directive_artifacts_for_staging()?;
    let dry_run = matches!(args.mode, MigrationStageMode::DryRun);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| {
            OffsetDateTime::from_unix_timestamp_nanos(elapsed.as_nanos() as i128).ok()
        })
        .ok_or_else(|| HiveError::new(ErrorCode::IoError, "Could not read the migration time."))?;

    let task_plan = args.tasks.then(|| reviewed.prepare_tasks()).transpose()?;
    let project_plan = reviewed.prepare_projects(task_plan.as_ref())?;
    let task_import = task_plan
        .as_ref()
        .map(|plan| TaskImport::new(plan.staging_tasks(), now))
        .transpose()?;
    let issue_count = task_import.as_ref().map_or(0, TaskImport::len);

    let issue_ids = project_plan.as_ref().map(|_| {
        task_import
            .iter()
            .flat_map(TaskImport::assigned_issues)
            .collect()
    });
    let rows = ImportRows {
        areas: &areas,
        queens,
        resources: reviewed.resources.as_ref(),
        tasks: task_import.as_ref(),
        projects: project_plan.as_ref().zip(issue_ids.as_ref()),
        records: intake
            .as_ref()
            .map_or(&[], |intake| intake.records.as_slice()),
    };

    require_inactive_destination(&target.path)?;
    let (cleanup, stage) = target.stage(&args.workspace_id)?;
    let stage_path = fd_path(&stage);
    let scratch = create_private_directory(&stage_path.join("scratch"))?;
    let payload = stage_path.join("workspace");
    let workspace = create_private_directory(&payload)?;
    lock_target(&workspace)?;
    let db = payload.join("db");
    let assets = payload.join("assets");
    create_private_directory(&db)?;
    let assets_dir = create_private_directory(&assets)?;
    let conversations = create_private_directory(&payload.join("conversations"))?;
    let installed = Installed::check(&scratch)?;
    let (base, _) = installed.initialize_db(&db, &args.workspace_id)?;
    let (asset_base, _) = installed.initialize_assets(&assets)?;
    let asset_commit = stage_souls(&installed, &assets, &assets_dir, &asset_base, queens)?;
    let expected = apply_rows(&installed, &db, &scratch, &rows, now, dry_run)?;

    let confirmed = if dry_run {
        installed.verify_db(&db, &args.workspace_id, Some(&base))?;
        verify_rows(&installed.query(&db, COUNTS)?, &counts(0, 0, 0, 0))?;
        verify_rows(&installed.query(&db, COUNTER)?, &counter(0))?;
        StagedResources::verify_rollback(&installed, &db)?;
        StagedProjects::verify_rollback(&installed, &db)?;
        directive_artifact::Expected::verify_empty(&installed, &db)?;
        None
    } else {
        installed.dolt(
            &db,
            &[
                "commit",
                "-a",
                "--allow-empty",
                "--author",
                "Hive <bootstrap@hive.invalid>",
                "-m",
                match (
                    args.tasks,
                    queens.is_empty(),
                    rows.resources.is_some(),
                    intake.is_some() || rows.projects.is_some(),
                ) {
                    (false, true, false, false) => "Import reviewed Hive v1 scopes",
                    (true, true, false, false) => "Import reviewed Hive v1 scopes and Tasks",
                    (false, false, false, false) => "Import reviewed Hive v1 scopes and Queens",
                    _ => "Import reviewed Hive v1 records",
                },
            ],
            None,
        )?;
        let confirmed = verify_staged_db(&installed, &db, &args.workspace_id, None)?;
        if confirmed == base
            || installed.query(&db, "SELECT DOLT_HASHOF('HEAD^') AS parent")?
                != [json!({"parent": base})]
        {
            return Err(unverified(
                "The staged Dolt commit must extend the migration baseline.",
            ));
        }
        // verify_staged_db pins HEAD to the newly created clean snapshot.
        verify_rows(&installed.query(&db, COUNTS)?, &expected.counts)?;
        verify_rows(
            &installed.query(&db, &format!(
                "SELECT name, next_value FROM workspace_counters AS OF '{confirmed}' ORDER BY name"
            ))?,
            &counter(issue_count),
        )?;
        if let Some(import) = &task_import {
            for (sql, rows) in import.checks(Some(&confirmed)) {
                verify_rows(&installed.query(&db, &sql)?, rows)?;
            }
        }
        expected
            .resources
            .verify_committed(&installed, &db, &confirmed)?;
        for (sql, rows) in expected.projects.checks(Some(&confirmed)) {
            verify_rows(&installed.query(&db, &sql)?, &rows)?;
        }
        expected
            .records
            .verify_committed(&installed, &db, &confirmed)?;
        let rows = installed.query(&db, &format!(
            "SELECT id, parent_id, slug, HEX(name) AS name_hex, description, archived_at, revision \
             FROM areas AS OF '{confirmed}' ORDER BY id"
        ))?;
        if rows != expected.areas {
            return Err(unverified(
                "Committed Areas differ from the reviewed scope rows.",
            ));
        }
        let rows = installed.query(
            &db,
            &format!(
                "SELECT id, slug, HEX(display_name) AS name_hex, retired_at, revision \
             FROM bots AS OF '{confirmed}' ORDER BY id"
            ),
        )?;
        if rows != expected.bots {
            return Err(unverified(
                "Committed Bots differ from the reviewed Queen registry rows.",
            ));
        }
        validate_restored_records(&installed, &db, &confirmed, &args.workspace_id, |path| {
            installed.read_committed_soul(&assets, &asset_commit, path)
        })
        .map_err(|error| {
            HiveError::new(error.code, "Committed migration record validation failed.")
        })?;
        Some(confirmed)
    };

    verify_asset_commit(
        &installed,
        &assets,
        &assets_dir,
        &asset_base,
        &asset_commit,
        queens,
    )?;
    require_entries(&conversations, &[])?;
    let staged = if dry_run {
        None
    } else {
        verify_bootstrap_layout(&workspace)?;
        let descriptor = format!(
            "version: 2\nworkspace_id: {}\nschema_version: {STORAGE_SCHEMA_VERSION}\npolicy_revision: {POLICY_REVISION}\nbootstrap_layout: {BOOTSTRAP_LAYOUT}\n",
            args.workspace_id,
        );
        verify_descriptor(descriptor.as_bytes())?;
        private_file(&payload.join(MARKER))?
            .write_all(descriptor.as_bytes())
            .map_err(io_error)?;
        sync_tree_with_links(&workspace, Path::new(""), &HashSet::new())?;
        Some(StagedPayload {
            stage,
            workspace,
            _cleanup: cleanup,
        })
    };

    let mut report = reviewed.staging_evidence();
    report["mode"] = json!(if dry_run { "dry_run" } else { "staged_import" });
    report["observed_at"] = json!(format!(
        "{}T{:02}:{:02}:{:02}Z",
        now.date(),
        now.hour(),
        now.minute(),
        now.second()
    ));
    report["target"] = json!({
        "workspace_id": args.workspace_id,
        "policy_revision": POLICY_REVISION,
        "dolt_version": DOLT_VERSION,
        "schema_version": STORAGE_SCHEMA_VERSION,
        "migration_digest": format!("{:x}", Sha256::digest(INITIAL_SCHEMA)),
        "base_dolt_commit": base,
        "confirmed_dolt_commit": confirmed,
        "asset_git_commit": asset_commit,
        "issue_number_next_value": if dry_run { 1 } else { issue_count + 1 },
        "active_claims": 0,
    });
    report["checks"] = json!({
        "source_unchanged": true,
        "area_ancestry": "verified",
        "transaction": if dry_run { "rolled_back" } else { "committed" },
        "committed_record_validation": if dry_run { "not_run" } else { "passed" },
        "asset_commit_verified": true,
        "published_inactive": !dry_run,
        "target_area_rows": if dry_run { 0 } else { areas.len() },
    });
    if report.get("queen_preparation").is_some() {
        report["checks"]["target_bot_rows"] = json!(if dry_run { 0 } else { queens.len() });
        report["counts"]["prepared_queens"] = json!(queens.len());
    }
    if let (Some(plan), Some(import)) = (&task_plan, &task_import) {
        let mut ledger = plan.staging_ledger();
        report["coverage"] = json!("scopes_and_tasks");
        report["counts"]["tasks"] = ledger["counts"].take();
        report["id_map"]
            .as_array_mut()
            .expect("scope ID map")
            .extend(import.id_map());
        for key in ["holds", "exclusions"] {
            let entries = ledger[key].as_array_mut().expect("Task ledger entries");
            for entry in entries.iter_mut() {
                entry["kind"] = json!("task");
            }
            report[key]
                .as_array_mut()
                .expect("scope ledger entries")
                .append(entries);
        }
        report["checks"]["task_rows_and_links"] = json!("verified");
        report["checks"]["target_issue_rows"] = json!(if dry_run { 0 } else { issue_count });
        report["target"]["projected_issue_number_next_value"] = json!(issue_count + 1);
    }
    expected
        .resources
        .add_report(&mut report, reviewed.resources.as_ref(), dry_run);
    if let Some((projects, ids)) = rows.projects {
        expected
            .projects
            .add_report(&mut report, projects, ids, dry_run);
    }
    let include_reviewed_records = intake.is_some();
    if let Some(intake) = intake {
        directive_artifact::add_report(&mut report, intake);
        let (directives, artifacts) = expected.records.counts();
        report["checks"]["target_directive_rows"] = json!(if dry_run { 0 } else { directives });
        report["checks"]["target_artifact_rows"] = json!(if dry_run { 0 } else { artifacts });
        report["checks"]["target_artifact_links"] = json!(0);
        if let Some(observed) = reviewed.directive_observation() {
            report["directive_observed_at"] = json!(observed);
        }
    }
    let mut surfaces = Vec::new();
    for (selected, label) in [
        (args.tasks, "tasks"),
        (reviewed.resources.is_some(), "resources"),
        (project_plan.is_some(), "projects"),
        (report.get("queen_preparation").is_some(), "queen_souls"),
        (include_reviewed_records, "directives_artifacts"),
    ] {
        if selected {
            surfaces.push(label);
        }
    }
    if surfaces.len() > 1 {
        report["coverage"] = json!("mixed");
        report["included_surfaces"] = json!(surfaces);
    }
    Ok((report, staged))
}

fn stage_souls(
    installed: &Installed<'_>,
    assets: &Path,
    assets_dir: &File,
    base: &str,
    queens: &[PreparedQueen],
) -> Result<String, HiveError> {
    if queens.is_empty() {
        return Ok(base.to_owned());
    }
    let bots = create_private_directory(&fd_path(assets_dir).join("bots"))?;
    for queen in queens {
        let directory = create_private_directory(&fd_path(&bots).join(&queen.registry.slug))?;
        private_file(&fd_path(&directory).join("SOUL.md"))?
            .write_all(&queen.soul_bytes)
            .map_err(io_error)?;
    }
    installed.git(assets, &["-c", "core.autocrlf=false", "add", "--", "bots"])?;
    installed.git(
        assets,
        &[
            "commit",
            "--no-gpg-sign",
            "-m",
            "Import reviewed Hive v1 Queen Souls",
        ],
    )?;
    let commit = installed.git(assets, &["rev-parse", "--verify", "HEAD^{commit}"])?;
    let commit = commit.trim();
    verify_asset_commit(installed, assets, assets_dir, base, commit, queens)?;
    Ok(commit.to_owned())
}

fn verify_asset_commit(
    installed: &Installed<'_>,
    assets: &Path,
    assets_dir: &File,
    base: &str,
    commit: &str,
    queens: &[PreparedQueen],
) -> Result<(), HiveError> {
    let invalid_assets =
        || unverified("The staged Git asset commit differs from the reviewed Queen Souls.");
    if commit.len() != 40 || !crate::config_git::is_full_lowercase_oid(commit) {
        return Err(invalid_assets());
    }
    installed.git(assets, &["fsck", "--full", "--no-reflogs", commit])?;
    if queens.is_empty() {
        installed.verify_initial_assets(assets, base)?;
        return require_entries(assets_dir, &[".git"]);
    }
    if installed.git(assets, &["symbolic-ref", "HEAD"])?.trim() != "refs/heads/main"
        || installed
            .git(assets, &["rev-list", "--parents", "-n", "1", "HEAD"])?
            .trim()
            != format!("{commit} {base}")
        || !installed
            .git(assets, &["status", "--porcelain", "--untracked-files=all"])?
            .is_empty()
        || !installed.git(assets, &["remote"])?.is_empty()
    {
        return Err(invalid_assets());
    }

    require_entries(assets_dir, &[".git", "bots"])?;
    let bots = open_target(&fd_path(assets_dir).join("bots"))?.ok_or_else(invalid_assets)?;
    let slugs = queens
        .iter()
        .map(|queen| queen.registry.slug.as_str())
        .collect::<Vec<_>>();
    require_entries(&bots, &slugs)?;
    let tree = installed.git(assets, &["ls-tree", "-rzt", "--full-tree", commit])?;
    let mut expected = BTreeMap::from([("bots".to_owned(), "040000 tree ")]);
    for slug in slugs {
        expected.insert(format!("bots/{slug}"), "040000 tree ");
        expected.insert(format!("bots/{slug}/SOUL.md"), "100644 blob ");
    }
    for entry in tree.split_terminator('\0') {
        let (metadata, path) = entry.split_once('\t').ok_or_else(invalid_assets)?;
        if !expected
            .remove(path)
            .is_some_and(|kind| metadata.starts_with(kind))
        {
            return Err(invalid_assets());
        }
    }
    if !expected.is_empty() {
        return Err(invalid_assets());
    }
    for queen in queens {
        let slug = &queen.registry.slug;
        let directory = open_target(&fd_path(&bots).join(slug))?.ok_or_else(invalid_assets)?;
        require_entries(&directory, &["SOUL.md"])?;
        let metadata =
            fs::symlink_metadata(fd_path(&directory).join("SOUL.md")).map_err(io_error)?;
        if !metadata.is_file() || metadata.mode() & 0o777 != 0o600 {
            return Err(invalid_assets());
        }
        let bytes =
            installed.read_committed_soul(assets, commit, &format!("bots/{slug}/SOUL.md"))?;
        if bytes != queen.soul_bytes
            || format!("{:x}", Sha256::digest(&bytes)) != queen.source_raw_sha256
        {
            return Err(invalid_assets());
        }
    }
    Ok(())
}

struct ImportRows<'a> {
    areas: &'a [&'a Area],
    queens: &'a [PreparedQueen],
    resources: Option<&'a PreparedResources<'a>>,
    tasks: Option<&'a TaskImport<'a>>,
    projects: Option<(&'a PreparedProjects<'a>, &'a IssueIds<'a>)>,
    records: &'a [PreparedRecord],
}

struct Expected {
    areas: Vec<Value>,
    bots: Vec<Value>,
    counts: [Value; 1],
    resources: StagedResources,
    projects: StagedProjects,
    records: directive_artifact::Expected,
}

fn apply_rows(
    installed: &Installed<'_>,
    db: &Path,
    scratch: &File,
    rows: &ImportRows<'_>,
    now: OffsetDateTime,
    dry_run: bool,
) -> Result<Expected, HiveError> {
    let mut input = tempfile::tempfile_in(fd_path(scratch)).map_err(io_error)?;
    input.write_all(b"START TRANSACTION;\n").map_err(io_error)?;
    writeln!(input, "{COUNTS};\n{COUNTER};").map_err(io_error)?;
    let mut checks = vec![counts(0, 0, 0, 0).to_vec(), counter(0).to_vec()];
    let mut areas = BTreeMap::new();
    for area in rows.areas {
        let revision = new_uuid_v7(now)?;
        let parent = area
            .parent_id
            .as_deref()
            .map_or_else(|| "NULL".to_owned(), sql_text);
        let name_hex = hex(&area.name);
        // The 64 KiB mapping and Area field limits bound this script. All
        // strings reach SQL through hex literals, including the reviewed name.
        writeln!(input,
            "INSERT INTO areas (id, parent_id, slug, name, revision) VALUES ({}, {parent}, {}, CONVERT(X'{name_hex}' USING utf8mb4), {});",
            sql_text(&area.id), sql_text(&area.slug), sql_text(&revision),
        ).map_err(io_error)?;
        let mut row =
            json!({"id": area.id, "slug": area.slug, "name_hex": name_hex, "revision": revision});
        if let Some(parent) = &area.parent_id {
            row["parent_id"] = json!(parent);
        }
        areas.insert(&area.id, row);
    }
    let mut bots = BTreeMap::new();
    for queen in rows.queens {
        let registry = &queen.registry;
        let revision = new_uuid_v7(now)?;
        let name_hex = hex(&registry.display_name);
        writeln!(input,
            "INSERT INTO bots (id, slug, display_name, revision) VALUES ({}, {}, CONVERT(X'{name_hex}' USING utf8mb4), {});",
            sql_text(&registry.id), sql_text(&registry.slug), sql_text(&revision),
        ).map_err(io_error)?;
        bots.insert(&registry.id, json!({
            "id": registry.id, "slug": registry.slug, "name_hex": name_hex, "revision": revision,
        }));
    }
    let resources = StagedResources::append(&mut input, rows.resources, now)?;
    let (issues, dependencies) = if let Some(tasks) = rows.tasks {
        tasks.append_sql(&mut input)?;
        for (sql, expected) in tasks.checks(None) {
            writeln!(input, "{sql};").map_err(io_error)?;
            checks.push(expected.to_vec());
        }
        (tasks.len(), tasks.dependency_count())
    } else {
        (0, 0)
    };
    let projects = StagedProjects::append(&mut input, rows.projects, now)?;
    for (sql, expected) in projects.transaction_checks() {
        writeln!(input, "{sql};").map_err(io_error)?;
        checks.push(expected);
    }
    let records = directive_artifact::append(&mut input, rows.records, now)?;
    for (sql, expected) in records.transaction_checks() {
        writeln!(input, "{sql};").map_err(io_error)?;
        checks.push(expected);
    }
    let expected_counts = counts(areas.len(), bots.len(), issues, dependencies);
    let end = if dry_run { "ROLLBACK" } else { "COMMIT" };
    writeln!(
        input,
        "{COUNTS};\n{COUNTER};\n{};\n{end};",
        resources::COUNTS
    )
    .map_err(io_error)?;
    checks.extend([expected_counts.to_vec(), counter(issues).to_vec()]);
    input.rewind().map_err(io_error)?;
    let output = installed.dolt(db, &["sql", "-r", "json"], Some(input))?;
    // Dolt emits empty JSON objects for statements such as START TRANSACTION
    // and ROLLBACK. Only the SELECT results belong to the expected checks.
    let mut results = serde_json::Deserializer::from_str(&output)
        .into_iter::<Value>()
        .filter(|result| !matches!(result, Ok(Value::Object(fields)) if fields.is_empty()));
    for expected in &checks {
        verify_rows(&transaction_rows(&mut results)?, expected)?;
    }
    resources.verify_transaction_counts(&transaction_rows(&mut results)?)?;
    if results.next().is_some() {
        return Err(unverified(
            "The migration transaction returned unexpected checks.",
        ));
    }
    Ok(Expected {
        areas: areas.into_values().collect(),
        bots: bots.into_values().collect(),
        counts: expected_counts,
        resources,
        projects,
        records,
    })
}

fn transaction_rows(
    results: &mut impl Iterator<Item = Result<Value, serde_json::Error>>,
) -> Result<Vec<Value>, HiveError> {
    match results.next() {
        Some(Ok(Value::Object(mut result))) => match result.remove("rows") {
            Some(Value::Array(rows)) => Ok(rows),
            _ => Err(unverified(
                "Could not read the migration transaction check rows.",
            )),
        },
        _ => Err(unverified(
            "Could not read all migration transaction checks.",
        )),
    }
}

fn counts(areas: usize, bots: usize, issues: usize, dependencies: usize) -> [Value; 1] {
    [
        json!({"areas": areas, "bots": bots, "memories": 0, "issues": issues,
        "dependencies": dependencies, "claims": 0, "next_number": issues + 1}),
    ]
}

fn counter(issues: usize) -> [Value; 1] {
    [json!({"name": "issue_number", "next_value": issues + 1})]
}

fn verify_rows(rows: &[Value], expected: &[Value]) -> Result<(), HiveError> {
    if rows != expected {
        return Err(unverified(
            "Migration row, link, Claim or Issue counter checks failed.",
        ));
    }
    Ok(())
}

fn hex(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len() * 2);
    for byte in value.bytes() {
        write!(encoded, "{byte:02X}").expect("format hex into a string");
    }
    encoded
}

fn sql_text(value: &str) -> String {
    format!("CONVERT(X'{}' USING utf8mb4)", hex(value))
}
