#!/usr/bin/env python3
"""Probe Hive #27 with Dolt 2.7.6 in HIVE_DOLT_BIN/dolt and disposable repositories.

The manifest, active-root check and graph validation belong to this fixture.
"""

import csv
import hashlib
import io
import json
import os
import re
import shlex
import subprocess
import tempfile
from contextlib import contextmanager
from graphlib import CycleError, TopologicalSorter
from pathlib import Path

DOLT_VERSION = "2.7.6"
SCHEMA = Path(__file__).resolve().parents[2] / "docs/architecture/dolt-schema.md"
PAIRED_REF = "refs/tags/hive-paired-soul"
SOUL = Path("bots/synthetic/SOUL.md")
PAIRED_SOUL = "# Synthetic Bot\n\nUse the paired snapshot identity.\n"
LATER_SOUL = "# Synthetic Bot\n\nUse the later identity.\n"
TIMESTAMP = "2026-01-01 00:00:00.000000"


def fixture_id(number):
    return f"01900000-0000-7000-8000-{number:012x}"


WORKSPACE, AREA, BOT, MEMORY, REQUEST = map(fixture_id, range(1, 6))
A, B, C, D = ISSUES = tuple(map(fixture_id, range(10, 14)))
BASE_PREREQUISITES = [(B, C), (D, A)]


class Rejected(RuntimeError):
    pass


@contextmanager
def rejects(reason):
    try:
        yield
    except Rejected as error:
        assert str(error) == reason, str(error)
    else:
        raise AssertionError(f"Expected rejection for {reason}")


class Tools:
    def __init__(self, dolt_bin, home):
        self.dolt_bin = dolt_bin
        home.mkdir()
        (home / "tmp").mkdir()
        (home / "git-template").mkdir()
        self.env = {
            "PATH": os.environ["PATH"],
            "HOME": str(home),
            "TMPDIR": str(home / "tmp"),
            "XDG_CACHE_HOME": str(home / "cache"),
            "XDG_CONFIG_HOME": str(home / "config"),
            "XDG_DATA_HOME": str(home / "data"),
            "XDG_STATE_HOME": str(home / "state"),
            "DOLT_ROOT_PATH": str(home),
            "DOLT_DISABLE_EVENT_FLUSH": "true",
            "DOLT_DISABLE_VERSION_CHECKS": "true",
            "GIT_CONFIG_NOSYSTEM": "1",
            "GIT_TEMPLATE_DIR": str(home / "git-template"),
            "GIT_ALLOW_PROTOCOL": "file",
            "GIT_TERMINAL_PROMPT": "0",
            "GIT_AUTHOR_NAME": "Hive Restore Spike",
            "GIT_COMMITTER_NAME": "Hive Restore Spike",
            "GIT_AUTHOR_EMAIL": "restore@example.invalid",
            "GIT_COMMITTER_EMAIL": "restore@example.invalid",
            "LANG": "C.UTF-8",
            "TZ": "UTC",
        }
        self.dolt(home, "config", "--global", "--add", "metrics.disabled", "true")
        self.dolt(
            home, "config", "--global", "--add", "user.name", "Hive Restore Spike"
        )
        self.dolt(
            home, "config", "--global", "--add", "user.email", "restore@example.invalid"
        )

    def run(self, program, cwd, *args, stdin=None, check=True):
        command = [str(program), *map(str, args)]
        result = subprocess.run(
            command,
            cwd=cwd,
            env=self.env,
            input=stdin,
            encoding="utf-8",
            capture_output=True,
            timeout=60,
            check=False,
        )
        if check and result.returncode:
            raise AssertionError(
                f"{shlex.join(command)} in {cwd} exited {result.returncode}\n"
                f"{result.stdout}{result.stderr}"
            )
        return result

    def dolt(self, cwd, *args, stdin=None, check=True):
        return self.run(self.dolt_bin, cwd, *args, stdin=stdin, check=check)

    def git(self, cwd, *args):
        return self.run("git", cwd, *args).stdout

    def rows(self, db, query):
        output = self.dolt(db, "sql", "-r", "csv", "-q", query).stdout
        header, *rows = csv.reader(io.StringIO(output))
        assert all(len(row) == len(header) for row in rows), output
        return rows

    def scalar(self, db, query):
        rows = self.rows(db, query)
        assert len(rows) == 1 and len(rows[0]) == 1, (query, rows)
        return rows[0][0]

    def head(self, db):
        revision = self.scalar(db, "SELECT DOLT_HASHOF('HEAD')")
        assert re.fullmatch(r"[0-9a-v]{32}", revision), revision
        return revision

    def workspace_id(self, db):
        return self.scalar(
            db, "SELECT workspace_id FROM workspace_meta WHERE singleton_id=1"
        )

    def commit(self, db, message):
        self.dolt(db, "add", ".")
        self.dolt(db, "commit", "-m", message)
        return self.head(db)


def seed_database(tools, db):
    db.mkdir()
    tools.dolt(db, "init", "--initial-branch", "main")
    schema = SCHEMA.read_text(encoding="utf-8")
    ddl = "\n".join(re.findall(r"(?ms)^```sql\n(.*?)^```$", schema))
    assert len(re.findall(r"\bCREATE TABLE\b", ddl)) == 24
    assert "CREATE VIEW ready_issue_candidates" in ddl
    tools.dolt(db, "sql", stdin=ddl)
    tools.commit(db, "candidate schema")

    ddl_digest = hashlib.sha256(ddl.encode()).hexdigest()
    issue_rows = ",\n".join(
        f"('{issue}',{number},'{AREA}','Fixture {number}','task','todo',"
        f"'{fixture_id(20 + number)}','{TIMESTAMP}','{TIMESTAMP}')"
        for number, issue in enumerate(ISSUES, 1)
    )
    tools.dolt(
        db,
        "sql",
        stdin=f"""
        INSERT INTO workspace_meta VALUES (1,'{WORKSPACE}');
        INSERT INTO schema_migrations VALUES (1,'{ddl_digest}','{TIMESTAMP}');
        INSERT INTO workspace_counters VALUES ('issue_number',5);
        INSERT INTO areas (id,slug,name,revision)
        VALUES ('{AREA}','synthetic','Synthetic Area','{fixture_id(25)}');
        INSERT INTO issues (id,number,area_id,title,type,status,revision,created_at,updated_at)
        VALUES {issue_rows};
        INSERT INTO issue_dependencies VALUES ('{B}','{C}'),('{D}','{A}');
        INSERT INTO bots (id,slug,display_name,revision)
        VALUES ('{BOT}','synthetic','Synthetic Bot','{fixture_id(26)}');
        INSERT INTO bot_memories
        (id,bot_id,area_id,kind,recall_tier,topic_key,title,body,source_ref,author_ref,
         authorization_mode,authorization_ref,revision,created_at,updated_at)
        VALUES ('{MEMORY}','{BOT}','{AREA}','durable','core','fixture','Fixture Memory',
                'Earlier memory','synthetic-source','synthetic-bot','delegated',
                'synthetic-delegation','{fixture_id(27)}','{TIMESTAMP}','{TIMESTAMP}');
    """,
    )
    earlier = tools.commit(db, "synthetic workspace")

    tools.dolt(
        db,
        "sql",
        stdin=f"""
        START TRANSACTION;
        UPDATE bot_memories SET body='Paired memory',revision='{fixture_id(31)}'
        WHERE id='{MEMORY}';
        UPDATE bots SET revision='{fixture_id(32)}' WHERE id='{BOT}';
        INSERT INTO operation_receipts
        (request_id,kind,fingerprint,actor_ref,authorization_ref,workflow_evidence_json,
         base_commit,result_json,created_at)
        VALUES ('{REQUEST}','memory.update',REPEAT('a',64),'synthetic-bot',
                'synthetic-delegation','{{}}','{earlier}',
                '{{"id":"{MEMORY}","revision":"{fixture_id(31)}"}}','{TIMESTAMP}');
        COMMIT;
    """,
    )
    paired = tools.commit(db, "paired memory and receipt")
    assert earlier != paired
    return earlier, paired


def commit_soul(tools, assets, body, message):
    (assets / SOUL).write_text(body, encoding="utf-8")
    tools.git(assets, "add", SOUL)
    tools.git(assets, "commit", "--quiet", "-m", message)
    assert not tools.git(assets, "status", "--porcelain")
    return tools.git(assets, "rev-parse", "HEAD").strip()


def snapshot_digests(package):
    backup = package / "native"
    files = [
        package / "assets.bundle",
        *(path for path in backup.rglob("*") if path.is_file()),
    ]
    return {
        path.relative_to(package).as_posix(): hashlib.sha256(
            path.read_bytes()
        ).hexdigest()
        for path in sorted(files)
    }


def write_manifest(package, manifest):
    (package / "manifest.json").write_text(
        json.dumps(manifest, indent=2) + "\n", encoding="utf-8"
    )


def restore_snapshot(tools, package, target):
    # All packages and targets are generated by this spike. The manifest supplies
    # expected values only; its paths do not select files to open.
    if target.exists() and (not target.is_dir() or any(target.iterdir())):
        raise Rejected("populated restore destination")
    manifest = json.loads((package / "manifest.json").read_text(encoding="utf-8"))
    if (
        manifest["format"] != 1
        or manifest["schema"] != 1
        or manifest["dolt_version"] != DOLT_VERSION
    ):
        raise Rejected("incompatible fixture manifest")
    if snapshot_digests(package) != manifest["files"]:
        raise Rejected("snapshot digest mismatch")

    bundle = package / "assets.bundle"
    pair = tools.git(package, "bundle", "list-heads", bundle, PAIRED_REF).strip()
    if pair != f"{manifest['asset_commit']} {PAIRED_REF}":
        raise Rejected("paired Git revision mismatch")

    with tempfile.TemporaryDirectory(prefix="restore-", dir=target.parent) as staging:
        stage = Path(staging)
        db = stage / "db"
        tools.dolt(stage, "backup", "restore", (package / "native").as_uri(), "db")
        if tools.head(db) != manifest["dolt_commit"]:
            raise Rejected("Dolt revision mismatch")
        assert tools.workspace_id(db) == manifest["workspace_id"]
        assert tools.rows(db, "SELECT version FROM schema_migrations") == [
            [str(manifest["schema"])]
        ]

        assets = stage / "assets"
        tools.git(stage, "clone", "--quiet", "--no-checkout", bundle, assets)
        tools.git(assets, "checkout", "--quiet", "--detach", manifest["asset_commit"])
        tools.git(assets, "remote", "remove", "origin")
        stage.rename(target)


def select_active_root(tools, registrations, workspace):
    """Model explicit local selection by DB identity within one state directory."""
    identity = tools.workspace_id(workspace / "db")
    assert identity == WORKSPACE
    registration = registrations / identity
    root = str(workspace.resolve()) + "\n"
    try:
        with registration.open("x", encoding="utf-8") as record:
            record.write(root)
    except FileExistsError:
        if registration.read_text(encoding="utf-8") != root:
            raise Rejected(
                "workspace identity already active at another root"
            ) from None


def graph_edges(tools, db):
    dependencies = tools.rows(
        db,
        """
        SELECT issue_id, prerequisite_id FROM issue_dependencies ORDER BY issue_id, prerequisite_id
    """,
    )
    children = tools.rows(
        db,
        """
        SELECT parent_id, id FROM issues WHERE parent_id IS NOT NULL ORDER BY parent_id, id
    """,
    )
    return [tuple(row) for row in dependencies], [tuple(row) for row in children]


def require_acyclic(edges):
    graph = TopologicalSorter()
    for issue, obligation in edges:
        graph.add(issue, obligation)
    graph.prepare()


def verify_restored(tools, restored, manifest, earlier, later_soul, db_hash):
    db = restored / "db"
    assets = restored / "assets"
    assert tools.head(db) == manifest["dolt_commit"]
    assert tools.workspace_id(db) == WORKSPACE
    assert tools.scalar(db, "SELECT DOLT_HASHOF_DB()") == db_hash
    assert tools.rows(
        db, "SELECT number, title, status FROM issues ORDER BY number"
    ) == [[str(number), f"Fixture {number}", "todo"] for number in range(1, 5)]
    assert graph_edges(tools, db) == (BASE_PREREQUISITES, [])
    assert tools.scalar(db, "SELECT COUNT(*) FROM issue_claims") == "0"
    assert tools.scalar(db, "SELECT body FROM bot_memories") == "Paired memory"
    assert (
        tools.scalar(db, f"SELECT body FROM bot_memories AS OF '{earlier}'")
        == "Earlier memory"
    )
    assert tools.rows(db, "SELECT request_id, base_commit FROM operation_receipts") == [
        [REQUEST, earlier]
    ]
    assert (
        tools.scalar(db, f"SELECT COUNT(*) FROM operation_receipts AS OF '{earlier}'")
        == "0"
    )
    assert tools.rows(db, "SELECT * FROM dolt_status") == []

    assert tools.git(assets, "rev-parse", "HEAD").strip() == manifest["asset_commit"]
    assert tools.git(assets, "rev-parse", "main").strip() == later_soul
    assert tools.git(assets, "show", f"{later_soul}:{SOUL}") == LATER_SOUL
    assert (assets / SOUL).read_bytes() == PAIRED_SOUL.encode()
    assert not tools.git(assets, "status", "--porcelain")
    print(
        f"PASS: independent Dolt readback {manifest['dolt_commit']} (history {earlier}, root {db_hash})"
    )
    print(
        f"PASS: exact paired Soul {manifest['asset_commit']} with later Git main {later_soul}"
    )


def probe_restore(tools, root):
    originals = root / "originals"
    source = originals / "source"
    source.mkdir(parents=True)
    db, assets = source / "db", source / "assets"
    earlier, db_commit = seed_database(tools, db)
    (assets / SOUL).parent.mkdir(parents=True)
    tools.git(assets, "init", "--quiet", "--initial-branch=main")
    asset_commit = commit_soul(tools, assets, PAIRED_SOUL, "paired Soul")
    tools.git(assets, "tag", "hive-paired-soul", asset_commit)
    db_hash = tools.scalar(db, "SELECT DOLT_HASHOF_DB()")
    assert tools.rows(db, "SELECT * FROM dolt_status") == []

    registrations = Path(tools.env["XDG_STATE_HOME"]) / "active-roots"
    registrations.mkdir(parents=True)
    select_active_root(tools, registrations, source)

    package = originals / "snapshot"
    native = package / "native"
    native.mkdir(parents=True)
    tools.dolt(db, "backup", "sync-url", native.as_uri())

    # Keep the later Git tip in the bundle to exercise exact selection at restore.
    later_soul = commit_soul(tools, assets, LATER_SOUL, "later Soul")
    assert later_soul != asset_commit
    tools.git(
        assets,
        "bundle",
        "create",
        package / "assets.bundle",
        "HEAD",
        "refs/heads/main",
        PAIRED_REF,
    )
    manifest = {
        "format": 1,
        "schema": 1,
        "workspace_id": WORKSPACE,
        "dolt_version": DOLT_VERSION,
        "dolt_commit": db_commit,
        "asset_commit": asset_commit,
        "files": snapshot_digests(package),
    }
    bad_digests = manifest["files"] | {"assets.bundle": "0" * 64}
    missing = root / "rejected-restore"
    for change, reason in (
        ({"files": bad_digests}, "snapshot digest mismatch"),
        ({"asset_commit": later_soul}, "paired Git revision mismatch"),
        ({"dolt_commit": earlier}, "Dolt revision mismatch"),
    ):
        write_manifest(package, manifest | change)
        with rejects(reason):
            restore_snapshot(tools, package, missing)
        assert not missing.exists(), reason
        print(f"PASS: rejected {reason} before publishing a destination")

    write_manifest(package, manifest)
    populated = root / "populated"
    populated.mkdir()
    sentinel = populated / "sentinel"
    sentinel.write_bytes(b"keep this synthetic file\n")
    with rejects("populated restore destination"):
        restore_snapshot(tools, package, populated)
    assert list(populated.iterdir()) == [sentinel]
    assert sentinel.read_bytes() == b"keep this synthetic file\n"

    restored = root / "restored"
    restored.mkdir()
    restore_snapshot(tools, package, restored)
    result = tools.dolt(
        restored, "backup", "restore", native.as_uri(), "db", check=False
    )
    assert result.returncode == 1 and "already exists" in result.stderr, result
    print(
        "PASS: fixture refused a populated directory and native restore refused the existing DB"
    )

    # Prove readback with both original paths unavailable and fresh CLI config.
    offline = originals.rename(root / "offline-originals")
    try:
        reader = Tools(tools.dolt_bin, root / "readback-home")
        verify_restored(reader, restored, manifest, earlier, later_soul, db_hash)
    finally:
        offline.rename(originals)

    with rejects("workspace identity already active at another root"):
        select_active_root(tools, registrations, restored)
    registration = registrations / WORKSPACE
    assert list(registrations.iterdir()) == [registration]
    assert registration.read_text(encoding="utf-8") == str(source.resolve()) + "\n"
    select_active_root(tools, registrations, source)
    assert (
        tools.head(db) == db_commit
        and tools.scalar(db, "SELECT DOLT_HASHOF_DB()") == db_hash
    )
    assert tools.rows(db, "SELECT * FROM dolt_status") == []
    assert tools.git(assets, "rev-parse", "HEAD").strip() == later_soul
    assert (assets / SOUL).read_bytes() == LATER_SOUL.encode()
    assert not tools.git(assets, "status", "--porcelain")
    print(
        "PASS: restored identity stayed inactive, duplicate selection refused, source preserved"
    )
    return native, db_commit


def probe_git_transport(tools, root, native, base):
    root.mkdir()
    tools.dolt(root, "backup", "restore", native.as_uri(), "transport-db")
    db = root / "transport-db"
    bare = root / "transport.git"
    seed = root / "ordinary-git"
    seed.mkdir()
    tools.git(root, "init", "--quiet", "--bare", "--initial-branch=main", bare)
    uri = "git+" + bare.as_uri()
    tools.dolt(db, "remote", "add", "origin", uri)
    empty_push = tools.dolt(db, "push", "origin", "main", check=False)
    assert (
        empty_push.returncode == 1 and "git remote has no branches" in empty_push.stderr
    ), empty_push
    assert not tools.git(bare, "for-each-ref")
    print(
        "PASS: git+file:// push to an empty bare Git repository requires a seed branch"
    )

    tools.git(seed, "init", "--quiet", "--initial-branch=main")
    tools.git(seed, "commit", "--quiet", "--allow-empty", "-m", "ordinary Git seed")
    git_main = tools.git(seed, "rev-parse", "HEAD").strip()
    tools.git(seed, "push", "--quiet", bare.as_uri(), "main")
    tools.dolt(db, "push", "origin", "main")
    assert tools.git(bare, "cat-file", "-t", "refs/dolt/data").strip() == "commit"
    for name in ("left", "right"):
        tools.dolt(root, "clone", "--branch", "main", uri, name)
        assert tools.head(root / name) == base
    left, right = root / "left", root / "right"

    # Base has B -> C and D -> A. The edits change disjoint Issue revisions.
    # A -> B (prerequisite) and C -> D (parent completion) close a cycle together.
    tools.dolt(
        left,
        "sql",
        stdin=f"""
        INSERT INTO issue_dependencies VALUES ('{A}','{B}');
        UPDATE issues SET revision='{fixture_id(40)}' WHERE id='{A}';
        UPDATE issues SET revision='{fixture_id(41)}' WHERE id='{B}';
    """,
    )
    left_commit = tools.commit(left, "add A prerequisite B")
    tools.dolt(
        right,
        "sql",
        stdin=f"""
        UPDATE issues SET parent_id='{C}',revision='{fixture_id(42)}' WHERE id='{D}';
        UPDATE issues SET revision='{fixture_id(43)}' WHERE id='{C}';
    """,
    )
    right_commit = tools.commit(right, "make D a child of C")
    left_dependencies = [(A, B), *BASE_PREREQUISITES]
    right_children = [(C, D)]
    assert graph_edges(tools, left) == (left_dependencies, [])
    assert graph_edges(tools, right) == (BASE_PREREQUISITES, right_children)
    require_acyclic(left_dependencies)
    require_acyclic([*BASE_PREREQUISITES, *right_children])

    tools.dolt(left, "push", "origin", "main")
    published = tools.git(bare, "rev-parse", "refs/dolt/data").strip()
    push = tools.dolt(right, "push", "origin", "main", check=False)
    assert push.returncode == 1 and "non-fast-forward" in push.stderr, push
    tools.dolt(right, "fetch", "origin")
    assert tools.scalar(right, "SELECT DOLT_HASHOF('origin/main')") == left_commit
    assert tools.dolt(right, "merge-base", "HEAD", "origin/main").stdout.strip() == base
    ff = tools.dolt(right, "merge", "--ff-only", "origin/main", check=False)
    assert ff.returncode == 1 and "Not possible to fast-forward" in ff.stdout, ff
    assert tools.head(right) == right_commit
    assert tools.rows(right, "SELECT * FROM dolt_status") == []
    print(
        f"PASS: local git+file:// round trip from {base}; divergent push and fast-forward refused"
    )

    tools.dolt(right, "merge", "--no-ff", "--no-commit", "origin/main")
    try:
        assert tools.rows(
            right, "SELECT is_merging, source_commit FROM dolt_merge_status"
        ) == [["1", left_commit]]
        assert tools.head(right) == right_commit
        assert tools.rows(right, "SELECT * FROM dolt_conflicts") == []
        assert tools.rows(right, "SELECT * FROM dolt_constraint_violations") == []
        print(
            f"PASS: Dolt preview combined {left_commit} and {right_commit} without native violations"
        )

        dependencies, children = graph_edges(tools, right)
        assert (dependencies, children) == (left_dependencies, right_children)
        require_acyclic(dependencies)
        require_acyclic(children)
        try:
            require_acyclic(dependencies + children)
        except CycleError as error:
            cycle = error.args[1]
            assert len(cycle) == 5 and set(cycle) == set(ISSUES), cycle
            print(
                "PASS: fixture domain check rejected the combined completion cycle A -> B -> C -> D -> A"
            )
        else:
            raise AssertionError("Merged completion graph should contain a cycle")
    finally:
        tools.dolt(right, "merge", "--abort")

    assert tools.head(right) == right_commit
    assert graph_edges(tools, right) == (BASE_PREREQUISITES, right_children)
    assert tools.rows(right, "SELECT * FROM dolt_status") == []
    assert tools.scalar(right, "SELECT is_merging FROM dolt_merge_status") == "0"
    assert tools.git(bare, "rev-parse", "refs/dolt/data").strip() == published
    assert tools.git(bare, "rev-parse", "refs/heads/main").strip() == git_main
    print("PASS: preview aborted, local Git and Dolt remote refs preserved")


def main():
    if not __debug__:
        raise SystemExit("Run the restore probe with Python assertions enabled.")
    bin_dir = os.environ.get("HIVE_DOLT_BIN")
    if not bin_dir:
        raise SystemExit(
            "Set HIVE_DOLT_BIN to the directory containing pinned Dolt 2.7.6."
        )
    dolt = (Path(bin_dir) / "dolt").resolve(strict=True)
    with tempfile.TemporaryDirectory(prefix="hive-dolt-restore-") as tmp:
        root = Path(tmp).resolve()
        tools = Tools(dolt, root / "home")
        version = tools.dolt(root, "version").stdout.strip()
        assert version == f"dolt version {DOLT_VERSION}", version
        print(version, flush=True)
        print(tools.git(root, "--version").strip(), flush=True)
        native, base = probe_restore(tools, root)
        probe_git_transport(tools, root / "transport", native, base)
    print("PROBES PASSED: isolated backup, inactive restore and local Git divergence")


if __name__ == "__main__":
    main()
