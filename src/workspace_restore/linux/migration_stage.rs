use std::fmt::Write as _;
use std::io::Seek;
use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};
use time::OffsetDateTime;

use super::*;
use crate::cli::{MigrationStageArgs, MigrationStageMode};
use crate::config_git::migration::{Area, ReviewedScopes, with_scope_mapping};
use crate::config_v2::{require_inactive_destination, verify_descriptor};
use crate::dolt::{INITIAL_SCHEMA, issue_writer::new_uuid_v7};
use crate::workspace_init::POLICY_REVISION;

const COUNTS: &str = "SELECT (SELECT COUNT(*) FROM areas) AS areas, \
    (SELECT COUNT(*) FROM issues) AS issues, (SELECT COUNT(*) FROM issue_claims) AS claims, \
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
    let dry_run = matches!(args.mode, MigrationStageMode::DryRun);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| {
            OffsetDateTime::from_unix_timestamp_nanos(elapsed.as_nanos() as i128).ok()
        })
        .ok_or_else(|| HiveError::new(ErrorCode::IoError, "Could not read the migration time."))?;

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
    let (asset_commit, _) = installed.initialize_assets(&assets)?;
    let expected = apply_areas(&installed, &db, &scratch, &areas, now, dry_run)?;

    let confirmed = if dry_run {
        installed.verify_db(&db, &args.workspace_id, Some(&base))?;
        verify_counts(&installed.query(&db, COUNTS)?, 0)?;
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
                "Import reviewed Hive v1 scopes",
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
        verify_counts(&installed.query(&db, COUNTS)?, expected.len())?;
        let rows = installed.query(&db, &format!(
            "SELECT id, parent_id, slug, HEX(name) AS name_hex, description, archived_at, revision \
             FROM areas AS OF '{confirmed}' ORDER BY id"
        ))?;
        if rows != expected {
            return Err(unverified(
                "Committed Areas differ from the reviewed scope rows.",
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

    installed.git(&assets, &["fsck", "--full", "--no-reflogs", &asset_commit])?;
    installed.verify_initial_assets(&assets, &asset_commit)?;
    require_entries(&assets_dir, &[".git"])?;
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
        "issue_number_next_value": 1,
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
    Ok((report, staged))
}

fn apply_areas(
    installed: &Installed<'_>,
    db: &Path,
    scratch: &File,
    areas: &[&Area],
    now: OffsetDateTime,
    dry_run: bool,
) -> Result<Vec<Value>, HiveError> {
    let mut input = tempfile::tempfile_in(fd_path(scratch)).map_err(io_error)?;
    input.write_all(b"START TRANSACTION;\n").map_err(io_error)?;
    let mut expected = BTreeMap::new();
    for area in areas {
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
        expected.insert(&area.id, row);
    }
    let end = if dry_run { "ROLLBACK" } else { "COMMIT" };
    writeln!(input, "{COUNTS};\n{end};").map_err(io_error)?;
    input.rewind().map_err(io_error)?;
    let output = installed.dolt(db, &["sql", "-r", "json"], Some(input))?;
    let mut counts = None;
    for result in serde_json::Deserializer::from_str(&output).into_iter::<Value>() {
        let result =
            result.map_err(|_| unverified("Could not read the migration transaction checks."))?;
        if let Some(rows) = result.get("rows") {
            if counts.is_some() {
                return Err(unverified(
                    "The migration transaction returned unexpected checks.",
                ));
            }
            verify_counts(rows.as_array().map_or(&[], Vec::as_slice), areas.len())?;
            counts = Some(());
        }
    }
    counts.ok_or_else(|| unverified("The migration transaction did not return its checks."))?;
    Ok(expected.into_values().collect())
}

fn verify_counts(rows: &[Value], areas: usize) -> Result<(), HiveError> {
    if rows != [json!({"areas": areas, "issues": 0, "claims": 0, "next_number": 1})] {
        return Err(unverified(
            "Migration Area, Issue, Claim or counter checks failed.",
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
