---
description: "Use when defining project outcomes, requirements, scope, non-goals and acceptance criteria."
---

# Goals

Hive helps one developer work efficiently across multiple projects and AI agent sessions through a persistent, user-controlled collaboration workspace. Human-facing Manager Bots and task-scoped agents support that developer's work. It supports long-running development and preserves useful context across sessions, with durable communication between the developer and agents. It separates collaboration records from external project ownership.

Dolt v2 is the adopted MVP development baseline. Existing Markdown/Git workspaces use their reviewed v1 contracts.

## Outcomes

- Support one active development environment per project, with multiple agent sessions and sequential, controlled environment moves; no simultaneous multi-environment development of one project is required.
- Support long-running development sessions and make project switching, interruption and later resumption straightforward.
- Preserve selected work context, decisions, progress, evidence and next actions without capturing full session state or transcripts.
- Support developer-agent reporting, briefings, discussions and decisions through durable records, and agent handoffs through references read when needed.
- Keep setup, daily operation and maintenance lightweight through local-first operation. Synchronization is explicit.
- Preserve portability across IDEs, coding-agent vendors, model providers and Git hosts through portable records and vendor-neutral operations. Optional integrations must not define the core contract.
- Keep Hive records and harness assets outside external projects by default. Using Hive to manage a developer's work must not require a project to adopt Hive, install its tooling or change its repository.
- Use one authoritative local Dolt database per workspace for structured collaboration records, curated Memory and their revision history. Keep Bot Soul in Git-tracked Markdown.
- Give each Issue one owning Area in a recursive hierarchy. Purpose-based Projects can group Issues across Areas. Confirm Issue number allocation and exclusive Claims against the selected workspace authority.
- Minimize repeated parameters through explicit configuration and optional local bindings. Reuse existing per-repository Git identity and authentication for Git assets.
- Define durable, scoped Issue, Artifact, Directive, Bot and Memory contracts and clear responsibility for delegation, handoff, authorization and acceptance.
- Deliver an independent Rust CLI and Hive Skill for repeatable, safe workspace operations. Support existing Markdown and Git workspaces under their pinned v1 contracts.
- Separate reusable system software and policies from user collaboration data. Use synthetic fixtures; no private workspace contents or history belong in the public system.

## Non-goals

Hive is not a multi-user collaboration platform, enterprise project management system, distributed agent orchestrator or identity and access management service. Hive is not an agent execution platform, another project specification owner, a backend router, a personal or environment-specific configuration manager, or an automatic synchronization service. Hive does not own IDE configuration, agent-vendor or model-provider integrations, or the external project's development harness. It is not an instant messaging or real-time communication system between agent sessions. It does not implement message delivery guarantees, automatic notifications, scheduling, session control, distributed lock services, automatic leases or automatic retention cleanup. The MVP uses a local Dolt SQL Server and defers MCP, a separate Hive service and remote workspace synchronization.

## Acceptance

A usable system should demonstrate that one developer can coordinate and resume work across multiple agent sessions and projects without requiring a vendor service or hosting provider. A later session should reconstruct useful work context, and the developer should be able to review reports, briefings and material discussion outcomes from retained records. Demonstrate the same record and resumption semantics through distinct supported agent environments. Verify that ordinary Hive record operations and setup leave a synthetic non-adopting external project's tracked files, untracked files, Git configuration and hooks unchanged. Exercise explicitly opted-in local bindings and repository-specific Git identities where Git assets are involved.

Verify scoped reads and validation, machine-readable failures, guarded mutations and recovery from stale or uncertain writes. Demonstrate unique Issue numbers and at most one confirmed Claim per Issue under contention in the same authoritative workspace. Reject invalid workspace or credential selections. Verify paired Dolt and Soul Git backup and restore, v1 compatibility, and migration mapping and rollback on disposable copies. Use synthetic records and executable tests for the [v2 acceptance matrix](../architecture/dolt-mvp-contract.md#acceptance-matrix-and-evidence). Live migration and a switch in workspace authority require separate explicit operator approval.

See [Architecture](../architecture/README.md) for the adopted v2 design and v1 compatibility references. See [Development](../development/README.md) for implementation and verification procedures.
