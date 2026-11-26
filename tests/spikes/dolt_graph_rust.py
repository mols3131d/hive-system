#!/usr/bin/env python3
"""Run the #26 graph oracle with the shared Rust writer.

Set HIVE_DOLT_BIN to the verified Dolt 2.7.6 bin directory and run
python3 -B tests/spikes/dolt_graph_rust.py with Rust 1.100.0 available.
"""

import hashlib
import json
import os
import socket
import subprocess
import tempfile
from functools import partial
from pathlib import Path

from dolt_graph import Outcome, run_suite
from dolt_graph_fixture import DATABASE, VERSION, WAIT_SECONDS, fixture_env
from dolt_rust import MANIFEST, RUST_VERSION


def read_reply(stream):
    reply = stream.readline(128)
    if not reply.endswith(b"\n"):
        raise RuntimeError("Missing or oversized Rust graph writer reply")
    return reply[:-1].decode("ascii")


def rust_write(binary, fixture, change, *, check_combined=True, on_lock=None):
    endpoints = set(change.edge)
    if (
        change.expected_revisions.keys() != endpoints
        or change.revisions.keys() != endpoints
    ):
        raise ValueError(
            "Graph edit needs expected and new revisions for both endpoints"
        )
    # The CLI control uses this same payload for its fixture receipt fingerprint.
    payload = json.dumps(
        [change.kind, change.edge, change.expected_revisions], sort_keys=True
    )
    fingerprint = hashlib.sha256(payload.encode()).hexdigest()
    args = [
        binary,
        fixture.lock,
        change.kind,
        str(check_combined).lower(),
        change.request_id,
        fingerprint,
    ]
    for issue in change.edge:
        args.extend((issue, change.expected_revisions[issue], change.revisions[issue]))

    env = fixture_env(fixture.root) | {
        "HIVE_SPIKE_PORT": str(fixture.port),
        "HIVE_SPIKE_DATABASE": DATABASE,
        "HIVE_SPIKE_WORKSPACE_ID": fixture.workspace_id,
        "HIVE_SPIKE_USER": "root",
        "HIVE_SPIKE_PASSWORD": "",
    }
    parent, child = socket.socketpair()
    with parent, child:
        parent.settimeout(WAIT_SECONDS)
        with subprocess.Popen(
            args, cwd=fixture.root, env=env, stdin=child, stdout=child
        ) as worker:
            child.close()
            try:
                with parent.makefile("rb") as stream:
                    event = read_reply(stream)
                    if event == "busy":
                        if on_lock is not None:
                            on_lock(event)
                        event = read_reply(stream)
                    if event != "acquired":
                        raise RuntimeError(
                            f"Unexpected graph writer lock event {event!r}"
                        )
                    if on_lock is not None:
                        on_lock(event)
                    # Let Rust connect only after the acquired callback returns.
                    parent.sendall(b"go\n")
                    outcome = Outcome(*read_reply(stream).split())
                if worker.wait(timeout=WAIT_SECONDS) != 0:
                    raise RuntimeError(
                        f"Rust graph writer exited with {worker.returncode}"
                    )
                return outcome
            finally:
                if worker.poll() is None:
                    worker.kill()


def main():
    directory = os.environ.get("HIVE_DOLT_BIN")
    if not directory:
        raise SystemExit(
            f"Set HIVE_DOLT_BIN to the verified Dolt {VERSION} binary directory"
        )
    dolt = (Path(directory) / "dolt").resolve(strict=True)
    with tempfile.TemporaryDirectory(prefix="hive-dolt-graph-build-") as target_dir:
        subprocess.run(
            [
                "cargo",
                f"+{RUST_VERSION}",
                "build",
                "--locked",
                "--manifest-path",
                MANIFEST,
                "--bin",
                "graph_writer",
                "--target-dir",
                target_dir,
            ],
            check=True,
            timeout=300,
        )
        binary = Path(target_dir) / "debug" / "graph_writer"
        run_suite(dolt, writer=partial(rust_write, binary))


if __name__ == "__main__":
    main()
