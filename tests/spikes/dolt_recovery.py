#!/usr/bin/env python3
"""Run the Linux #28 probe with HIVE_DOLT_BIN set to the Dolt 2.7.6 bin directory."""

import json
import os
from pathlib import Path
import secrets
import shutil
import subprocess
import tempfile

from dolt_rust import MANIFEST, RUST_VERSION, fixture, isolated_env


def main():
    if not os.environ.get("HIVE_DOLT_BIN"):
        raise SystemExit("Set HIVE_DOLT_BIN to the verified Dolt 2.7.6 binary directory")
    with tempfile.TemporaryDirectory(prefix="hive-dolt-recovery-build-") as temp:
        root = Path(temp)
        build_env = isolated_env(root)
        rustup = shutil.which("rustup")
        if rustup is None:
            raise RuntimeError(f"Install the pinned Rust {RUST_VERSION} toolchain")
        cargo = Path(subprocess.check_output(
            [rustup, "which", "--toolchain", RUST_VERSION, "cargo"], cwd=root,
            env=build_env | {
                "RUSTUP_HOME": os.environ.get("RUSTUP_HOME", str(Path.home() / ".rustup")),
            }, text=True, timeout=10,
        ).strip())
        cargo_env = {
            "PATH": str(cargo.parent) + os.pathsep + os.defpath,
            "CARGO_HOME": str(root / "cargo"),
            "CARGO_TARGET_DIR": str(root / "target"),
        }
        command = [cargo, "test", "--locked", "--manifest-path", MANIFEST, "--test", "recovery"]
        build = subprocess.check_output(
            [*command, "--no-run", "--message-format=json-render-diagnostics"], cwd=root,
            env=build_env | cargo_env, text=True, timeout=300,
        )
        messages = map(json.loads, build.splitlines())
        [recovery] = [
            message["executable"] for message in messages
            if message["reason"] == "compiler-artifact"
            and message["target"]["name"] == "recovery" and message["executable"]
        ]
        # Cargo changes the test working directory to the crate. Run the binary
        # directly so the marker and writer lock stay inside the fixture.
        for case in (
            "death_before_commit",
            "death_after_version_commit",
            "working_set_receipt_is_unconfirmed",
            "live_session_blocks_unrelated_write",
        ):
            with fixture() as (directory, env):
                subprocess.run(
                    [recovery, "--nocapture", "--exact", case],
                    cwd=directory, env=env | {
                        "HIVE_RECOVERY_NONCE": secrets.token_hex(16),
                    }, timeout=60, check=True,
                )


if __name__ == "__main__":
    main()
