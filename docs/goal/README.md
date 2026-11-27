---
description: "Use when defining project outcomes, requirements, scope, non-goals and acceptance criteria."
---

# Goals

Hive is an agent-native, user-owned personal work management system. It enables an individual to manage and sustain their own work across projects, organizations, collaboration services, and execution environments through active collaboration with AI agents. Even when a team maintains its own Notion, Jira, or GitHub workspace, the individual keeps an independent personal workspace for their own work. AI agents are a core operating assumption: the individual delegates both work execution and work management while retaining control over direction, decisions, review, and external representation.

Dolt v2 is the adopted MVP development baseline, reconciled with the product identity, role, authority, and continuity decisions in [#34](https://github.com/mols3131d/hive-system/issues/34). Existing Markdown/Git workspaces continue under their reviewed v1 contracts.

## Outcomes

- Own the individual's personal work records, direction, artifacts, reports, decisions, verification evidence, and handoffs while minimizing human operational and cognitive overhead.
- Keep external project and team records with their canonical owners. A Hive Issue may reference an external Issue or workspace without mirroring its lifecycle, duplicating organizational data, or requiring synchronization.
- Preserve independence from any ADE, IDE, agent harness, model provider, Git host, or collaboration service. Environments may be combined or replaced without losing Hive-owned work records or preserved human-agent collaboration history. Optional integrations must not define the core contract.
- Support one active development environment per project, with multiple concurrent agent sessions and sequential, controlled environment moves; no simultaneous multi-environment development of one project is required.
- Use logical **Queen**, **Overlord**, and **Drone** roles to distinguish internal Hive work authority, external representation, and durable session retention without imposing fixed process types, model configurations, or a mandatory runtime hierarchy.
- Restrict authorized external collaboration writes on the human's behalf to **Queen**, guided by natural-language policies (preferring existing Directives, with no new policy DSL or approval engine). Limit Overlord and Drone to authorized external reads and internal drafts for Queen review; keep authorized local implementation (code edits, tests, local commits) distinct from external publication.
- Permit multiple registered Queens in one workspace without a one-to-one Queen–Project mapping, and allow multiple concurrent independent sessions per Queen. Retain portable `SOUL.md` identity in Git-tracked Markdown, and track execution claims at execution/work scope rather than conflating all sessions of one Queen.
- Preserve **Queen conversation history** in a user-controlled, readable form for **human recall** across projects and hosts, explicitly distinct from host-provided **native session resume** and **worker continuity**. Do not require durable archival of Overlord or Drone sessions; preserve all roles' work through Issues, claims, progress/history, artifacts, reports, decisions, verification evidence, and handoffs so a replacement worker can resume from records.
- Do not require autonomous Bot memory that independently learns and accumulates private AI personalities. Treat preserved Queen conversations as shared human-agent collaboration history, keep `SOUL.md`, and reassess whether Bot-specific Curated Memory is needed for MVP beyond work records and user-owned curated context.
- Keep setup, daily operation, and maintenance lightweight through local-first operation, explicit synchronization, and user-controlled backup and restoration.
- Keep Hive records and harness assets outside external projects by default. Using Hive to manage personal work must not require a project to adopt Hive, install its tooling, or change its repository.
- Use one authoritative local Dolt database per workspace for structured collaboration records and their revision history, with Git-tracked Markdown for `SOUL.md`. Give each Issue one owning Area in a recursive hierarchy, allow purpose-based Projects across Areas, and confirm Issue number allocation and exclusive Claims against the selected workspace authority.
- Minimize repeated parameters through explicit configuration and optional local bindings. Reuse existing per-repository Git identity and authentication for Git assets.
- Deliver an independent Rust CLI and Hive Skill for repeatable, safe workspace operations, using host-native orchestration rather than a competing orchestration engine. Support existing Markdown and Git workspaces under their pinned v1 contracts.
- Separate reusable system software and policies from user collaboration data. Use synthetic fixtures; no private workspace contents or history belong in the public system.

## Non-goals

Hive is not a multi-user collaboration platform, team-wide project management replacement, competing agent orchestration engine, model-provider router, or identity and access management (IAM) / sandbox / credential-broker service. Hive is not a mandatory IDE or ADE, another project specification owner, a personal or environment-specific configuration manager, or an automatic synchronization service. Hive does not own IDE configuration, agent-vendor or model-provider integrations, or an external project's development harness. It is not an instant messaging or real-time communication system between agent sessions, nor a generalized execution-log or worker-session archive. It does not implement message delivery guarantees, automatic notifications, scheduling, session control, distributed lock services, automatic leases, automatic retention cleanup, or autonomous Bot memory learning. The MVP uses a local Dolt SQL Server and defers MCP, a separate Hive service, remote workspace synchronization, and any wholesale Dolt redesign.

## Acceptance

A usable system must demonstrate that one individual can coordinate, review, and resume work across multiple Queens, sessions, projects, and execution environments without requiring a vendor service or hosting provider. Verify scoped reads and validation, machine-readable failures, guarded mutations, deterministic domain guards, and recovery from stale or uncertain writes. Demonstrate unique Issue numbers and at most one confirmed Claim per Issue under contention in the same authoritative workspace, and reject invalid workspace or credential selections. Verify that ordinary Hive record operations and setup leave a synthetic non-adopting external project's tracked files, untracked files, Git configuration, and hooks unchanged. Exercise explicitly opted-in local bindings and repository-specific Git identities where Git assets are involved.

For minimal MVP dogfooding, demonstrate: returning to a prior preserved Queen conversation in user-owned readable form; resuming Overlord or Drone work from Hive records without the original worker session; operating multiple concurrent Queen sessions while keeping claims bound to execution scope; preparing proposed external changes internally and publishing externally only through authorized Queen action; and switching execution environments without losing Hive-owned work. Document which role restrictions are technically enforced in the selected host versus procedural; never claim enforcement without evidence.

Verify paired Dolt and Soul Git backup and restore, v1 compatibility, and migration mapping and rollback on disposable copies. Use synthetic records and executable tests for the [v2 acceptance matrix](../architecture/dolt-mvp-contract.md#acceptance-matrix-and-evidence). Live migration and a switch in workspace authority require separate explicit operator approval.

See [Architecture](../architecture/README.md) for the adopted v2 design and v1 compatibility references. See [Development](../development/README.md) for implementation and verification procedures.
