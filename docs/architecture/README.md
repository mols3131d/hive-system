---
description: "Use when locating component boundaries, design decisions, dependencies or technical tradeoffs."
---

# Architecture

[Goals](../goal/README.md) owns product outcomes and acceptance criteria. This area owns Hive's technical decisions and implementation guidance.

## Dolt v2 development baseline

The operator adopted Dolt v2 for MVP development in [#22](https://github.com/mols3131d/hive-system/issues/22#issuecomment-7198810176). Read the [Dolt architecture](dolt-redesign.md) for the Area and Issue model and storage boundaries, the [MVP contract](dolt-mvp-contract.md) for domain operations and acceptance, and the [relational schema](dolt-schema.md) for tables, constraints and validation. Begin with [read-only schema and record validation (#10)](https://github.com/mols3131d/hive-system/issues/10) and follow the [delivery order and acceptance plan (#15)](https://github.com/mols3131d/hive-system/issues/15).

## v1 compatibility

Use the [v1 system design](system-design.md), [v1 CLI contract](cli-contract.md) and the applicable pinned [workspace policies](../policies/workspace/README.md) for Markdown/Git compatibility. The current [Rust CLI](../../src/main.rs) handles arguments, output, and read-only v1 configuration and context inspection. Existing workspaces operate under their reviewed pins. Live migration and a switch in workspace authority require separate explicit operator approval.
