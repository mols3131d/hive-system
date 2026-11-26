"""Disposable Dolt SQL Server and process-held lock for the graph spike."""

import csv
import fcntl
import io
import re
import socket
import subprocess
import tempfile
import time
import uuid
from contextlib import contextmanager
from dataclasses import dataclass
from pathlib import Path

VERSION = "2.7.6"
HOST = "127.0.0.1"
DATABASE = "graph_db"
AUTHOR = "Graph Spike"
EMAIL = "graph@example.invalid"
SCHEMA = Path(__file__).resolve().parents[2] / "docs/architecture/dolt-schema.md"
WAIT_SECONDS = 20


def fixture_env(root):
    home = root / "home"
    return {
        "HOME": str(home),
        "DOLT_ROOT_PATH": str(home),
        "DOLT_DISABLE_VERSION_CHECKS": "true",
        "DOLT_DISABLE_EVENT_FLUSH": "1",
        "XDG_CONFIG_HOME": str(home / "config"),
        "XDG_DATA_HOME": str(home / "data"),
        "XDG_STATE_HOME": str(home / "state"),
        "TMPDIR": str(root / "tmp"),
        "LANG": "C.UTF-8",
    }


def run_cli(dolt, root, cwd, *args, stdin=None):
    result = subprocess.run(
        [dolt, *args],
        cwd=cwd,
        env=fixture_env(root),
        input=stdin,
        stdin=subprocess.DEVNULL if stdin is None else None,
        capture_output=True,
        text=True,
        timeout=10,
        check=False,
    )
    if result.returncode:
        raise RuntimeError(
            f"Dolt exited {result.returncode}: {result.stderr or result.stdout}"
        )
    return result.stdout


def csv_rows(output):
    header, *rows = csv.reader(io.StringIO(output))
    assert header, output
    assert all(len(row) == len(header) for row in rows), output
    return [tuple(row) for row in rows]


@dataclass(frozen=True)
class DoltFixture:
    dolt: Path
    root: Path
    port: int
    workspace_id: str
    base: str

    @property
    def lock(self):
        return self.root / f"{self.workspace_id}.lock"

    def execute(self, query):
        return run_cli(
            self.dolt,
            self.root,
            self.root / "home",
            "--host",
            HOST,
            "--port",
            str(self.port),
            "--use-db",
            DATABASE,
            "--user",
            "root",
            "--password",
            "",
            "--no-tls",
            "sql",
            "-r",
            "csv",
            "-q",
            query,
        )

    def rows(self, query):
        return csv_rows(self.execute(query))

    def history(self, depth):
        """Read a first-parent chain ending at the current authoritative HEAD."""
        refs = ["HEAD", *(f"HEAD~{index}" for index in range(1, depth + 1))]
        [hashes] = self.rows(
            "SELECT " + ", ".join(f"DOLT_HASHOF('{ref}')" for ref in refs)
        )
        assert len(hashes) == depth + 1, hashes
        assert all(re.fullmatch(r"[0-9a-v]{32}", value) for value in hashes), hashes
        return tuple(reversed(hashes))


@contextmanager
def open_fixture(dolt, seed_sql):
    with tempfile.TemporaryDirectory(prefix="hive-dolt-graph-") as tmp:
        root = Path(tmp)
        db = root / DATABASE
        for path in (db, root / "home", root / "tmp"):
            path.mkdir()

        def local(*args, stdin=None):
            return run_cli(dolt, root, db, *args, stdin=stdin)

        local("config", "--global", "--add", "versioncheck.disabled", "true")
        local("config", "--global", "--add", "metrics.disabled", "true")
        version = local("version").strip()
        assert version == f"dolt version {VERSION}", version
        local("init", "--initial-branch", "main", "--name", AUTHOR, "--email", EMAIL)
        local("config", "--local", "--add", "user.name", AUTHOR)
        local("config", "--local", "--add", "user.email", EMAIL)
        ddl = "\n".join(
            re.findall(r"(?ms)^```sql\n(.*?)^```$", SCHEMA.read_text(encoding="utf-8"))
        )
        workspace_id = str(uuid.uuid4())
        local("sql", stdin=ddl)
        local(
            "sql",
            stdin=f"INSERT INTO workspace_meta VALUES (1, '{workspace_id}');\n{seed_sql}",
        )
        local("add", ".")
        local("commit", "-m", "synthetic graph schema and seed")
        [(base,)] = csv_rows(
            local("sql", "-r", "csv", "-q", "SELECT DOLT_HASHOF('HEAD')")
        )

        with socket.socket() as sock:
            sock.bind((HOST, 0))
            port = sock.getsockname()[1]
        fixture = DoltFixture(dolt, root, port, workspace_id, base)
        with (root / "server.log").open("w+", encoding="utf-8") as log:
            server = subprocess.Popen(
                [
                    dolt,
                    "sql-server",
                    "--host",
                    HOST,
                    "--port",
                    str(port),
                    "--socket",
                    str(root / "dolt.sock"),
                ],
                cwd=db,
                env=fixture_env(root),
                stdin=subprocess.DEVNULL,
                stdout=log,
                stderr=subprocess.STDOUT,
            )
            try:
                identity = await_server(fixture, server)
                assert identity == [(DATABASE, "main", VERSION, base, workspace_id)], (
                    identity
                )
                yield fixture
            except Exception:
                log.seek(0)
                print(log.read()[-4000:], flush=True)
                raise
            finally:
                server.terminate()
                try:
                    server.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    server.kill()
                    server.wait(timeout=5)


def await_server(fixture, server):
    deadline = time.monotonic() + WAIT_SECONDS
    last_error = "No SQL response"
    while server.poll() is None and time.monotonic() < deadline:
        try:
            return fixture.rows(
                "SELECT DATABASE(), ACTIVE_BRANCH(), DOLT_VERSION(), DOLT_HASHOF('HEAD'), "
                "workspace_id FROM workspace_meta WHERE singleton_id=1"
            )
        except RuntimeError as error:
            last_error = str(error)
            time.sleep(0.05)
    raise RuntimeError(
        f"Disposable Dolt server failed to start ({server.poll()}): {last_error}"
    )


@contextmanager
def writer_lock(path, on_lock=None):
    """Report the first kernel lock conflict and bound the retry window."""
    deadline = time.monotonic() + WAIT_SECONDS
    reported_busy = False
    with path.open("a", encoding="utf-8") as lock:
        while True:
            try:
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError:
                if not reported_busy and on_lock is not None:
                    on_lock("busy")
                    reported_busy = True
                if time.monotonic() >= deadline:
                    raise TimeoutError("Workspace writer lock remained busy")
                time.sleep(0.02)
            else:
                break
        if on_lock is not None:
            on_lock("acquired")
        yield
