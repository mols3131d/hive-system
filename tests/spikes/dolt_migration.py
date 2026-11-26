#!/usr/bin/env python3
"""Exercise a bounded v1 mapping and rollback on disposable Git and Dolt data."""

import csv
import io
import os
import re
import shutil
import subprocess
import tempfile
from pathlib import Path

SOURCE = Path(__file__).resolve().parents[2]
DOLT_VERSION = "2.7.6"
FILES = {
    "tasks/synthetic/backlog/planned/README.md": (
        '---\ndescription: "Synthetic planned task"\n---\n'
        "Created: 2026-01-01 00:00:00\nUpdated: 2026-01-01 01:00:00\n"
    ),
    "tasks/synthetic/archive/completed/README.md": (
        '---\ndescription: "Synthetic closed task"\nstatus: done\n---\n'
        "Created: 2026-01-01 00:00:00\nUpdated: 2026-01-01 01:00:00\n"
        "Closed: 2026-01-01 00:00:00\n"
        "Accepted by: synthetic-reviewer\nOutcome: synthetic evidence verified\n"
    ),
    "tasks/synthetic/archive/undated/README.md": (
        '---\ndescription: "Synthetic historical record"\nstatus: done\n---\n'
        "Outcome: historical result retained\n"
    ),
    "tasks/synthetic/active/running/README.md": (
        '---\ndescription: "Synthetic running task"\nstatus: in-progress\n'
        "claim:\n  id: synthetic-claim\n  holder: synthetic-holder\n---\n"
        "Created: 2026-01-01 00:00:00\nUpdated: 2026-01-01 01:00:00\n"
    ),
    "tasks/synthetic/active/done-pending-close/README.md": (
        '---\ndescription: "Accepted in v1, awaiting closure"\nstatus: done\n---\n'
        "Created: 2026-01-01 00:00:00\nUpdated: 2026-01-01 01:00:00\n"
    ),
    "tasks/synthetic/active/claimed-todo/README.md": (
        '---\ndescription: "Claimed v1 todo"\nstatus: todo\n'
        "claim:\n  id: synthetic-claim-todo\n  holder: synthetic-holder\n---\n"
        "Created: 2026-01-01 00:00:00\nUpdated: 2026-01-01 01:00:00\n"
    ),
    "tasks/synthetic/active/claimed-review/README.md": (
        '---\ndescription: "Claimed v1 review"\nstatus: review\n'
        "claim:\n  id: synthetic-claim-review\n  holder: synthetic-holder\n---\n"
        "Created: 2026-01-01 00:00:00\nUpdated: 2026-01-01 01:00:00\n"
    ),
    "tasks/synthetic/active/legacy-review/README.md": (
        '---\ndescription: "Review without typed v2 submission evidence"\nstatus: review\n---\n'
        "Created: 2026-01-01 00:00:00\nUpdated: 2026-01-01 01:00:00\n"
    ),
}


def id_for(number):
    return f"01900000-0000-7000-8000-{number:012}"


def sql_text(value):
    if value is None:
        return "NULL"
    return "'" + value.replace("'", "''") + "'"


def mapped_record(text, lifecycle):
    def field(label):
        return next(
            (
                line.removeprefix(label + " ")
                for line in text.splitlines()
                if line.startswith(label + " ")
            ),
            None,
        )

    status = "backlog" if lifecycle == "backlog" else field("status:")
    if status == "in-progress" or "claim:" in text.splitlines():
        return None, "execution needs reconciliation"
    if status == "review":
        return None, "typed submission evidence is missing"
    created = field("Created:")
    updated = field("Updated:")
    closed = field("Closed:")
    reviewer = field("Accepted by:")
    outcome = field("Outcome:")
    if not (created and updated) or (
        status == "done" and not (closed and reviewer and outcome)
    ):
        return None, "historical provenance is incomplete"
    return (status, created, updated, closed, reviewer, outcome), None


def run_rehearsal(root, dolt, git):
    legacy = root / "legacy"
    database = root / "target"
    home = root / "home"
    for directory in (legacy, database, home):
        directory.mkdir()
    env = {
        "PATH": os.environ["PATH"],
        "HOME": str(home),
        "DOLT_ROOT_PATH": str(home),
        "DOLT_DISABLE_VERSION_CHECKS": "true",
        "DOLT_DISABLE_EVENT_FLUSH": "1",
        "XDG_CONFIG_HOME": str(home / "config"),
        "GIT_CONFIG_NOSYSTEM": "1",
        "GIT_CONFIG_GLOBAL": "/dev/null",
        "GIT_TERMINAL_PROMPT": "0",
        "LANG": "C.UTF-8",
    }

    def command(executable, directory, *args, stdin=None):
        return subprocess.run(
            [executable, *args],
            cwd=directory,
            env=env,
            input=stdin,
            text=True,
            capture_output=True,
            timeout=45,
            check=True,
        ).stdout.strip()

    def native(*args):
        return command(git, legacy, *args)

    def store(*args, stdin=None):
        return command(dolt, database, *args, stdin=stdin)

    def query(statement):
        output = store("sql", "-r", "csv", "-q", statement)
        return list(csv.reader(io.StringIO(output)))[1:]

    def head():
        [row] = query("SELECT DOLT_HASHOF('HEAD') AS revision")
        return row[0]

    native("init", "-q", "--initial-branch=main")
    native("config", "user.name", "Synthetic rehearsal")
    native("config", "user.email", "rehearsal@example.invalid")
    for path, body in FILES.items():
        target = legacy / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(body, encoding="utf-8")
    native("add", "tasks")
    native("commit", "-qm", "synthetic v1 snapshot")
    source_commit = native("rev-parse", "HEAD")

    assert re.search(rf"\b{re.escape(DOLT_VERSION)}\b", store("version"))
    store(
        "init",
        "--initial-branch",
        "main",
        "--name",
        "Synthetic rehearsal",
        "--email",
        "rehearsal@example.invalid",
    )
    schema = (SOURCE / "docs/architecture/dolt-schema.md").read_text(encoding="utf-8")
    store(
        "sql",
        stdin="\n".join(
            re.findall(r"(?ms)^\x60\x60\x60sql\n(.*?)^\x60\x60\x60$", schema)
        ),
    )
    store("add", ".")
    store("commit", "-m", "synthetic target schema")
    base = head()

    area, project, revision = id_for(1), id_for(2), id_for(3)
    statements = [
        (
            f"INSERT INTO areas (id, slug, name, revision) "
            f"VALUES ('{area}', 'mapped-scope', 'Mapped scope', '{revision}');"
        ),
        (
            f"INSERT INTO projects (id, name, status, revision) "
            f"VALUES ('{project}', 'Mapped project', 'active', '{revision}');"
        ),
    ]
    id_map = {}
    quarantined = {}
    for path in FILES:
        _, old_scope, lifecycle, old_id, _ = path.split("/")
        assert old_scope == "synthetic"
        record, reason = mapped_record(
            native("show", f"{source_commit}:{path}"), lifecycle
        )
        legacy_key = (old_scope, old_id)
        if reason:
            quarantined[legacy_key] = reason
            continue
        number = len(id_map) + 1
        issue = id_for(number + 10)
        id_map[legacy_key] = issue
        status, created, updated, closed, reviewer, outcome = record
        values = [
            issue,
            area,
            old_id,
            status,
            revision,
            created,
            updated,
            closed,
            reviewer,
            outcome,
        ]
        statements.append(
            "INSERT INTO issues (number, type, id, area_id, title, status, revision, "
            "created_at, updated_at, closed_at, closed_by_ref, resolution_note) "
            f"VALUES ({number}, 'task', {', '.join(map(sql_text, values))});"
        )
        statements.append(
            f"INSERT INTO project_issues VALUES ('{project}', '{issue}');"
        )

    assert id_map == {
        ("synthetic", "planned"): id_for(11),
        ("synthetic", "completed"): id_for(12),
    }
    assert quarantined == {
        ("synthetic", "undated"): "historical provenance is incomplete",
        ("synthetic", "running"): "execution needs reconciliation",
        ("synthetic", "done-pending-close"): "historical provenance is incomplete",
        ("synthetic", "claimed-todo"): "execution needs reconciliation",
        ("synthetic", "claimed-review"): "execution needs reconciliation",
        ("synthetic", "legacy-review"): "typed submission evidence is missing",
    }
    mutations = "\n".join(statements)
    begin = "SET @@SESSION.dolt_transaction_commit=1; START TRANSACTION;\n"
    store("sql", stdin=begin + mutations + "\nROLLBACK;")
    assert head() == base
    assert query("SELECT COUNT(*) FROM areas") == [["0"]]
    assert query("SELECT COUNT(*) FROM projects") == [["0"]]
    assert query("SELECT COUNT(*) FROM issues") == [["0"]]
    assert query("SELECT COUNT(*) FROM project_issues") == [["0"]]

    store("sql", stdin=begin + mutations + "\nCOMMIT;")
    confirmed = head()
    assert confirmed != base
    assert query(
        f"SELECT id, status FROM issues AS OF '{confirmed}' ORDER BY number"
    ) == [
        [id_for(11), "backlog"],
        [id_for(12), "done"],
    ]
    assert query(
        f"SELECT issue_id FROM project_issues AS OF '{confirmed}' ORDER BY issue_id"
    ) == [
        [id_for(11)],
        [id_for(12)],
    ]
    assert query(f"SELECT COUNT(*) FROM issue_claims AS OF '{confirmed}'") == [["0"]]
    assert query(
        "SELECT closed_by_ref, resolution_note FROM issues WHERE status = 'done'"
    ) == [
        ["synthetic-reviewer", "synthetic evidence verified"],
    ]
    undated = subprocess.run(
        [
            dolt,
            "sql",
            "-q",
            (
                "INSERT INTO issues (id, number, area_id, title, type, status, revision, "
                "created_at, updated_at, closed_at, closed_by_ref, resolution_note) "
                f"VALUES ('{id_for(13)}', 3, '{area}', 'undated', 'task', 'done', '{revision}', "
                "NULL, NULL, NULL, NULL, 'historical result retained');"
            ),
        ],
        cwd=database,
        env=env,
        capture_output=True,
        text=True,
        timeout=45,
        check=False,
    )
    assert undated.returncode != 0
    assert "null" in undated.stderr.lower(), undated.stderr
    assert head() == confirmed
    assert query("SELECT COUNT(*) FROM issues WHERE number = 3") == [["0"]]
    assert native("rev-parse", "HEAD") == source_commit
    assert not native("status", "--porcelain")
    print("PASS: qualified legacy IDs and explicit Area/Project mapping")
    print(
        "PASS: incomplete closure, claimed records and legacy review held for reconciliation"
    )
    print(
        "PASS: rolled-back target unchanged, successful target versioned, v1 source preserved"
    )


def main():
    if not __debug__:
        raise SystemExit("Run the rehearsal with Python assertions enabled.")
    dolt = shutil.which("dolt")
    git = shutil.which("git")
    if not dolt or not git:
        raise SystemExit(
            "Pinned Dolt and Git are required for the migration rehearsal."
        )
    with tempfile.TemporaryDirectory(prefix="hive-dolt-migration-") as temporary:
        run_rehearsal(Path(temporary), dolt, git)


if __name__ == "__main__":
    main()
