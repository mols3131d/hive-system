use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};

use mysql::Conn;
use mysql::prelude::Queryable;
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::{ADMIN, Fixture, id, storage_state};

const POLICY_PIN: &str = "0e584830a7eff314d85b7af8c6f8cd69e735730a";
const REVISION: u32 = 4;
const AREA_GENERAL: u32 = 0x101;
const AREA_USERDATA: u32 = 0x102;
const AREA_APP: u32 = 0x103;
const PROJECT_RELEASE: u32 = 0x201;
const RESOURCE_REPO: u32 = 0x301;
const DIRECTIVE_V1: u32 = 0x401;
const DIRECTIVE_V2: u32 = 0x402;
const ARTIFACT_REPORT: u32 = 0x501;
const BOT_QUEEN: u32 = 0x601;

// Explicit synthetic manifest mapping supplying reviewed author_ref for positive artifacts only.
// Artifacts absent from this map (even if prose mentions a name) are held as incomplete_historical_provenance.
const REVIEWED_ARTIFACT_AUTHORS: &[(&str, &str)] = &[(
    "tasks/synth-app/backlog/implement-widget/artifacts/design-note.md",
    "synthetic-author",
)];

// Compact synthetic v1 workspace fixture conforming to pinned frontmatter (0e58483).
// Intentionally deficient records are marked in comments and descriptions.
const SYNTHETIC_V1_FILES: &[(&str, &str)] = &[
    (
        "AGENTS.md",
        "# Synthetic v1 workspace\n\n[Workspace policy](https://github.com/mols3131d/hive-system/blob/0e584830a7eff314d85b7af8c6f8cd69e735730a/docs/policies/workspace/README.md)\n",
    ),
    ("hive.yml", "version: 1\ncoordination:\n  mode: local\n"),
    (
        "tasks/_general/README.md",
        "---\ndescription: \"Synthetic reserved general scope.\"\n---\n# General\n",
    ),
    (
        "tasks/_hive-userdata/README.md",
        "---\ndescription: \"Synthetic reserved workspace operations scope.\"\n---\n# Workspace Operations\n",
    ),
    (
        "tasks/synth-app/README.md",
        "---\ndescription: \"Synthetic ordinary application scope.\"\n---\n# Synthetic App\n\nProject: [Synthetic App Repo](https://forge.example/synthetic/app)\n",
    ),
    // Intentionally deficient: legacy pre-split _hive scope.
    (
        "tasks/_hive/README.md",
        "---\ndescription: \"Intentionally unsplit legacy scope fixture.\"\n---\n# Legacy Pre-Split\n",
    ),
    (
        "tasks/external-team/README.md",
        "---\ndescription: \"External Notion-owned scope excluded from Dolt.\"\n---\n# External Team\n",
    ),
    (
        "tasks/synth-app/backlog/implement-widget/README.md",
        "---\ndescription: \"Synthetic backlog task.\"\npriority: high\n---\n# Implement synthetic widget\n\nCreated: 2026-01-02 00:00:00\nUpdated: 2026-01-02 01:00:00\n",
    ),
    (
        "tasks/_general/archive/close-checklist/README.md",
        "---\ndescription: \"Synthetic archived closed task.\"\ntitle: \"Close synthetic checklist\"\nstatus: done\n---\nCreated: 2026-01-01 00:00:00\nUpdated: 2026-01-01 02:00:00\nClosed: 2026-01-01 03:00:00\nAccepted by: synthetic-reviewer\nOutcome: synthetic checklist verified\n",
    ),
    // Intentionally deficient: active done task missing Closed / Accepted by / Outcome.
    (
        "tasks/_hive-userdata/active/unproven-done/README.md",
        "---\ndescription: \"Intentionally deficient done task missing closure provenance.\"\ntitle: \"Undated synthetic done\"\nstatus: done\n---\nCreated: 2026-01-03 00:00:00\nUpdated: 2026-01-03 01:00:00\n",
    ),
    (
        "directives/README.md",
        "---\ndescription: \"Synthetic directives root navigation.\"\n---\n# Directives Index\n",
    ),
    (
        "directives/synth-app/README.md",
        "---\ndescription: \"Synthetic application directives navigation.\"\n---\n# App Directives\n",
    ),
    (
        "directives/synth-app/v1-rule.md",
        "---\ndescription: \"Superseded synthetic rule replaced by v2-rule.\"\ntitle: \"Synthetic Rule v1\"\nstatus: superseded\n---\n## Metadata\n\nIssuer: synthetic-owner\nSource: synthetic-policy-v1\nReplaced by: synth-app/v2-rule\nCreated: 2026-01-01 00:00:00\nUpdated: 2026-01-02 00:00:00\n\n## Direction\n\nOld synthetic rule body.\n",
    ),
    (
        "directives/synth-app/v2-rule.md",
        "---\ndescription: \"Active synthetic rule replacing v1-rule.\"\ntitle: \"Synthetic Rule v2\"\nstatus: active\n---\n## Metadata\n\nIssuer: synthetic-owner\nSource: synthetic-policy-v2\nApproval: synthetic-approval-42\nCreated: 2026-01-02 00:00:00\nUpdated: 2026-01-02 00:00:00\n\n## Direction\n\nReplacement synthetic rule body.\n",
    ),
    // Intentionally deficient: active directive lacking Approval in body.
    (
        "directives/_hive-userdata/unapproved-active.md",
        "---\ndescription: \"Intentionally deficient active directive without approval.\"\ntitle: \"Unapproved Synthetic Active\"\nstatus: active\n---\n## Metadata\n\nIssuer: synthetic-owner\nSource: synthetic-policy-v3\nCreated: 2026-01-02 00:00:00\nUpdated: 2026-01-02 00:00:00\n\n## Direction\n\nMissing approval reference.\n",
    ),
    (
        "tasks/synth-app/backlog/implement-widget/artifacts/design-note.md",
        "---\ndescription: \"Synthetic work-scoped report artifact.\"\ntitle: \"Widget synthetic report\"\nkind: report\n---\n## Context\n\nCreated: 2026-01-02 00:30:00\nSubstantive updated: 2026-01-02 01:00:00\n\nSynthetic work-scoped report body.\n",
    ),
    // Intentionally deficient: artifact with omitted kind in frontmatter.
    (
        "artifacts/unclassified-note.md",
        "---\ndescription: \"Intentionally deficient artifact with omitted kind.\"\ntitle: \"Artifact without kind\"\n---\nSynthetic unclassified artifact body.\n",
    ),
    // Intentionally deficient: artifact absent from reviewed author manifest (prose is not provenance).
    (
        "artifacts/no-author-note.md",
        "---\ndescription: \"Intentionally deficient artifact lacking reviewed author provenance.\"\ntitle: \"Artifact without reviewed author\"\nkind: research\n---\nMentioned in prose: synthetic-prose-name (not structured author provenance).\n",
    ),
    (
        "bots/queen-synth/SOUL.md",
        "---\ndescription: \"Synthetic Queen persona definition.\"\ntitle: \"Synthetic Queen\"\n---\n# Synthetic Queen Soul\n\nSynthetic persona bytes.\n",
    ),
    (
        "bots/queen-synth/README.md",
        "---\ndescription: \"Synthetic Queen management and routing notes.\"\n---\n# Queen Synth Management\n\nExcluded routing notes.\n",
    ),
    (
        "bots/queen-synth/MEMORY.md",
        "---\ndescription: \"Synthetic Queen memory navigation index.\"\n---\n# Memory Navigation\n\n- [General](memory/_general/MEMORY.md)\n",
    ),
    (
        "bots/queen-synth/memory/_general/MEMORY.md",
        "---\ndescription: \"Synthetic deferred global historical memory.\"\n---\n# General Historical Memory\n\nDeferred.\n",
    ),
    (
        "bots/queen-synth/memory/synth-app/architecture.md",
        "---\ndescription: \"Synthetic deferred scoped historical memory.\"\n---\n# Scoped Historical Memory\n\nDeferred.\n",
    ),
];

#[derive(Debug, PartialEq, Eq)]
struct FixtureProjection {
    source_commit: String,
    policy_pin: String,
    hive_yml_version: String,
    hive_yml_mode: String,
    issue_number_next_value: u64,
    // (inventoried, imported, held, excluded_non_record) per surface.
    counts: BTreeMap<&'static str, (usize, usize, usize, usize)>,
    // (source_path, reason_code) sorted deterministically.
    holds: BTreeMap<&'static str, &'static str>,
    // target_id -> (source_raw_sha256, target_body_sha256)
    hashes: BTreeMap<String, (String, String)>,
    task_bodies: BTreeMap<String, String>,
    directive_bodies: BTreeMap<String, String>,
    artifact_body: String,
    artifact_author: String,
    soul_bytes: Vec<u8>,
    reversed_supersedes_id: String,
}

fn git(repo: &Path, home: &Path, args: &[&str]) -> Vec<u8> {
    let output = Command::new("git")
        .args([
            "--no-pager",
            "--no-replace-objects",
            "--no-lazy-fetch",
            "--no-optional-locks",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "maintenance.auto=false",
            "-C",
        ])
        .arg(repo)
        .args(args)
        .env_clear()
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ALLOW_PROTOCOL", "")
        .env("LC_ALL", "C")
        .env("PATH", std::env::var_os("PATH").unwrap())
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn git_text(repo: &Path, home: &Path, args: &[&str]) -> String {
    String::from_utf8(git(repo, home, args))
        .unwrap()
        .trim()
        .to_owned()
}

fn init_git(repo: &Path, home: &Path, name: &str, email: &str) {
    fs::create_dir_all(repo).unwrap();
    git(repo, home, &["init", "-q", "--initial-branch=main"]);
    git(repo, home, &["config", "user.name", name]);
    git(repo, home, &["config", "user.email", email]);
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn split_doc(raw: &[u8]) -> (&str, &str) {
    let text = std::str::from_utf8(raw).unwrap();
    text.strip_prefix("---\n")
        .and_then(|rest| rest.split_once("\n---\n"))
        .unwrap_or(("", text))
}

fn field<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    text.lines()
        .find_map(|line| line.trim_start().strip_prefix(prefix).map(str::trim))
}

fn reviewed_artifact_author(path: &str) -> Option<&'static str> {
    REVIEWED_ARTIFACT_AUTHORS
        .iter()
        .find_map(|(entry_path, author)| (*entry_path == path).then_some(*author))
}

// Fixture-only projection over one pinned v1 commit; not a general importer.
fn project_fixture_snapshot(
    source_repo: &Path,
    home: &Path,
    source_commit: &str,
) -> FixtureProjection {
    assert!(
        git(
            source_repo,
            home,
            &["status", "--porcelain=v1", "-z", "--untracked-files=normal"],
        )
        .is_empty(),
        "source v1 fixture must be clean"
    );
    let show = |path: &str| {
        git(
            source_repo,
            home,
            &["show", &format!("{source_commit}:{path}")],
        )
    };

    // Verify source policy pin and hive.yml version/mode directly from committed blobs.
    let agents_raw = show("AGENTS.md");
    let agents_text = std::str::from_utf8(&agents_raw).unwrap();
    assert!(agents_text.contains(POLICY_PIN));
    let hive_yml_raw = show("hive.yml");
    let hive_yml_text = std::str::from_utf8(&hive_yml_raw).unwrap();
    let hive_yml_version = field(hive_yml_text, "version:").unwrap().to_owned();
    let hive_yml_mode = field(hive_yml_text, "mode:").unwrap().to_owned();
    assert_eq!(hive_yml_version, "1");
    assert_eq!(hive_yml_mode, "local");

    let mut holds = BTreeMap::new();
    let mut hashes = BTreeMap::new();

    // Chosen fixture/manifest dispositions: unsplit _hive held, external Notion excluded.
    let _ = show("tasks/_hive/README.md");
    holds.insert("tasks/_hive/README.md", "unsplit_legacy_scope");
    let _ = show("tasks/external-team/README.md");

    // Tasks sorted deterministically by qualified (scope, work_id):
    // 1: (_general, close-checklist), 2: (synth-app, implement-widget); held task consumes no number.
    let mut task_bodies = BTreeMap::new();
    for (path, issue_id) in [
        (
            "tasks/_general/archive/close-checklist/README.md",
            id(0x111),
        ),
        (
            "tasks/synth-app/backlog/implement-widget/README.md",
            id(0x112),
        ),
    ] {
        let raw = show(path);
        let (_, body) = split_doc(&raw);
        hashes.insert(
            issue_id.clone(),
            (sha256_hex(&raw), sha256_hex(body.as_bytes())),
        );
        task_bodies.insert(issue_id, body.to_owned());
    }
    // Derived check: done task missing Closed / Accepted by / Outcome in body is held.
    let unproven_done = show("tasks/_hive-userdata/active/unproven-done/README.md");
    let (_, unproven_body) = split_doc(&unproven_done);
    if field(unproven_body, "Closed:").is_none()
        || field(unproven_body, "Accepted by:").is_none()
        || field(unproven_body, "Outcome:").is_none()
    {
        holds.insert(
            "tasks/_hive-userdata/active/unproven-done/README.md",
            "incomplete_historical_provenance",
        );
    }

    // Directives: navigation READMEs excluded; replacement and approval derived from body fields.
    let _ = show("directives/README.md");
    let _ = show("directives/synth-app/README.md");
    let d1_raw = show("directives/synth-app/v1-rule.md");
    let (d1_fm, d1_body) = split_doc(&d1_raw);
    let d2_raw = show("directives/synth-app/v2-rule.md");
    let (d2_fm, d2_body) = split_doc(&d2_raw);
    assert_eq!(field(d1_fm, "status:"), Some("superseded"));
    assert_eq!(field(d2_fm, "status:"), Some("active"));
    assert_eq!(field(d1_body, "Replaced by:"), Some("synth-app/v2-rule"));
    assert_eq!(field(d2_body, "Approval:"), Some("synthetic-approval-42"));
    let reversed_supersedes_id = id(DIRECTIVE_V1);
    hashes.insert(
        id(DIRECTIVE_V1),
        (sha256_hex(&d1_raw), sha256_hex(d1_body.as_bytes())),
    );
    hashes.insert(
        id(DIRECTIVE_V2),
        (sha256_hex(&d2_raw), sha256_hex(d2_body.as_bytes())),
    );
    let directive_bodies = BTreeMap::from([
        (id(DIRECTIVE_V1), d1_body.to_owned()),
        (id(DIRECTIVE_V2), d2_body.to_owned()),
    ]);
    // Derived check: active directive without Approval in body is held.
    let unapproved_raw = show("directives/_hive-userdata/unapproved-active.md");
    let (unapproved_fm, unapproved_body) = split_doc(&unapproved_raw);
    if field(unapproved_fm, "status:") == Some("active")
        && field(unapproved_body, "Approval:").is_none()
    {
        holds.insert(
            "directives/_hive-userdata/unapproved-active.md",
            "missing_directive_approval",
        );
    }

    // Artifacts: kind comes from frontmatter; author_ref comes only from reviewed manifest mapping.
    let art_path = "tasks/synth-app/backlog/implement-widget/artifacts/design-note.md";
    let art_raw = show(art_path);
    let (art_fm, art_body) = split_doc(&art_raw);
    assert_eq!(field(art_fm, "kind:"), Some("report"));
    let artifact_author = reviewed_artifact_author(art_path)
        .expect("positive artifact must have reviewed manifest author_ref")
        .to_owned();
    hashes.insert(
        id(ARTIFACT_REPORT),
        (sha256_hex(&art_raw), sha256_hex(art_body.as_bytes())),
    );
    // Derived check: omitted frontmatter kind is held.
    let unclassified_raw = show("artifacts/unclassified-note.md");
    if field(split_doc(&unclassified_raw).0, "kind:").is_none() {
        holds.insert(
            "artifacts/unclassified-note.md",
            "unclassified_artifact_kind",
        );
    }
    // Derived check: artifact absent from reviewed author manifest is held (prose ignored).
    let no_author_path = "artifacts/no-author-note.md";
    let no_author_raw = show(no_author_path);
    assert_eq!(
        field(split_doc(&no_author_raw).0, "kind:"),
        Some("research")
    );
    if reviewed_artifact_author(no_author_path).is_none() {
        holds.insert(no_author_path, "incomplete_historical_provenance");
    }

    // Bot SOUL.md kept byte-identical (frontmatter retained in Git); README & historical Memory deferred.
    let soul_bytes = show("bots/queen-synth/SOUL.md");
    let soul_sha = sha256_hex(&soul_bytes);
    hashes.insert(id(BOT_QUEEN), (soul_sha.clone(), soul_sha));
    let _ = show("bots/queen-synth/README.md");
    let _ = show("bots/queen-synth/MEMORY.md");
    for memory_path in [
        "bots/queen-synth/memory/_general/MEMORY.md",
        "bots/queen-synth/memory/synth-app/architecture.md",
    ] {
        let _ = show(memory_path);
        holds.insert(memory_path, "curated_memory_deferred");
    }

    let counts = BTreeMap::from([
        ("scopes", (5, 3, 1, 1)),
        ("tasks", (3, 2, 1, 0)),
        ("directives", (5, 2, 1, 2)),
        ("artifacts_work_scoped", (1, 1, 0, 0)),
        ("artifacts_standalone", (2, 0, 2, 0)),
        ("bots_soul", (2, 1, 0, 1)),
        ("bot_memories_historical", (3, 0, 2, 1)),
    ]);

    FixtureProjection {
        source_commit: source_commit.to_owned(),
        policy_pin: POLICY_PIN.to_owned(),
        hive_yml_version,
        hive_yml_mode,
        issue_number_next_value: 3,
        counts,
        holds,
        hashes,
        task_bodies,
        directive_bodies,
        artifact_body: art_body.to_owned(),
        artifact_author,
        soul_bytes,
        reversed_supersedes_id,
    }
}

fn apply_fixture_sql(admin: &mut Conn, p: &FixtureProjection) {
    let rev = id(REVISION);
    for (area_id, slug, name) in [
        (id(AREA_GENERAL), "general", "General"),
        (id(AREA_USERDATA), "hive-userdata", "Workspace Operations"),
        (id(AREA_APP), "synth-app", "Synthetic App"),
    ] {
        admin
            .exec_drop(
                "INSERT INTO areas (id, slug, name, revision) VALUES (?, ?, ?, ?)",
                (area_id, slug, name, &rev),
            )
            .unwrap();
    }
    admin
        .exec_drop(
            "INSERT INTO projects (id, name, status, revision) VALUES (?, 'Synthetic Release', 'active', ?)",
            (id(PROJECT_RELEASE), &rev),
        )
        .unwrap();
    admin
        .exec_drop(
            "INSERT INTO resources (id, kind, name, uri, revision)
             VALUES (?, 'git-repo', 'Synthetic App Repo', 'https://forge.example/synthetic/app', ?)",
            (id(RESOURCE_REPO), &rev),
        )
        .unwrap();
    admin
        .exec_drop(
            "INSERT INTO area_resources (area_id, resource_id) VALUES (?, ?)",
            (id(AREA_APP), id(RESOURCE_REPO)),
        )
        .unwrap();
    admin
        .exec_drop(
            "INSERT INTO issues
             (id, number, area_id, title, body, type, status, priority, revision,
              created_at, updated_at, closed_at, closed_by_ref, resolution_note)
             VALUES
             (?, 1, ?, 'Close synthetic checklist', ?, 'task', 'done', NULL, ?,
              '2026-01-01 00:00:00', '2026-01-01 02:00:00', '2026-01-01 03:00:00',
              'synthetic-reviewer', 'synthetic checklist verified'),
             (?, 2, ?, 'Implement synthetic widget', ?, 'task', 'backlog', 0, ?,
              '2026-01-02 00:00:00', '2026-01-02 01:00:00', NULL, NULL, NULL)",
            (
                id(0x111),
                id(AREA_GENERAL),
                &p.task_bodies[&id(0x111)],
                &rev,
                id(0x112),
                id(AREA_APP),
                &p.task_bodies[&id(0x112)],
                &rev,
            ),
        )
        .unwrap();
    admin
        .exec_drop(
            "INSERT INTO project_issues (project_id, issue_id) VALUES (?, ?)",
            (id(PROJECT_RELEASE), id(0x112)),
        )
        .unwrap();
    admin
        .exec_drop(
            "INSERT INTO issue_resources (issue_id, resource_id) VALUES (?, ?)",
            (id(0x112), id(RESOURCE_REPO)),
        )
        .unwrap();
    admin
        .exec_drop(
            "UPDATE workspace_counters SET next_value = ? WHERE name = 'issue_number'",
            (p.issue_number_next_value,),
        )
        .unwrap();
    admin
        .exec_drop(
            "INSERT INTO directives
             (id, scope_kind, area_id, include_descendants, title, body, status,
              issuer_ref, source_ref, approval_ref, supersedes_id, revision, created_at, updated_at)
             VALUES
             (?, 'area', ?, FALSE, 'Synthetic Rule v1', ?, 'superseded',
              'synthetic-owner', 'synthetic-policy-v1', NULL, NULL, ?,
              '2026-01-01 00:00:00', '2026-01-02 00:00:00'),
             (?, 'area', ?, FALSE, 'Synthetic Rule v2', ?, 'active',
              'synthetic-owner', 'synthetic-policy-v2', 'synthetic-approval-42', ?, ?,
              '2026-01-02 00:00:00', '2026-01-02 00:00:00')",
            (
                id(DIRECTIVE_V1),
                id(AREA_APP),
                &p.directive_bodies[&id(DIRECTIVE_V1)],
                &rev,
                id(DIRECTIVE_V2),
                id(AREA_APP),
                &p.directive_bodies[&id(DIRECTIVE_V2)],
                &p.reversed_supersedes_id,
                &rev,
            ),
        )
        .unwrap();
    admin
        .exec_drop(
            "INSERT INTO artifacts
             (id, kind, title, body, author_ref, source_ref, revision, created_at, substantive_updated_at)
             VALUES (?, 'report', 'Widget synthetic report', ?, ?, 'synthetic-run-1', ?,
                     '2026-01-02 00:30:00', '2026-01-02 01:00:00')",
            (id(ARTIFACT_REPORT), &p.artifact_body, &p.artifact_author, &rev),
        )
        .unwrap();
    admin
        .exec_drop(
            "INSERT INTO artifact_issues (artifact_id, issue_id) VALUES (?, ?)",
            (id(ARTIFACT_REPORT), id(0x112)),
        )
        .unwrap();
    admin
        .exec_drop(
            "INSERT INTO artifact_areas (artifact_id, area_id) VALUES (?, ?)",
            (id(ARTIFACT_REPORT), id(AREA_APP)),
        )
        .unwrap();
    admin
        .exec_drop(
            "INSERT INTO bots (id, slug, display_name, revision) VALUES (?, 'queen-synth', 'Synthetic Queen', ?)",
            (id(BOT_QUEEN), &rev),
        )
        .unwrap();
}

fn row_count(admin: &mut Conn, table: &str) -> u64 {
    admin
        .query_first(format!("SELECT COUNT(*) FROM {table}"))
        .unwrap()
        .unwrap()
}

#[test]
#[ignore = "requires the pinned Dolt binary at HIVE_DOLT_BIN"]
fn rehearses_v1_to_v2_migration_mapping_rollback_and_committed_validation() {
    let fixture = Fixture::start_empty();
    let home = fixture.root.join("home");
    let source_repo = fixture.root.join("legacy-v1");
    init_git(
        &source_repo,
        &home,
        "Synthetic Fixture",
        "synthetic@example.invalid",
    );
    for (rel, content) in SYNTHETIC_V1_FILES {
        let path = source_repo.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, content).unwrap();
    }
    git(&source_repo, &home, &["add", "."]);
    git(&source_repo, &home, &["commit", "-qm", "Synthetic v1"]);
    let source_commit = git_text(&source_repo, &home, &["rev-parse", "HEAD"]);
    let source_hashes_before: BTreeMap<&str, String> = SYNTHETIC_V1_FILES
        .iter()
        .map(|(rel, _)| (*rel, sha256_hex(&fs::read(source_repo.join(rel)).unwrap())))
        .collect();

    // Verify v1 source workspace config and policy pin through existing v1 CLI in isolated fixture.
    let v1_config_output = Command::new(env!("CARGO_BIN_EXE_hive"))
        .args([
            "--json",
            "config",
            "show",
            "--scope",
            "workspace",
            "--workspace",
        ])
        .arg(&source_repo)
        .args(["--at", &source_commit])
        .current_dir(&fixture.root)
        .env_clear()
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", &home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("PATH", std::env::var_os("PATH").unwrap())
        .output()
        .unwrap();
    assert!(
        v1_config_output.status.success(),
        "v1 config show failed: {}",
        String::from_utf8_lossy(&v1_config_output.stderr)
    );
    let v1_config: Value = serde_json::from_slice(&v1_config_output.stdout).unwrap();
    assert_eq!(v1_config["schema_version"], 1);
    assert_eq!(v1_config["ok"], true);
    assert_eq!(v1_config["context"]["policy_revision"], POLICY_PIN);
    assert_eq!(v1_config["context"]["revision"], source_commit);
    assert_eq!(v1_config["data"]["scope"], "workspace");
    assert_eq!(v1_config["data"]["present"], true);
    assert_eq!(v1_config["data"]["coordination"]["mode"], "local");

    // Repeatability: projecting twice from the same pinned Git commit yields identical hashes/counts/holds.
    let projection = project_fixture_snapshot(&source_repo, &home, &source_commit);
    assert_eq!(
        projection,
        project_fixture_snapshot(&source_repo, &home, &source_commit)
    );
    for (surface, (inv, imp, held, excl)) in &projection.counts {
        assert_eq!(*inv, imp + held + excl, "unbalanced counts for {surface}");
    }
    assert_eq!(
        projection.holds,
        BTreeMap::from([
            (
                "artifacts/no-author-note.md",
                "incomplete_historical_provenance",
            ),
            (
                "artifacts/unclassified-note.md",
                "unclassified_artifact_kind",
            ),
            (
                "bots/queen-synth/memory/_general/MEMORY.md",
                "curated_memory_deferred",
            ),
            (
                "bots/queen-synth/memory/synth-app/architecture.md",
                "curated_memory_deferred",
            ),
            (
                "directives/_hive-userdata/unapproved-active.md",
                "missing_directive_approval",
            ),
            (
                "tasks/_hive-userdata/active/unproven-done/README.md",
                "incomplete_historical_provenance",
            ),
            ("tasks/_hive/README.md", "unsplit_legacy_scope"),
        ])
    );

    // SQL rollback leaves zero domain rows, counter == 1, and unchanged HEAD/work/stage.
    let mut admin = fixture.connect(ADMIN);
    let base_state = storage_state(&mut admin);
    fixture.check(0, &mut admin);

    admin
        .query_drop("SET @@SESSION.dolt_transaction_commit = 1")
        .unwrap();
    admin.query_drop("START TRANSACTION").unwrap();
    apply_fixture_sql(&mut admin, &projection);
    admin.query_drop("ROLLBACK").unwrap();

    assert_eq!(storage_state(&mut admin), base_state);
    for table in [
        "areas",
        "projects",
        "resources",
        "area_resources",
        "issues",
        "project_issues",
        "issue_resources",
        "issue_claims",
        "directives",
        "artifacts",
        "artifact_issues",
        "artifact_areas",
        "bots",
        "bot_memories",
    ] {
        assert_eq!(
            row_count(&mut admin, table),
            0,
            "rollback left rows in {table}"
        );
    }
    let counter_rollback: u64 = admin
        .query_first("SELECT next_value FROM workspace_counters WHERE name = 'issue_number'")
        .unwrap()
        .unwrap();
    assert_eq!(counter_rollback, 1);

    // Commit Git-tracked Soul bytes BEFORE committing Dolt bots row; exclude README & Memory.
    let target_assets = fixture.root.join("target-assets-git");
    init_git(
        &target_assets,
        &home,
        "Synthetic Target",
        "target@example.invalid",
    );
    let soul_path = target_assets.join("assets/bots/queen-synth/SOUL.md");
    fs::create_dir_all(soul_path.parent().unwrap()).unwrap();
    fs::write(&soul_path, &projection.soul_bytes).unwrap();
    git(&target_assets, &home, &["add", "."]);
    git(
        &target_assets,
        &home,
        &["commit", "-qm", "Synthetic v2 Soul"],
    );
    let asset_commit = git_text(&target_assets, &home, &["rev-parse", "HEAD"]);
    assert_eq!(
        git_text(
            &target_assets,
            &home,
            &["ls-tree", "--name-only", "-r", &asset_commit]
        ),
        "assets/bots/queen-synth/SOUL.md"
    );
    let committed_soul = git(
        &target_assets,
        &home,
        &[
            "show",
            &format!("{asset_commit}:assets/bots/queen-synth/SOUL.md"),
        ],
    );
    assert_eq!(
        sha256_hex(&committed_soul),
        projection.hashes[&id(BOT_QUEEN)].0
    );

    // Commit isolated v2 Dolt target and verify with the actual compiled Rust v2 validator.
    admin.query_drop("START TRANSACTION").unwrap();
    apply_fixture_sql(&mut admin, &projection);
    admin.query_drop("COMMIT").unwrap();

    let committed = storage_state(&mut admin);
    assert_ne!(committed.0, base_state.0);
    assert_eq!(committed.1, committed.2);
    assert!(committed.3.is_empty());

    let validation = fixture.check(0, &mut admin);
    assert_eq!(validation["data"]["storage_schema_version"], 1);
    assert_eq!(validation["context"]["db_commit"], committed.0);

    // Confirm mapped counter, zero claims/memory, reversed supersession edge, artifact link, and stored hashes.
    let counter_committed: u64 = admin
        .query_first("SELECT next_value FROM workspace_counters WHERE name = 'issue_number'")
        .unwrap()
        .unwrap();
    assert_eq!(counter_committed, 3);
    assert_eq!(row_count(&mut admin, "issue_claims"), 0);
    assert_eq!(row_count(&mut admin, "bot_memories"), 0);

    let supersession: (String, Option<String>) = admin
        .exec_first(
            "SELECT status, supersedes_id FROM directives WHERE id = ?",
            (id(DIRECTIVE_V2),),
        )
        .unwrap()
        .unwrap();
    assert_eq!(supersession, ("active".to_owned(), Some(id(DIRECTIVE_V1))));
    let replaced_supersedes: Option<String> = admin
        .exec_first(
            "SELECT supersedes_id FROM directives WHERE id = ?",
            (id(DIRECTIVE_V1),),
        )
        .unwrap()
        .unwrap();
    assert_eq!(replaced_supersedes, None);

    let artifact_link: (String, String, String) = admin
        .exec_first(
            "SELECT a.kind, DATE_FORMAT(a.substantive_updated_at, '%Y-%m-%d %H:%i:%s'), ai.issue_id
             FROM artifacts a JOIN artifact_issues ai ON ai.artifact_id = a.id WHERE a.id = ?",
            (id(ARTIFACT_REPORT),),
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        artifact_link,
        (
            "report".to_owned(),
            "2026-01-02 01:00:00".to_owned(),
            id(0x112)
        )
    );

    let stored_bodies: Vec<(String, String)> = admin
        .query(
            "SELECT id, body FROM issues
             UNION ALL SELECT id, body FROM directives
             UNION ALL SELECT id, body FROM artifacts",
        )
        .unwrap();
    for (record_id, body) in stored_bodies {
        assert_eq!(
            sha256_hex(body.as_bytes()),
            projection.hashes[&record_id].1,
            "stored Dolt body hash mismatch for {record_id}"
        );
    }

    // Source v1 Git HEAD, status, and file bytes remain untouched.
    assert_eq!(
        git_text(&source_repo, &home, &["rev-parse", "HEAD"]),
        source_commit
    );
    assert!(
        git(
            &source_repo,
            &home,
            &["status", "--porcelain=v1", "-z", "--untracked-files=normal"],
        )
        .is_empty()
    );
    let source_hashes_after: BTreeMap<&str, String> = SYNTHETIC_V1_FILES
        .iter()
        .map(|(rel, _)| (*rel, sha256_hex(&fs::read(source_repo.join(rel)).unwrap())))
        .collect();
    assert_eq!(source_hashes_after, source_hashes_before);
}
