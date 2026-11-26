#!/usr/bin/env python3
"""Isolated first Dolt probe. No Hive userdata, server configuration, or remote access."""
import csv
import io
import os
from pathlib import Path
import re
import shutil
import socket
import subprocess
import sys
import tempfile
import time

ROOT = Path(__file__).resolve().parents[2]
VERSION = "2.7.6"
DOLT = shutil.which("dolt")
if not DOLT:
    raise SystemExit("Dolt is not installed; this probe has not run.")

schema = (ROOT / "docs/architecture/dolt-schema.md").read_text(encoding="utf-8")
ddl = "\n".join(re.findall(r"(?ms)^```sql\n(.*?)^```$", schema))
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
    run("config", "--local", "--add", "user.name", "Hive Spike")
    run("config", "--local", "--add", "user.email", "spike@example.invalid")
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

    # Phase two: actual loopback SQL Server, separate client processes.
    # No host installation/configuration; this server belongs to the temporary fixture.
    issue2 = "01900000-0000-7000-8000-000000000010"
    sql(f"""INSERT INTO issues
        (id,number,area_id,title,type,status,revision,created_at,updated_at)
        VALUES ('{issue2}',2,'{area}','Race','task','todo','{revision}',NOW(6),NOW(6))""")
    commit("server race fixture")
    server_base = head()
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]
    client = [DOLT, "--host", "127.0.0.1", "--port", str(port),
              "--user", "root", "--password", "", "--no-tls", "--use-db", "synthetic_db",
              "sql", "-r", "csv"]
    with (root / "server.log").open("w+", encoding="utf-8") as log:
        server = subprocess.Popen(
            [DOLT, "sql-server", "--host", "127.0.0.1", "--port", str(port)],
            cwd=db, env=env, stdout=log, stderr=subprocess.STDOUT,
        )
        try:
            def remote(query, check=True):
                return subprocess.run(
                    [*client, "-q", query], cwd=home, env=env,
                    capture_output=True, text=True, timeout=10, check=check,
                )

            for _ in range(40):
                if server.poll() is not None:
                    raise AssertionError("Disposable server exited during startup")
                probe = remote("SELECT DOLT_HASHOF('HEAD') AS h", check=False)
                if probe.returncode == 0 and server_base in probe.stdout:
                    break
                time.sleep(0.25)
            else:
                raise AssertionError(f"Could not connect to the exact synthetic database: "
                                     f"exit={probe.returncode}, expected={server_base}, "
                                     f"stdout={probe.stdout!r}, stderr={probe.stderr!r}")

            remote(f"""
                SET @@SESSION.dolt_transaction_commit=1;
                START TRANSACTION;
                UPDATE issues SET status='in-progress' WHERE id='{issue2}';
                ROLLBACK;
            """)
            assert server_base in remote("SELECT DOLT_HASHOF('HEAD') AS h").stdout
            assert "todo" in remote(f"SELECT status FROM issues WHERE id='{issue2}'").stdout
            print("PASS: loopback SQL Server transaction rollback", flush=True)

            # Two independent clients serialize the read/check/commit via an
            # OS-held lock, not a Dolt merge or a read outside the lock.
            worker = """
import fcntl, subprocess, sys, csv, io
lock, issue, suffix, *client = sys.argv[1:]
def query(s):
    return subprocess.run([*client, "-q", s], capture_output=True,
                          text=True, timeout=15, check=True).stdout
with open(lock, "a", encoding="utf-8") as file:
    fcntl.flock(file, fcntl.LOCK_EX)
    rows = list(csv.reader(io.StringIO(query(
        f"SELECT COUNT(*) AS n FROM ready_issue_candidates WHERE id='{issue}'"))))
    if rows != [["n"], ["1"]]:
        if rows == [["n"], ["0"]]:
            sys.exit(3)
        raise AssertionError(rows)
    base = list(csv.reader(io.StringIO(query("SELECT DOLT_HASHOF('HEAD') AS h"))))[1][0]
    claim = "01900000-0000-7000-8000-0000000000" + suffix
    request = "01900000-0000-7000-8000-0000000001" + suffix
    query(f'''
        SET @@SESSION.dolt_transaction_commit=1;
        START TRANSACTION;
        UPDATE issues SET status='in-progress', revision='{claim}' WHERE id='{issue}';
        INSERT INTO issue_claims VALUES ('{issue}','{claim}','worker-{suffix}',NOW(6));
        INSERT INTO operation_receipts
        (request_id,kind,fingerprint,actor_ref,authorization_ref,workflow_evidence_json,
         base_commit,result_json,created_at)
        VALUES ('{request}','claim',REPEAT('1',64),'worker-{suffix}','synthetic',
                '{{}}','{base}','{{}}',NOW(6));
        COMMIT;
    ''')
"""
            lock = root / "writer.lock"
            workers = [
                subprocess.Popen(
                    [sys.executable, "-c", worker, str(lock), issue2, suffix, *client],
                    cwd=home, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
                )
                for suffix in ("11", "12")
            ]
            statuses = []
            try:
                for process in workers:
                    out, err = process.communicate(timeout=30)
                    if process.returncode not in (0, 3):
                        raise AssertionError(f"Worker failed: {out}\n{err}")
                    statuses.append(process.returncode)
            finally:
                for process in workers:
                    if process.poll() is None:
                        process.kill()
                        process.wait(timeout=5)
            assert sorted(statuses) == [0, 3], statuses
            confirmed_server = list(csv.reader(io.StringIO(
                remote("SELECT DOLT_HASHOF('HEAD') AS h").stdout)))[1][0]
            assert confirmed_server != server_base
            rows = list(csv.reader(io.StringIO(remote(
                f"SELECT COUNT(*) AS n FROM issue_claims AS OF '{confirmed_server}' WHERE issue_id='{issue2}'").stdout)))
            assert rows == [["n"], ["1"]], rows
            rows = list(csv.reader(io.StringIO(remote(
                f"SELECT COUNT(*) AS n FROM operation_receipts AS OF '{confirmed_server}' WHERE base_commit='{server_base}'").stdout)))
            assert rows == [["n"], ["1"]], rows
            print("PASS: two separate SQL Server clients with one OS lock, one confirmed claim and receipt", flush=True)

            # A process-held advisory lock is released by process death.
            # This tests the primitive only, not the production recovery protocol.
            import fcntl
            import select
            holder = subprocess.Popen(
                [sys.executable, "-c",
                 "import fcntl,sys,time; f=open(sys.argv[1],'a'); fcntl.flock(f,fcntl.LOCK_EX); print('locked',flush=True); time.sleep(60)",
                 str(lock)], cwd=db, env=env, stdout=subprocess.PIPE, text=True,
            )
            try:
                assert select.select([holder.stdout], [], [], 5)[0], "Lock holder did not start"
                assert holder.stdout.readline().strip() == "locked"
                with lock.open("a", encoding="utf-8") as file:
                    try:
                        fcntl.flock(file, fcntl.LOCK_EX | fcntl.LOCK_NB)
                    except BlockingIOError:
                        pass
                    else:
                        raise AssertionError("Lock was not exclusive")
                    holder.kill()
                    holder.wait(timeout=5)
                    fcntl.flock(file, fcntl.LOCK_EX | fcntl.LOCK_NB)
                print("PASS: OS lock releases after holder process death", flush=True)
            finally:
                if holder.poll() is None:
                    holder.kill()
                    holder.wait(timeout=5)
                holder.stdout.close()
        except Exception:
            log.flush()
            log.seek(0)
            print(log.read(), flush=True)
            raise
        finally:
            if server.poll() is None:
                server.terminate()
                try:
                    server.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    server.kill()
                    server.wait(timeout=5)

print("PROBES PASSED: embedded CLI and loopback server; Rust driver, production recovery, restore, host remain untested.")
