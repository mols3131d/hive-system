#!/usr/bin/env python3
"""Run the #25 Rust spike with HIVE_DOLT_BIN set to the Dolt 2.7.6 bin directory.

Use fixture() to run another test against a fresh candidate-schema server.
It yields a disposable working directory and the explicit Rust client environment.
"""

from contextlib import contextmanager
import os
from pathlib import Path
import re
import secrets
import socket
import subprocess
import sys
import tempfile
import time


ROOT = Path(__file__).resolve().parents[2]
MANIFEST = ROOT / "tests/spikes/dolt-rust/Cargo.toml"
DOLT_VERSION = "2.7.6"
RUST_VERSION = "1.100.0"
HOST = "127.0.0.1"
DATABASE = "synthetic_db"
USER = "fixture_writer"
WORKSPACE = "01900000-0000-7000-8000-000000000025"


def isolated_env(root):
    home = root / "home"
    home.mkdir()
    return {
        "PATH": os.defpath,
        "HOME": str(home),
        "TMPDIR": str(root),
        "XDG_CONFIG_HOME": str(home / "config"),
        "XDG_DATA_HOME": str(home / "data"),
        "XDG_STATE_HOME": str(home / "state"),
        "XDG_CACHE_HOME": str(home / "cache"),
        "LANG": "C.UTF-8",
    }


def wait_for_listener(server, port):
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        if server.poll() is not None:
            raise RuntimeError(f"Fixture Dolt server exited with {server.returncode}")
        try:
            with socket.create_connection((HOST, port), timeout=0.2):
                return
        except OSError:
            time.sleep(0.1)
    raise RuntimeError("Fixture Dolt loopback listener did not start within 10 seconds")


@contextmanager
def fixture():
    dolt = Path(os.environ["HIVE_DOLT_BIN"]).resolve() / "dolt"
    schema = (ROOT / "docs/architecture/dolt-schema.md").read_text(encoding="utf-8")
    ddl = "\n".join(re.findall(r"(?ms)^```sql\n(.*?)^```$", schema))
    if len(re.findall(r"\bCREATE TABLE\b", ddl)) != 24:
        raise RuntimeError("Unexpected candidate DDL table count")

    with tempfile.TemporaryDirectory(prefix="hive-dolt-rust-") as temp:
        root = Path(temp)
        env = isolated_env(root)
        dolt_env = env | {
            "DOLT_ROOT_PATH": env["HOME"],
            "DOLT_DISABLE_EVENT_FLUSH": "true",
            "DOLT_DISABLE_VERSION_CHECKS": "true",
        }
        db = root / DATABASE
        db.mkdir()
        password = secrets.token_hex(24)

        def run(*args, sql=None):
            result = subprocess.run(
                [dolt, *args], cwd=db, env=dolt_env, input=sql,
                capture_output=True, text=True, timeout=30, check=False,
            )
            if result.returncode:
                detail = result.stderr.replace(password, "<redacted>")[-1500:]
                raise RuntimeError(f"Dolt {args[0]} setup failed ({result.returncode})\n{detail}")
            return result.stdout.strip()

        version = run("version")
        if version != f"dolt version {DOLT_VERSION}":
            raise RuntimeError(f"Expected Dolt {DOLT_VERSION}, got {version}")
        print(version, flush=True)
        run("config", "--global", "--add", "metrics.disabled", "true",
            "user.name", "Hive Rust Spike", "user.email", "spike@example.invalid")
        run("init", "--initial-branch", "main")
        run("sql", sql=f"{ddl}\nINSERT INTO workspace_meta VALUES (1, '{WORKSPACE}');\n")
        run("add", ".")
        run("commit", "-m", "Candidate schema and synthetic workspace")
        run("sql", sql=f"""
            CREATE USER '{USER}'@'{HOST}' IDENTIFIED BY '{password}';
            GRANT SELECT, INSERT, UPDATE ON {DATABASE}.* TO '{USER}'@'{HOST}';
            DROP USER 'root'@'localhost';
        """)

        # Keep the refusal port reserved without a listener for the whole test.
        with socket.socket() as unavailable, (root / "server.log").open("w+b") as log:
            unavailable.bind((HOST, 0))
            with socket.socket() as available:
                available.bind((HOST, 0))
                port = available.getsockname()[1]
            config = root / "server.yml"
            config.write_text(
                "log_level: warning\nmax_logged_query_len: -1\n"
                f"listener:\n  host: {HOST}\n  port: {port}\n",
                encoding="utf-8",
            )
            server = subprocess.Popen(
                [dolt, "sql-server", "--config", config],
                cwd=db, env=dolt_env, stdout=log, stderr=subprocess.STDOUT,
            )
            try:
                wait_for_listener(server, port)
                yield root, env | {
                    "HIVE_SPIKE_PORT": str(port),
                    "HIVE_SPIKE_UNAVAILABLE_PORT": str(unavailable.getsockname()[1]),
                    "HIVE_SPIKE_DATABASE": DATABASE,
                    "HIVE_SPIKE_WORKSPACE_ID": WORKSPACE,
                    "HIVE_SPIKE_USER": USER,
                    "HIVE_SPIKE_PASSWORD": password,
                }
            except Exception:
                size = log.seek(0, os.SEEK_END)
                log.seek(max(0, size - 2048))
                print(log.read().decode(errors="replace").replace(password, "<redacted>"),
                      file=sys.stderr)
                raise
            finally:
                if server.poll() is None:
                    server.terminate()
                    try:
                        server.wait(timeout=10)
                    except subprocess.TimeoutExpired:
                        server.kill()
                        server.wait(timeout=5)


def main():
    cargo = Path(subprocess.check_output(
        ["rustup", "which", "--toolchain", RUST_VERSION, "cargo"],
        text=True, timeout=10,
    ).strip())
    command = [cargo, "test", "--locked", "--manifest-path", MANIFEST, "--test", "loopback"]
    with tempfile.TemporaryDirectory(prefix="hive-dolt-rust-build-") as temp:
        root = Path(temp)
        cargo_env = {
            "PATH": str(cargo.parent) + os.pathsep + os.defpath,
            "CARGO_HOME": str(root / "cargo"),
            "CARGO_TARGET_DIR": str(root / "target"),
        }
        subprocess.run(
            [*command, "--no-run"], cwd=root, env=isolated_env(root) | cargo_env,
            check=True, timeout=300,
        )
        with fixture() as (directory, env):
            subprocess.run(
                [*command, "--offline", "--", "--nocapture"],
                cwd=directory, env=env | cargo_env, check=True, timeout=60,
            )


if __name__ == "__main__":
    main()
