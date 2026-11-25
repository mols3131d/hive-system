---
description: "Use when locating component boundaries, design decisions, dependencies or technical tradeoffs."
---

# Architecture

Use this area for the derived project's implemented or explicitly proposed system structure and decisions: component responsibilities, boundaries, data flow, constraints and meaningful tradeoffs.

[Goals](../goal/README.md) owns project outcomes and acceptance criteria; this area owns the technical decisions that support them.

Read [Hive System Design](system-design.md) for the Rust CLI, system/data separation, storage and implementation stages. [Hive CLI and Domain Contract](cli-contract.md) defines the command interface and workspace operations. The [Rust entry point](../../src/main.rs) handles arguments, output, and read-only configuration and context inspection.
