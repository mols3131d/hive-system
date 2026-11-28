use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};

use mysql::Conn;
use mysql::prelude::Queryable;
use serde::Serialize;
use serde_json::{Value, json};
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

// The synthetic operator's reviewed selection for the pinned fixture snapshot.
struct FixtureManifest<'a> {
    source_commit: &'a str,
    external_scope: &'static str,
    external_backend: &'static str,
    scopes: BTreeMap<&'static str, (u32, &'static str, &'static str)>,
    area_resource: (u32, u32),
    project_issue: (u32, u32),
    issue_resource: (u32, u32),
    artifact_issue: (u32, u32),
    artifact_area: (u32, u32),
    artifact_authors: &'static [(&'static str, &'static str)],
}

impl<'a> FixtureManifest<'a> {
    fn reviewed(source_commit: &'a str) -> Self {
        Self {
            source_commit,
            external_scope: "tasks/external-team/README.md",
            external_backend: "notion",
            scopes: BTreeMap::from([
                ("_general", (AREA_GENERAL, "general", "General")),
                (
                    "_hive-userdata",
                    (AREA_USERDATA, "hive-userdata", "Workspace Operations"),
                ),
                ("synth-app", (AREA_APP, "synth-app", "Synthetic App")),
            ]),
            area_resource: (AREA_APP, RESOURCE_REPO),
            project_issue: (PROJECT_RELEASE, 0x112),
            issue_resource: (0x112, RESOURCE_REPO),
            artifact_issue: (ARTIFACT_REPORT, 0x112),
            artifact_area: (ARTIFACT_REPORT, AREA_APP),
            artifact_authors: &[(
                "tasks/synth-app/backlog/implement-widget/artifacts/design-note.md",
                "synthetic-author",
            )],
        }
    }

    fn artifact_author(&self, path: &str) -> Option<&'static str> {
        self.artifact_authors
            .iter()
            .find_map(|(entry_path, author)| (*entry_path == path).then_some(*author))
    }
}

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

#[derive(Clone, Copy)]
enum Disposition {
    Imported,
    Held,
    Excluded,
}

use Disposition::{Excluded, Held, Imported};

// Declare every path in the fixed source tree, including non-record files.
const FIXTURE_INVENTORY: &[(&str, &str, Disposition)] = &[
    ("AGENTS.md", "policy_metadata", Excluded),
    ("hive.yml", "policy_metadata", Excluded),
    ("tasks/_general/README.md", "scopes", Imported),
    ("tasks/_hive-userdata/README.md", "scopes", Imported),
    ("tasks/synth-app/README.md", "scopes", Imported),
    ("tasks/_hive/README.md", "scopes", Held),
    (
        "tasks/synth-app/backlog/implement-widget/README.md",
        "tasks",
        Imported,
    ),
    (
        "tasks/_general/archive/close-checklist/README.md",
        "tasks",
        Imported,
    ),
    (
        "tasks/_hive-userdata/active/unproven-done/README.md",
        "tasks",
        Held,
    ),
    ("directives/README.md", "directives", Excluded),
    ("directives/synth-app/README.md", "directives", Excluded),
    ("directives/synth-app/v1-rule.md", "directives", Imported),
    ("directives/synth-app/v2-rule.md", "directives", Imported),
    (
        "directives/_hive-userdata/unapproved-active.md",
        "directives",
        Held,
    ),
    (
        "tasks/synth-app/backlog/implement-widget/artifacts/design-note.md",
        "artifacts_work_scoped",
        Imported,
    ),
    (
        "artifacts/unclassified-note.md",
        "artifacts_standalone",
        Held,
    ),
    ("artifacts/no-author-note.md", "artifacts_standalone", Held),
    ("bots/queen-synth/SOUL.md", "bots_soul", Imported),
    ("bots/queen-synth/README.md", "bots_soul", Excluded),
    ("bots/queen-synth/MEMORY.md", "navigation_only", Excluded),
    (
        "bots/queen-synth/memory/_general/MEMORY.md",
        "bot_memories_historical",
        Held,
    ),
    (
        "bots/queen-synth/memory/synth-app/architecture.md",
        "bot_memories_historical",
        Held,
    ),
];

#[derive(Debug, Default, PartialEq, Eq, Serialize)]
struct SurfaceCounts {
    inventoried: usize,
    imported: usize,
    held: usize,
    excluded_non_record: usize,
}

fn inventory_counts(
    tree: &[u8],
    manifest: &FixtureManifest<'_>,
    holds: &BTreeMap<&str, &str>,
) -> Result<BTreeMap<&'static str, SurfaceCounts>, String> {
    let mut remaining: BTreeMap<_, _> = FIXTURE_INVENTORY
        .iter()
        .map(|&(path, surface, disposition)| (path, (surface, disposition)))
        .collect();
    assert_eq!(remaining.len(), FIXTURE_INVENTORY.len());
    assert_eq!(manifest.external_backend, "notion");
    assert!(
        remaining
            .insert(manifest.external_scope, ("scopes", Excluded))
            .is_none()
    );

    let mut counts = BTreeMap::<_, SurfaceCounts>::new();
    let mut held_paths = 0;
    for path in tree
        .split(|&byte| byte == 0)
        .filter(|path| !path.is_empty())
    {
        let path = std::str::from_utf8(path).unwrap();
        let (surface, disposition) = remaining
            .remove(path)
            .ok_or_else(|| format!("unexpected fixture path: {path}"))?;
        if holds.contains_key(path) != matches!(disposition, Held) {
            return Err(format!("fixture hold disposition changed: {path}"));
        }
        let surface = counts.entry(surface).or_default();
        surface.inventoried += 1;
        match disposition {
            Imported => surface.imported += 1,
            Held => {
                surface.held += 1;
                held_paths += 1;
            }
            Excluded => surface.excluded_non_record += 1,
        }
    }
    if !remaining.is_empty() {
        return Err(format!("missing fixture paths: {:?}", remaining.keys()));
    }
    assert_eq!(held_paths, holds.len());
    Ok(counts)
}

#[derive(Debug, PartialEq, Eq)]
struct FixtureProjection {
    source_commit: String,
    policy_pin: String,
    hive_yml_version: String,
    hive_yml_mode: String,
    issue_number_next_value: u64,
    counts: BTreeMap<&'static str, SurfaceCounts>,
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

impl FixtureProjection {
    fn redacted_evidence(&self) -> Value {
        json!({
            "source_commit": self.source_commit,
            "policy_pin": self.policy_pin,
            "counts": self.counts,
            "holds": self.holds,
            "hashes": self.hashes,
        })
    }
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

// Fixture-only projection over one pinned v1 commit.
fn project_fixture_snapshot(
    source_repo: &Path,
    home: &Path,
    manifest: &FixtureManifest<'_>,
) -> FixtureProjection {
    let source_commit = manifest.source_commit;
    let tree = git(
        source_repo,
        home,
        &["ls-tree", "-r", "-z", "--name-only", source_commit],
    );
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

    // The unsplit legacy scope requires reconciliation.
    let _ = show("tasks/_hive/README.md");
    holds.insert("tasks/_hive/README.md", "unsplit_legacy_scope");

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
    let artifact_author = manifest
        .artifact_author(art_path)
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
    if manifest.artifact_author(no_author_path).is_none() {
        holds.insert(no_author_path, "incomplete_historical_provenance");
    }

    // Bot SOUL.md kept byte-identical (frontmatter retained in Git); README & historical Memory deferred.
    let soul_bytes = show("bots/queen-synth/SOUL.md");
    let soul_sha = sha256_hex(&soul_bytes);
    hashes.insert(id(BOT_QUEEN), (soul_sha.clone(), soul_sha));
    for memory_path in [
        "bots/queen-synth/memory/_general/MEMORY.md",
        "bots/queen-synth/memory/synth-app/architecture.md",
    ] {
        let _ = show(memory_path);
        holds.insert(memory_path, "curated_memory_deferred");
    }

    let counts = inventory_counts(&tree, manifest, &holds).unwrap();

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

fn apply_fixture_sql(admin: &mut Conn, p: &FixtureProjection, manifest: &FixtureManifest<'_>) {
    let rev = id(REVISION);
    assert_eq!(p.source_commit, manifest.source_commit);
    let app_area = manifest.scopes["synth-app"].0;
    let general_area = manifest.scopes["_general"].0;
    for &(area_id, slug, name) in manifest.scopes.values() {
        admin
            .exec_drop(
                "INSERT INTO areas (id, slug, name, revision) VALUES (?, ?, ?, ?)",
                (id(area_id), slug, name, &rev),
            )
            .unwrap();
    }
    admin
        .exec_drop(
            "INSERT INTO projects (id, name, status, revision) VALUES (?, 'Synthetic Release', 'active', ?)",
            (id(manifest.project_issue.0), &rev),
        )
        .unwrap();
    admin
        .exec_drop(
            "INSERT INTO resources (id, kind, name, uri, revision)
             VALUES (?, 'git-repo', 'Synthetic App Repo', 'https://forge.example/synthetic/app', ?)",
            (id(manifest.area_resource.1), &rev),
        )
        .unwrap();
    admin
        .exec_drop(
            "INSERT INTO area_resources (area_id, resource_id) VALUES (?, ?)",
            (id(manifest.area_resource.0), id(manifest.area_resource.1)),
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
                id(general_area),
                &p.task_bodies[&id(0x111)],
                &rev,
                id(0x112),
                id(app_area),
                &p.task_bodies[&id(0x112)],
                &rev,
            ),
        )
        .unwrap();
    admin
        .exec_drop(
            "INSERT INTO project_issues (project_id, issue_id) VALUES (?, ?)",
            (id(manifest.project_issue.0), id(manifest.project_issue.1)),
        )
        .unwrap();
    admin
        .exec_drop(
            "INSERT INTO issue_resources (issue_id, resource_id) VALUES (?, ?)",
            (id(manifest.issue_resource.0), id(manifest.issue_resource.1)),
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
                id(app_area),
                &p.directive_bodies[&id(DIRECTIVE_V1)],
                &rev,
                id(DIRECTIVE_V2),
                id(app_area),
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
            (id(manifest.artifact_issue.0), &p.artifact_body, &p.artifact_author, &rev),
        )
        .unwrap();
    admin
        .exec_drop(
            "INSERT INTO artifact_issues (artifact_id, issue_id) VALUES (?, ?)",
            (id(manifest.artifact_issue.0), id(manifest.artifact_issue.1)),
        )
        .unwrap();
    admin
        .exec_drop(
            "INSERT INTO artifact_areas (artifact_id, area_id) VALUES (?, ?)",
            (id(manifest.artifact_area.0), id(manifest.artifact_area.1)),
        )
        .unwrap();
    admin
        .exec_drop(
            "INSERT INTO bots (id, slug, display_name, revision) VALUES (?, 'queen-synth', 'Synthetic Queen', ?)",
            (id(BOT_QUEEN), &rev),
        )
        .unwrap();
}

fn init_source(source_repo: &Path, home: &Path) -> String {
    init_git(
        source_repo,
        home,
        "Synthetic Fixture",
        "synthetic@example.invalid",
    );
    for (rel, content) in SYNTHETIC_V1_FILES {
        let path = source_repo.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, content).unwrap();
    }
    git(source_repo, home, &["add", "."]);
    git(source_repo, home, &["commit", "-qm", "Synthetic v1"]);
    git_text(source_repo, home, &["rev-parse", "HEAD"])
}

#[test]
fn reconciles_pinned_fixture_inventory_and_redacts_evidence() {
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TempRoot(PathBuf);

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("hive-migration-{}-{nonce}", std::process::id()));
    fs::create_dir(&root).unwrap();
    let root = TempRoot(root);
    let root = &root.0;
    let home = root.join("home");
    fs::create_dir(&home).unwrap();
    let source_repo = root.join("legacy-v1");
    let source_commit = init_source(&source_repo, &home);
    let manifest = FixtureManifest::reviewed(&source_commit);

    // Repeatability: projecting twice from the same pinned Git commit yields identical hashes/counts/holds.
    let projection = project_fixture_snapshot(&source_repo, &home, &manifest);
    assert_eq!(
        projection,
        project_fixture_snapshot(&source_repo, &home, &manifest)
    );
    for (surface, counts) in &projection.counts {
        assert_eq!(
            counts.inventoried,
            counts.imported + counts.held + counts.excluded_non_record,
            "unbalanced counts for {surface}"
        );
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

    let evidence = serde_json::to_string(&projection.redacted_evidence()).unwrap();
    let decoded: Value = serde_json::from_str(&evidence).unwrap();
    assert_eq!(
        decoded,
        json!({
            "source_commit": source_commit,
            "policy_pin": POLICY_PIN,
            "counts": projection.counts,
            "holds": projection.holds,
            "hashes": projection.hashes,
        })
    );
    for sentinel in [
        "Synthetic work-scoped report body.",
        "Missing approval reference.",
        "Synthetic persona bytes.",
        root.to_str().unwrap(),
        source_repo.to_str().unwrap(),
    ] {
        assert!(!evidence.contains(sentinel), "evidence leaked {sentinel}");
    }

    // Exercise drift against the same NUL-delimited inventory used for projection.
    let mut tree = git(
        &source_repo,
        &home,
        &["ls-tree", "-r", "-z", "--name-only", &source_commit],
    );
    let missing = tree.strip_prefix(b"AGENTS.md\0").unwrap();
    assert_eq!(
        inventory_counts(missing, &manifest, &projection.holds).unwrap_err(),
        "missing fixture paths: [\"AGENTS.md\"]"
    );
    tree.extend_from_slice(b"unexpected\npath.md\0");
    assert_eq!(
        inventory_counts(&tree, &manifest, &projection.holds).unwrap_err(),
        "unexpected fixture path: unexpected\npath.md"
    );
}

fn row_count(admin: &mut Conn, table: &str) -> u64 {
    admin
        .query_first(format!("SELECT COUNT(*) FROM {table}"))
        .unwrap()
        .unwrap()
}

#[cfg(target_os = "linux")]
fn migration_preflight(home: &Path, config: &Path, args: &[&str], status: i32) -> Value {
    migration_cli(home, config, "migration-preflight", args, status)
}

#[cfg(target_os = "linux")]
fn migration_preflight_with_path(
    home: &Path,
    config: &Path,
    args: &[&str],
    status: i32,
    search_path: &std::ffi::OsStr,
) -> Value {
    migration_cli_with_path(
        home,
        config,
        "migration-preflight",
        args,
        status,
        search_path,
    )
}

#[cfg(target_os = "linux")]
fn migration_cli(home: &Path, config: &Path, action: &str, args: &[&str], status: i32) -> Value {
    let search_path = std::env::var_os("PATH").unwrap();
    migration_cli_with_path(home, config, action, args, status, &search_path)
}

#[cfg(target_os = "linux")]
fn migration_cli_with_path(
    home: &Path,
    config: &Path,
    action: &str,
    args: &[&str],
    status: i32,
    search_path: &std::ffi::OsStr,
) -> Value {
    let result = Command::new(env!("CARGO_BIN_EXE_hive"))
        .current_dir(home)
        .env_clear()
        .env("PATH", search_path)
        .env("TMPDIR", std::env::temp_dir())
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", config)
        // An inherited repository selection must not replace the explicit one.
        .env("GIT_DIR", home.join("unrelated.git"))
        .args(["--contract", "2", "--json", "workspace", action])
        .args(args)
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(status), "{result:?}");
    assert!(result.stderr.is_empty(), "{result:?}");
    let report: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(report["schema_version"], 2);
    assert_eq!(report["command"], format!("workspace {action}"));
    assert_eq!(report["ok"], status == 0);
    if let Some([_, path]) = args.windows(2).find(|pair| pair[0] == "--source")
        && Path::new(path).is_absolute()
    {
        assert!(!String::from_utf8_lossy(&result.stdout).contains(path));
    }
    report
}

#[cfg(target_os = "linux")]
#[test]
fn cli_migration_plan_maps_and_holds_scopes_from_the_verified_snapshot() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    fs::create_dir(&home).unwrap();
    let config = root.path().join("config");
    let source = root.path().join("source");
    init_source(&source, &home);
    for (path, content) in [
        (
            "directives/synth-app/README.md",
            "# Project direction\n\nProject: <!-- hidden https://forge.example/synthetic/other --> https://forge.example/synthetic/app. <!-- hidden https://forge.example/synthetic/other -->\n",
        ),
        (
            "bots/queen-synth/memory/synth-app/MEMORY.md",
            "# Project memory\n\n<!--\nhttps://forge.example/synthetic/other\n~~~\n-->\n\n~~~markdown\n<!--\nhttps://forge.example/synthetic/other\n~~~~\n\nProject: https://forge.example/synthetic/app\n",
        ),
        (
            "tasks/hive-system/README.md",
            "# Synthetic product area\n\nProject: https://forge.example/synthetic/product\n",
        ),
        ("tasks/unmapped/README.md", "# Unmapped\n"),
        ("tasks/empty-ref/README.md", ""),
        (
            "tasks/local-ref/README.md",
            "# Local project\n\nThe canonical checkout is /home/synthetic/private/app.\n",
        ),
        (
            "tasks/conflicting-refs/README.md",
            "# Project navigation\n\nSee <https://forge.example/synthetic/app> for this project.\n",
        ),
        (
            "directives/conflicting-refs/README.md",
            "# Directive scope\n\n[Project](https://forge.example/synthetic/app(other)) owns these directives.\n",
        ),
        (
            "tasks/ambiguous-refs/README.md",
            "# Project navigation\n\nProject: <https://forge.example/synthetic/app> or <https://forge.example/synthetic/app-other>.\n",
        ),
        (
            "tasks/frontmatter-only/README.md",
            "---\ndescription: https://forge.example/synthetic/app\n---\n# Project navigation\n",
        ),
        (
            "tasks/inline-comment-only/README.md",
            "# Project navigation\n\nProject: /home/synthetic/private/app <!-- https://forge.example/synthetic/app -->\n",
        ),
        (
            "tasks/multiline-comment-only/README.md",
            "# Project navigation\n\n<!-- example\nhttps://forge.example/synthetic/app\n--> Project: /home/synthetic/private/app\n",
        ),
        (
            "tasks/fenced-example-only/README.md",
            "# Project navigation\n\nProject: /home/synthetic/private/app\n\n````markdown\n```\nhttps://forge.example/synthetic/app\n````\n",
        ),
        (
            "tasks/inline-html-only/README.md",
            "# Project navigation\n\nProject: /home/synthetic/private/app <span data-project=\" https://forge.example/synthetic/app \">example</span>\n",
        ),
        (
            "tasks/multiline-html-only/README.md",
            "# Project navigation\n\nProject: /home/synthetic/private/app <span data-project=\"\nhttps://forge.example/synthetic/app\n\">example</span>\n",
        ),
        (
            "tasks/quoted-fence-only/README.md",
            "# Project navigation\n\nProject: /home/synthetic/private/app\n\n> ```markdown\n> https://forge.example/synthetic/app\n> ```\n",
        ),
        (
            "tasks/broken-link-only/README.md",
            "# Project navigation\n\nProject: /home/synthetic/private/app Owner](https://forge.example/synthetic/app)\n",
        ),
        (
            "tasks/bad-percent/README.md",
            "# Project navigation\n\n[Project](https://forge.example/synthetic/app%zz)\n",
        ),
        (
            "tasks/good-percent/README.md",
            "# Project navigation\n\n[Project](https://forge.example/synthetic/app%20one)\n",
        ),
    ] {
        let file = source.join(path);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, content).unwrap();
    }
    git(&source, &home, &["add", "."]);
    git(
        &source,
        &home,
        &["commit", "-qm", "Synthetic scope entries"],
    );
    let commit = git_text(&source, &home, &["rev-parse", "HEAD"]);
    let workspace = id(1);
    let file = root.path().join("mapping.json");
    let args = [
        "--source",
        source.to_str().unwrap(),
        "--source-commit",
        &commit,
    ];
    let plan_args = [
        args.as_slice(),
        &[
            "--workspace-id",
            &workspace,
            "--mapping-file",
            file.to_str().unwrap(),
        ],
    ]
    .concat();
    let plan = |mapping: &Value| {
        fs::write(&file, serde_json::to_vec(mapping).unwrap()).unwrap();
        migration_cli(&home, &config, "migration-plan", &plan_args, 0)["data"].clone()
    };
    let area = |scope, id_value, slug, evidence: Value| {
        json!({
            "scope": scope,
            "decision": {"kind": "area", "area": {"id": id(id_value), "slug": slug, "name": "Synthetic Area"}, "project_evidence": evidence}
        })
    };
    let evidence = |path, reference| json!({"path": path, "canonical_ref": reference});
    let app_ref = "https://forge.example/synthetic/app";
    let local_path = "/home/synthetic/private/app";
    let mut mapping = json!({
        "version": 1,
        "source_commit": commit,
        "policy_pin": POLICY_PIN,
        "workspace_id": workspace,
        "scopes": [
            area("synth-app", AREA_APP, "application", json!([
                evidence("tasks/synth-app/README.md", app_ref),
                evidence("directives/synth-app/README.md", app_ref),
                evidence("bots/queen-synth/memory/synth-app/MEMORY.md", app_ref),
            ])),
            area("_general", AREA_GENERAL, "general", json!([])),
            area("_hive-userdata", AREA_USERDATA, "workspace-operations", json!([])),
            area("hive-system", 0x104, "product", json!([
                evidence("tasks/hive-system/README.md", "https://forge.example/synthetic/product")
            ])),
            {"scope": "external-team", "decision": {"kind": "external"}},
            area("_hive", 0x105, "legacy-hive", json!([])),
            area("empty-ref", 0x106, "empty", json!([
                evidence("tasks/empty-ref/README.md", app_ref)
            ])),
            area("local-ref", 0x107, "local", json!([
                evidence("tasks/local-ref/README.md", local_path)
            ])),
            area("conflicting-refs", 0x108, "conflicting", json!([
                evidence("tasks/conflicting-refs/README.md", app_ref),
                evidence("directives/conflicting-refs/README.md", app_ref),
            ])),
            area("ambiguous-refs", 0x109, "ambiguous", json!([
                evidence("tasks/ambiguous-refs/README.md", app_ref)
            ])),
            area("frontmatter-only", 0x10a, "frontmatter", json!([
                evidence("tasks/frontmatter-only/README.md", app_ref)
            ])),
            area("inline-comment-only", 0x10b, "inline-comment", json!([
                evidence("tasks/inline-comment-only/README.md", app_ref)
            ])),
            area("multiline-comment-only", 0x10c, "multiline-comment", json!([
                evidence("tasks/multiline-comment-only/README.md", app_ref)
            ])),
            area("fenced-example-only", 0x10d, "fenced-example", json!([
                evidence("tasks/fenced-example-only/README.md", app_ref)
            ])),
            area("inline-html-only", 0x10e, "inline-html", json!([
                evidence("tasks/inline-html-only/README.md", app_ref)
            ])),
            area("multiline-html-only", 0x10f, "multiline-html", json!([
                evidence("tasks/multiline-html-only/README.md", app_ref)
            ])),
            area("quoted-fence-only", 0x110, "quoted-fence", json!([
                evidence("tasks/quoted-fence-only/README.md", app_ref)
            ])),
            area("broken-link-only", 0x111, "broken-link", json!([
                evidence("tasks/broken-link-only/README.md", app_ref)
            ])),
            area("bad-percent", 0x112, "bad-percent", json!([
                evidence("tasks/bad-percent/README.md", "https://forge.example/synthetic/app%zz")
            ])),
            area("good-percent", 0x113, "good-percent", json!([
                evidence("tasks/good-percent/README.md", "https://forge.example/synthetic/app%20one")
            ])),
        ]
    });
    mapping["scopes"][0]["decision"]["area"]["name"] = json!(local_path);
    let original_git_files = [
        ".git/index",
        ".git/HEAD",
        ".git/refs/heads/main",
        ".git/config",
    ]
    .map(|path| fs::read(source.join(path)).unwrap());
    let snapshot = migration_preflight(&home, &config, &args, 0);
    let data = plan(&mapping);
    assert_eq!(data, plan(&mapping));
    assert_eq!(data["mode"], "scope_plan");
    assert_eq!(data["mapping_version"], 1);
    assert_eq!(
        data["mapping_sha256"],
        sha256_hex(&fs::read(&file).unwrap())
    );
    assert_eq!(data["source"], snapshot["data"]["source"]);
    assert_eq!(data["inventory"], snapshot["data"]["inventory"]);
    assert_eq!(data["target"], json!({"workspace_id": workspace}));
    assert_eq!(data["scopes"].as_array().unwrap().len(), 21);
    let outcomes = |report: &Value| {
        report["scopes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|scope| {
                (
                    scope["scope"].as_str().unwrap().to_owned(),
                    scope["outcome"].clone(),
                )
            })
            .collect::<BTreeMap<_, _>>()
    };
    let mapped = outcomes(&data);
    assert_eq!(mapped["_general"]["status"], "mapped");
    assert_eq!(mapped["_hive-userdata"]["status"], "mapped");
    assert_eq!(mapped["hive-system"]["status"], "mapped");
    assert_eq!(mapped["good-percent"]["status"], "mapped");
    assert_eq!(
        mapped["synth-app"],
        json!({"status": "mapped", "area": {
            "id": id(AREA_APP), "slug": "application", "parent_id": null
        }})
    );
    assert_eq!(
        mapped["external-team"],
        json!({"status": "excluded", "reason": "external_backend_owned"})
    );
    assert_eq!(
        mapped["_hive"],
        json!({"status": "held", "reason": "unsplit_legacy_scope"})
    );
    let held = json!({"status": "held", "reason": "ambiguous_scope_or_collision"});
    for scope in [
        "unmapped",
        "empty-ref",
        "local-ref",
        "conflicting-refs",
        "ambiguous-refs",
        "frontmatter-only",
        "inline-comment-only",
        "multiline-comment-only",
        "fenced-example-only",
        "inline-html-only",
        "multiline-html-only",
        "quoted-fence-only",
        "broken-link-only",
        "bad-percent",
    ] {
        assert_eq!(mapped[scope], held, "{scope}");
    }
    let app = data["scopes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|scope| scope["scope"] == "synth-app")
        .unwrap();
    assert_eq!(
        app["source_paths"],
        json!([
            "bots/queen-synth/memory/synth-app",
            "directives/synth-app",
            "tasks/synth-app"
        ])
    );
    let encoded = data.to_string();
    for private in [
        "https://forge.example",
        local_path,
        "Synthetic Area",
        "Synthetic persona bytes.",
    ] {
        assert!(!encoded.contains(private));
    }

    let mut changed = mapping.clone();
    changed["scopes"][0]["decision"]["project_evidence"][2]["canonical_ref"] =
        json!("https://forge.example/synthetic/other-owner");
    changed["scopes"][2]["decision"]["area"]["id"] = json!(id(0x104));
    let conflicts = outcomes(&plan(&changed));
    for scope in ["synth-app", "_hive-userdata", "hive-system"] {
        assert_eq!(conflicts[scope], held, "{scope}");
    }

    let mut changed = mapping.clone();
    changed["scopes"][0]["decision"]["project_evidence"]
        .as_array_mut()
        .unwrap()
        .pop();
    changed["scopes"][1]["decision"]["area"]["slug"] = json!("product");
    let conflicts = outcomes(&plan(&changed));
    for scope in ["synth-app", "_general", "hive-system"] {
        assert_eq!(conflicts[scope], held, "{scope}");
    }

    changed["scopes"][1]["decision"]["area"]["parent_id"] = json!(id(AREA_USERDATA));
    changed["scopes"][3]["decision"]["area"]["parent_id"] = json!(id(AREA_USERDATA));
    assert_eq!(outcomes(&plan(&changed))["_general"], held);
    assert_eq!(outcomes(&plan(&changed))["hive-system"], held);
    changed["scopes"][1]["decision"]["area"]["parent_id"] = json!(id(AREA_APP));
    let separate_parents = outcomes(&plan(&changed));
    assert_eq!(separate_parents["_general"]["status"], "mapped");
    assert_eq!(
        separate_parents["hive-system"]["area"]["parent_id"],
        id(AREA_USERDATA)
    );

    let mut changed = mapping.clone();
    changed["scopes"]
        .as_array_mut()
        .unwrap()
        .push(mapping["scopes"][4].clone());
    assert_eq!(outcomes(&plan(&changed))["external-team"], held);
    changed["scopes"] = json!([]);
    let unreviewed = outcomes(&plan(&changed));
    for scope in ["_general", "_hive-userdata", "external-team"] {
        assert_eq!(unreviewed[scope], held, "{scope}");
    }

    assert_eq!(
        [
            ".git/index",
            ".git/HEAD",
            ".git/refs/heads/main",
            ".git/config"
        ]
        .map(|path| fs::read(source.join(path)).unwrap()),
        original_git_files,
    );
    assert!(git(&source, &home, &["status", "--porcelain"]).is_empty());
    assert!(!config.exists());
}

#[cfg(target_os = "linux")]
#[test]
fn cli_migration_plan_redacts_malformed_scope_and_bot_directories() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    fs::create_dir(&home).unwrap();
    let config = root.path().join("config");
    let source = root.path().join("source");
    init_source(&source, &home);

    let malformed_scope = "Synthetic Private Scope";
    let malformed_scope_paths = [
        "bots/queen-synth/memory/Synthetic Private Scope",
        "directives/Synthetic Private Scope",
        "tasks/Synthetic Private Scope",
    ];
    let portable_paths = [
        "bots/queen-synth/memory/portable-scope",
        "directives/portable-scope",
        "tasks/portable-scope",
    ];
    let malformed_bot_paths = [
        "bots/Synthetic Private Bot/memory/portable-scope",
        "bots/synthetic.private.bot/memory/portable-scope",
    ];
    let reference = "https://forge.example/synthetic/portable";
    let mut evidence = Vec::new();
    for path in malformed_bot_paths
        .into_iter()
        .chain(portable_paths)
        .chain(malformed_scope_paths)
        .rev()
    {
        let entrypoint = if path.starts_with("bots/") {
            "MEMORY.md"
        } else {
            "README.md"
        };
        let path = format!("{path}/{entrypoint}");
        let file = source.join(&path);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(
            file,
            format!("# Synthetic project\n\nProject: {reference}\n"),
        )
        .unwrap();
        if path.contains("/portable-scope/") {
            evidence.push(json!({"path": path, "canonical_ref": reference}));
        }
    }
    git(&source, &home, &["add", "."]);
    git(
        &source,
        &home,
        &["commit", "-qm", "Synthetic scope locator inputs"],
    );
    let commit = git_text(&source, &home, &["rev-parse", "HEAD"]);
    let workspace = id(1);
    let map = root.path().join("mapping.json");
    let input = json!({
        "version": 1,
        "source_commit": commit,
        "policy_pin": POLICY_PIN,
        "workspace_id": workspace,
        "scopes": [{
            "scope": "portable-scope",
            "decision": {
                "kind": "area",
                "area": {"id": id(AREA_APP), "slug": "portable", "name": "Portable project"},
                "project_evidence": evidence,
            }
        }],
    });
    fs::write(&map, serde_json::to_vec(&input).unwrap()).unwrap();
    let args = [
        "--source",
        source.to_str().unwrap(),
        "--source-commit",
        &commit,
        "--workspace-id",
        &workspace,
        "--mapping-file",
        map.to_str().unwrap(),
    ];
    let plan = || migration_cli(&home, &config, "migration-plan", &args, 0);
    let report = plan();
    assert_eq!(report, plan());
    let encoded = report.to_string();
    for private in [
        malformed_scope,
        "Synthetic Private Bot",
        "synthetic.private.bot",
    ] {
        assert!(!encoded.contains(private), "leaked {private}");
    }
    let scopes = report["data"]["scopes"].as_array().unwrap();
    let scope_digest = sha256_hex(malformed_scope.as_bytes());
    let digest_path = |path: &str| sha256_hex(path.as_bytes());
    let held = json!({"status": "held", "reason": "ambiguous_scope_or_collision"});
    assert_eq!(
        scopes
            .iter()
            .find(|scope| scope["scope_sha256"] == scope_digest),
        Some(&json!({
            "scope_sha256": scope_digest,
            "source_paths": [],
            "source_path_sha256s": malformed_scope_paths.map(digest_path),
            "outcome": held,
        })),
    );
    assert_eq!(
        scopes
            .iter()
            .find(|scope| scope["scope"] == "portable-scope"),
        Some(&json!({
            "scope": "portable-scope",
            "source_paths": portable_paths,
            "source_path_sha256s": malformed_bot_paths.map(digest_path),
            "outcome": held,
        })),
    );
    assert_eq!(git_text(&source, &home, &["rev-parse", "HEAD"]), commit);
    assert!(git(&source, &home, &["status", "--porcelain"]).is_empty());
}

#[cfg(target_os = "linux")]
#[test]
fn cli_migration_plan_intakes_qualified_tasks_and_holds_unverified_evidence() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    fs::create_dir(&home).unwrap();
    let config = root.path().join("config");
    let source = root.path().join("source");
    init_source(&source, &home);
    let app_ref = "https://forge.example/synthetic/app";
    let app_entrypoints = [
        "tasks/synth-app/README.md",
        "directives/synth-app/README.md",
        "bots/queen-synth/memory/synth-app/MEMORY.md",
    ];
    // The Task scope entrypoint comes from init_source. Add the same literal
    // reference to the other two scope entrypoints before committing the source.
    for path in &app_entrypoints[1..] {
        let entrypoint = source.join(path);
        fs::create_dir_all(entrypoint.parent().unwrap()).unwrap();
        fs::write(
            entrypoint,
            format!("# Synthetic App\n\nProject: {app_ref}\n"),
        )
        .unwrap();
    }
    let malformed_bot_origin = "bots/Synthetic-Bot@sample.invalid/memory/unmapped";
    fs::create_dir_all(source.join(malformed_bot_origin)).unwrap();
    fs::write(
        source.join(malformed_bot_origin).join("MEMORY.md"),
        "# Synthetic memory\n",
    )
    .unwrap();
    let claim = "claim: {id: private-claim-id, holder: private-claim-holder}\n";
    let invalid_work = "_general/backlog/Synthetic-Work@sample.invalid";
    let invalid_scope = "Synthetic-Scope@sample.invalid";
    let invalid_scope_work = format!("{invalid_scope}/backlog/item");
    let excluded_invalid_work = "external-team/active/Synthetic-External@sample.invalid";
    let fixtures = [
        ("_general/backlog/shared", "", "# Private H1 title\n"),
        (
            "_hive-userdata/backlog/shared",
            "tags: [private-synthetic-tag]\n",
            "Description fallback.\n",
        ),
        (
            "_general/active/shared",
            &format!("status: todo\n{claim}"),
            "Duplicated across areas.\n",
        ),
        (
            "_general/active/waiting",
            "status: todo\n",
            "## Acceptance\n\n- [ ] Private acceptance criterion\n",
        ),
        (
            "_general/active/claimed",
            &format!("status: todo\n{claim}"),
            "",
        ),
        (
            "_general/active/running",
            &format!("status: in-progress\n{claim}"),
            "",
        ),
        (
            "_general/active/running-no-claim",
            "status: in-progress\n",
            "",
        ),
        (
            "_general/active/reviewing",
            &format!("status: review\n{claim}"),
            "",
        ),
        (
            "_general/active/unclaimed-review",
            "status: review\n",
            "Submitted by: private-prose-holder\n",
        ),
        (
            "_general/active/claimed-done",
            &format!("status: done\n{claim}"),
            "",
        ),
        (
            "_general/archive/claimed-closed",
            &format!("status: cancelled\n{claim}"),
            "",
        ),
        (
            "_general/archive/cancelled",
            "status: cancelled\n",
            "Cancelled by: private-reviewer\n",
        ),
        ("_general/backlog/claimed-backlog", "claim: null\n", ""),
        ("_general/backlog/null-status", "status: null\n", ""),
        ("_general/backlog/null-tags", "tags: null\n", ""),
        (
            "_general/backlog/conflict",
            "",
            "# Private H1 one\n\n# Private H1 two\n",
        ),
        (
            "_general/backlog/title-override",
            "title: Private chosen title\n",
            "# First\n\n# Second\n",
        ),
        (
            "_general/backlog/duplicate-field",
            "title: First\ntitle: Second\n",
            "",
        ),
        (
            "_general/backlog/unknown-field",
            "depends_on: [synth-app/example]\n",
            "",
        ),
        ("_general/backlog/broken-blob", "", ""),
        (invalid_work, "", ""),
        (&invalid_scope_work, "", ""),
        (
            "_general/archive/with-child",
            "status: done\n",
            "Closed: 2026-04-01 00:00:00\nAccepted by: private-reviewer\nOutcome: private result\n\nChild: _general/cancelled\n",
        ),
        (
            "_general/archive/with-prerequisite",
            "status: done\n",
            "Prerequisite: _general/waiting\n",
        ),
        ("external-team/backlog/native", "", ""),
        (
            excluded_invalid_work,
            &format!("status: in-progress\n{claim}"),
            "",
        ),
        ("_hive/backlog/legacy", "", ""),
        (
            "unmapped/active/uncertain",
            &format!("status: in-progress\n{claim}"),
            "",
        ),
    ];
    for (bundle, fields, body) in fixtures {
        let file = source.join(format!("tasks/{bundle}/README.md"));
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        let record = format!("---\ndescription: \"Private task description\"\n{fields}---\n{body}");
        if bundle.ends_with("/broken-blob") {
            fs::write(file, [0xff]).unwrap();
        } else {
            fs::write(file, record).unwrap();
        }
    }
    let missing = source.join("tasks/_general/backlog/no-readme");
    fs::create_dir_all(&missing).unwrap();
    fs::write(missing.join("notes.md"), "Private supporting note").unwrap();
    git(&source, &home, &["add", "."]);
    git(
        &source,
        &home,
        &["commit", "-qm", "Synthetic task intake entries"],
    );
    let commit = git_text(&source, &home, &["rev-parse", "HEAD"]);
    let workspace = id(1);
    let file = root.path().join("mapping.json");
    let mapping = json!({
        "version": 1,
        "source_commit": commit,
        "policy_pin": POLICY_PIN,
        "workspace_id": workspace,
        "scopes": [
            {"scope": "_general", "decision": {"kind": "area", "area": {
                "id": id(AREA_GENERAL), "slug": "general", "name": "General"
            }}},
            {"scope": "_hive-userdata", "decision": {"kind": "area", "area": {
                "id": id(AREA_USERDATA), "slug": "operations", "name": "Operations"
            }}},
            {"scope": "external-team", "decision": {"kind": "external"}},
            {"scope": "synth-app", "decision": {
                "kind": "area",
                "area": {"id": id(AREA_APP), "slug": "app", "name": "Private Area name"},
                "project_evidence": app_entrypoints.map(|path| json!({
                    "path": path,
                    "canonical_ref": app_ref,
                })),
            }},
        ]
    });
    fs::write(&file, serde_json::to_vec(&mapping).unwrap()).unwrap();
    let mut args = vec![
        "--source",
        source.to_str().unwrap(),
        "--source-commit",
        &commit,
        "--workspace-id",
        &workspace,
        "--mapping-file",
        file.to_str().unwrap(),
    ];
    let scope_plan = migration_cli(&home, &config, "migration-plan", &args, 0)["data"].clone();
    assert!(scope_plan.get("task_intake").is_none());
    args.push("--tasks");

    let source_files = [
        ".git/index",
        ".git/HEAD",
        ".git/refs/heads/main",
        ".git/config",
        "AGENTS.md",
        "hive.yml",
    ];
    let original = source_files.map(|path| fs::read(source.join(path)).unwrap());
    let plan = migration_cli(&home, &config, "migration-plan", &args, 0)["data"].clone();
    assert_eq!(
        plan,
        migration_cli(&home, &config, "migration-plan", &args, 0)["data"]
    );
    for (key, value) in scope_plan.as_object().unwrap() {
        assert_eq!(&plan[key], value, "{key}");
    }
    assert_eq!(plan["source"]["source_commit"], commit);
    let intake = &plan["task_intake"];
    assert_eq!(intake["coverage"], "partial");
    let tasks = intake["tasks"].as_array().unwrap();
    assert_eq!(tasks.len(), fixtures.len() + 4);
    assert_eq!(
        intake["counts"],
        json!({
            "inventoried": tasks.len(), "held": tasks.len() - 2, "excluded": 2
        })
    );
    let redacted_paths = [invalid_work, &invalid_scope_work, excluded_invalid_work]
        .map(|bundle| format!("tasks/{bundle}/README.md"));
    let redacted_locators = redacted_paths
        .each_ref()
        .map(|path| sha256_hex(path.as_bytes()));
    let path_for = |task: &Value| {
        if let Some(path) = task["source_path"].as_str() {
            assert!(task.get("source_path_sha256").is_none());
            path.to_owned()
        } else {
            assert!(task.get("legacy_key").is_none());
            assert!(task.get("source_path").is_none());
            let index = redacted_locators
                .iter()
                .position(|digest| task["source_path_sha256"] == *digest)
                .expect("known redacted path digest");
            redacted_paths[index].clone()
        }
    };
    let ordered: Vec<_> = tasks
        .iter()
        .map(|task| {
            let path = path_for(task);
            let mut parts = path.split('/');
            let scope = parts.nth(1).unwrap();
            let work_id = parts.nth(1).unwrap();
            if task.get("legacy_key").is_some() {
                assert_eq!(
                    task["legacy_key"],
                    json!({"scope": scope, "work_id": work_id})
                );
            }
            (scope.to_owned(), work_id.to_owned(), path)
        })
        .collect();
    assert!(ordered.windows(2).all(|pair| pair[0] < pair[1]));
    for (task, (_, _, path)) in tasks.iter().zip(&ordered) {
        if path.ends_with("/no-readme") {
            assert!(task["source_git_blob_sha"].is_null());
            assert!(task["source_raw_sha256"].is_null());
        } else {
            let object = format!("{commit}:{path}");
            assert_eq!(
                task["source_git_blob_sha"],
                git_text(&source, &home, &["rev-parse", &object])
            );
            assert_eq!(
                task["source_raw_sha256"],
                sha256_hex(&git(&source, &home, &["show", &object]))
            );
        }
    }
    let outcome = |path| {
        &tasks
            .iter()
            .zip(&ordered)
            .find(|(_, (_, _, source_path))| source_path == &path)
            .map(|(task, _)| task)
            .unwrap()["outcome"]
    };
    for (path, reason) in [
        (
            "_general/backlog/shared/README.md",
            "duplicate_legacy_work_id",
        ),
        (
            "_general/active/claimed/README.md",
            "execution_needs_reconciliation",
        ),
        (
            "_general/active/running/README.md",
            "execution_needs_reconciliation",
        ),
        (
            "_general/active/running-no-claim/README.md",
            "invalid_legacy_state",
        ),
        (
            "_general/active/reviewing/README.md",
            "execution_needs_reconciliation",
        ),
        (
            "_general/active/unclaimed-review/README.md",
            "missing_typed_submission_evidence",
        ),
        (
            "_general/active/claimed-done/README.md",
            "invalid_legacy_state",
        ),
        (
            "_general/archive/claimed-closed/README.md",
            "invalid_legacy_state",
        ),
        (
            "_general/backlog/claimed-backlog/README.md",
            "invalid_legacy_state",
        ),
        (
            "_general/backlog/null-status/README.md",
            "invalid_legacy_state",
        ),
        (
            "_general/backlog/conflict/README.md",
            "missing_or_conflicting_title",
        ),
        (
            "_general/backlog/duplicate-field/README.md",
            "unsupported_task_metadata",
        ),
        (
            "_general/backlog/unknown-field/README.md",
            "unsupported_task_metadata",
        ),
        (
            "_general/backlog/broken-blob/README.md",
            "unsupported_task_metadata",
        ),
        (
            "_general/backlog/null-tags/README.md",
            "unsupported_task_metadata",
        ),
        ("_general/backlog/no-readme", "missing_task_entrypoint"),
        ("_hive/backlog/legacy/README.md", "unsplit_legacy_scope"),
    ] {
        assert_eq!(
            outcome(format!("tasks/{path}")),
            &json!({
                "status": "held",
                "reason_codes": [reason],
            }),
            "{path}"
        );
    }
    for (path, expected) in [
        (
            "tasks/_general/active/shared/README.md".to_owned(),
            json!({"status": "held", "reason_codes": ["duplicate_legacy_work_id", "execution_needs_reconciliation"]}),
        ),
        (
            "tasks/unmapped/active/uncertain/README.md".to_owned(),
            json!({"status": "held", "reason_codes": ["ambiguous_scope_or_collision", "execution_needs_reconciliation"]}),
        ),
        (
            redacted_paths[0].clone(),
            json!({"status": "held", "reason_codes": ["invalid_legacy_state"]}),
        ),
        (
            redacted_paths[1].clone(),
            json!({"status": "held", "reason_codes": ["ambiguous_scope_or_collision"]}),
        ),
        (
            redacted_paths[2].clone(),
            json!({"status": "excluded", "reason": "external_backend_owned"}),
        ),
    ] {
        assert_eq!(outcome(path), &expected);
    }
    let scopes = plan["scopes"].as_array().unwrap();
    let malformed_scope = scopes
        .iter()
        .find(|scope| scope["scope_sha256"] == sha256_hex(invalid_scope.as_bytes()))
        .unwrap();
    assert!(malformed_scope.get("scope").is_none());
    assert_eq!(malformed_scope["source_paths"], json!([]));
    assert_eq!(
        malformed_scope["source_path_sha256s"],
        json!([sha256_hex(format!("tasks/{invalid_scope}").as_bytes())])
    );
    let unmapped = scopes
        .iter()
        .find(|scope| scope["scope"] == "unmapped")
        .unwrap();
    assert_eq!(unmapped["source_paths"], json!(["tasks/unmapped"]));
    assert_eq!(
        unmapped["source_path_sha256s"],
        json!([sha256_hex(malformed_bot_origin.as_bytes())])
    );
    for (path, status, area, reasons) in [
        (
            "synth-app/backlog/implement-widget",
            "backlog",
            AREA_APP,
            vec!["unresolved_task_relations"],
        ),
        (
            "_hive-userdata/backlog/shared",
            "backlog",
            AREA_USERDATA,
            vec!["unresolved_task_relations"],
        ),
        (
            "_general/backlog/title-override",
            "backlog",
            AREA_GENERAL,
            vec!["unresolved_task_relations"],
        ),
        (
            "_general/active/waiting",
            "todo",
            AREA_GENERAL,
            vec![
                "unresolved_task_relations",
                "unverified_acceptance_criteria",
            ],
        ),
        (
            "_hive-userdata/active/unproven-done",
            "done",
            AREA_USERDATA,
            vec!["unresolved_completion_obligation"],
        ),
        (
            "_general/archive/with-child",
            "done",
            AREA_GENERAL,
            vec!["unresolved_completion_obligation"],
        ),
        (
            "_general/archive/with-prerequisite",
            "done",
            AREA_GENERAL,
            vec!["unresolved_completion_obligation"],
        ),
        (
            "_general/archive/close-checklist",
            "done",
            AREA_GENERAL,
            vec!["unresolved_completion_obligation"],
        ),
        (
            "_general/archive/cancelled",
            "cancelled",
            AREA_GENERAL,
            vec!["unresolved_task_relations"],
        ),
    ] {
        let mut reason_codes = vec!["incomplete_historical_provenance"];
        reason_codes.extend(reasons);
        assert_eq!(
            outcome(format!("tasks/{path}/README.md")),
            &json!({
                "status": "held", "candidate_status": status, "area_id": id(area), "reason_codes": reason_codes,
            }),
            "{path}"
        );
    }
    assert_eq!(
        outcome("tasks/external-team/backlog/native/README.md".to_owned()),
        &json!({
            "status": "excluded", "reason": "external_backend_owned"
        })
    );
    let output = plan.to_string().to_lowercase();
    for private in [
        "private",
        app_ref,
        "synthetic-reviewer",
        "synthetic checklist verified",
        "2026-04-01",
        "@sample.invalid",
        file.to_str().unwrap(),
    ] {
        assert!(!output.contains(private), "report exposed fixture data");
    }
    let mut conflicting = mapping.clone();
    conflicting["scopes"][3]["decision"]["project_evidence"][2]["canonical_ref"] =
        json!("https://forge.example/synthetic/other");
    fs::write(&file, serde_json::to_vec(&conflicting).unwrap()).unwrap();
    let conflicted = migration_cli(&home, &config, "migration-plan", &args, 0);
    let task = conflicted["data"]["task_intake"]["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|task| task["legacy_key"]["work_id"] == "implement-widget")
        .unwrap();
    assert_eq!(
        task["outcome"],
        json!({
            "status": "held",
            "reason_codes": ["ambiguous_scope_or_collision"],
        })
    );
    assert_eq!(
        original,
        source_files.map(|path| fs::read(source.join(path)).unwrap())
    );
    assert!(git(&source, &home, &["status", "--porcelain"]).is_empty());
    assert!(!config.exists());

    // The earlier pin has no Git-backed claims. An unclaimed in-progress task
    // remains an execution hold, and a claim on it is unsupported by that pin.
    fs::remove_file(source.join("hive.yml")).unwrap();
    let older_pin = "0fd0f8dbe632d14094a93599cb1b5f653789ac9c";
    fs::write(
        source.join("AGENTS.md"),
        format!("hive-system@{older_pin}\n"),
    )
    .unwrap();
    git(&source, &home, &["add", "-A"]);
    git(
        &source,
        &home,
        &["commit", "-qm", "Earlier synthetic policy pin"],
    );
    let older_commit = git_text(&source, &home, &["rev-parse", "HEAD"]);
    let mut mapping = mapping;
    mapping["policy_pin"] = json!(older_pin);
    mapping["source_commit"] = json!(older_commit);
    fs::write(&file, serde_json::to_vec(&mapping).unwrap()).unwrap();
    args[3] = &older_commit;
    let earlier = migration_cli(&home, &config, "migration-plan", &args, 0);
    for (id, reason) in [
        ("running", "invalid_legacy_state"),
        ("running-no-claim", "execution_needs_reconciliation"),
    ] {
        let task = earlier["data"]["task_intake"]["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|task| task["legacy_key"]["work_id"] == id)
            .unwrap();
        assert_eq!(task["outcome"]["reason_codes"], json!([reason]));
    }
}

#[cfg(target_os = "linux")]
#[test]
fn cli_migration_plan_rejects_unbound_inputs_and_unverified_source() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    fs::create_dir(&home).unwrap();
    let config = root.path().join("config");
    let source = root.path().join("source");
    let commit = init_source(&source, &home);
    let file = root.path().join("mapping.json");
    let workspace = id(1);
    let args = [
        "--source",
        source.to_str().unwrap(),
        "--source-commit",
        &commit,
        "--workspace-id",
        &workspace,
        "--mapping-file",
        file.to_str().unwrap(),
    ];
    let run = |args: &[&str], status, code: &str| {
        let report = migration_cli(&home, &config, "migration-plan", args, status);
        assert_eq!(report["error"]["code"], code);
        assert!(report.get("data").is_none());
    };
    let mapping = json!({
        "version": 1,
        "source_commit": commit,
        "policy_pin": POLICY_PIN,
        "workspace_id": workspace,
        "scopes": [],
    });
    let write = |value: &Value| fs::write(&file, serde_json::to_vec(value).unwrap()).unwrap();
    run(&args[..6], 2, "INVALID_ARGUMENT");
    run(&args, 2, "INVALID_CONFIG");
    for (key, value) in [
        ("version", json!(2)),
        (
            "source_commit",
            json!("1111111111111111111111111111111111111111"),
        ),
        (
            "policy_pin",
            json!("0fd0f8dbe632d14094a93599cb1b5f653789ac9c"),
        ),
        ("workspace_id", json!(id(2))),
        ("unexpected", json!(true)),
        (
            "scopes",
            json!([{"scope": "external-team", "decision": {"kind": "external", "area": {}}}]),
        ),
        (
            "scopes",
            json!([{"scope": "absent", "decision": {"kind": "external"}}]),
        ),
    ] {
        let mut changed = mapping.clone();
        changed[key] = value;
        write(&changed);
        run(&args, 2, "INVALID_CONFIG");
    }
    fs::write(
        &file,
        format!("{{\"version\":1,{}", &mapping.to_string()[1..]),
    )
    .unwrap();
    run(&args, 2, "INVALID_CONFIG");
    write(&mapping);
    let duplicate = [args.as_slice(), &args[6..]].concat();
    run(&duplicate, 2, "INVALID_ARGUMENT");
    let extra = [args.as_slice(), &["--workspace", source.to_str().unwrap()]].concat();
    run(&extra, 2, "INVALID_ARGUMENT");

    let tracked = source.join("tasks/synth-app/README.md");
    let original = fs::read(&tracked).unwrap();
    fs::write(&tracked, "# Uncommitted identity").unwrap();
    run(&args, 5, "DIRTY_CONFLICT");
    fs::write(&tracked, original).unwrap();

    let object = git_text(
        &source,
        &home,
        &["rev-parse", "HEAD:tasks/synth-app/README.md"],
    );
    let object_file = source
        .join(".git/objects")
        .join(&object[..2])
        .join(&object[2..]);
    let hidden = home.join("missing-scope-object");
    fs::rename(&object_file, &hidden).unwrap();
    run(&args, 7, "OBJECT_UNAVAILABLE");
    fs::rename(hidden, object_file).unwrap();
    migration_cli(&home, &config, "migration-plan", &args, 0);
    assert!(!config.exists());
}

#[cfg(target_os = "linux")]
#[test]
fn cli_preflights_only_a_clean_pinned_v1_source() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    fs::create_dir(&home).unwrap();
    let config = root.path().join("xdg-config");
    let source = root.path().join("legacy-v1");
    let commit = init_source(&source, &home);
    let path = source.to_str().unwrap();
    let cli = |args: &[&str], status| migration_preflight(&home, &config, args, status);
    let args = ["--source", path, "--source-commit", &commit];
    let original_git_files = [
        ".git/HEAD",
        ".git/index",
        ".git/config",
        ".git/refs/heads/main",
    ]
    .map(|name| fs::read(source.join(name)).unwrap());
    let report = cli(&args, 0);
    assert_eq!(report, cli(&args, 0));
    let data = &report["data"];
    assert_eq!(data["mode"], "source_preflight");
    assert_eq!(data["source"]["source_commit"], commit);
    assert_eq!(data["source"]["source_clean"], true);
    assert_eq!(data["source"]["policy_pin"], POLICY_PIN);
    assert_eq!(
        data["source"]["hive_yml"],
        json!({"version": 1, "mode": "local"})
    );
    assert_eq!(data["inventory"]["file_count"], SYNTHETIC_V1_FILES.len());
    assert_eq!(
        data["inventory"]["blob_bytes"],
        SYNTHETIC_V1_FILES
            .iter()
            .map(|(_, content)| content.len())
            .sum::<usize>()
    );
    assert_eq!(data["inventory"]["rulesync_directory"], false);
    assert_eq!(
        data["source"]["root_tree_sha256"],
        sha256_hex(&git(&source, &home, &["cat-file", "tree", "HEAD^{tree}"]))
    );
    let encoded = report.to_string();
    assert!(!encoded.contains("tasks/synth-app/"));
    assert!(!encoded.contains("Synthetic persona bytes."));
    assert!(!config.exists());
    assert_eq!(
        [
            ".git/HEAD",
            ".git/index",
            ".git/config",
            ".git/refs/heads/main"
        ]
        .map(|name| fs::read(source.join(name)).unwrap()),
        original_git_files
    );
    assert!(git(&source, &home, &["status", "--porcelain"]).is_empty());

    let refuse =
        |args: &[&str], status, code: &str| assert_eq!(cli(args, status)["error"]["code"], code);
    refuse(&["--source", path], 2, "INVALID_ARGUMENT");
    refuse(
        &["--source", path, "--source-commit", "HEAD"],
        2,
        "INVALID_ARGUMENT",
    );
    refuse(
        &["--source", "legacy-v1", "--source-commit", &commit],
        2,
        "INVALID_ARGUMENT",
    );
    let duplicate = [args.as_slice(), &["--source", path]].concat();
    refuse(&duplicate, 2, "INVALID_ARGUMENT");
    let blob = git_text(
        &source,
        &home,
        &["rev-parse", "HEAD:bots/queen-synth/SOUL.md"],
    );
    let object = source
        .join(".git/objects")
        .join(&blob[..2])
        .join(&blob[2..]);
    let hidden_object = home.join("missing-object");
    fs::rename(&object, &hidden_object).unwrap();
    refuse(&args, 7, "OBJECT_UNAVAILABLE");
    fs::rename(hidden_object, object).unwrap();
    let nested = source.join("tasks");
    refuse(
        &[
            "--source",
            nested.to_str().unwrap(),
            "--source-commit",
            &commit,
        ],
        3,
        "NOT_FOUND",
    );
    let untracked = source.join("untracked");
    fs::write(&untracked, "dirty").unwrap();
    refuse(&args, 5, "DIRTY_CONFLICT");
    fs::create_dir_all(config.join("git")).unwrap();
    let external_ignore = config.join("git/ignore");
    fs::write(&external_ignore, "/untracked\n").unwrap();
    refuse(&args, 5, "DIRTY_CONFLICT");
    git(
        &source,
        &home,
        &[
            "config",
            "core.excludesFile",
            external_ignore.to_str().unwrap(),
        ],
    );
    refuse(&args, 5, "DIRTY_CONFLICT");
    fs::remove_file(untracked).unwrap();
    git(
        &source,
        &home,
        &["update-index", "--assume-unchanged", "hive.yml"],
    );
    refuse(&args, 7, "AUTHORITY_UNVERIFIED");
    git(
        &source,
        &home,
        &["update-index", "--no-assume-unchanged", "hive.yml"],
    );

    let yaml = source.join("hive.yml");
    fs::write(
        &yaml,
        "version: 1\ncoordination:\n  mode: remote\n  remote: synthetic-private-alias\n",
    )
    .unwrap();
    refuse(&args, 5, "DIRTY_CONFLICT");
    git(&source, &home, &["add", "hive.yml"]);
    refuse(&args, 5, "DIRTY_CONFLICT");
    let commit_all = || {
        git(&source, &home, &["add", "-A"]);
        git(&source, &home, &["commit", "-qm", "Synthetic variation"]);
        git_text(&source, &home, &["rev-parse", "HEAD"])
    };
    fs::create_dir(source.join(".rulesync")).unwrap();
    fs::write(source.join(".rulesync/rule.md"), "Synthetic rule").unwrap();
    fs::write(source.join("rulesync.jsonc"), "{}").unwrap();
    fs::create_dir_all(source.join(".git/info")).unwrap();
    fs::write(source.join(".git/info/exclude"), "/ignored-scratch\n").unwrap();
    fs::write(source.join("ignored-scratch"), "Ignored local bytes").unwrap();
    let current = commit_all();
    refuse(&args, 5, "STALE_REVISION");
    let current_report = cli(&["--source", path, "--source-commit", &current], 0);
    assert_eq!(
        current_report["data"]["source"]["hive_yml"]["mode"],
        "remote"
    );
    assert_eq!(
        current_report["data"]["inventory"]["rulesync_directory"],
        true
    );
    assert_eq!(current_report["data"]["inventory"]["rulesync_config"], true);
    assert_eq!(
        current_report["data"]["inventory"]["file_count"],
        SYNTHETIC_V1_FILES.len() + 2
    );
    assert_ne!(
        current_report["data"]["inventory"]["catalog_sha256"],
        data["inventory"]["catalog_sha256"]
    );
    assert!(
        !current_report
            .to_string()
            .contains("synthetic-private-alias")
    );

    fs::write(&yaml, "version: 2\ncoordination:\n  mode: local\n").unwrap();
    let current = commit_all();
    refuse(
        &["--source", path, "--source-commit", &current],
        4,
        "INCOMPATIBLE_POLICY",
    );
    fs::remove_file(yaml).unwrap();
    let current = commit_all();
    refuse(
        &["--source", path, "--source-commit", &current],
        2,
        "INVALID_CONFIG",
    );
    let policy = source.join("AGENTS.md");
    fs::write(
        &policy,
        format!("hive-system@{POLICY_PIN}\nhive-system@1111111111111111111111111111111111111111\n"),
    )
    .unwrap();
    let current = commit_all();
    refuse(
        &["--source", path, "--source-commit", &current],
        4,
        "INCOMPATIBLE_POLICY",
    );
    fs::remove_file(policy).unwrap();
    let current = commit_all();
    refuse(
        &["--source", path, "--source-commit", &current],
        7,
        "POLICY_UNAVAILABLE",
    );
}

#[cfg(target_os = "linux")]
fn loose_object(repo: &Path, oid: &str) -> std::path::PathBuf {
    repo.join(".git/objects").join(&oid[..2]).join(&oid[2..])
}

#[cfg(target_os = "linux")]
#[test]
fn cli_preflight_rejects_symlinked_repository_excludes() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    fs::create_dir(&home).unwrap();
    let source = root.path().join("legacy-v1");
    init_source(&source, &home);
    fs::write(
        source.join(".gitignore"),
        "/ignored-by-gitignore\n!/visible-by-gitignore\n",
    )
    .unwrap();
    git(&source, &home, &["add", ".gitignore"]);
    git(&source, &home, &["commit", "-qm", "Synthetic ignore"]);
    let commit = git_text(&source, &home, &["rev-parse", "HEAD"]);
    fs::write(source.join("ignored-by-gitignore"), "Local ignored bytes").unwrap();
    let untracked = source.join("synthetic-untracked-probe");
    fs::write(&untracked, "Synthetic untracked bytes").unwrap();

    let info = source.join(".git/info");
    fs::create_dir_all(&info).unwrap();
    let exclude = info.join("exclude");
    let external_info = home.join("synthetic-private-exclude-location");
    fs::create_dir(&external_info).unwrap();
    let external_exclude = external_info.join("exclude");
    let rules =
        b"# Synthetic private ignore contents\n/synthetic-untracked-probe\n/visible-by-gitignore\n";
    fs::write(&external_exclude, rules).unwrap();
    fs::write(&exclude, rules).unwrap();
    let args = [
        "--source",
        source.to_str().unwrap(),
        "--source-commit",
        &commit,
    ];
    let preflight = |status| migration_preflight(&home, &home, &args, status);
    let git_files = [
        ".git/HEAD",
        ".git/index",
        ".git/config",
        ".git/refs/heads/main",
    ];
    let git_before = git_files.map(|name| fs::read(source.join(name)).unwrap());
    let original = preflight(0);
    let visible = source.join("visible-by-gitignore");
    fs::write(&visible, "Negated by .gitignore").unwrap();
    assert_eq!(preflight(5)["error"]["code"], "DIRTY_CONFLICT");
    fs::remove_file(visible).unwrap();

    let refuse = || {
        assert!(git(&source, &home, &["status", "--porcelain"]).is_empty());
        let report = preflight(2);
        assert_eq!(report["error"]["code"], "INVALID_CONFIG");
        assert!(report.get("data").is_none_or(Value::is_null));
        let text = report.to_string();
        for private in [
            "synthetic-private-exclude-location",
            "Synthetic private ignore contents",
            "synthetic-untracked-probe",
        ] {
            assert!(!text.contains(private));
        }
        assert_eq!(fs::read(&external_exclude).unwrap(), rules);
        assert_eq!(fs::read(&untracked).unwrap(), b"Synthetic untracked bytes");
        assert_eq!(
            git_files.map(|name| fs::read(source.join(name)).unwrap()),
            git_before
        );
    };

    fs::remove_file(&exclude).unwrap();
    symlink(&external_exclude, &exclude).unwrap();
    refuse();
    assert_eq!(fs::read_link(&exclude).unwrap(), external_exclude);
    fs::remove_file(&exclude).unwrap();
    let saved_info = home.join("saved-info");
    fs::rename(&info, &saved_info).unwrap();
    symlink(&external_info, &info).unwrap();
    refuse();
    assert_eq!(fs::read_link(&info).unwrap(), external_info);
    fs::remove_file(&info).unwrap();
    fs::rename(&saved_info, &info).unwrap();

    assert_eq!(preflight(5)["error"]["code"], "DIRTY_CONFLICT");
    fs::remove_file(&untracked).unwrap();
    assert_eq!(preflight(0), original);
}

#[cfg(target_os = "linux")]
#[test]
fn cli_preflight_uses_the_copied_exclude_during_untracked_enumeration() {
    use std::os::unix::fs::{PermissionsExt, symlink};

    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    fs::create_dir(&home).unwrap();
    let source = root.path().join("legacy-v1");
    let commit = init_source(&source, &home);
    let info = source.join(".git/info");
    fs::create_dir_all(&info).unwrap();
    let exclude = info.join("exclude");
    fs::write(&exclude, "/safe-scratch\n").unwrap();
    fs::write(source.join("safe-scratch"), "Ignored local bytes").unwrap();
    let untracked = source.join("synthetic-visible-file");
    fs::write(&untracked, "Untracked bytes").unwrap();
    let external = home.join("external-exclude");
    fs::write(&external, "/synthetic-visible-file\n/safe-scratch\n").unwrap();

    let bin = home.join("bin");
    fs::create_dir(&bin).unwrap();
    let path = std::env::var_os("PATH").unwrap();
    let git_path = std::env::split_paths(&path)
        .map(|dir| dir.join("git"))
        .find(|path| path.is_file())
        .unwrap()
        .canonicalize()
        .unwrap();
    symlink(git_path, home.join("real-git")).unwrap();
    let wrapper = bin.join("git");
    fs::write(
        &wrapper,
        r#"#!/bin/sh
set -eu
for arg in "$@"; do
    case "$arg" in
        status|--others)
            exclude="$HOME/../legacy-v1/.git/info/exclude"
            mv "$exclude" "$exclude.saved"
            trap 'rm -f "$exclude"; mv "$exclude.saved" "$exclude"' EXIT
            ln -s "$HOME/external-exclude" "$exclude"
            echo "$arg" >> "$HOME/swaps"
            "$HOME/real-git" "$@"
            exit
            ;;
    esac
done
exec "$HOME/real-git" "$@"
"#,
    )
    .unwrap();
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();
    let search_path =
        std::env::join_paths([bin].into_iter().chain(std::env::split_paths(&path))).unwrap();
    let args = [
        "--source",
        source.to_str().unwrap(),
        "--source-commit",
        &commit,
    ];
    let report = migration_preflight_with_path(&home, &home, &args, 5, &search_path);
    assert_eq!(report["error"]["code"], "DIRTY_CONFLICT");
    let text = report.to_string();
    assert!(!text.contains("synthetic-visible-file"));
    assert!(!text.contains("external-exclude"));
    let swaps = fs::read_to_string(home.join("swaps")).unwrap();
    assert_eq!(swaps, "status\n--others\n");
    assert_eq!(fs::read(&exclude).unwrap(), b"/safe-scratch\n");
    assert!(fs::symlink_metadata(exclude).unwrap().is_file());
    assert_eq!(fs::read(&untracked).unwrap(), b"Untracked bytes");
    assert_eq!(
        fs::read(&external).unwrap(),
        b"/synthetic-visible-file\n/safe-scratch\n"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn cli_preflight_detects_a_racy_tracked_edit_without_changing_source_metadata() {
    use std::os::unix::fs::MetadataExt;
    use std::time::{Duration, UNIX_EPOCH};

    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    fs::create_dir(&home).unwrap();
    let source = root.path().join("legacy-v1");
    init_source(&source, &home);
    git(&source, &home, &["config", "core.trustctime", "false"]);
    let tracked = source.join("synthetic-racy-task.md");
    let index = source.join(".git/index");
    let timestamp = UNIX_EPOCH + Duration::new(1_700_000_000, 123_456_789);
    let set_mtime = |path: &Path| {
        fs::File::open(path)
            .unwrap()
            .set_modified(timestamp)
            .unwrap();
    };
    fs::write(&tracked, b"Synthetic task A\n").unwrap();
    set_mtime(&tracked);
    git(&source, &home, &["add", "synthetic-racy-task.md"]);
    git(
        &source,
        &home,
        &["commit", "-qm", "Synthetic racy index entry"],
    );
    let commit = git_text(&source, &home, &["rev-parse", "HEAD"]);
    set_mtime(&index);

    fs::write(&tracked, b"Synthetic task B\n").unwrap();
    set_mtime(&tracked);
    let snapshot = || {
        [&index, &tracked].map(|path| {
            let metadata = fs::metadata(path).unwrap();
            (
                fs::read(path).unwrap(),
                metadata.modified().unwrap(),
                metadata.ctime(),
                metadata.ctime_nsec(),
            )
        })
    };
    let before = snapshot();
    assert_eq!(before[0].1, before[1].1);
    assert_eq!(
        git(
            &source,
            &home,
            &["status", "--porcelain=v1", "--untracked-files=no"]
        ),
        b" M synthetic-racy-task.md\n",
    );
    let args = [
        "--source",
        source.to_str().unwrap(),
        "--source-commit",
        &commit,
    ];
    let report = migration_preflight(&home, &home, &args, 5);
    assert_eq!(report["error"]["code"], "DIRTY_CONFLICT");
    assert!(!report.to_string().contains("synthetic-racy-task"));
    assert_eq!(snapshot(), before);
}

#[cfg(target_os = "linux")]
#[test]
fn cli_preflight_refuses_a_split_index_without_refreshing_source_metadata() {
    use std::os::unix::fs::MetadataExt;
    use std::time::{Duration, UNIX_EPOCH};

    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    fs::create_dir(&home).unwrap();
    let source = root.path().join("legacy-v1");
    let commit = init_source(&source, &home);
    git(&source, &home, &["update-index", "--split-index"]);
    let git_dir = source.join(".git");
    let shared = fs::read_dir(&git_dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            path.file_name()
                .unwrap()
                .as_encoded_bytes()
                .starts_with(b"sharedindex.")
        })
        .unwrap();
    let paths = [git_dir.join("index"), shared];
    for path in &paths {
        fs::File::open(path)
            .unwrap()
            .set_modified(UNIX_EPOCH + Duration::from_secs(1_700_000_000))
            .unwrap();
    }
    let snapshot = || {
        paths.each_ref().map(|path| {
            let metadata = fs::metadata(path).unwrap();
            (
                fs::read(path).unwrap(),
                metadata.modified().unwrap(),
                metadata.ctime(),
                metadata.ctime_nsec(),
            )
        })
    };
    let before = snapshot();
    let args = [
        "--source",
        source.to_str().unwrap(),
        "--source-commit",
        &commit,
    ];
    let report = migration_preflight(&home, &home, &args, 7);
    assert_eq!(report["error"]["code"], "AUTHORITY_UNVERIFIED");
    assert!(report.get("data").is_none_or(Value::is_null));
    assert!(!report.to_string().contains("sharedindex."));
    assert_eq!(snapshot(), before);
}

#[cfg(target_os = "linux")]
#[test]
fn cli_preflight_rejects_a_truncated_task_blob_with_readable_size() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    fs::create_dir(&home).unwrap();
    let source = root.path().join("legacy-v1");
    let commit = init_source(&source, &home);
    let task = "tasks/synth-app/backlog/implement-widget/README.md";
    let oid = git_text(&source, &home, &["rev-parse", &format!("HEAD:{task}")]);
    let object = loose_object(&source, &oid);
    let args = [
        "--source",
        source.to_str().unwrap(),
        "--source-commit",
        &commit,
    ];
    let preflight = |status| migration_preflight(&home, &home, &args, status);
    let original = preflight(0);

    // Keep the compressed header intact while discarding most of the body.
    let bytes = fs::read(&object).unwrap();
    let damaged = &bytes[..bytes.len() / 2];
    let size = git_text(&source, &home, &["cat-file", "-s", &oid]);
    fs::remove_file(&object).unwrap();
    fs::write(&object, damaged).unwrap();
    assert_eq!(git_text(&source, &home, &["cat-file", "-s", &oid]), size);
    assert!(git(&source, &home, &["status", "--porcelain"]).is_empty());

    let report = preflight(7);
    assert_eq!(report["error"]["code"], "OBJECT_UNAVAILABLE");
    assert!(report.get("data").is_none_or(Value::is_null));
    assert!(!report.to_string().contains(task));
    assert!(!report.to_string().contains(&oid));
    assert_eq!(fs::read(&object).unwrap(), damaged);
    fs::write(&object, bytes).unwrap();
    assert_eq!(preflight(0), original);
}

#[cfg(target_os = "linux")]
#[test]
fn cli_preflight_rejects_subtree_bytes_that_do_not_match_its_oid() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    fs::create_dir(&home).unwrap();
    let source = root.path().join("legacy-v1");
    let commit = init_source(&source, &home);
    let oid = git_text(&source, &home, &["rev-parse", "HEAD:tasks"]);
    let object = loose_object(&source, &oid);
    let bytes = fs::read(&object).unwrap();
    let args = [
        "--source",
        source.to_str().unwrap(),
        "--source-commit",
        &commit,
    ];
    let preflight = |status| migration_preflight(&home, &home, &args, status);
    let original = preflight(0);

    // An empty directory changes the committed catalog but adds no index entries.
    let raw = home.join("raw-tree");
    let write_tree = || {
        git_text(
            &source,
            &home,
            &["hash-object", "-w", "-t", "tree", raw.to_str().unwrap()],
        )
    };
    fs::write(&raw, []).unwrap();
    let empty = write_tree();
    let mut tree = git(&source, &home, &["cat-file", "tree", &oid]);
    tree.extend_from_slice(b"40000 z-synthetic-empty\0");
    for offset in (0..empty.len()).step_by(2) {
        tree.push(u8::from_str_radix(&empty[offset..offset + 2], 16).unwrap());
    }
    fs::write(&raw, &tree).unwrap();
    let replacement = write_tree();
    assert_ne!(replacement, oid);
    let substituted = fs::read(loose_object(&source, &replacement)).unwrap();
    fs::remove_file(&object).unwrap();
    fs::write(&object, &substituted).unwrap();
    assert_eq!(git(&source, &home, &["cat-file", "tree", &oid]), tree);
    assert!(git(&source, &home, &["status", "--porcelain"]).is_empty());

    let report = preflight(7);
    assert_eq!(report["error"]["code"], "OBJECT_UNAVAILABLE");
    assert!(report.get("data").is_none_or(Value::is_null));
    assert!(!report.to_string().contains("z-synthetic-empty"));
    assert!(!report.to_string().contains(&oid));
    assert_eq!(fs::read(&object).unwrap(), substituted);
    fs::write(&object, bytes).unwrap();
    assert_eq!(preflight(0), original);
}

#[cfg(target_os = "linux")]
#[test]
fn cli_preflight_preserves_legacy_policy_and_catalog_after_packing() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    fs::create_dir(&home).unwrap();
    for format in ["sha1", "sha256"] {
        let source = root.path().join(format);
        fs::create_dir(&source).unwrap();
        git(
            &source,
            &home,
            &["init", "-q", &format!("--object-format={format}")],
        );
        let policy = "hive-system@0fd0f8dbe632d14094a93599cb1b5f653789ac9c\n";
        fs::write(source.join("AGENTS.md"), policy).unwrap();
        fs::create_dir(source.join("tasks")).unwrap();
        let body = "Synthetic duplicate blob";
        fs::write(source.join("tasks/one.md"), body).unwrap();
        fs::write(source.join("tasks/two.md"), body).unwrap();
        git(&source, &home, &["add", "."]);
        git(
            &source,
            &home,
            &[
                "-c",
                "user.name=Synthetic Fixture",
                "-c",
                "user.email=synthetic@example.invalid",
                "commit",
                "-qm",
                "Synthetic legacy workspace",
            ],
        );
        let commit = git_text(&source, &home, &["rev-parse", "HEAD"]);
        let args = [
            "--source",
            source.to_str().unwrap(),
            "--source-commit",
            &commit,
        ];
        let report = migration_preflight(&home, &home, &args, 0);
        assert_eq!(report["data"]["source"]["source_commit"], commit);
        assert!(report["data"]["source"]["hive_yml"].is_null());
        assert_eq!(report["data"]["inventory"]["file_count"], 3);
        assert_eq!(
            report["data"]["inventory"]["blob_bytes"],
            policy.len() + 2 * body.len()
        );
        git(&source, &home, &["repack", "-ad"]);
        assert!(!loose_object(&source, &commit).exists());
        assert_eq!(migration_preflight(&home, &home, &args, 0), report);
    }
}

#[test]
#[ignore = "requires the pinned Dolt binary at HIVE_DOLT_BIN"]
fn rehearses_v1_to_v2_migration_mapping_rollback_and_committed_validation() {
    let fixture = Fixture::start_empty();
    let home = fixture.root.join("home");
    let source_repo = fixture.root.join("legacy-v1");
    let source_commit = init_source(&source_repo, &home);
    let manifest = FixtureManifest::reviewed(&source_commit);
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

    let projection = project_fixture_snapshot(&source_repo, &home, &manifest);

    // SQL rollback leaves zero domain rows, counter == 1, and unchanged HEAD/work/stage.
    let mut admin = fixture.connect(ADMIN);
    let base_state = storage_state(&mut admin);
    fixture.check(0, &mut admin);

    admin
        .query_drop("SET @@SESSION.dolt_transaction_commit = 1")
        .unwrap();
    admin.query_drop("START TRANSACTION").unwrap();
    apply_fixture_sql(&mut admin, &projection, &manifest);
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
    let target_assets = fixture.root.join("target-workspace").join("assets");
    init_git(
        &target_assets,
        &home,
        "Synthetic Target",
        "target@example.invalid",
    );
    let soul_path = target_assets.join("bots/queen-synth/SOUL.md");
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
        "bots/queen-synth/SOUL.md"
    );
    let committed_soul = git(
        &target_assets,
        &home,
        &["show", &format!("{asset_commit}:bots/queen-synth/SOUL.md")],
    );
    assert_eq!(
        sha256_hex(&committed_soul),
        projection.hashes[&id(BOT_QUEEN)].0
    );

    // Commit isolated v2 Dolt target and verify with the actual compiled Rust v2 validator.
    admin.query_drop("START TRANSACTION").unwrap();
    apply_fixture_sql(&mut admin, &projection, &manifest);
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
    for (table, expected) in [
        ("areas", 3),
        ("projects", 1),
        ("resources", 1),
        ("area_resources", 1),
        ("issues", 2),
        ("project_issues", 1),
        ("issue_resources", 1),
        ("issue_claims", 0),
        ("directives", 2),
        ("artifacts", 1),
        ("artifact_issues", 1),
        ("artifact_areas", 1),
        ("bots", 1),
        ("bot_memories", 0),
    ] {
        assert_eq!(
            row_count(&mut admin, table),
            expected,
            "committed rows in {table}"
        );
    }

    let membership: (u64, String, String, String) = admin
        .exec_first(
            "SELECT i.number, i.area_id, pi.project_id, ir.resource_id
             FROM issues i
             JOIN project_issues pi ON pi.issue_id = i.id
             JOIN issue_resources ir ON ir.issue_id = i.id
             JOIN area_resources ar ON ar.area_id = i.area_id AND ar.resource_id = ir.resource_id
             WHERE i.id = ?",
            (id(0x112),),
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        membership,
        (2, id(AREA_APP), id(PROJECT_RELEASE), id(RESOURCE_REPO))
    );

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

    let artifact_link: (String, String, String, String, String) = admin
        .exec_first(
            "SELECT a.kind, DATE_FORMAT(a.substantive_updated_at, '%Y-%m-%d %H:%i:%s'), ai.issue_id, a.author_ref, aa.area_id
             FROM artifacts a
             JOIN artifact_issues ai ON ai.artifact_id = a.id
             JOIN artifact_areas aa ON aa.artifact_id = a.id
             WHERE a.id = ?",
            (id(ARTIFACT_REPORT),),
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        artifact_link,
        (
            "report".to_owned(),
            "2026-01-02 01:00:00".to_owned(),
            id(0x112),
            "synthetic-author".to_owned(),
            id(AREA_APP),
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
