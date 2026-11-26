---
description: "Use for contributor setup, development practices, agent guidance and verification responsibilities."
---

# Development

Use this area for how contributors understand, change and verify Hive system software. The [Rust CLI](../../Cargo.toml) and the TypeScript development scripts have separate build and test commands. Use synthetic workspace records for development fixtures.

## Current setup

Shared tooling and setup are covered under [Tools](../tools/README.md).

Find existing automation through the [scripts index](../../scripts/README.md). When implementing or modifying project-owned scripts, use [script maintenance](scripts.md); task declarations remain in [mise.toml](../../mise.toml).

## Rust CLI

Install Rust through Rustup. The [toolchain file](../../rust-toolchain.toml) selects the compiler, rustfmt and Clippy for the checkout. Run the same locked checks used by [Rust CI](../../.github/workflows/rust.yml).

```sh
cargo test --locked --all-targets
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo build --locked
```

To check installation without changing your usual executable directory, install into a temporary root.

```sh
hive_install_root=$(mktemp -d)
cargo install --locked --path . --root "$hive_install_root"
"$hive_install_root/bin/hive" --help
"$hive_install_root/bin/hive" --version
```

Use an isolated synthetic workspace to test `config show`, `config validate`, `context` and local `doctor`. Git-backed inspection requires native Git with `--no-lazy-fetch` support and was tested on Linux with Git 2.53.0. Commands that are still pending return `NOT_IMPLEMENTED`. Follow the [CLI contract](../architecture/cli-contract.md) for configuration, output and exit codes.

## Dolt record validation

The initial v2 validator reads a single committed snapshot of Dolt `main` through a dedicated SELECT-only account on loopback. It verifies the pinned server version, required tables, migration ledger version and digest, and expected workspace UUID. It checks canonical UUIDv7 Area and Issue IDs, their graph references, lowercase sibling slugs, Issue numbers, active Claim cardinality, and cycles in Area ancestry, Issue parentage, prerequisites and combined completion obligations. Validation returns up to 100 diagnostics and refuses scans above 100,000 rows per checked table. The JSON result identifies the observed Dolt commit after schema and identity verification.

For an explicitly provisioned synthetic server on Dolt 2.7.6, supply the reader credentials in the process environment as `HIVE_DOLT_USER` and `HIVE_DOLT_PASSWORD`. Select the database, port and expected UUID explicitly.

```sh
hive --contract 2 --json validate --all \
  --database fixture_db --port 3307 \
  --workspace-id 01900000-0000-7000-8000-000000000001
```

Contract 1 remains the CLI default. Under contract 2, schema incompatibility and record violations exit 4. Reader authentication failures exit 6. Unverified workspace identity and unavailable runtime exit 7. Explicit workspace path and scoped validation, full lifecycle provenance checks, and bootstrap and migration commands are follow-up work.

Run the integration test with the pinned Dolt executable. The test provisions disposable data and accounts, invokes the Hive binary built by Cargo, and compares the database commit and working and staged states before and after validation.

```sh
HIVE_DOLT_BIN=/absolute/path/to/dolt cargo test --locked --test dolt_validation -- --ignored
```

## Agent-assisted development

Use [instruction ownership](agents/rule-ownership.md) to place scoped guidance, [asset lifecycle](agents/assets.md) when selecting or updating reusable Agent Assets, and [harness verification](agents/harness.md) to distinguish configuration from observed runtime behavior.

## When the project grows

Document the source layout, application setup, tests and contribution process as they become relevant. Keep design decisions in [Architecture](../architecture/README.md) and executable settings in their configuration files.
