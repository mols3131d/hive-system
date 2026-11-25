---
description: "Use for contributor setup, development practices, agent guidance and verification responsibilities."
---

# Development

Use this area for how contributors understand, change and verify Hive system software. Rust is the chosen application language; the [system design](../architecture/system-design.md) is a proposal, and no Cargo build or application tests exist yet. Keep the inherited development tooling independent of the application. Never use private workspace records as development fixtures.

## Current setup

Shared tooling and setup are covered under [Tools](../tools/README.md).

Find existing automation through the [scripts index](../../scripts/README.md). When implementing or modifying project-owned scripts, use [script maintenance](scripts.md); task declarations remain in [mise.toml](../../mise.toml).

## Agent-assisted development

Use [instruction ownership](agents/rule-ownership.md) to place scoped guidance, [asset lifecycle](agents/assets.md) when selecting or updating reusable Agent Assets, and [harness verification](agents/harness.md) to distinguish configuration from observed runtime behavior.

## When the project grows

Document the source layout, application setup, tests and contribution process as they become relevant. Keep design decisions in [Architecture](../architecture/README.md) and executable settings in their configuration files.
