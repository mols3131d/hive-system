---
description: "Use when defining project outcomes, requirements, scope, non-goals and acceptance criteria."
---

# Goals

Hive helps one developer work efficiently across multiple projects and AI agent sessions through a persistent, user-controlled collaboration workspace. Human-facing Manager Bots and task-scoped agents support that developer's work. It separates agent-internal collaboration from external project ownership and keeps only intentional, useful records.

## Outcomes

- Make project switching, task resumption, delegation, handoffs and artifact sharing across agent sessions straightforward for an individual developer.
- Keep setup, daily operation and maintenance lightweight; prefer local files and native Git over infrastructure.
- Preserve Markdown and Git as the canonical workspace data and revision history.
- Define durable, scoped Task, Artifact, Directive, Bot and Memory contracts and clear responsibility for delegation, handoff, authorization and acceptance.
- Deliver an independent Rust CLI and Hive Skill for repeatable, safe workspace operations. Keep direct authorized Git and document use possible.
- Separate reusable system software and policies from user collaboration data. Use synthetic fixtures; no private workspace contents or history belong in the public system.

## Non-goals

Hive is not a multi-user collaboration platform, enterprise project management system, distributed agent orchestrator or identity and access management service. Hive is not an agent execution platform, another project specification owner, a backend router, a personal or environment-specific configuration manager, or an automatic synchronization service. It does not implement scheduling, session control, distributed claims or automatic retention cleanup. Initial CLI development does not include MCP or a separate service/database. See [workspace policies](../policies/workspace/README.md) for the current semantic boundaries.

## Acceptance

A usable system should demonstrate that one developer can coordinate and resume work across multiple agent sessions and projects without requiring a shared service or hosting provider. It should demonstrate scoped read and validation operations, machine-readable output with distinct failure meanings, safe eligible record mutations with revision conflict handling, compatibility with the existing Markdown contract, and representative authorized agent use. Verify these with synthetic records and actual tests; a proposal or passing documentation check is not implemented behavior.

[Architecture](../architecture/system-design.md) owns technical choices and incremental implementation; [Development](../development/README.md) owns implementation and verification procedures.
