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

The Bootstrap executable validates arguments and reports unavailable workspace operations with `NOT_IMPLEMENTED`. Follow the [CLI contract](../architecture/cli-contract.md) for output and exit codes.

## Agent-assisted development

Use [instruction ownership](agents/rule-ownership.md) to place scoped guidance, [asset lifecycle](agents/assets.md) when selecting or updating reusable Agent Assets, and [harness verification](agents/harness.md) to distinguish configuration from observed runtime behavior.

## When the project grows

Document the source layout, application setup, tests and contribution process as they become relevant. Keep design decisions in [Architecture](../architecture/README.md) and executable settings in their configuration files.
