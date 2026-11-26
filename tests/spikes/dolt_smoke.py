#!/usr/bin/env python3
"""Isolated first Dolt probe. No Hive userdata, server configuration, or remote access."""
import csv
import io
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[2]
VERSION = "2.7.6"
DOLT = shutil.which("dolt")
if not DOLT:
    raise SystemExit("Dolt is not installed; this probe has not run.")

schema = (ROOT / "docs/architecture/dolt-schema.md").read_text(encoding="utf-8")
ddl = "\n".join(re.findall(r"(?ms)^``sql\n(.*?)^```$", schema))
assert len(re.findall(r"\bCREATE TABLE\b", ddl)) == 24, "Unexpected DDL table count"
assert "CREATE VIEW ready_issue_candidates" in ddl, "Missing readiness view"

with tempfile.TemporaryDirectory(prefix="hive-dolt-spike-") as tmp:
    root = Path(tmp)
    home = root / "home"
    db = root / "synthetic_db"
    home.mkdir()
    db.mkdir()
    env = {
        "PATH": os.environ["PATH"],
        "HOME": str(home),
        "XDG_CONFIG_HOME": str(home / "config"),
        "XDG_DATA_HOME": str(home / "data"),
        "XDG_STATE_HOME": str(home / "state"),
        "DOLT_ROOT_PATH": str(home),
        "LANG": "C.UTF-8",
    }

    def run(*args, ok=True, stdin=None):
        proc = subprocess.run(
            [DOLT, *args], cwd=db, env=env, input=stdin,
            text=True, capture_output=True, timeout=60, check=False,
        )
        if ok and proc.returncode != 0:
            raise AssertionError(f"{args[0]} failed: {proc.stderr}\n{proc.stdout}")
        if not ok and proc.returncode == 0:
            raise AssertionError(f"Expected rejected SQL: {args!r}")
        return proc.stdout

    def sql(query, ok=True):
        return run("sql", "-r", "csv", "-q", query, ok=ok)

    def scalar(query):
        rows = list(csv.reader(io.StringIO(sql(query))))
        assert len(rows) == 2 and len(rows[1]) == 1, rows
        return rows[1][0]

    def count(table):
        return int(scalar(f"SELECT COUNT(*) AS n FROM {table}"))

    def head():
        value = scalar("SELECT DOLT_HASHOF('HEAD') AS h")
        assert re.fullmatch("[0-9a-v]{32}", value), value
        return value

    def commit(message):
        run("add", ".")
        run("commit", "-m", message)

    version = run("version").strip()
    print(version, flush=True)
    assert re.search(r"\b" + re.escape(VERSION) + r"\b", version), version
    run("init", "--initial-branch", "main", "--name", "Hive Spike",
        "--email", "spike@example.invalid")
    run("sql", stdin=ddl)
    commit("synthetic schema")
    print("PASS: candidate DDL and readiness view created", flush=True)

    area = "01900000-0000-7000-8000-000000000001"
    issue = "01900000-0000-7000-8000-000000000002"
    bot = "01900000-0000-7000-8000-000000000003"
    revision = "01900000-0000-7000-8000-000000000004"
    claim = "01900000-0000-7000-8000-000000000005"
    request = "01900000-0000-7000-8000-000000000006"
    memory = "01900000-0000-7000-8000-000000000007"
    missing = "01900000-0000-7000-8000-000000000099"
    sql(f"""
        INSERT INTO areas (id,slug,name,revision)
        VALUES ('{area}','synthetic','Synthetic','{revision}');
        INSERT INTO bots (id,slug,display_name,revision)
        VALUES ('{bot}','synthetic','Synthetic','{revision}');
        INSERT INTO issues (id,number,area_id,title,type,status,revision,created_at,updated_at)
        VALUES ('{issue}',1,'{area}','Synthetic','task','todo','{revision}',NOW(6),NOW(6));
    """)
    commit("synthetic fixtures")
    base = head()
    assert count("ready_issue_candidates") == 1

    sql(f"INSERT INTO issue_dependencies VALUES ('{issue}','{missing}')", ok=False)
    assert count("issue_dependencies") == 0
    sql(f"UPDATE issues SET status='invalid' WHERE id='{issue}'", ok=False)
    assert scalar(f"SELECT status FROM issues WHERE id='{issue}'") == "todo"
    sql(f"""
        INSERT INTO bot_memories
        (id,bot_id,kind,recall_tier,topic_key,title,body,source_ref,author_ref,
         authorization_mode,authorization_ref,revision,created_at,updated_at)
        VALUES ('{memory}','{bot}','durable',NULL,'test','Test','Synthetic',
                'synthetic','synthetic','delegated','synthetic','{revision}',NOW(6),NOW(6))
    """, ok=False)
    assert count("bot_memories") == 0
    assert head() == base
    print("PASS: FK, lifecycle CHECK and NULL-aware memory CHECK reject invalid writes", flush=True)

    sql(f"""
        SET @@SESSION.dolt_transaction_commit=1;
        START TRANSACTION;
        UPDATE issues SET status='in-progress' WHERE id='{issue}';
        INSERT INTO issue_claims VALUES ('{issue}','{claim}','synthetic-holder',NOW(6));
        ROLLBACK;
    """)
    assert head() == base
    assert count("issue_claims") == 0
    assert scalar(f"SELECT status FROM issues WHERE id='{issue}'") == "todo"
    print("PASS: rollback leaves both rows and version HEAD unchanged", flush=True)

    sql(f"""
        SET @@SESSION.dolt_transaction_commit=1;
        START TRANSACTION;
        UPDATE issues SET status='in-progress' WHERE id='{issue}';
        INSERT INTO issue_claims VALUES ('{issue}','{claim}','synthetic-holder',NOW(6));
        INSERT INTO operation_receipts
        (request_id,kind,fingerprint,actor_ref,authorization_ref,workflow_evidence_json,
         base_commit,result_json,created_at)
        VALUES ('{request}','claim',REPEAT('0',64),'synthetic','synthetic',
                '{{}}','{base}','{{}}',NOW(6));
        COMMIT;
    """)
    confirmed = head()
    assert confirmed != base, "SQL commit did not advance version HEAD"
    assert count("issue_claims") == 1
    assert count("operation_receipts") == 1
    assert count("ready_issue_candidates") == 0
    assert scalar(f"SELECT COUNT(*) AS n FROM operation_receipts AS OF '{confirmed}'") == "1"
    assert scalar(f"SELECT COUNT(*) AS n FROM operation_receipts AS OF '{base}'") == "0"
    assert scalar(f"SELECT status FROM issues AS OF '{confirmed}' WHERE id='{issue}'") == "in-progress"
    sql(f"INSERT INTO issue_claims VALUES ('{issue}','{missing}','other',NOW(6))", ok=False)
    assert count("issue_claims") == 1
    assert head() == confirmed
    print(f"PASS: SQL commit publishes claim and receipt together; {base} -> {confirmed}", flush=True)

print("FIRST PROBE PASSED (embedded CLI SQL only). Server/Rust/lock/crash/restore remain untested.")
