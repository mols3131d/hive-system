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

#[cfg(target_os = "linux")]
#[path = "migration/directive_artifact.rs"]
mod directive_artifact;

const POLICY_PIN: &str = "0e584830a7eff314d85b7af8c6f8cd69e735730a";
const EARLIER_POLICY_PIN: &str = "0fd0f8dbe632d14094a93599cb1b5f653789ac9c";
const FORM_FEED_CRITERIA: &str = "A\x0cB";
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
#[ignore = "requires HIVE_STAGING_INSTALLED_BIN, pinned Dolt at HIVE_DOLT_BIN, and native Git"]
fn installed_cli_stages_only_reviewed_scopes_and_preserves_both_contracts() {
    const V2_POLICY: &str = "c66b3c50150e18426485d134de92c6b311dac125";
    const REFERENCE: &str = "https://forge.example/synthetic/application";
    const NAME: &str = "Synthetic café '); DROP TABLE areas; -- 🐝";
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    let home = root.join("home");
    fs::create_dir(&home).unwrap();
    let config = root.join("config");
    let state = root.join("state");
    let data = root.join("data");
    let source = root.join("source");
    let target = root.join("inactive workspace");
    let mapping_file = root.join("reviewed.json");
    let workspace_id = id(1);
    let git_at_source = |args: &[&str]| git_text(&source, &home, args);
    init_source(&source, &home);
    let app_sources = [
        "tasks/synth-app/README.md",
        "directives/synth-app/README.md",
        "bots/queen-synth/memory/synth-app/MEMORY.md",
    ];
    for path in app_sources {
        fs::write(source.join(path), format!("# Application\n\n{REFERENCE}\n")).unwrap();
    }
    git_at_source(&["add", "."]);
    git_at_source(&["commit", "-qm", "Review synthetic scope identity"]);
    let commit = git_at_source(&["rev-parse", "HEAD"]);
    let verify_source = || {
        assert_eq!(git_at_source(&["rev-parse", "HEAD"]), commit);
        assert!(git_at_source(&["status", "--porcelain", "--untracked-files=all"]).is_empty());
    };
    let sources = [
        "AGENTS.md",
        "hive.yml",
        ".git/index",
        ".git/config",
        ".git/HEAD",
    ];
    let source_bytes = sources.map(|name| fs::read(source.join(name)).unwrap());

    fs::create_dir_all(config.join("hive/v2")).unwrap();
    let v1_config = config.join("hive/config.yml");
    let v2_config = config.join("hive/v2/config.yml");
    let v1_config_bytes = format!("workspace: {}\n", source.display());
    let v2_config_bytes = format!("version: 2\nworkspace: {}\n", target.display());
    fs::write(&v1_config, &v1_config_bytes).unwrap();
    let installed = std::env::var_os("HIVE_STAGING_INSTALLED_BIN").unwrap();
    let cli = |contract: &str| {
        let mut command = Command::new(&installed);
        command
            .current_dir(&home)
            .env_clear()
            .env("HOME", &home)
            .env("PATH", std::env::var_os("PATH").unwrap())
            .env("XDG_CONFIG_HOME", &config)
            .env("XDG_STATE_HOME", &state)
            .env("XDG_DATA_HOME", &data)
            .env("TMPDIR", root)
            .args(["--contract", contract, "--json"]);
        command
    };
    let response = |command: &mut Command, status| {
        let output = command.output().unwrap();
        assert_eq!(output.status.code(), Some(status), "{output:?}");
        assert!(output.stderr.is_empty(), "{output:?}");
        serde_json::from_slice::<Value>(&output.stdout).unwrap()
    };
    let v1 = || response(cli("1").args(["config", "show", "--scope", "workspace"]), 0);
    let before = v1();
    assert_eq!(before["data"]["policy_revision"], POLICY_PIN);

    let general = id(AREA_GENERAL);
    let userdata = id(AREA_USERDATA);
    let app = id(AREA_APP);
    let mapping = json!({
        "version": 1, "source_commit": commit, "policy_pin": POLICY_PIN, "workspace_id": workspace_id,
        "queen_assets": [],
        "scopes": [
            {"scope": "_hive-userdata", "decision": {"kind": "area",
             "area": {"id": userdata, "parent_id": app, "slug": "workspace-operations", "name": "Operations"}}},
            {"scope": "synth-app", "decision": {"kind": "area",
             "area": {"id": app, "parent_id": general, "slug": "application", "name": NAME},
             "project_evidence": app_sources.map(|path| json!({"path": path, "canonical_ref": REFERENCE}))}},
            {"scope": "_general", "decision": {"kind": "area",
             "area": {"id": general, "slug": "general", "name": "General"}}},
            {"scope": "external-team", "decision": {"kind": "external"}},
        ],
    });
    let stage_at = |mode: &str, policy: &str, destination: &Path, revision: &str| {
        let mut command = cli("2");
        command
            .args([
                "workspace",
                "migration-stage",
                "--mode",
                mode,
                "--policy-revision",
                policy,
                "--source",
            ])
            .arg(&source)
            .args([
                "--source-commit",
                revision,
                "--workspace-id",
                &workspace_id,
                "--mapping-file",
            ])
            .arg(&mapping_file)
            .arg("--target")
            .arg(destination);
        command
    };
    let stage_command =
        |mode: &str, policy: &str, destination: &Path| stage_at(mode, policy, destination, &commit);

    let fallback_data = root.join("fallback-data");
    let default_workspace = fallback_data.join("hive/v2/userdata");
    fs::create_dir_all(&default_workspace).unwrap();
    fs::write(
        default_workspace.join("hive-workspace.yml"),
        format!(
            "version: 2\nworkspace_id: {}\nschema_version: 1\npolicy_revision: {V2_POLICY}\n",
            id(40)
        ),
    )
    .unwrap();
    let fallback = || {
        response(
            cli("2").env("XDG_DATA_HOME", &fallback_data).args([
                "config",
                "show",
                "--scope",
                "workspace",
            ]),
            0,
        )
    };
    let before_fallback = fallback();
    assert_eq!(
        before_fallback["data"]["sources"]["workspace"],
        "xdg-default"
    );
    fs::write(&mapping_file, serde_json::to_vec(&mapping).unwrap()).unwrap();
    let refuse_reserved = |command: &mut Command, destination: &Path| {
        let report = response(command, 2);
        assert_eq!(report["error"]["code"], "INVALID_ARGUMENT");
        assert!(
            report["error"]["message"]
                .as_str()
                .unwrap()
                .contains("reserved")
        );
        assert!(!report.to_string().contains(root.to_str().unwrap()));
        assert!(
            fs::symlink_metadata(destination)
                .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
        );
    };
    let spelled_config = config.join("hive//v2/./config.yml/");
    refuse_reserved(
        stage_command("staged-import", V2_POLICY, &spelled_config)
            .env("XDG_DATA_HOME", &fallback_data),
        &v2_config,
    );
    let alias = root.join("config-alias");
    std::os::unix::fs::symlink(&config, &alias).unwrap();
    refuse_reserved(
        stage_command("staged-import", V2_POLICY, &v2_config).env("XDG_CONFIG_HOME", alias),
        &v2_config,
    );
    let bindings = config.join("hive/v2/bindings.yml");
    refuse_reserved(
        &mut stage_command("dry-run", V2_POLICY, &bindings),
        &bindings,
    );
    fs::create_dir_all(home.join(".config/hive/v2")).unwrap();
    for relative_path in [".config/hive/config.yml", ".config/hive/v2/config.yml"] {
        let reserved = home.join(relative_path);
        refuse_reserved(
            stage_command("staged-import", V2_POLICY, &reserved)
                .env("XDG_CONFIG_HOME", "relative-config"),
            &reserved,
        );
    }
    let discovery = root.join("discovery-data");
    let absent_default = discovery.join("hive/v2/userdata");
    fs::create_dir_all(absent_default.parent().unwrap()).unwrap();
    refuse_reserved(
        stage_command("staged-import", V2_POLICY, &absent_default).env("XDG_DATA_HOME", discovery),
        &absent_default,
    );
    let isolated_state = root.join("isolated-state");
    let reserved_state = isolated_state
        .join("hive/v2/workspaces")
        .join(&workspace_id);
    fs::create_dir_all(reserved_state.parent().unwrap()).unwrap();
    refuse_reserved(
        stage_command("staged-import", V2_POLICY, &reserved_state)
            .env("XDG_STATE_HOME", isolated_state),
        &reserved_state,
    );
    assert_eq!(fallback(), before_fallback);
    assert_eq!(v1(), before);
    fs::write(&v2_config, &v2_config_bytes).unwrap();

    let combined = response(
        cli("2")
            .args([
                "workspace",
                "migration-plan",
                "--tasks",
                "--include-directives-artifacts",
                "--source",
            ])
            .arg(&source)
            .args([
                "--source-commit",
                &commit,
                "--workspace-id",
                &workspace_id,
                "--mapping-file",
            ])
            .arg(&mapping_file),
        0,
    );
    let projections = ["task_intake", "directive_artifact_intake", "queen_assets"];
    for projection in projections {
        assert!(combined["data"][projection].is_object());
    }
    assert!(!target.exists());
    verify_source();

    let stage = |mode: &str, policy: &str, input: &Value, status| {
        fs::write(&mapping_file, serde_json::to_vec(input).unwrap()).unwrap();
        let report = response(&mut stage_command(mode, policy, &target), status);
        assert_eq!(report["command"], "workspace migration-stage");
        assert_eq!(report["ok"], status == 0);
        for projection in projections {
            assert!(report["data"].get(projection).is_none());
        }
        for private in [
            root.to_str().unwrap(),
            NAME,
            REFERENCE,
            "synth-app",
            "workspace-operations",
            "external-team",
            "Synthetic persona bytes.",
        ] {
            assert!(!report.to_string().contains(private));
        }
        report
    };
    let reject = |policy, input: &Value, status, code| {
        let error = stage("staged-import", policy, input, status)["error"].clone();
        assert_eq!(error["code"], code);
        error
    };
    for (pointer, positional) in [
        (
            "",
            json!([1, commit, POLICY_PIN, workspace_id, mapping["scopes"]]),
        ),
        (
            "/scopes/0",
            json!(["_hive-userdata", mapping["scopes"][0]["decision"]]),
        ),
        (
            "/scopes/1/decision/area",
            json!([app, "application", NAME, general]),
        ),
        (
            "/scopes/1/decision/project_evidence/0",
            json!([app_sources[0], REFERENCE]),
        ),
    ] {
        let mut input = mapping.clone();
        *input.pointer_mut(pointer).unwrap() = positional;
        for mode in ["dry-run", "staged-import"] {
            let rejected = stage(mode, V2_POLICY, &input, 2);
            assert_eq!(rejected["error"]["code"], "INVALID_CONFIG");
            assert_eq!(
                rejected["error"]["message"],
                "The scope mapping must match the versioned JSON format."
            );
            assert!(rejected.get("data").is_none());
            assert!(!target.exists());
        }
    }
    verify_source();
    assert_eq!(
        sources.map(|name| fs::read(source.join(name)).unwrap()),
        source_bytes
    );
    assert_eq!(fs::read_to_string(&v1_config).unwrap(), v1_config_bytes);
    assert_eq!(fs::read_to_string(&v2_config).unwrap(), v2_config_bytes);
    assert!(!state.exists());
    assert!(!data.exists());

    let mut task_review = mapping.clone();
    task_review["task_reviews"] = json!([{
        "legacy_key": {"scope": "synth-app", "work_id": "example"},
        "source_git_blob_sha": commit,
        "area_id": app,
    }]);
    let mut record_review = mapping.clone();
    record_review["reviewed_records"] = json!([{
        "source_path": "artifacts/example.md",
        "source_git_blob_sha": commit,
        "target_id": id(689),
        "provenance": {
            "kind": "standalone_artifact",
            "artifact_kind": "report",
            "source_ref": "https://example.test/source",
            "author_ref": "human:fixture",
            "created_at": "2026-01-01T00:00:00Z",
            "substantive_updated_at": "2026-01-01T00:00:00Z",
            "links": "none",
            "archive": "none",
        },
    }]);
    for input in [&task_review, &record_review] {
        for mode in ["dry-run", "staged-import"] {
            let refused = stage(mode, V2_POLICY, input, 2);
            assert_eq!(refused["error"]["code"], "INVALID_CONFIG");
            assert_eq!(
                refused["error"]["message"],
                "Area staging requires a mapping without Task, Directive, or Artifact reviews."
            );
            assert!(!target.exists());
        }
    }
    let mut queen_review = mapping.clone();
    queen_review["queen_preparation"] = json!([{
        "slug": "queen-synth",
        "source_git_blob_sha": commit,
        "source_raw_sha256": "0".repeat(64),
        "bot_id": id(690),
        "display_name": "Synthetic Review",
    }]);
    for mode in ["dry-run", "staged-import"] {
        let refused = stage(mode, V2_POLICY, &queen_review, 2);
        assert_eq!(refused["error"]["code"], "INVALID_CONFIG");
        assert_eq!(
            refused["error"]["message"],
            "Area staging requires a mapping without Queen preparation reviews."
        );
        assert!(!target.exists());
    }
    reject(POLICY_PIN, &mapping, 4, "INCOMPATIBLE_POLICY");
    for (field, wrong) in [
        ("workspace_id", id(2)),
        ("policy_pin", V2_POLICY.to_owned()),
    ] {
        let mut invalid = mapping.clone();
        invalid[field] = json!(wrong);
        reject(V2_POLICY, &invalid, 2, "INVALID_CONFIG");
    }
    let mut missing = mapping.clone();
    missing["scopes"][1]["decision"]["area"]["parent_id"] = json!(id(999));
    assert!(
        reject(V2_POLICY, &missing, 4, "INVALID_RECORD")["message"]
            .as_str()
            .unwrap()
            .contains("missing or held")
    );
    let mut held_parent = mapping.clone();
    held_parent["scopes"][1]["decision"]["project_evidence"] = json!([]);
    reject(V2_POLICY, &held_parent, 4, "INVALID_RECORD");
    let mut cyclic = mapping.clone();
    cyclic["scopes"][2]["decision"]["area"]["parent_id"] = json!(userdata);
    assert!(
        reject(V2_POLICY, &cyclic, 4, "INVALID_RECORD")["message"]
            .as_str()
            .unwrap()
            .contains("cycle")
    );
    assert!(!target.exists());

    let dry_run = stage("dry-run", V2_POLICY, &mapping, 0)["data"].clone();
    assert_eq!(dry_run["mode"], "dry_run");
    assert_eq!(dry_run["coverage"], "scope_only");
    assert_eq!(
        dry_run["counts"]["scopes"],
        json!({"inventoried": 5, "imported": 3, "held": 1, "excluded_non_record": 1})
    );
    assert_eq!(dry_run["holds"][0]["reason_code"], "unsplit_legacy_scope");
    assert_eq!(dry_run["holds"][0]["scope_sha256"], sha256_hex(b"_hive"));
    assert_eq!(
        dry_run["exclusions"][0]["reason_code"],
        "external_backend_owned"
    );
    assert_eq!(dry_run["target"]["confirmed_dolt_commit"], Value::Null);
    assert_eq!(
        dry_run["checks"],
        json!({"transaction": "rolled_back", "target_area_rows": 0,
        "source_unchanged": true, "area_ancestry": "verified", "committed_record_validation": "not_run",
        "asset_commit_verified": true, "published_inactive": false})
    );
    assert!(!target.exists());
    assert!(!state.exists());
    assert!(!data.exists());
    verify_source();

    fs::create_dir(&target).unwrap();
    reject(V2_POLICY, &mapping, 5, "ALREADY_EXISTS");
    assert!(fs::read_dir(&target).unwrap().next().is_none());
    fs::remove_dir(&target).unwrap();
    let registration_dir = state.join("hive/v2/workspaces").join(&workspace_id);
    fs::create_dir_all(&registration_dir).unwrap();
    let registration = registration_dir.join("active-root.json");
    let registration_bytes = serde_json::to_vec(&json!({
        "version": 2, "workspace_id": workspace_id, "workspace_root": target,
    }))
    .unwrap();
    fs::write(&registration, &registration_bytes).unwrap();
    reject(V2_POLICY, &mapping, 7, "AUTHORITY_UNVERIFIED");
    assert_eq!(fs::read(&registration).unwrap(), registration_bytes);
    assert!(!target.exists());
    fs::remove_dir_all(&state).unwrap();

    let worktree_edit = source.join("untracked-private-record.md");
    fs::write(&worktree_edit, "synthetic untracked record").unwrap();
    reject(V2_POLICY, &mapping, 5, "DIRTY_CONFLICT");
    assert!(!target.exists());
    fs::remove_file(&worktree_edit).unwrap();

    // Dolt's fsck runs after the private import commit and before publication.
    // Simulate a separate process changing the source while Hive is staging.
    use std::os::unix::fs::PermissionsExt;
    let dolt = std::env::var_os("HIVE_DOLT_BIN").unwrap();
    let hooks = root.join("fixture-bin");
    fs::create_dir(&hooks).unwrap();
    let wrapper = hooks.join("dolt");
    let quote = |path: &Path| format!("'{}'", path.to_str().unwrap().replace('\'', "'\\''"));
    fs::write(&wrapper, format!(
        "#!/bin/sh\nif [ \"$1\" = fsck ]; then printf '%s\\n' synthetic > {}; fi\nexec {} \"$@\"\n",
        quote(&worktree_edit), quote(Path::new(&dolt))
    )).unwrap();
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o700)).unwrap();
    let search_path = std::env::var_os("PATH").unwrap();
    let injected_path =
        std::env::join_paths(std::iter::once(hooks).chain(std::env::split_paths(&search_path)))
            .unwrap();
    let late_drift = response(
        stage_command("staged-import", V2_POLICY, &target).env("PATH", injected_path),
        5,
    );
    assert_eq!(late_drift["error"]["code"], "DIRTY_CONFLICT");
    assert!(
        !late_drift
            .to_string()
            .contains(worktree_edit.to_str().unwrap())
    );
    assert!(!target.exists());
    fs::remove_file(worktree_edit).unwrap();

    let imported = stage("staged-import", V2_POLICY, &mapping, 0)["data"].clone();
    assert_eq!(imported["mode"], "staged_import");
    for key in [
        "counts",
        "id_map",
        "source",
        "mapping_sha256",
        "holds",
        "exclusions",
    ] {
        assert_eq!(imported[key], dry_run[key]);
    }
    assert_eq!(imported["checks"]["committed_record_validation"], "passed");
    assert_eq!(imported["checks"]["published_inactive"], true);
    let db_commit = imported["target"]["confirmed_dolt_commit"]
        .as_str()
        .unwrap();
    assert_ne!(db_commit, imported["target"]["base_dolt_commit"]);
    let v2 = response(cli("2").args(["config", "show", "--scope", "workspace"]), 0);
    assert_eq!(v2["data"]["workspace_id"], workspace_id);
    assert_eq!(v2["data"]["policy_revision"], V2_POLICY);
    assert_eq!(v1(), before);
    let sql = |query: &str| {
        let result = Command::new(&dolt)
            .env_clear()
            .env("HOME", &home)
            .env("DOLT_ROOT_PATH", &home)
            .env("DOLT_DISABLE_VERSION_CHECKS", "true")
            .env("DOLT_DISABLE_EVENT_FLUSH", "true")
            .current_dir(target.join("db"))
            .args(["sql", "-r", "json", "-q", query])
            .output()
            .unwrap();
        assert!(result.status.success(), "{result:?}");
        serde_json::from_slice::<Value>(&result.stdout).unwrap()["rows"].clone()
    };
    assert_eq!(
        sql(&format!(
            "SELECT id, parent_id, name FROM areas AS OF '{db_commit}' ORDER BY id"
        )),
        json!([
            {"id": general, "name": "General"}, {"id": userdata, "parent_id": app, "name": "Operations"},
            {"id": app, "parent_id": general, "name": NAME},
        ])
    );
    let baseline = imported["target"]["base_dolt_commit"].as_str().unwrap();
    assert_eq!(
        sql(&format!(
            "SELECT COUNT(*) AS n FROM areas AS OF '{baseline}'"
        )),
        json!([{"n": 0}])
    );
    assert_eq!(
        sql("SELECT next_value FROM workspace_counters"),
        json!([{"next_value": 1}])
    );
    assert_eq!(
        git_text(&target.join("assets"), &home, &["rev-parse", "HEAD"]),
        imported["target"]["asset_git_commit"]
    );
    assert!(git_text(&target.join("assets"), &home, &["ls-tree", "HEAD"]).is_empty());
    assert!(
        fs::read_dir(target.join("conversations"))
            .unwrap()
            .next()
            .is_none()
    );
    reject(V2_POLICY, &mapping, 5, "ALREADY_EXISTS");
    verify_source();
    assert_eq!(
        sources.map(|name| fs::read(source.join(name)).unwrap()),
        source_bytes
    );
    assert_eq!(fs::read_to_string(&v1_config).unwrap(), v1_config_bytes);
    assert_eq!(fs::read_to_string(&v2_config).unwrap(), v2_config_bytes);
    assert!(!state.exists());
    assert!(!data.exists());

    // Build a separate committed snapshot of the synthetic fixture for the
    // older v1 policy, which predates the tracked coordination file.
    let legacy_policy = "0fd0f8dbe632d14094a93599cb1b5f653789ac9c";
    git_at_source(&["rm", "hive.yml"]);
    fs::write(
        source.join("AGENTS.md"),
        format!("hive-system@{legacy_policy}\n"),
    )
    .unwrap();
    git_at_source(&["add", "AGENTS.md"]);
    git_at_source(&["commit", "-qm", "Use the older synthetic policy pin"]);
    let legacy_commit = git_at_source(&["rev-parse", "HEAD"]);
    let legacy_index = fs::read(source.join(".git/index")).unwrap();
    let mut legacy_mapping = mapping;
    legacy_mapping["source_commit"] = json!(legacy_commit);
    legacy_mapping["policy_pin"] = json!(legacy_policy);
    fs::write(&mapping_file, serde_json::to_vec(&legacy_mapping).unwrap()).unwrap();
    let legacy_target = root.join("legacy-dry-run");
    let legacy = response(
        &mut stage_at("dry-run", V2_POLICY, &legacy_target, &legacy_commit),
        0,
    );
    assert_eq!(legacy["data"]["source"]["policy_pin"], legacy_policy);
    assert_eq!(legacy["data"]["source"]["hive_yml"], Value::Null);
    assert_eq!(legacy["data"]["checks"]["transaction"], "rolled_back");
    assert_eq!(legacy["data"]["checks"]["target_area_rows"], 0);
    assert!(!legacy_target.exists());
    assert_eq!(git_at_source(&["rev-parse", "HEAD"]), legacy_commit);
    assert!(git_at_source(&["status", "--porcelain", "--untracked-files=all"]).is_empty());
    assert_eq!(fs::read(source.join(".git/index")).unwrap(), legacy_index);
    assert_eq!(v1()["data"]["policy_revision"], legacy_policy);
    assert_eq!(fs::read_to_string(&v1_config).unwrap(), v1_config_bytes);
    assert_eq!(fs::read_to_string(&v2_config).unwrap(), v2_config_bytes);
    assert!(!state.exists());
    assert!(!data.exists());
    assert!(!fs::read_dir(root).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".hive-init-incomplete-")
    }));
}

#[cfg(target_os = "linux")]
#[test]
fn cli_migration_plan_requires_named_json_objects_on_both_policy_pins() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    fs::create_dir(&home).unwrap();
    let config = root.path().join("config");
    let source = root.path().join("source");
    init_git(
        &source,
        &home,
        "Synthetic Fixture",
        "synthetic@example.invalid",
    );
    let mapping_file = root.path().join("mapping.json");
    let workspace = id(1);
    let area_id = id(AREA_APP);
    let path = "tasks/synth-app/README.md";
    let reference = "https://forge.example/synthetic/app";
    fs::create_dir_all(source.join(path).parent().unwrap()).unwrap();
    fs::write(
        source.join(path),
        format!("# Synthetic App\n\nProject: {reference}\n"),
    )
    .unwrap();

    for pin in ["0fd0f8dbe632d14094a93599cb1b5f653789ac9c", POLICY_PIN] {
        fs::write(source.join("AGENTS.md"), format!("hive-system@{pin}\n")).unwrap();
        if pin == POLICY_PIN {
            fs::write(
                source.join("hive.yml"),
                "version: 1\ncoordination:\n  mode: local\n",
            )
            .unwrap();
        }
        git(&source, &home, &["add", "."]);
        git(&source, &home, &["commit", "-qm", "Synthetic policy pin"]);
        let commit = git_text(&source, &home, &["rev-parse", "HEAD"]);
        let args = [
            "--source",
            source.to_str().unwrap(),
            "--source-commit",
            &commit,
            "--workspace-id",
            &workspace,
            "--mapping-file",
            mapping_file.to_str().unwrap(),
        ];
        let plan = |input: &str, status| {
            fs::write(&mapping_file, input).unwrap();
            migration_cli(&home, &config, "migration-plan", &args, status)
        };

        let evidence = json!({"path": path, "canonical_ref": reference});
        let area = json!({"id": area_id, "slug": "application", "name": "Synthetic App", "parent_id": null});
        let decision = json!({"kind": "area", "area": area, "project_evidence": [evidence]});
        let entry = json!({"scope": "synth-app", "decision": decision});
        let mapping = json!({
            "version": 1,
            "source_commit": commit,
            "policy_pin": pin,
            "workspace_id": workspace,
            "scopes": [entry],
        });
        let named = mapping.to_string();
        let report = plan(&named, 0);
        assert_eq!(report["data"]["source"]["policy_pin"], pin);
        assert_eq!(
            report["data"]["scopes"][0]["outcome"],
            json!({
                "status": "mapped",
                "area": {"id": area_id, "slug": "application", "parent_id": null},
            })
        );

        let reject = |input: &str| {
            let report = plan(input, 2);
            assert_eq!(report["error"]["code"], "INVALID_CONFIG");
            assert_eq!(
                report["error"]["message"],
                "The scope mapping must match the versioned JSON format."
            );
            assert!(report.get("data").is_none());
        };
        for (pointer, positional) in [
            ("", json!([1, commit, pin, workspace, [entry]])),
            ("/scopes/0", json!(["synth-app", decision])),
            ("/scopes/0/decision", json!(["area", area, [evidence]])),
            (
                "/scopes/0/decision/area",
                json!([area_id, "application", "Synthetic App", null]),
            ),
            (
                "/scopes/0/decision/project_evidence/0",
                json!([path, reference]),
            ),
            ("/scopes", entry),
            ("/scopes/0/decision/project_evidence", evidence),
        ] {
            let mut changed = mapping.clone();
            *changed.pointer_mut(pointer).unwrap() = positional;
            reject(&changed.to_string());
        }
        for member in [
            r#""version":1"#,
            r#""scope":"synth-app""#,
            r#""kind":"area""#,
            r#""parent_id":null"#,
            r#""canonical_ref":"https://forge.example/synthetic/app""#,
        ] {
            reject(&named.replacen(member, &format!("{member},{member}"), 1));
            reject(&named.replacen(member, &format!(r#"{member},"unexpected":true"#), 1));
        }
        reject(&format!("{named}\n{{}}"));
    }
    assert!(!config.exists());
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
        (
            "tasks/inline-code-tag/README.md",
            "# Project navigation\n\nProject: /home/synthetic/private/app <code> https://forge.example/synthetic/app </code>\n",
        ),
        (
            "tasks/hidden-span/README.md",
            "# Project navigation\n\nProject: /home/synthetic/private/app <span hidden> https://forge.example/synthetic/app </span>\n",
        ),
        (
            "tasks/hidden-image-alt/README.md",
            "# Project navigation\n\nProject: /home/synthetic/private/app ![ https://forge.example/synthetic/app ](example.png)\n",
        ),
        (
            "tasks/broken-spaced-link/README.md",
            "# Project navigation\n\nProject: /home/synthetic/private/app Owner]( https://forge.example/synthetic/app )\n",
        ),
        (
            "tasks/valid-spaced-link/README.md",
            "# Project navigation\n\nExamples <span hidden> https://forge.example/synthetic/other </span> and <code> https://forge.example/synthetic/other </code>\n\nProject: [Owner]( https://forge.example/synthetic/app )\n",
        ),
        (
            "tasks/valid-angle-link/README.md",
            "[Owner]( <https://forge.example/synthetic/app> )\n\n<https://forge.example/synthetic/app>\n",
        ),
        (
            "tasks/broken-angle-link/README.md",
            "Owner]( <https://forge.example/synthetic/app> )\n",
        ),
        (
            "tasks/hidden-across-blocks/README.md",
            "<div hidden>\n\nProject: https://forge.example/synthetic/app\n\n</div>\n",
        ),
        (
            "tasks/visible-after-html-block/README.md",
            "<div hidden>\nhttps://forge.example/synthetic/other\n</div>\n\nProject: https://forge.example/synthetic/app\n",
        ),
        (
            "tasks/apparent-html-close/README.md",
            "<div title=\"</div>\">\n\nProject: https://forge.example/synthetic/app\n\n</div>\n",
        ),
        (
            "tasks/inline-wrapper-across-blocks/README.md",
            "Intro <div hidden>\n\nProject: https://forge.example/synthetic/app\n\nEnd </div>\n",
        ),
        (
            "tasks/unclosed-inline-span/README.md",
            "Intro <span hidden>\n\nProject: https://forge.example/synthetic/app\n",
        ),
        (
            "tasks/nested-inline-tags/README.md",
            "Example <span hidden> <code> hidden </code> </span>\n\nProject: https://forge.example/synthetic/app\n",
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
            area("inline-code-tag", 0x114, "inline-code", json!([
                evidence("tasks/inline-code-tag/README.md", app_ref)
            ])),
            area("hidden-span", 0x115, "hidden-span", json!([
                evidence("tasks/hidden-span/README.md", app_ref)
            ])),
            area("hidden-image-alt", 0x116, "hidden-image", json!([
                evidence("tasks/hidden-image-alt/README.md", app_ref)
            ])),
            area("broken-spaced-link", 0x117, "broken-spaced", json!([
                evidence("tasks/broken-spaced-link/README.md", app_ref)
            ])),
            area("valid-spaced-link", 0x118, "valid-spaced", json!([
                evidence("tasks/valid-spaced-link/README.md", app_ref)
            ])),
            area("broken-angle-link", 0x119, "broken-angle", json!([
                evidence("tasks/broken-angle-link/README.md", app_ref)
            ])),
            area("hidden-across-blocks", 0x11a, "hidden-blocks", json!([
                evidence("tasks/hidden-across-blocks/README.md", app_ref)
            ])),
            area("visible-after-html-block", 0x11b, "visible-after-html", json!([
                evidence("tasks/visible-after-html-block/README.md", app_ref)
            ])),
            area("apparent-html-close", 0x11c, "apparent-close", json!([
                evidence("tasks/apparent-html-close/README.md", app_ref)
            ])),
            area("valid-angle-link", 0x11d, "valid-angle", json!([
                evidence("tasks/valid-angle-link/README.md", app_ref)
            ])),
            area("inline-wrapper-across-blocks", 0x11e, "inline-wrapper", json!([
                evidence("tasks/inline-wrapper-across-blocks/README.md", app_ref)
            ])),
            area("unclosed-inline-span", 0x11f, "unclosed-span", json!([
                evidence("tasks/unclosed-inline-span/README.md", app_ref)
            ])),
            area("nested-inline-tags", 0x120, "nested-inline", json!([
                evidence("tasks/nested-inline-tags/README.md", app_ref)
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
    assert_eq!(data["scopes"].as_array().unwrap().len(), 34);
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
    assert_eq!(mapped["valid-spaced-link"]["status"], "mapped");
    assert_eq!(mapped["visible-after-html-block"]["status"], "mapped");
    assert_eq!(mapped["valid-angle-link"]["status"], "mapped");
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
        "inline-code-tag",
        "hidden-span",
        "hidden-image-alt",
        "broken-spaced-link",
        "broken-angle-link",
        "hidden-across-blocks",
        "apparent-html-close",
        "inline-wrapper-across-blocks",
        "unclosed-inline-span",
        "nested-inline-tags",
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
    let device_bot_path = "bots/aux/memory/_general";
    let device_scope_path = "bots/clean/memory/aux";
    let general_memory = "bots/clean/memory/_general";
    let userdata_memory = "bots/clean/memory/_hive-userdata";
    let unsplit_memory = "bots/clean/memory/_hive";
    let reference = "https://forge.example/synthetic/portable";
    let mut evidence = Vec::new();
    for path in malformed_bot_paths
        .into_iter()
        .chain(portable_paths)
        .chain(malformed_scope_paths)
        .chain([
            device_bot_path,
            device_scope_path,
            general_memory,
            userdata_memory,
            unsplit_memory,
        ])
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
    let mut input = json!({
        "version": 1,
        "source_commit": commit,
        "policy_pin": POLICY_PIN,
        "workspace_id": workspace,
        "scopes": [
            {
                "scope": "portable-scope",
                "decision": {
                    "kind": "area",
                    "area": {"id": id(AREA_APP), "slug": "portable", "name": "Portable project"},
                    "project_evidence": evidence,
                }
            },
            {
                "scope": "aux",
                "decision": {
                    "kind": "area",
                    "area": {"id": id(0x104), "slug": "synthetic-device", "name": "Synthetic project"},
                    "project_evidence": [{
                        "path": format!("{device_scope_path}/MEMORY.md"),
                        "canonical_ref": reference
                    }]
                }
            },
            {"scope": "_general", "decision": {"kind": "area", "area": {
                "id": id(AREA_GENERAL), "slug": "general", "name": "General"
            }}},
            {"scope": "_hive-userdata", "decision": {"kind": "area", "area": {
                "id": id(AREA_USERDATA), "slug": "operations", "name": "Workspace operations"
            }}},
        ],
    });
    fs::write(&map, serde_json::to_vec(&input).unwrap()).unwrap();
    let plan = |revision| {
        let args = [
            "--source",
            source.to_str().unwrap(),
            "--source-commit",
            revision,
            "--workspace-id",
            &workspace,
            "--mapping-file",
            map.to_str().unwrap(),
        ];
        migration_cli(&home, &config, "migration-plan", &args, 0)
    };
    let report = plan(&commit);
    assert_eq!(report, plan(&commit));
    let encoded = report.to_string();
    for private in [
        malformed_scope,
        "Synthetic Private Bot",
        "synthetic.private.bot",
    ] {
        assert!(!encoded.contains(private), "leaked {private}");
    }
    for path in [device_bot_path, device_scope_path, unsplit_memory] {
        assert!(!encoded.contains(&json!(path).to_string()), "leaked {path}");
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
    let scope = |key| scopes.iter().find(|scope| scope["scope"] == key).unwrap();
    assert_eq!(
        scope("_general"),
        &json!({
            "scope": "_general",
            "source_paths": [general_memory, "bots/queen-synth/memory/_general", "tasks/_general"],
            "source_path_sha256s": [digest_path(device_bot_path)],
            "outcome": held,
        }),
    );
    assert_eq!(
        scope("aux"),
        &json!({
            "scope": "aux",
            "source_paths": [],
            "source_path_sha256s": [digest_path(device_scope_path)],
            "outcome": held,
        }),
    );
    assert_eq!(
        scope("_hive-userdata")["source_paths"],
        json!([
            userdata_memory,
            "directives/_hive-userdata",
            "tasks/_hive-userdata"
        ]),
    );
    assert_eq!(scope("_hive-userdata")["outcome"]["status"], "mapped");
    assert_eq!(scope("_hive")["source_paths"], json!(["tasks/_hive"]));
    assert_eq!(
        scope("_hive")["source_path_sha256s"],
        json!([digest_path(unsplit_memory)]),
    );
    assert_eq!(scope("_hive")["outcome"]["reason"], "unsplit_legacy_scope");
    assert_eq!(git_text(&source, &home, &["rev-parse", "HEAD"]), commit);
    assert!(git(&source, &home, &["status", "--porcelain"]).is_empty());

    let older_pin = EARLIER_POLICY_PIN;
    fs::write(
        source.join("AGENTS.md"),
        format!("hive-system@{older_pin}\n"),
    )
    .unwrap();
    fs::remove_file(source.join("hive.yml")).unwrap();
    git(&source, &home, &["add", "."]);
    git(
        &source,
        &home,
        &["commit", "-qm", "Use the older synthetic policy pin"],
    );
    let older_commit = git_text(&source, &home, &["rev-parse", "HEAD"]);
    input["source_commit"] = json!(older_commit);
    input["policy_pin"] = json!(older_pin);
    fs::write(&map, serde_json::to_vec(&input).unwrap()).unwrap();
    assert_eq!(
        plan(&older_commit)["data"]["scopes"],
        report["data"]["scopes"]
    );
    assert!(git(&source, &home, &["status", "--porcelain"]).is_empty());
}

#[cfg(target_os = "linux")]
#[test]
fn cli_migration_plan_classifies_reviewed_queen_assets_without_writes() {
    check_queen_asset_intake(POLICY_PIN);
}

#[cfg(target_os = "linux")]
#[test]
fn cli_migration_plan_preserves_queen_asset_classification_under_legacy_pin() {
    check_queen_asset_intake("0fd0f8dbe632d14094a93599cb1b5f653789ac9c");
}

#[cfg(target_os = "linux")]
fn check_queen_asset_intake(policy_pin: &str) {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    fs::create_dir(&home).unwrap();
    let source = root.path().join("source");
    let config = root.path().join("config");
    init_source(&source, &home);

    let write = |path: &str, bytes: &[u8]| {
        let path = source.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    };
    if policy_pin != POLICY_PIN {
        let policy = fs::read_to_string(source.join("AGENTS.md")).unwrap();
        write(
            "AGENTS.md",
            policy.replace(POLICY_PIN, policy_pin).as_bytes(),
        );
        fs::remove_file(source.join("hive.yml")).unwrap();
    }
    let soul = |slug: &str, layer: &str| {
        format!(
            "---\ndescription: \"Select the synthetic Queen identity.\"\n---\n# Synthetic Queen\n\n## Identity\n\n- ID: {slug}\n- Name: Synthetic Private Display\n- Layer: {layer}\n\n## Purpose\n\nSynthetic private persona sentinel.\n"
        )
    };
    let exact_soul = soul("queen-synth", "1 — Manager").replace('\n', "\r\n");
    write("bots/queen-synth/SOUL.md", exact_soul.as_bytes());
    let compatible = [
        (
            "identity-steward",
            soul("identity-steward", "1 — Manager")
                .replace("# Synthetic Queen", "# Identity Steward")
                .replace(
                    "- Name: Synthetic Private Display",
                    "- Name: Identity Steward",
                ),
        ),
        (
            "purpose-duties",
            format!(
                "{}\n- Role-specific review duties are described in [Agent Layout](https://example.invalid/agent-layout).\n- Role assignments follow the review policy.\n- **Role**-specific guidance belongs in the review policy.\n- Review duties follow **Role** assignments.\n",
                soul("purpose-duties", "1 — Manager")
            ),
        ),
    ];
    for (slug, contents) in &compatible {
        write(&format!("bots/{slug}/SOUL.md"), contents.as_bytes());
    }
    for (slug, layer) in [
        ("worker", "2 — Coordinator"),
        ("wrong-id", "1 — Manager"),
        ("collision", "1 — Manager"),
        ("Collision", "1 — Manager"),
        ("repeat-review", "1 — Manager"),
        ("stale", "1 — Manager"),
        ("unreviewed", "1 — Manager"),
        ("alias", "1 — Manager"),
        ("aux", "1 — Manager"),
    ] {
        let id = if slug == "wrong-id" { "other" } else { slug };
        write(&format!("bots/{slug}/SOUL.md"), soul(id, layer).as_bytes());
    }
    write("bots/alias/soul.md", b"Case alias.");
    let conflicting = [
        ("conflict-star", "* Layer: 2 — Coordinator"),
        ("conflict-plus", "+ **Role:** Coordinator"),
        ("conflict-ordered", "1) Bot ID: different-queen"),
        ("conflict-nested", "- > * Layer: 2 — Coordinator"),
        ("conflict-heading", "### Layer: 2 — Coordinator ###"),
        ("conflict-heading-section", "### Role\n\nCoordinator"),
        ("conflict-setext", "Role\n----\nCoordinator"),
        ("conflict-multiword-atx", "### Bot ID different-queen"),
        ("conflict-bold-atx", "### **Bot ID** different-queen"),
        ("conflict-multiword-setext", "Bot ID different-queen\n---"),
        ("conflict-empty-field", "* **Display Name**:"),
        ("conflict-empty-heading", "### **Bot ID**"),
        ("conflict-malformed-field", "* **Bot ID = different-queen"),
        ("conflict-dash-field", "* Role - Coordinator"),
        ("conflict-strong-id", "- **Bot ID** different-queen"),
        ("conflict-strong-role", "- **Role** Coordinator"),
        ("conflict-emphasis-id", "- *Bot ID* different-queen"),
        ("conflict-nested-format", "- ***Role*** Coordinator"),
        ("conflict-strong-empty", "- **Bot ID**"),
    ];
    for (slug, declaration) in conflicting {
        write(
            &format!("bots/{slug}/SOUL.md"),
            format!("{}\n{declaration}\n", soul(slug, "1 — Manager")).as_bytes(),
        );
    }
    write(
        "bots/conflict-leading/SOUL.md",
        soul("conflict-leading", "1 — Manager")
            .replace("# Synthetic Queen", "# Bot ID different-queen")
            .as_bytes(),
    );
    write(
        "bots/missing/README.md",
        b"Synthetic private routing sentinel.",
    );
    write("bots/binary/SOUL.md", &[0xff]);
    write(
        "bots/bad-metadata/SOUL.md",
        soul("bad-metadata", "1 — Manager")
            .replacen("\n---\n", "\nrole: Manager\n---\n", 1)
            .as_bytes(),
    );
    write(
        "bots/queen-synth/memory/_general/AGENTS.md",
        b"Synthetic instructions.",
    );
    let nav = b"---\ndescription: \"Select memory navigation.\"\n---\n# Memory Navigation\n\n- [General](memory/_general/MEMORY.md)\n";
    write("bots/queen-synth/MEMORY.md", nav);
    write("bots/unreviewed/MEMORY.md", nav);
    let (header, navigation_body) = std::str::from_utf8(nav)
        .unwrap()
        .split_once("\n---\n")
        .unwrap();
    write(
        "bots/missing/MEMORY.md",
        format!(
            "---\ndescription: \"Synthetic learned metadata sentinel.\"\n---\n{navigation_body}"
        )
        .as_bytes(),
    );
    write("bots/missing/memory/_general/MEMORY.md", nav);
    write(
        "bots/alias/MEMORY.md",
        format!(
            "{header}\ntitle: \"Synthetic learned metadata sentinel.\"\n---\n{navigation_body}"
        )
        .as_bytes(),
    );
    write("bots/alias/memory/_general/MEMORY.md", nav);
    write(
        "bots/stale/MEMORY.md",
        format!("{header}\n# Synthetic learned metadata sentinel.\n---\n{navigation_body}")
            .as_bytes(),
    );
    write("bots/stale/memory/_general/MEMORY.md", nav);
    let device_bot_scope = "bots/aux/memory/_general";
    let device_memory_scope = "bots/clean/memory/aux";
    let reserved_memory = [
        ("_general", "bots/clean/memory/_general"),
        ("_hive-userdata", "bots/clean/memory/_hive-userdata"),
    ];
    for (_, directory) in reserved_memory {
        write(&format!("{directory}/MEMORY.md"), nav);
    }
    let mut clean_nav = nav.to_vec();
    clean_nav.extend(b"- [Hive Userdata](memory/_hive-userdata/MEMORY.md)\n");
    write("bots/clean/MEMORY.md", &clean_nav);
    let nonportable_paths = [
        (
            "bot_memories_historical",
            "bots/aux/memory/_general/MEMORY.md",
        ),
        ("bot_memories_historical", "bots/clean/memory/aux/MEMORY.md"),
        (
            "bot_memories_historical",
            "bots/queen-synth/memory/Host:Path/MEMORY.md",
        ),
        (
            "bot_memories_historical",
            "bots/queen-synth/memory/_general/private topic.md",
        ),
        (
            "bot_memories_historical",
            "bots/queen-synth/memory/_general/con.md",
        ),
        ("bots_soul", "bots/queen-synth/Local Notes/AGENTS.md"),
        (
            "bots_soul",
            "bots/queen-synth/memory/_general/nul/AGENTS.md",
        ),
    ];
    for (_, path) in nonportable_paths {
        write(path, b"Synthetic historical evidence.");
    }
    let mut mixed = nav.to_vec();
    mixed.extend(b"\nSynthetic private memory sentinel.\n");
    write("bots/worker/MEMORY.md", &mixed);
    write("bots/worker/memory/_general/MEMORY.md", nav);
    git(&source, &home, &["add", "."]);
    git(
        &source,
        &home,
        &["commit", "-qm", "Synthetic Queen and Memory assets"],
    );
    let commit = git_text(&source, &home, &["rev-parse", "HEAD"]);
    let review = |slug: &str| {
        let oid = git_text(
            &source,
            &home,
            &["rev-parse", &format!("HEAD:bots/{slug}/SOUL.md")],
        );
        json!({"slug": slug, "source_git_blob_sha": oid})
    };
    let workspace_id = id(1);
    let mapping_file = root.path().join("queen-mapping.json");
    let mut mapping = json!({
        "version": 1,
        "source_commit": commit,
        "policy_pin": policy_pin,
        "workspace_id": workspace_id,
        "scopes": [],
    });
    let run_json = |manifest: &str, revision: &str, status| {
        fs::write(&mapping_file, manifest).unwrap();
        migration_cli(
            &home,
            &config,
            "migration-plan",
            &[
                "--source",
                source.to_str().unwrap(),
                "--source-commit",
                revision,
                "--workspace-id",
                &workspace_id,
                "--mapping-file",
                mapping_file.to_str().unwrap(),
            ],
            status,
        )
    };
    let run = |mapping: &Value, status| {
        run_json(
            &mapping.to_string(),
            mapping["source_commit"].as_str().unwrap(),
            status,
        )
    };
    assert!(run(&mapping, 0)["data"].get("queen_assets").is_none());
    let index_before = fs::read(source.join(".git/index")).unwrap();
    let mut changed = mapping.clone();
    changed["queen_assets"] = json!([]);
    let empty_intake = run(&changed, 0);
    let souls = empty_intake["data"]["queen_assets"]["bots_soul"]
        .as_array()
        .unwrap();
    assert!(!souls.is_empty());
    assert!(souls.iter().all(|record| record["status"] != "candidate"));

    let review_entry = review("queen-synth");
    let reject_shape = |label: &str, manifest: &str| {
        let report = run_json(manifest, &commit, 2);
        assert_eq!(
            report["error"]["code"], "INVALID_CONFIG",
            "{policy_pin}: {label}"
        );
        assert!(report.get("data").is_none(), "{policy_pin}: {label}");
    };
    let mut unknown_member = review_entry.clone();
    unknown_member["unknown"] = json!(true);
    for (label, value) in [
        (
            "positional row",
            json!([[review_entry["slug"], review_entry["source_git_blob_sha"]]]),
        ),
        ("null list", Value::Null),
        ("bare object", review_entry.clone()),
        ("null row", json!([null])),
        ("unknown member", json!([unknown_member])),
    ] {
        changed["queen_assets"] = value;
        reject_shape(label, &changed.to_string());
    }
    changed["queen_assets"] = json!([review_entry]);
    let row = review_entry.to_string();
    let repeated_slug = format!("{{\"slug\":{},{}", review_entry["slug"], &row[1..]);
    let manifest = changed.to_string();
    reject_shape(
        "repeated row member",
        &manifest.replace(&row, &repeated_slug),
    );
    reject_shape(
        "repeated manifest member",
        &format!("{{\"queen_assets\":[],{}", &manifest[1..]),
    );

    mapping["queen_assets"] = json!([
        review("queen-synth"), review("worker"), review("wrong-id"), review("collision"),
        review("repeat-review"), review("repeat-review"), review("alias"), review("binary"),
        review("bad-metadata"), review("aux"), review("conflict-leading"),
        {"slug": "stale", "source_git_blob_sha": "1111111111111111111111111111111111111111"}
    ]);
    mapping["queen_assets"].as_array_mut().unwrap().extend(
        conflicting
            .iter()
            .map(|(slug, _)| *slug)
            .chain(compatible.iter().map(|(slug, _)| *slug))
            .map(review),
    );
    let report = run(&mapping, 0);
    let assets = &report["data"]["queen_assets"];
    let find = |surface: &str, path: &str| {
        assets[surface]
            .as_array()
            .unwrap()
            .iter()
            .find(|record| record["source_path"] == path)
            .unwrap_or_else(|| panic!("Missing {surface} outcome for {path}"))
    };
    let candidate = find("bots_soul", "bots/queen-synth/SOUL.md");
    assert_eq!(candidate["status"], "candidate");
    assert_eq!(candidate["slug"], "queen-synth");
    assert_eq!(candidate["target_path"], "assets/bots/queen-synth/SOUL.md");
    assert_eq!(
        candidate["source_git_blob_sha"],
        review("queen-synth")["source_git_blob_sha"]
    );
    assert_eq!(
        candidate["source_raw_sha256"],
        sha256_hex(exact_soul.as_bytes())
    );
    for (slug, contents) in &compatible {
        let candidate = find("bots_soul", &format!("bots/{slug}/SOUL.md"));
        assert_eq!(candidate["status"], "candidate", "{slug}");
        assert_eq!(
            candidate["source_git_blob_sha"],
            review(slug)["source_git_blob_sha"]
        );
        assert_eq!(
            candidate["source_raw_sha256"],
            sha256_hex(contents.as_bytes())
        );
    }
    for slug in [
        "worker",
        "wrong-id",
        "collision",
        "repeat-review",
        "stale",
        "unreviewed",
        "missing",
        "alias",
        "binary",
        "bad-metadata",
        "conflict-leading",
    ] {
        let record = find("bots_soul", &format!("bots/{slug}/SOUL.md"));
        assert_eq!(record["status"], "held", "{slug}");
        assert_eq!(record["reason"], "invalid_soul_or_slug", "{slug}");
    }
    let redacted = assets["bots_soul"]
        .as_array()
        .unwrap()
        .iter()
        .find(|record| record["source_path_sha256"] == sha256_hex(b"bots/Collision/SOUL.md"))
        .unwrap();
    assert_eq!(redacted["status"], "held");
    assert!(redacted.get("source_path").is_none());
    let device_slug = assets["bots_soul"]
        .as_array()
        .unwrap()
        .iter()
        .find(|record| record["source_path_sha256"] == sha256_hex(b"bots/aux/SOUL.md"))
        .unwrap();
    assert_eq!(device_slug["reason"], "invalid_soul_or_slug");
    assert!(device_slug.get("source_path").is_none());
    let serialized = report.to_string();
    for (surface, path) in nonportable_paths {
        let record = assets[surface]
            .as_array()
            .unwrap()
            .iter()
            .find(|record| record["source_path_sha256"] == sha256_hex(path.as_bytes()))
            .unwrap();
        assert!(record.get("source_path").is_none(), "{path}");
        assert!(record["source_git_blob_sha"].is_string(), "{path}");
        assert!(!serialized.contains(path), "{path}");
    }
    let scopes = report["data"]["scopes"].as_array().unwrap();
    let malformed_scope_digest = sha256_hex(b"Host:Path");
    for (field, key, directory) in [
        (
            "scope_sha256",
            malformed_scope_digest.as_str(),
            "bots/queen-synth/memory/Host:Path",
        ),
        ("scope", "aux", device_memory_scope),
    ] {
        let scope = scopes.iter().find(|scope| scope[field] == key).unwrap();
        assert_eq!(scope["outcome"]["status"], "held", "{directory}");
        assert_eq!(scope["source_paths"], json!([]), "{directory}");
        assert_eq!(
            scope["source_path_sha256s"],
            json!([sha256_hex(directory.as_bytes())]),
            "{directory}"
        );
    }
    for (key, directory) in reserved_memory {
        let scope = scopes.iter().find(|scope| scope["scope"] == key).unwrap();
        assert!(
            scope["source_paths"]
                .as_array()
                .unwrap()
                .contains(&json!(directory))
        );
        assert_eq!(
            find("bot_memories_historical", &format!("{directory}/MEMORY.md"))["reason"],
            "curated_memory_deferred"
        );
        if key == "_general" {
            assert_eq!(
                scope["source_path_sha256s"],
                json!([sha256_hex(device_bot_scope.as_bytes())])
            );
        }
    }
    for (slug, _) in conflicting {
        let outcome = find("bots_soul", &format!("bots/{slug}/SOUL.md"));
        assert_eq!(outcome["status"], "held", "{slug}");
        assert_eq!(outcome["reason"], "invalid_soul_or_slug", "{slug}");
    }
    for path in [
        "bots/queen-synth/README.md",
        "bots/queen-synth/memory/_general/AGENTS.md",
    ] {
        let record = find("bots_soul", path);
        assert_eq!(record["status"], "excluded");
        assert_eq!(record["reason"], "management_only");
    }
    assert_eq!(
        find("navigation_only", "bots/queen-synth/MEMORY.md")["reason"],
        "navigation_only"
    );
    assert_eq!(
        find("navigation_only", "bots/clean/MEMORY.md")["reason"],
        "navigation_only"
    );
    for path in [
        "bots/queen-synth/memory/_general/MEMORY.md",
        "bots/queen-synth/memory/synth-app/architecture.md",
        "bots/unreviewed/MEMORY.md",
        "bots/worker/MEMORY.md",
        "bots/worker/memory/_general/MEMORY.md",
        "bots/missing/MEMORY.md",
        "bots/alias/MEMORY.md",
        "bots/stale/MEMORY.md",
    ] {
        assert_eq!(
            find("bot_memories_historical", path)["reason"],
            "curated_memory_deferred"
        );
    }
    for private in [
        "Synthetic Private Display",
        "Identity Steward",
        "Role-specific review duties",
        "Host:Path",
        device_bot_scope,
        device_memory_scope,
        "sentinel",
        "memory/_general/MEMORY.md)",
    ] {
        assert!(!serialized.contains(private), "{private}");
    }
    assert_eq!(run(&mapping, 0), report);
    mapping["queen_assets"] = json!([{"slug": "../invalid", "source_git_blob_sha": commit}]);
    assert_eq!(run(&mapping, 2)["error"]["code"], "INVALID_CONFIG");
    assert_eq!(fs::read(source.join(".git/index")).unwrap(), index_before);
    assert_eq!(git_text(&source, &home, &["rev-parse", "HEAD"]), commit);
    assert!(git(&source, &home, &["status", "--porcelain"]).is_empty());
    assert_eq!(
        fs::read(source.join("bots/queen-synth/SOUL.md")).unwrap(),
        exact_soul.as_bytes()
    );
    assert!(!source.join("assets").exists());
    assert!(!config.exists());

    let external = root.path().join("outside-soul.md");
    let outside_bytes = soul("escape", "1 — Manager");
    fs::write(&external, &outside_bytes).unwrap();
    fs::create_dir(source.join("bots/escape")).unwrap();
    std::os::unix::fs::symlink(&external, source.join("bots/escape/SOUL.md")).unwrap();
    git(&source, &home, &["add", "."]);
    git(
        &source,
        &home,
        &["commit", "-qm", "Synthetic symlink escape"],
    );
    mapping["source_commit"] = json!(git_text(&source, &home, &["rev-parse", "HEAD"]));
    mapping["queen_assets"] = json!([review("escape")]);
    let rejected = run(&mapping, 4);
    assert_eq!(rejected["error"]["code"], "INVALID_RECORD");
    assert!(!rejected.to_string().contains(external.to_str().unwrap()));
    assert_eq!(fs::read_to_string(&external).unwrap(), outside_bytes);
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
    let repeated_title = "title: Private first title\ntitle: Private second title\n";
    // Expected execution hold on the adopted pin and the earlier pin.
    let projection_cases = [
        (
            "repeat-title-claimed",
            format!("status: in-progress\n{claim}{repeated_title}"),
            [true, false],
        ),
        (
            "repeat-title-unclaimed",
            format!("status: in-progress\n{repeated_title}"),
            [false, true],
        ),
        (
            "repeat-title-todo",
            format!("status: todo\n{repeated_title}"),
            [false, false],
        ),
        (
            "repeat-status-claimed",
            format!("status: in-progress\nstatus: in-progress\n{claim}"),
            [false, false],
        ),
        (
            "repeat-status-unclaimed",
            "status: in-progress\nstatus: in-progress\n".to_owned(),
            [false, false],
        ),
        (
            "repeat-claim",
            format!("status: in-progress\n{claim}{claim}"),
            [false, false],
        ),
        (
            "repeat-claim-holder",
            "status: in-progress\nclaim: {id: private-claim-id, holder: private-first, holder: private-second}\n".to_owned(),
            [false, false],
        ),
        (
            "repeat-claim-id",
            "status: in-progress\nclaim: {id: private-first, id: private-second, holder: private-claim-holder}\n".to_owned(),
            [false, false],
        ),
        (
            "invalid-yaml",
            format!("status: in-progress\n{claim}tags: [unterminated\n"),
            [false, false],
        ),
        (
            "invalid-yaml-unclaimed",
            "status: in-progress\ntags: [unterminated\n".to_owned(),
            [false, false],
        ),
    ];
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
        ("_general/backlog/shared-malformed", "", ""),
        (
            "_general/active/shared-malformed",
            &format!("status: in-progress\n{claim}tags: null\n"),
            "",
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
        ("synth-app/backlog/null-title", "title: null\n", ""),
        ("synth-app/backlog/description-fallback", "", ""),
        (
            "synth-app/active/malformed-claim",
            "status: in-progress\nclaim: null\ntags: null\n",
            "",
        ),
        (
            "synth-app/active/malformed-tags",
            &format!("status: in-progress\n{claim}tags: null\n"),
            "",
        ),
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
            "_hive/active/legacy",
            &format!("status: in-progress\n{claim}"),
            "",
        ),
        (
            "unmapped/active/uncertain",
            &format!("status: in-progress\n{claim}"),
            "",
        ),
        (
            "unmapped/active/uncertain-malformed",
            &format!("status: in-progress\n{claim}tags: null\n"),
            "",
        ),
        (
            "unmapped/active/unclaimed-malformed",
            "status: in-progress\ntags: null\n",
            "",
        ),
        (
            "unmapped/active/ambiguous-status",
            &format!("status: in-progress\nstatus: done\n{claim}tags: null\n"),
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
    for (work_id, fields, _) in &projection_cases {
        let bundle = source.join(format!("tasks/unmapped/active/{work_id}"));
        fs::create_dir_all(&bundle).unwrap();
        fs::write(
            bundle.join("README.md"),
            format!("---\ndescription: Private task description\n{fields}---\n"),
        )
        .unwrap();
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
    assert_eq!(tasks.len(), fixtures.len() + projection_cases.len() + 4);
    assert_eq!(
        intake["counts"],
        json!({
            "inventoried": tasks.len(), "held": tasks.len() - 2, "excluded": 2, "prepared": 0
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
        (
            "synth-app/backlog/null-title/README.md",
            "unsupported_task_metadata",
        ),
        (
            "synth-app/active/malformed-claim/README.md",
            "unsupported_task_metadata",
        ),
        ("_general/backlog/no-readme", "missing_task_entrypoint"),
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
            "tasks/_general/active/shared-malformed/README.md".to_owned(),
            json!({"status": "held", "reason_codes": ["duplicate_legacy_work_id", "unsupported_task_metadata", "execution_needs_reconciliation"]}),
        ),
        (
            "tasks/synth-app/active/malformed-tags/README.md".to_owned(),
            json!({"status": "held", "reason_codes": ["unsupported_task_metadata", "execution_needs_reconciliation"]}),
        ),
        (
            "tasks/_hive/backlog/legacy/README.md".to_owned(),
            json!({"status": "held", "reason_codes": ["duplicate_legacy_work_id", "unsplit_legacy_scope"]}),
        ),
        (
            "tasks/_hive/active/legacy/README.md".to_owned(),
            json!({"status": "held", "reason_codes": ["duplicate_legacy_work_id", "unsplit_legacy_scope", "execution_needs_reconciliation"]}),
        ),
        (
            "tasks/unmapped/active/uncertain/README.md".to_owned(),
            json!({"status": "held", "reason_codes": ["ambiguous_scope_or_collision", "execution_needs_reconciliation"]}),
        ),
        (
            "tasks/unmapped/active/uncertain-malformed/README.md".to_owned(),
            json!({"status": "held", "reason_codes": ["ambiguous_scope_or_collision", "unsupported_task_metadata", "execution_needs_reconciliation"]}),
        ),
        (
            "tasks/unmapped/active/unclaimed-malformed/README.md".to_owned(),
            json!({"status": "held", "reason_codes": ["ambiguous_scope_or_collision", "unsupported_task_metadata"]}),
        ),
        (
            "tasks/unmapped/active/ambiguous-status/README.md".to_owned(),
            json!({"status": "held", "reason_codes": ["ambiguous_scope_or_collision", "unsupported_task_metadata"]}),
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
            "synth-app/backlog/description-fallback",
            "backlog",
            AREA_APP,
            vec!["unresolved_task_relations"],
        ),
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
    let early_tasks = earlier["data"]["task_intake"]["tasks"].as_array().unwrap();
    for (pin_index, rows) in [tasks, early_tasks].into_iter().enumerate() {
        for (work_id, _, expected_execution) in &projection_cases {
            let path = format!("tasks/unmapped/active/{work_id}/README.md");
            let row = rows.iter().find(|row| row["source_path"] == path).unwrap();
            let mut reasons = vec!["ambiguous_scope_or_collision", "unsupported_task_metadata"];
            if expected_execution[pin_index] {
                reasons.push("execution_needs_reconciliation");
            }
            assert_eq!(
                row["outcome"],
                json!({"status": "held", "reason_codes": reasons}),
                "{path} under pin {pin_index}"
            );
        }
    }
    for (path, expected) in [
        (
            "tasks/synth-app/backlog/null-title/README.md",
            json!({"status": "held", "reason_codes": ["unsupported_task_metadata"]}),
        ),
        (
            "tasks/_general/active/shared-malformed/README.md",
            json!({"status": "held", "reason_codes": ["duplicate_legacy_work_id", "unsupported_task_metadata"]}),
        ),
        (
            "tasks/_hive/active/legacy/README.md",
            json!({"status": "held", "reason_codes": ["duplicate_legacy_work_id", "unsplit_legacy_scope", "invalid_legacy_state"]}),
        ),
        (
            "tasks/unmapped/active/uncertain-malformed/README.md",
            json!({"status": "held", "reason_codes": ["ambiguous_scope_or_collision", "unsupported_task_metadata"]}),
        ),
        (
            "tasks/unmapped/active/unclaimed-malformed/README.md",
            json!({"status": "held", "reason_codes": ["ambiguous_scope_or_collision", "unsupported_task_metadata", "execution_needs_reconciliation"]}),
        ),
        (
            "tasks/synth-app/backlog/description-fallback/README.md",
            json!({
                "status": "held", "candidate_status": "backlog", "area_id": id(AREA_APP),
                "reason_codes": ["incomplete_historical_provenance", "unresolved_task_relations"],
            }),
        ),
    ] {
        let task = early_tasks
            .iter()
            .find(|task| task["source_path"] == path)
            .unwrap();
        assert_eq!(task["outcome"], expected, "{path}");
    }
}

#[cfg(target_os = "linux")]
#[test]
fn cli_migration_plan_prepares_only_exact_reviews_of_independent_tasks() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    fs::create_dir(&home).unwrap();
    let config = root.path().join("config");
    let source = root.path().join("source");
    init_git(
        &source,
        &home,
        "Synthetic operator",
        "operator@sample.invalid",
    );
    fs::write(
        source.join("AGENTS.md"),
        format!("hive-system@{POLICY_PIN}\n"),
    )
    .unwrap();
    fs::write(
        source.join("hive.yml"),
        "version: 1\ncoordination:\n  mode: local\n",
    )
    .unwrap();

    let created = "2026-01-02T11:20:00.123456+02:00";
    let updated = "2026-01-02T09:25:00Z";
    let criteria = "- [ ] Verify synthetic café output.";
    let heading = "Synthetic fixture heading";
    let title = "Synthetic frontmatter title";
    let body = format!(
        "# {heading} ###\r\n\r\nCreated: {created}\r\nUpdated: {updated}\r\n\r\n## Acceptance\r\n{criteria}\r\n"
    );
    let backlog = format!("---\r\ndescription: Synthetic description\r\n---\r\n{body}");
    let task = |fields: &str, body: &str| {
        format!("---\ndescription: Synthetic description\n{fields}---\n{body}")
    };
    let todo = task(
        &format!("title: {title}\nstatus: todo\npriority: high\n"),
        &body,
    );
    let archived = task("status: done\n", &body);
    let claim = task(
        "status: todo\nclaim: {id: synthetic-id, holder: synthetic-holder}\n",
        &body,
    );
    let review = task("status: review\n", &body);
    let relations = task(
        "status: todo\n",
        &format!("{body}Prerequisite: _general/absent\n"),
    );
    let tags = task("status: todo\ntags: [Synthetic]\n", &body);
    let fixtures = [
        ("_general/backlog/shared", &backlog),
        ("_hive-userdata/backlog/shared", &backlog),
        ("_general/active/independent", &todo),
        ("_general/active/claimed", &claim),
        ("_general/active/review", &review),
        ("_general/archive/terminal", &archived),
        ("_general/active/unresolved", &relations),
        ("_general/active/stale", &todo),
        ("_general/active/wrong-area", &todo),
        ("_general/active/duplicate-review", &todo),
        ("_general/active/missing-dates", &todo),
        ("_general/active/invalid-dates", &todo),
        ("_general/active/reversed-dates", &todo),
        ("_general/active/missing-criteria", &todo),
        ("_general/active/invalid-criteria", &todo),
        ("_general/active/missing-relations", &todo),
        ("_general/active/tagged", &tags),
    ];
    for (bundle, raw) in fixtures {
        let path = source.join(format!("tasks/{bundle}/README.md"));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, raw).unwrap();
    }
    git(&source, &home, &["add", "."]);
    git(
        &source,
        &home,
        &["commit", "-qm", "Synthetic reviewed Task fixtures"],
    );
    let commit = git_text(&source, &home, &["rev-parse", "HEAD"]);
    let workspace = id(1);
    let area = id(AREA_GENERAL);
    let mapping_file = root.path().join("mapping.json");
    let range = |value: &str| {
        let start = body.find(value).unwrap();
        json!({"start": start, "end": start + value.len()})
    };
    let mut task_reviews = Vec::new();
    for (bundle, _) in fixtures {
        let (scope, lifecycle_and_id) = bundle.split_once('/').unwrap();
        let (_, work_id) = lifecycle_and_id.split_once('/').unwrap();
        if scope != "_general" {
            continue;
        }
        let oid = git_text(
            &source,
            &home,
            &["rev-parse", &format!("HEAD:tasks/{bundle}/README.md")],
        );
        let mut decision = json!({
            "legacy_key": {"scope": scope, "work_id": work_id},
            "source_git_blob_sha": oid,
            "area_id": area,
            "relations": "no_edges",
            "created_at": range(created),
            "updated_at": range(updated),
            "acceptance_criteria": range(criteria),
        });
        match work_id {
            "shared" | "missing-criteria" => {
                decision
                    .as_object_mut()
                    .unwrap()
                    .remove("acceptance_criteria");
            }
            "unresolved" => decision["relations"] = json!("unresolved"),
            "missing-relations" => decision["relations"] = Value::Null,
            "stale" => decision["source_git_blob_sha"] = json!("f".repeat(oid.len())),
            "wrong-area" => decision["area_id"] = json!(id(AREA_USERDATA)),
            "duplicate-review" => task_reviews.push(decision.clone()),
            "missing-dates" => decision["created_at"] = Value::Null,
            "invalid-dates" => decision["updated_at"] = range(heading),
            "reversed-dates" => {
                decision["created_at"] = range(updated);
                decision["updated_at"] = range(created);
            }
            "invalid-criteria" => {
                let start = body.find('é').unwrap() + 1;
                decision["acceptance_criteria"] = json!({"start": start, "end": start + 2});
            }
            _ => {}
        }
        task_reviews.push(decision);
    }
    let mut mapping = json!({
        "version": 1,
        "source_commit": commit,
        "policy_pin": POLICY_PIN,
        "workspace_id": workspace,
        "scopes": [
            {"scope": "_general", "decision": {"kind": "area", "area": {
                "id": area, "slug": "general", "name": "General"
            }}},
            {"scope": "_hive-userdata", "decision": {"kind": "area", "area": {
                "id": id(AREA_USERDATA), "slug": "operations", "name": "Operations"
            }}}
        ],
        "task_reviews": task_reviews,
    });
    let args = [
        "--source",
        source.to_str().unwrap(),
        "--source-commit",
        &commit,
        "--workspace-id",
        &workspace,
        "--mapping-file",
        mapping_file.to_str().unwrap(),
        "--tasks",
    ];
    fs::write(&mapping_file, serde_json::to_vec(&mapping).unwrap()).unwrap();
    let original_mapping = fs::read(&mapping_file).unwrap();
    let original_index = fs::read(source.join(".git/index")).unwrap();
    let plan = |args: &[&str], code| migration_cli(&home, &config, "migration-plan", args, code);
    let result = plan(&args, 0);
    assert_eq!(result, plan(&args, 0));
    let combined = plan(
        &[args.as_slice(), &["--include-directives-artifacts"]].concat(),
        0,
    );
    assert_eq!(
        combined["data"]["task_intake"],
        result["data"]["task_intake"]
    );
    assert!(combined["data"]["directive_artifact_intake"].is_object());
    assert_eq!(fs::read(&mapping_file).unwrap(), original_mapping);
    assert_eq!(fs::read(source.join(".git/index")).unwrap(), original_index);
    assert_eq!(git_text(&source, &home, &["rev-parse", "HEAD"]), commit);
    assert!(git(&source, &home, &["status", "--porcelain"]).is_empty());
    assert!(!config.exists());
    let intake = &result["data"]["task_intake"];
    assert_eq!(
        intake["counts"],
        json!({
            "inventoried": fixtures.len(), "held": fixtures.len()-2, "excluded": 0, "prepared": 2
        })
    );
    let row = |scope, work_id| {
        intake["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["legacy_key"] == json!({"scope": scope, "work_id": work_id}))
            .unwrap()
    };
    for (work_id, raw, status, title, title_source, priority, criteria_evidence) in [
        (
            "shared",
            backlog.as_str(),
            "backlog",
            heading,
            "heading",
            Value::Null,
            None,
        ),
        (
            "independent",
            todo.as_str(),
            "todo",
            title,
            "frontmatter_title",
            json!(0),
            Some(criteria),
        ),
    ] {
        let prepared = row("_general", work_id);
        assert_eq!(prepared["source_raw_sha256"], sha256_hex(raw.as_bytes()));
        let mut provenance = json!({
            "relations": "no_edges",
            "body_sha256": sha256_hex(body.as_bytes()),
            "title_source": title_source,
            "title_sha256": sha256_hex(title.as_bytes()),
            "priority": priority,
            "created_at": {"value": created, "body_range": range(created)},
            "updated_at": {"value": updated, "body_range": range(updated)},
        });
        if let Some(criteria) = criteria_evidence {
            provenance["acceptance_criteria"] = json!({
                "body_range": range(criteria), "sha256": sha256_hex(criteria.as_bytes()),
            });
        }
        assert_eq!(
            prepared["outcome"],
            json!({
                "status": "prepared", "candidate_status": status, "area_id": area,
                "provenance": provenance,
            })
        );
    }
    let qualified_hold = row("_hive-userdata", "shared");
    assert_eq!(
        qualified_hold["source_git_blob_sha"],
        row("_general", "shared")["source_git_blob_sha"]
    );
    assert_eq!(qualified_hold["outcome"]["status"], "held");
    for (work_id, reasons) in [
        ("claimed", vec!["execution_needs_reconciliation"]),
        ("review", vec!["missing_typed_submission_evidence"]),
        (
            "terminal",
            vec![
                "incomplete_historical_provenance",
                "unresolved_completion_obligation",
            ],
        ),
        ("unresolved", vec!["unresolved_task_relations"]),
        ("missing-relations", vec!["unresolved_task_relations"]),
        ("stale", vec!["task_review_binding_mismatch"]),
        ("wrong-area", vec!["task_review_binding_mismatch"]),
        ("duplicate-review", vec!["conflicting_task_review"]),
        ("missing-dates", vec!["incomplete_historical_provenance"]),
        ("invalid-dates", vec!["incomplete_historical_provenance"]),
        ("reversed-dates", vec!["incomplete_historical_provenance"]),
        ("missing-criteria", vec!["unverified_acceptance_criteria"]),
        ("invalid-criteria", vec!["unverified_acceptance_criteria"]),
        ("tagged", vec!["unverified_task_labels"]),
    ] {
        let outcome = &row("_general", work_id)["outcome"];
        assert_eq!(outcome["status"], "held", "{work_id}");
        assert_eq!(outcome["reason_codes"], json!(reasons), "{work_id}");
    }
    let output = result.to_string();
    for private in [
        title,
        heading,
        criteria,
        "synthetic-holder",
        "Prerequisite:",
        "issue_number",
    ] {
        assert!(!output.contains(private), "report exposed {private}");
    }

    mapping["task_reviews"][0]["legacy_key"]["work_id"] = json!("nonexistent");
    fs::write(&mapping_file, serde_json::to_vec(&mapping).unwrap()).unwrap();
    assert_eq!(plan(&args, 2)["error"]["code"], "INVALID_CONFIG");
}

#[cfg(target_os = "linux")]
#[test]
fn cli_migration_plan_prepares_terminal_provenance_and_checks_combined_graph() {
    for policy_pin in [POLICY_PIN, EARLIER_POLICY_PIN] {
        check_terminal_provenance_and_graph(policy_pin);
    }
}

#[cfg(target_os = "linux")]
fn check_terminal_provenance_and_graph(policy_pin: &str) {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    fs::create_dir(&home).unwrap();
    let config = temp.path().join("config");
    let source = temp.path().join("source");
    init_git(
        &source,
        &home,
        "Synthetic operator",
        "operator@sample.invalid",
    );
    fs::write(
        source.join("AGENTS.md"),
        format!("hive-system@{policy_pin}\n"),
    )
    .unwrap();
    if policy_pin == POLICY_PIN {
        fs::write(
            source.join("hive.yml"),
            "version: 1\ncoordination:\n  mode: local\n",
        )
        .unwrap();
    }

    let actor = "synthetic:release-reviewer";
    let outcome = "Approved synthetic verification and archived the release checklist.";
    let acceptance = "Cancellation of `_hive-userdata/widget` was accepted as a scope change.";
    let second_acceptance =
        "Cancellation of `_general/widget-extra` was accepted as a scope change.";
    let wrong_scope = "Cancellation of `example-scope/widget` was accepted as a scope change.";
    let created = "2026-04-01T10:00:00Z";
    let updated = "2026-04-03T11:00:00+01:00";
    let closed = "2026-04-02T10:00:00Z";
    let body = format!(
        "# Synthetic release check\n\nCreated: {created}\nClosed: {closed}\n\
         Updated: {updated}\nClosed by: {actor}\nOutcome: {outcome}\n"
    );
    let parent_body = format!(
        "{body}\n## Historical scope review\n\n{acceptance}\r\n\r\n{second_acceptance}\n\n{wrong_scope}\n\n```text\n{acceptance}\n```\n"
    );
    let cancelled_body =
        format!("{body}\nCancellation: workspace widget superseded by the plan.\n");
    let todo_criteria = "- [ ] Verify the replacement workspace widget.";
    let todo_body = format!("{body}\n## Acceptance\n\n{todo_criteria}\n");
    let claim = "claim: {id: synthetic-assignment, holder: synthetic-holder}\n";
    let fixtures = [
        (
            "_general/active/widget",
            "done",
            "priority: low\n",
            parent_body.as_str(),
        ),
        (
            "_hive-userdata/archive/widget",
            "cancelled",
            "",
            cancelled_body.as_str(),
        ),
        (
            "_general/archive/widget-extra",
            "cancelled",
            "",
            cancelled_body.as_str(),
        ),
        (
            "_general/archive/prepare-release",
            "done",
            "",
            parent_body.as_str(),
        ),
        ("_general/archive/verify-release", "done", "", body.as_str()),
        ("_general/archive/follow-up", "done", "", body.as_str()),
        (
            "_general/archive/missing-scope-acceptance",
            "done",
            "",
            parent_body.as_str(),
        ),
        (
            "_general/archive/child-without-approval",
            "cancelled",
            "",
            cancelled_body.as_str(),
        ),
        ("_general/archive/held-closure", "done", "", body.as_str()),
        (
            "_general/archive/uses-held-prerequisite",
            "done",
            "",
            body.as_str(),
        ),
        (
            "_general/archive/transitively-held",
            "done",
            "",
            body.as_str(),
        ),
        (
            "_general/archive/missing-prerequisite",
            "done",
            "",
            body.as_str(),
        ),
        (
            "_general/archive/uses-cancelled-prerequisite",
            "done",
            "",
            body.as_str(),
        ),
        (
            "_general/archive/independent-cancellation",
            "cancelled",
            "",
            cancelled_body.as_str(),
        ),
        (
            "_general/archive/unfinished-parent",
            "done",
            "",
            body.as_str(),
        ),
        (
            "_general/active/unfinished-child",
            "todo",
            "",
            todo_body.as_str(),
        ),
        (
            "_general/archive/incomplete-parent-list",
            "done",
            "",
            body.as_str(),
        ),
        (
            "_hive-userdata/archive/unlisted-child",
            "done",
            "",
            body.as_str(),
        ),
        (
            "_general/archive/uses-incomplete-parent",
            "done",
            "",
            body.as_str(),
        ),
        (
            "_general/archive/metadata-parent",
            "done",
            "",
            body.as_str(),
        ),
        (
            "_general/archive/metadata-child",
            "done",
            "unknown: true\n",
            body.as_str(),
        ),
        (
            "_general/archive/duplicate-parent",
            "done",
            "",
            body.as_str(),
        ),
        (
            "_general/archive/duplicate-child",
            "done",
            "",
            body.as_str(),
        ),
        (
            "_general/active/duplicate-child",
            "done",
            "",
            todo_body.as_str(),
        ),
        ("_general/archive/cycle-parent", "done", "", body.as_str()),
        ("_general/archive/cycle-child", "done", "", body.as_str()),
        (
            "_general/archive/depends-on-cycle",
            "done",
            "",
            body.as_str(),
        ),
        ("_general/active/claimed-done", "done", claim, body.as_str()),
        (
            "_general/archive/claimed-cancelled",
            "cancelled",
            claim,
            cancelled_body.as_str(),
        ),
        (
            "_general/active/pending-review",
            "review",
            "",
            body.as_str(),
        ),
        (
            "_general/active/claimed-todo",
            "todo",
            claim,
            todo_body.as_str(),
        ),
        (
            "_general/archive/uses-claimed-prerequisite",
            "done",
            "",
            body.as_str(),
        ),
        (
            "_general/archive/bad-closure-order",
            "done",
            "",
            body.as_str(),
        ),
    ];
    for (bundle, state, fields, body) in fixtures {
        let file = source.join(format!("tasks/{bundle}/README.md"));
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(
            file,
            format!(
                "---\ndescription: Synthetic release task\nstatus: {state}\n{fields}---\n{body}"
            ),
        )
        .unwrap();
    }
    git(&source, &home, &["add", "."]);
    git(
        &source,
        &home,
        &["commit", "-qm", "Synthetic terminal and graph evidence"],
    );
    let commit = git_text(&source, &home, &["rev-parse", "HEAD"]);
    let workspace = id(1);
    let general = id(AREA_GENERAL);
    let userdata = id(AREA_USERDATA);
    let key = |scope: &str, name: &str| json!({"scope": scope, "work_id": name});
    let local = |name: &str| key("_general", name);
    let range = |body: &str, evidence: &str| {
        let start = body.find(evidence).unwrap();
        json!({"start": start, "end": start + evidence.len()})
    };
    let child = |key: Value| json!({"legacy_key": key});
    let reviews: Vec<_> = fixtures.iter()
        .filter(|(bundle, ..)| *bundle != "_general/active/duplicate-child")
        .map(|(bundle, state, _, body)| {
        let (scope, rest) = bundle.split_once('/').unwrap();
        let (_, name) = rest.split_once('/').unwrap();
        let area = if scope == "_general" { &general } else { &userdata };
        let mut relations = json!({"parent": null, "prerequisites": [], "children": []});
        match (scope, name) {
            ("_general", "widget") => {
                relations["prerequisites"] = json!([local("verify-release"), local("prepare-release")]);
                relations["children"] = json!([
                    {"legacy_key": key("_hive-userdata", "widget"), "scope_acceptance": range(body, acceptance)},
                    {"legacy_key": local("widget-extra"), "scope_acceptance": range(body, second_acceptance)},
                ]);
            }
            ("_hive-userdata", "widget") | (_, "widget-extra") => relations["parent"] = local("widget"),
            (_, "follow-up") => relations["prerequisites"] = json!([local("widget")]),
            (_, "missing-scope-acceptance") => {
                relations["children"] = json!([child(local("child-without-approval"))]);
            }
            (_, "child-without-approval") => relations["parent"] = local("missing-scope-acceptance"),
            (_, "uses-held-prerequisite") => relations["prerequisites"] = json!([local("held-closure")]),
            (_, "transitively-held") => relations["prerequisites"] = json!([local("uses-held-prerequisite")]),
            (_, "missing-prerequisite") => relations["prerequisites"] = json!([local("unrecorded")]),
            (_, "uses-cancelled-prerequisite") => {
                relations["prerequisites"] = json!([local("independent-cancellation")]);
            }
            (_, "unfinished-parent") => relations["children"] = json!([child(local("unfinished-child"))]),
            (_, "unfinished-child") => relations["parent"] = local("unfinished-parent"),
            (_, "unlisted-child") => relations["parent"] = local("incomplete-parent-list"),
            (_, "uses-incomplete-parent") => {
                relations["prerequisites"] = json!([local("incomplete-parent-list")]);
            }
            (_, "metadata-child") => relations["parent"] = local("metadata-parent"),
            (_, "duplicate-child") if rest.starts_with("archive/") => {
                relations["parent"] = local("duplicate-parent");
            }
            (_, "cycle-parent") => relations["children"] = json!([child(local("cycle-child"))]),
            (_, "cycle-child") => {
                relations["parent"] = local("cycle-parent");
                relations["prerequisites"] = json!([local("cycle-parent")]);
            }
            (_, "depends-on-cycle") => relations["prerequisites"] = json!([local("cycle-parent")]),
            (_, "uses-claimed-prerequisite") => relations["prerequisites"] = json!([local("claimed-done")]),
            _ => {}
        }
        let mut review = json!({
            "legacy_key": key(scope, name),
            "source_git_blob_sha": git_text(&source, &home, &["rev-parse", &format!("HEAD:tasks/{bundle}/README.md")]),
            "area_id": area,
            "relations": relations,
            "created_at": range(body, created),
            "updated_at": range(body, updated),
        });
            if matches!(*state, "done" | "cancelled") {
            review["closure"] = json!({
                "closed_at": range(body, closed),
                "closed_by_ref": range(body, actor),
                "resolution_note": range(body, outcome),
            });
        }
            if name == "held-closure" {
                review["closure"].as_object_mut().unwrap().remove("closed_by_ref");
            }
        if name == "bad-closure-order" {
            review["created_at"] = range(body, updated);
        }
        if *state == "todo" {
            review["acceptance_criteria"] = range(body, todo_criteria);
        }
        review
    }).collect();
    let mut mapping = json!({
        "version": 1, "source_commit": commit, "policy_pin": policy_pin, "workspace_id": workspace,
        "scopes": [
            {"scope": "_general", "decision": {"kind":"area","area":{"id":general,"slug":"general","name":"General"}}},
            {"scope": "_hive-userdata", "decision": {"kind":"area","area":{"id":userdata,"slug":"operations","name":"Operations"}}}
        ],
        "task_reviews": reviews,
    });
    let file = temp.path().join("review.json");
    let args = [
        "--source",
        source.to_str().unwrap(),
        "--source-commit",
        &commit,
        "--workspace-id",
        &workspace,
        "--mapping-file",
        file.to_str().unwrap(),
        "--tasks",
    ];
    let run = |mapping: &str, status| {
        fs::write(&file, mapping).unwrap();
        migration_cli(&home, &config, "migration-plan", &args, status)
    };
    let plan = |mapping: &Value| run(&mapping.to_string(), 0)["data"]["task_intake"].clone();
    let index = fs::read(source.join(".git/index")).unwrap();
    let report = plan(&mapping);
    assert_eq!(report, plan(&mapping));
    let rows = |report: &Value| -> BTreeMap<String, Value> {
        report["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| {
                let k = &row["legacy_key"];
                let key = format!(
                    "{}/{}",
                    k["scope"].as_str().unwrap(),
                    k["work_id"].as_str().unwrap()
                );
                (key, row["outcome"].clone())
            })
            .collect()
    };
    let outcomes = rows(&report);
    assert_eq!(
        report["counts"],
        json!({
            "inventoried": fixtures.len(), "prepared": 7, "held": fixtures.len() - 7, "excluded": 0
        })
    );
    let parent = &outcomes["_general/widget"];
    assert_eq!(parent["status"], "prepared");
    assert_eq!(parent["candidate_status"], "done");
    assert_eq!(parent["provenance"]["priority"], 2);
    assert_eq!(
        parent["provenance"]["closure"],
        json!({
            "closed_at": {"value": closed, "body_range": range(&parent_body, closed)},
            "closed_by_ref": {"sha256": sha256_hex(actor.as_bytes()), "body_range": range(&parent_body, actor)},
            "resolution_note": {"sha256": sha256_hex(outcome.as_bytes()), "body_range": range(&parent_body, outcome)},
        })
    );
    assert_eq!(
        parent["provenance"]["relations"],
        json!({
            "parent": null,
            "prerequisites": [local("prepare-release"), local("verify-release")],
            "children": [
                {"legacy_key": local("widget-extra"),
                 "scope_acceptance": {"sha256": sha256_hex(second_acceptance.as_bytes()),
                    "body_range": range(&parent_body, second_acceptance)}},
                {"legacy_key": key("_hive-userdata", "widget"),
                 "scope_acceptance": {"sha256": sha256_hex(acceptance.as_bytes()),
                    "body_range": range(&parent_body, acceptance)}},
            ],
        })
    );
    let child = &outcomes["_hive-userdata/widget"];
    assert_eq!(child["status"], "prepared");
    assert_eq!(child["candidate_status"], "cancelled");
    assert_eq!(child["provenance"]["priority"], Value::Null);
    assert_eq!(child["provenance"]["relations"]["parent"], local("widget"));
    assert_eq!(outcomes["_general/widget-extra"]["status"], "prepared");
    assert_eq!(
        outcomes["_general/prepare-release"]["provenance"]["relations"]["children"],
        json!([])
    );
    assert_eq!(outcomes["_general/follow-up"]["status"], "prepared");
    assert_eq!(
        outcomes["_general/independent-cancellation"]["status"],
        "prepared"
    );
    for name in [
        "missing-scope-acceptance",
        "uses-held-prerequisite",
        "transitively-held",
        "missing-prerequisite",
        "uses-cancelled-prerequisite",
        "unfinished-parent",
        "incomplete-parent-list",
        "uses-incomplete-parent",
        "metadata-parent",
        "duplicate-parent",
        "cycle-parent",
        "cycle-child",
        "depends-on-cycle",
        "uses-claimed-prerequisite",
    ] {
        assert_eq!(
            outcomes[&format!("_general/{name}")]["reason_codes"],
            json!(["unresolved_completion_obligation"]),
            "{name}"
        );
    }
    for (name, reason) in [
        ("child-without-approval", "unresolved_task_relations"),
        ("unfinished-child", "unresolved_task_relations"),
        ("held-closure", "incomplete_historical_provenance"),
        ("bad-closure-order", "incomplete_historical_provenance"),
        ("claimed-done", "invalid_legacy_state"),
        ("claimed-cancelled", "invalid_legacy_state"),
        ("pending-review", "missing_typed_submission_evidence"),
        (
            "claimed-todo",
            if policy_pin == POLICY_PIN {
                "execution_needs_reconciliation"
            } else {
                "invalid_legacy_state"
            },
        ),
        ("metadata-child", "unsupported_task_metadata"),
    ] {
        assert_eq!(
            outcomes[&format!("_general/{name}")]["reason_codes"],
            json!([reason]),
            "{name}"
        );
    }
    assert_eq!(
        outcomes["_hive-userdata/unlisted-child"]["reason_codes"],
        json!(["unresolved_completion_obligation"])
    );
    let duplicates: Vec<_> = report["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row["legacy_key"] == local("duplicate-child"))
        .collect();
    assert_eq!(duplicates.len(), 2);
    for row in duplicates {
        assert_eq!(
            row["outcome"]["reason_codes"],
            json!(["duplicate_legacy_work_id"])
        );
    }
    for raw in [
        actor,
        outcome,
        acceptance,
        todo_criteria,
        "synthetic-holder",
        "issue_number",
    ] {
        assert!(!report.to_string().contains(raw), "report exposed {raw}");
    }

    let witness_in_code = parent_body.rfind(acceptance).unwrap();
    for (label, child_index, proof) in [
        ("empty range", 0, json!({"start": 0, "end": 0})),
        ("ordinary outcome", 0, range(&parent_body, outcome)),
        ("copied sibling witness", 1, range(&parent_body, acceptance)),
        (
            "same work ID in another scope",
            0,
            range(&parent_body, wrong_scope),
        ),
        (
            "quoted as code",
            0,
            json!({
                "start": witness_in_code, "end": witness_in_code + acceptance.len(),
            }),
        ),
    ] {
        let mut invalid = mapping.clone();
        invalid["task_reviews"][0]["relations"]["children"][child_index]["scope_acceptance"] =
            proof;
        let invalid_proof = rows(&plan(&invalid));
        for name in [
            "_general/widget",
            "_hive-userdata/widget",
            "_general/widget-extra",
            "_general/follow-up",
        ] {
            assert_eq!(invalid_proof[name]["status"], "held", "{label}: {name}");
        }
        assert_eq!(
            invalid_proof["_general/widget"]["reason_codes"],
            json!(["unresolved_completion_obligation"]),
            "{label}",
        );
        assert_eq!(
            invalid_proof["_general/verify-release"]["status"],
            "prepared"
        );
    }

    let review_index = mapping["task_reviews"]
        .as_array()
        .unwrap()
        .iter()
        .position(|review| review["legacy_key"] == key("_hive-userdata", "unlisted-child"))
        .unwrap();
    let mut unmapped = mapping.clone();
    unmapped["scopes"].as_array_mut().unwrap().pop();

    let mut repeated = mapping.clone();
    let mut alternate = mapping["task_reviews"][review_index].clone();
    alternate["relations"]["parent"] = Value::Null;
    repeated["task_reviews"]
        .as_array_mut()
        .unwrap()
        .push(alternate);

    let mut missing_closure = mapping.clone();
    missing_closure["task_reviews"][review_index]
        .as_object_mut()
        .unwrap()
        .remove("closure");

    for (manifest, reason) in [
        (unmapped, "ambiguous_scope_or_collision"),
        (repeated, "conflicting_task_review"),
        (missing_closure, "incomplete_historical_provenance"),
    ] {
        let held = rows(&plan(&manifest));
        assert_eq!(
            held["_hive-userdata/unlisted-child"]["reason_codes"],
            json!([reason])
        );
        for name in [
            "_general/incomplete-parent-list",
            "_general/uses-incomplete-parent",
        ] {
            assert_eq!(held[name]["status"], "held", "{reason}: {name}");
            assert_eq!(
                held[name]["reason_codes"],
                json!(["unresolved_completion_obligation"])
            );
        }
        assert_eq!(held["_general/verify-release"]["status"], "prepared");
    }

    // An assertion with a stale blob cannot veto preparation of another Task.
    let mut stale = mapping.clone();
    stale["task_reviews"][review_index]["source_git_blob_sha"] = json!("0".repeat(40));
    let stale = rows(&plan(&stale));
    assert_eq!(
        stale["_hive-userdata/unlisted-child"]["reason_codes"],
        json!(["task_review_binding_mismatch"])
    );
    assert_eq!(
        stale["_general/incomplete-parent-list"]["status"],
        "prepared"
    );
    assert_eq!(
        stale["_general/uses-incomplete-parent"]["status"],
        "prepared"
    );

    let positional_range = |evidence| {
        let selection = range(&parent_body, evidence);
        json!([selection["start"], selection["end"]])
    };
    let parent_review = &mapping["task_reviews"][0];
    for (pointer, positional) in [
        (
            "/relations",
            json!([
                null,
                parent_review["relations"]["prerequisites"],
                parent_review["relations"]["children"]
            ]),
        ),
        ("/relations/parent", json!(["_general", "widget"])),
        (
            "/relations/prerequisites/0",
            json!(["_general", "prepare-release"]),
        ),
        (
            "/relations/children/0",
            json!([
                key("_hive-userdata", "widget"),
                range(&parent_body, acceptance)
            ]),
        ),
        (
            "/relations/children/0/legacy_key",
            json!(["_hive-userdata", "widget"]),
        ),
        (
            "/relations/children/0/scope_acceptance",
            positional_range(acceptance),
        ),
        (
            "/closure",
            json!([
                range(&parent_body, closed),
                range(&parent_body, actor),
                range(&parent_body, outcome)
            ]),
        ),
        ("/closure/closed_at", positional_range(closed)),
    ] {
        let mut malformed = mapping.clone();
        *malformed["task_reviews"][0].pointer_mut(pointer).unwrap() = positional;
        let rejected = run(&malformed.to_string(), 2);
        assert_eq!(rejected["error"]["code"], "INVALID_CONFIG", "{pointer}");
        assert!(rejected.get("data").is_none());
    }
    let manifest = mapping.to_string();
    for (field, replacement) in [
        ("\"children\":", "\"unknown\":null,\"children\":"),
        ("\"parent\":null", "\"parent\":null,\"parent\":null"),
        ("\"closed_at\":", "\"closed_at\":null,\"closed_at\":"),
        ("\"start\":", "\"start\":0,\"start\":"),
    ] {
        assert!(manifest.contains(field));
        let rejected = run(&manifest.replacen(field, replacement, 1), 2);
        assert_eq!(rejected["error"]["code"], "INVALID_CONFIG", "{replacement}");
        assert!(rejected.get("data").is_none());
    }
    mapping["task_reviews"][0]["relations"]
        .as_object_mut()
        .unwrap()
        .remove("children");
    let incomplete_review = rows(&plan(&mapping));
    assert_eq!(
        incomplete_review["_general/widget"]["reason_codes"],
        json!(["unresolved_completion_obligation"])
    );
    assert_eq!(fs::read(source.join(".git/index")).unwrap(), index);
    assert_eq!(git_text(&source, &home, &["rev-parse", "HEAD"]), commit);
    assert!(git(&source, &home, &["status", "--porcelain"]).is_empty());
    assert!(!config.exists());
}

#[cfg(target_os = "linux")]
#[test]
fn cli_migration_plan_checks_task_review_boundaries_under_both_policy_pins() {
    let max_criteria = format!("{}A", "é".repeat(32_767));
    let oversized_criteria = format!("{max_criteria}B");
    let created = "2025-05-04T08:10:11Z";
    let updated = "2025-05-04t09:10:11z";
    let dates = "incomplete_historical_provenance";
    let titles = "missing_or_conflicting_title";
    let criteria = "unverified_acceptance_criteria";
    let description = Ok(("description", "Synthetic fallback"));
    let mut cases = Vec::new();
    cases.extend(
        [
            ("letter-created", "2025-05-04X08:10:11Z", updated),
            ("newline-updated", created, "2025-05-04\n09:10:11Z"),
            ("space-created", "2025-05-04 08:10:11Z", updated),
            ("tab-updated", created, "2025-05-04\t09:10:11Z"),
            ("padded-created", " 2025-05-04T08:10:11Z", updated),
            ("padded-updated", created, "2025-05-04T09:10:11Z\n"),
        ]
        .map(|(id, created, updated)| (id, "", "", created, updated, "A", Err(dates))),
    );
    cases.extend(
        [
            ("blockquote", "> # Nested H1\n"),
            ("list", "- # Nested H1\n"),
            ("ordered-nested-setext", "1. > Nested H1\n   > ===\n"),
            ("setext", "Setext H1\n===\n"),
            ("indented-h1", "  # Indented H1\n"),
            ("later-h1", "Prose first\n\n# Later H1\n"),
            ("nbsp-paragraph", "\u{00a0}\n\n# Later H1\n"),
            (
                "reference-before-h1",
                "[ref]: https://example.invalid\n\n# Later H1\n",
            ),
            ("additional-nested-h1", "# Leading H1\n\n> # Nested H1\n"),
        ]
        .map(|(id, opening)| (id, "", opening, created, updated, "A", Err(titles))),
    );
    cases.extend(
        [
            (
                "explicit-title",
                "title: Synthetic explicit title\n",
                "> # Nested H1\n",
                Ok(("frontmatter_title", "Synthetic explicit title")),
            ),
            (
                "leading-h1",
                "",
                "\n# Leading H1 ###\n\n~~~\n# Fenced example\n~~~\n",
                Ok(("heading", "Leading H1")),
            ),
            (
                "leading-h1-cr",
                "",
                "# Exact first title\rFollowing text\r",
                Ok(("heading", "Exact first title")),
            ),
            (
                "leading-h1-blank-cr",
                "",
                "\r# Exact first title ###\rFollowing text\r",
                Ok(("heading", "Exact first title")),
            ),
            (
                "leading-h1-crlf",
                "",
                "\r\n# Exact first title ###\r\nFollowing text\r\n",
                Ok(("heading", "Exact first title")),
            ),
            (
                "fenced-h1",
                "",
                "```md\n# Example H1\nText\n===\n```\n",
                description,
            ),
            ("indented-code", "", "    # Example H1\n", description),
        ]
        .map(|(id, header, opening, expected)| {
            (id, header, opening, created, updated, "A", expected)
        }),
    );
    cases.extend(
        [
            ("form-feed", FORM_FEED_CRITERIA, description),
            ("max-criteria", max_criteria.as_str(), description),
            (
                "oversize-criteria",
                oversized_criteria.as_str(),
                Err(criteria),
            ),
            ("blank-criteria", " \x0c\u{2003}", Err(criteria)),
        ]
        .map(|(id, criteria, expected)| (id, "", "", created, updated, criteria, expected)),
    );

    for pin in [POLICY_PIN, "0fd0f8dbe632d14094a93599cb1b5f653789ac9c"] {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        fs::create_dir(&home).unwrap();
        let config = root.path().join("config");
        let source = root.path().join("source");
        init_source(&source, &home);
        fs::write(source.join("AGENTS.md"), format!("hive-system@{pin}\n")).unwrap();
        if pin != POLICY_PIN {
            fs::remove_file(source.join("hive.yml")).unwrap();
        }
        let mut reviews = Vec::new();
        let area = id(AREA_GENERAL);
        for (work_id, header, opening, created, updated, criteria, _) in cases.iter().copied() {
            let path = format!("tasks/_general/active/{work_id}/README.md");
            let entrypoint = source.join(&path);
            let body = format!("{opening}\nCreated: {created}\nUpdated: {updated}\n\n{criteria}");
            let raw =
                format!("---\ndescription: Synthetic fallback\nstatus: todo\n{header}---\n{body}");
            fs::create_dir_all(entrypoint.parent().unwrap()).unwrap();
            fs::write(entrypoint, raw).unwrap();
            let blob = git_text(&source, &home, &["hash-object", &path]);
            let range = |start, value: &str| json!({"start": start, "end": start + value.len()});
            reviews.push(json!({
                "legacy_key": {"scope": "_general", "work_id": work_id},
                "source_git_blob_sha": blob,
                "area_id": area,
                "relations": "no_edges",
                "created_at": range(opening.len() + "\nCreated: ".len(), created),
                "updated_at": range(opening.len() + "\nCreated: ".len() + created.len() + "\nUpdated: ".len(), updated),
                "acceptance_criteria": range(body.len() - criteria.len(), criteria),
            }));
        }
        git(&source, &home, &["add", "."]);
        git(
            &source,
            &home,
            &["commit", "-qm", "Synthetic Task review boundaries"],
        );
        let commit = git_text(&source, &home, &["rev-parse", "HEAD"]);
        let workspace = id(1);
        let mapping_file = root.path().join("mapping.json");
        let mut mapping = json!({
            "version": 1, "source_commit": commit, "policy_pin": pin, "workspace_id": workspace,
            "scopes": [{"scope": "_general", "decision": {"kind": "area", "area": {
                "id": area, "slug": "general", "name": "General"
            }}}],
            "task_reviews": reviews,
        });
        let plan = |input: &str, status| {
            fs::write(&mapping_file, input).unwrap();
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
                    mapping_file.to_str().unwrap(),
                    "--tasks",
                ],
                status,
            )
        };
        let report = plan(&mapping.to_string(), 0);
        let tasks = report["data"]["task_intake"]["tasks"].as_array().unwrap();
        for (work_id, _, _, created, updated, criteria, expected) in cases.iter().copied() {
            let row = tasks
                .iter()
                .find(|row| row["legacy_key"]["work_id"] == work_id)
                .unwrap();
            let outcome = &row["outcome"];
            match expected {
                Ok((title_source, title)) => {
                    assert_eq!(outcome["status"], "prepared", "{pin} {work_id}: {outcome}");
                    let evidence = &outcome["provenance"];
                    assert_eq!(evidence["title_source"], title_source);
                    assert_eq!(evidence["title_sha256"], sha256_hex(title.as_bytes()));
                    assert_eq!(evidence["created_at"]["value"], created);
                    assert_eq!(evidence["updated_at"]["value"], updated);
                    assert_eq!(
                        evidence["acceptance_criteria"]["sha256"],
                        sha256_hex(criteria.as_bytes())
                    );
                }
                Err(reason) => {
                    assert_eq!(outcome["status"], "held", "{pin} {work_id}");
                    assert_eq!(outcome["reason_codes"], json!([reason]), "{pin} {work_id}");
                }
            }
        }

        mapping["task_reviews"] = json!([reviews
            .iter()
            .find(|review| review["legacy_key"]["work_id"] == "form-feed")
            .unwrap()]);
        let exact_json = mapping.to_string();
        for (pointer, fields) in [
            (
                "",
                &[
                    "version",
                    "source_commit",
                    "policy_pin",
                    "workspace_id",
                    "scopes",
                    "task_reviews",
                ][..],
            ),
            (
                "/task_reviews/0",
                &[
                    "legacy_key",
                    "source_git_blob_sha",
                    "area_id",
                    "relations",
                    "created_at",
                    "updated_at",
                    "acceptance_criteria",
                ],
            ),
            ("/task_reviews/0/legacy_key", &["scope", "work_id"]),
            ("/task_reviews/0/created_at", &["start", "end"]),
            ("/task_reviews/0/updated_at", &["start", "end"]),
            ("/task_reviews/0/acceptance_criteria", &["start", "end"]),
        ] {
            let mut positional = mapping.clone();
            let object = positional.pointer_mut(pointer).unwrap();
            *object = Value::Array(fields.iter().map(|field| object[*field].clone()).collect());
            let mut unknown = mapping.clone();
            unknown.pointer_mut(pointer).unwrap()["unexpected"] = json!(true);
            let original = mapping.pointer(pointer).unwrap();
            let original_json = original.to_string();
            let field = fields[0];
            let duplicate = exact_json.replacen(
                &original_json,
                &format!(
                    "{{{}:{},{}",
                    json!(field),
                    original[field],
                    &original_json[1..]
                ),
                1,
            );
            for input in [positional.to_string(), unknown.to_string(), duplicate] {
                let rejected = plan(&input, 2);
                assert_eq!(
                    rejected["error"]["code"], "INVALID_CONFIG",
                    "{pin} {pointer}"
                );
                assert!(rejected.get("data").is_none());
            }
        }
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
    admin
        .exec_drop(
            "UPDATE issues SET acceptance_criteria = ? WHERE id = ?",
            (FORM_FEED_CRITERIA, id(0x111)),
        )
        .unwrap();
    let stored: Option<String> = admin
        .exec_first(
            "SELECT HEX(acceptance_criteria) FROM issues WHERE id = ?",
            (id(0x111),),
        )
        .unwrap();
    assert_eq!(stored.as_deref(), Some("410C42"));
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

#[cfg(target_os = "linux")]
#[test]
fn cli_migration_plan_prepares_reviewed_queens_at_both_pins() {
    for pin in [POLICY_PIN, "0fd0f8dbe632d14094a93599cb1b5f653789ac9c"] {
        check_prepared_queen_plan(pin);
    }
}

#[cfg(target_os = "linux")]
fn check_prepared_queen_plan(pin: &str) {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    fs::create_dir(&home).unwrap();
    let source = root.path().join("source");
    let config = root.path().join("config");
    init_source(&source, &home);
    if pin != POLICY_PIN {
        let policy_path = source.join("AGENTS.md");
        let policy = fs::read_to_string(&policy_path).unwrap();
        fs::write(policy_path, policy.replace(POLICY_PIN, pin)).unwrap();
        fs::remove_file(source.join("hive.yml")).unwrap();
    }
    let exact_soul = "---\r\ndescription: \"Synthetic identity.\"\r\n---\r\n# Queen\r\n\r\n## Identity\r\n\r\n- ID: queen-example\r\n- Name: Source name sentinel\r\n- Layer: 1 — Manager\r\n\r\n## Purpose\r\n\r\nExact café sentinel.\r\n";
    let other_soul = exact_soul
        .replace("queen-example", "other-queen")
        .replace("# Queen", "# Role Coordinator")
        .replace("- Name: Source name sentinel", "- Name: Role Coordinator");
    let worker_soul = exact_soul
        .replace("queen-example", "worker")
        .replace("1 — Manager", "2 — Coordinator");
    let conflicting_title = exact_soul
        .replace("queen-example", "conflicting-title")
        .replace("# Queen", "# **Role** Coordinator");
    let conflicting_role =
        exact_soul.replace("queen-example", "conflicting-role") + "\r\n**Role**: Coordinator\r\n";
    let write = |path: &str, content: &str| {
        let path = source.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    };
    write("bots/queen-example/SOUL.md", exact_soul);
    write("bots/other-queen/SOUL.md", &other_soul);
    write("bots/worker/SOUL.md", &worker_soul);
    write("bots/conflicting-title/SOUL.md", &conflicting_title);
    write("bots/conflicting-role/SOUL.md", &conflicting_role);
    write("bots/queen-example/README.md", "Routing sentinel");
    write("bots/queen-example/AGENTS.md", "Harness sentinel");
    write(
        "bots/queen-example/MEMORY.md",
        "---\ndescription: \"Memory navigation.\"\n---\n# Memory\n\n- [_general](memory/_general/MEMORY.md)\n",
    );
    write(
        "bots/queen-example/memory/_general/MEMORY.md",
        "History sentinel",
    );
    git(&source, &home, &["add", "."]);
    git(
        &source,
        &home,
        &["commit", "-qm", "Synthetic preparation inputs"],
    );
    let commit = git_text(&source, &home, &["rev-parse", "HEAD"]);
    let workspace = id(1);
    let mapping_file = root.path().join("mapping.json");
    let oid = |slug: &str| {
        git_text(
            &source,
            &home,
            &["rev-parse", &format!("HEAD:bots/{slug}/SOUL.md")],
        )
    };
    let reviewed = |slug: &str| json!({"slug": slug, "source_git_blob_sha": oid(slug)});
    let preparation = |slug: &str, soul: &str, bot_id: String| {
        json!({
            "slug": slug,
            "source_git_blob_sha": oid(slug),
            "source_raw_sha256": sha256_hex(soul.as_bytes()),
            "bot_id": bot_id,
            "display_name": "Reviewed display sentinel",
        })
    };
    let queen = preparation("queen-example", exact_soul, id(2));
    let other = preparation("other-queen", &other_soul, id(3));
    let worker = preparation("worker", &worker_soul, id(4));
    let mapping = json!({
        "version": 1,
        "source_commit": commit,
        "policy_pin": pin,
        "workspace_id": workspace,
        "scopes": [],
        "queen_assets": [
            reviewed("queen-example"), reviewed("other-queen"), reviewed("worker"),
            reviewed("conflicting-title"), reviewed("conflicting-role"),
        ],
        "queen_preparation": [
            queen, other, worker,
            preparation("conflicting-title", &conflicting_title, id(6)),
            preparation("conflicting-role", &conflicting_role, id(7)),
        ],
    });
    let args = [
        "--source",
        source.to_str().unwrap(),
        "--source-commit",
        &commit,
        "--workspace-id",
        &workspace,
        "--mapping-file",
        mapping_file.to_str().unwrap(),
    ];
    let run_json = |manifest: &str, status| {
        fs::write(&mapping_file, manifest).unwrap();
        migration_cli(&home, &config, "migration-plan", &args, status)
    };
    let run = |mapping: &Value, status| run_json(&mapping.to_string(), status);
    let index_before = fs::read(source.join(".git/index")).unwrap();
    let report = run(&mapping, 0);
    let data = &report["data"];
    let prepared = &data["queen_preparation"];
    assert_eq!(
        prepared[0],
        json!({
            "status": "prepared",
            "slug": "queen-example",
            "bot_id": id(2),
            "source_git_blob_sha": oid("queen-example"),
            "source_raw_sha256": sha256_hex(exact_soul.as_bytes()),
        })
    );
    assert_eq!(prepared[1]["status"], "prepared");
    for (index, slug) in [
        (2, "worker"),
        (3, "conflicting-title"),
        (4, "conflicting-role"),
    ] {
        assert_eq!(
            prepared[index],
            json!({"status": "held", "slug": slug, "reason": "invalid_soul_or_slug"})
        );
    }
    assert_eq!(
        data["queen_assets"]["navigation_only"][0]["reason"],
        "navigation_only"
    );
    assert_eq!(
        data["queen_assets"]["bot_memories_historical"][0]["reason"],
        "curated_memory_deferred"
    );
    assert!(!report.to_string().contains("sentinel"));

    let mut intake_args = args.to_vec();
    intake_args.extend(["--tasks", "--include-directives-artifacts"]);
    let combined = migration_cli(&home, &config, "migration-plan", &intake_args, 0);
    assert_eq!(combined["data"]["queen_assets"], data["queen_assets"]);
    assert_eq!(combined["data"]["queen_preparation"], *prepared);
    for section in ["task_intake", "directive_artifact_intake"] {
        assert!(data.get(section).is_none(), "{pin}: {section}");
        assert_eq!(combined["data"][section]["coverage"], "partial", "{pin}");
    }

    let positional_manifest = json!([
        mapping["version"],
        mapping["source_commit"],
        mapping["policy_pin"],
        mapping["workspace_id"],
        mapping["scopes"],
        mapping["queen_assets"],
        mapping["queen_preparation"],
    ]);
    let rejects_manifest = |label: &str, manifest: &str| {
        let report = run_json(manifest, 2);
        assert_eq!(report["error"]["code"], "INVALID_CONFIG", "{pin}: {label}");
        assert!(report.get("data").is_none(), "{pin}: {label}");
    };
    rejects_manifest("positional manifest", &positional_manifest.to_string());
    for (section, fields) in [
        ("queen_assets", &["slug", "source_git_blob_sha"][..]),
        (
            "queen_preparation",
            &[
                "slug",
                "source_git_blob_sha",
                "source_raw_sha256",
                "bot_id",
                "display_name",
            ][..],
        ),
    ] {
        let entry = &mapping[section][0];
        let mut changed = mapping.clone();
        changed[section][0] = json!(
            fields
                .iter()
                .map(|field| &entry[*field])
                .collect::<Vec<_>>()
        );
        rejects_manifest(&format!("positional {section} entry"), &changed.to_string());

        for (shape, invalid_list) in [
            ("null", Value::Null),
            ("object", entry.clone()),
            ("array with a null entry", json!([null])),
        ] {
            let mut changed = mapping.clone();
            changed[section] = invalid_list;
            rejects_manifest(&format!("{section} is {shape}"), &changed.to_string());
        }

        let mut changed = mapping.clone();
        changed[section][0]["unknown"] = json!(true);
        rejects_manifest(
            &format!("unknown member in {section}"),
            &changed.to_string(),
        );

        // A Value cannot represent repeated JSON members, so supply the raw entry.
        let entry_json = entry.to_string();
        let repeated_slug = format!("{{\"slug\":{},{}", entry["slug"], &entry_json[1..]);
        let manifest = format!(
            "{{\"{section}\":[{entry_json}],{}",
            &mapping.to_string()[1..]
        );
        rejects_manifest(&format!("repeated {section} manifest member"), &manifest);
        let mut changed = mapping.clone();
        changed[section] = json!([entry]);
        let manifest = changed.to_string().replace(&entry_json, &repeated_slug);
        rejects_manifest(&format!("repeated member in {section} entry"), &manifest);
    }

    let mut changed = mapping.clone();
    changed.as_object_mut().unwrap().remove("queen_preparation");
    let report = run(&changed, 0);
    assert!(report["data"].get("queen_preparation").is_none());
    assert!(report["data"].get("queen_assets").is_some());

    let mut changed = mapping.clone();
    changed.as_object_mut().unwrap().remove("queen_assets");
    assert_eq!(
        run(&changed, 0)["data"]["queen_preparation"][0]["status"],
        "held"
    );
    changed["queen_preparation"] = json!([]);
    assert_eq!(run(&changed, 0)["data"]["queen_preparation"], json!([]));

    for (key, value) in [
        ("source_commit", json!("1".repeat(40))),
        ("workspace_id", json!(id(8))),
        ("policy_pin", json!("1".repeat(40))),
        (
            "scopes",
            json!([{"scope": "absent", "decision": {"kind": "external"}}]),
        ),
    ] {
        let mut changed = mapping.clone();
        changed[key] = value;
        let report = run(&changed, 2);
        assert_eq!(report["error"]["code"], "INVALID_CONFIG");
        assert!(report.get("data").is_none());
    }
    for (key, value) in [
        ("bot_id", json!("018f0000-0000-4000-8000-000000000001")),
        ("display_name", json!(" ")),
        ("slug", json!("../queen-example")),
    ] {
        let mut changed = mapping.clone();
        changed["queen_preparation"][0][key] = value;
        assert_eq!(run(&changed, 2)["error"]["code"], "INVALID_CONFIG");
    }
    for (key, value) in [
        ("source_git_blob_sha", json!("1".repeat(40))),
        ("source_raw_sha256", json!("1".repeat(64))),
    ] {
        let mut changed = mapping.clone();
        changed["queen_preparation"][0][key] = value;
        assert_eq!(
            run(&changed, 0)["data"]["queen_preparation"][0]["status"],
            "held"
        );
    }
    let mut changed = mapping.clone();
    changed["queen_assets"] = json!([]);
    assert_eq!(
        run(&changed, 0)["data"]["queen_preparation"][0]["status"],
        "held"
    );

    let mut changed = mapping.clone();
    changed["queen_preparation"][1]["bot_id"] = json!(id(2));
    let report = run(&changed, 0);
    assert_eq!(report["data"]["queen_preparation"][0]["status"], "held");
    assert_eq!(report["data"]["queen_preparation"][1]["status"], "held");

    let mut changed = mapping.clone();
    changed["queen_preparation"][1] = preparation("queen-example", exact_soul, id(5));
    let report = run(&changed, 0);
    assert_eq!(report["data"]["queen_preparation"][0]["status"], "held");
    assert_eq!(report["data"]["queen_preparation"][1]["status"], "held");

    assert_eq!(fs::read(source.join(".git/index")).unwrap(), index_before);
    assert_eq!(git_text(&source, &home, &["rev-parse", "HEAD"]), commit);
    assert!(git(&source, &home, &["status", "--porcelain"]).is_empty());
    assert!(!config.exists());
}
