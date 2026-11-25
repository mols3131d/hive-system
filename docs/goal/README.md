---
description: "Use when defining project outcomes, requirements, scope, non-goals and acceptance criteria."
---

# Goals

Hive helps one developer work efficiently across multiple projects and AI agent sessions through a persistent, user-controlled collaboration workspace. Human-facing Manager Bots and task-scoped agents support that developer's work. It supports long-running development and preserves useful context across sessions, with durable communication between the developer and agents. It separates collaboration records from external project ownership.

## Outcomes

- Support one active development environment per project, with multiple agent sessions and sequential, controlled environment moves; no simultaneous multi-environment development of one project is required.
- Support long-running development sessions and make project switching, interruption and later resumption straightforward.
- Preserve selected work context, decisions, progress, evidence and next actions without capturing full session state or transcripts.
- Support developer-agent reporting, briefings, discussions and decisions through durable records, and agent handoffs through references read when needed.
- Keep setup, daily operation and maintenance lightweight; prefer local files and native Git over infrastructure.
- Preserve portability across IDEs, coding-agent vendors, model providers and Git hosts through shared file formats and vendor-neutral operations. Optional integrations must not define the core contract.
- Keep Hive records and harness assets outside external projects by default. Using Hive to manage a developer's work must not require a project to adopt Hive, install its tooling or change its repository.
- Preserve Markdown and Git as the canonical workspace data and revision history; confirm ticket issuance and execution claims at a designated Git ref.
- Minimize repeated parameters through optional user/project YAML bindings and a reviewed workspace coordination contract. Reuse existing per-repository Git identity and authentication, independent of hosting provider.
- Define durable, scoped Task, Artifact, Directive, Bot and Memory contracts and clear responsibility for delegation, handoff, authorization and acceptance.
- Deliver an independent Rust CLI and Hive Skill for repeatable, safe workspace operations. Keep direct authorized Git and document use possible.
- Separate reusable system software and policies from user collaboration data. Use synthetic fixtures; no private workspace contents or history belong in the public system.

## Non-goals

Hive is not a multi-user collaboration platform, enterprise project management system, distributed agent orchestrator or identity and access management service. Hive is not an agent execution platform, another project specification owner, a backend router, a personal or environment-specific configuration manager, or an automatic synchronization service. Hive does not own IDE configuration, agent-vendor or model-provider integrations, or the external project's development harness. It is not an instant messaging or real-time communication system between agent sessions. It does not implement message delivery guarantees, automatic notifications, scheduling, session control, distributed lock services, automatic leases or automatic retention cleanup. Initial CLI development does not include MCP or a separate service/database. See [workspace policies](../policies/workspace/README.md) for the current semantic boundaries.

## Acceptance

A usable system should demonstrate that one developer can coordinate and resume work across multiple agent sessions and projects without requiring a vendor service or hosting provider. A shared workspace across independent repositories requires an explicitly selected common Git authority for coordination. Demonstrate the same record and resumption semantics through distinct supported agent environments, with a plain Git and Markdown path available independently of optional integrations. Verify that ordinary Hive record operations and setup leave a synthetic non-adopting external project's tracked files, untracked files, Git configuration and hooks unchanged. A later session should reconstruct useful work context, and the developer should be able to review reports, briefings and material discussion outcomes from retained records. It should demonstrate scoped read and validation operations, machine-readable output with distinct failure meanings, safe eligible record mutations with revision conflict handling, compatibility with the existing Markdown contract, and representative authorized agent use. Demonstrate competing ticket issuance and claims with at most one confirmed winner for the same task, stale and uncertain-write handling, and no credential or workspace fallback. Verify distinct repository Git identities and the explicitly opted-in untracked project binding. Verify these with synthetic records and actual tests; a proposal or passing documentation check is not implemented behavior.

[Architecture](../architecture/system-design.md) owns technical choices and incremental implementation; [Development](../development/README.md) owns implementation and verification procedures.
