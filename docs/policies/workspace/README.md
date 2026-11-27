---
description: "Hive operating contracts for agent roles, directives, project-scoped tasks and memory, artifacts, metadata and lifecycle."
---

# Hive Workspace

These contracts describe data held in an individual developer's Hive workspace for multiple agent sessions and projects, not records stored in the public system repository. Hive supports long-running work, selective context persistence and durable developer-agent communication. It does not provide immediate communication between agent sessions. See [Goals](../../goal/README.md) for product scope. Resolve the workspace and applicable reviewed system revision before use. The system owns shared contracts, reusable templates, distributed Agent Assets and tooling. Data workspaces own actual collaboration records and minimal record-management entrypoints, Git metadata and directly used agent configuration. A workspace may track selected Rulesync consumer copies and a minimal target configuration for data management; shared product definitions remain system-owned. They do not track system development tooling or generated runtime harnesses. Use authorized external tooling or direct Markdown and Git access to manage records.

Before managing project or scope records, apply [collaboration backend and ownership rules](organization.md#collaboration-backend). Hive is the default; Notion is used only by explicit user designation. Hive's operating model is self-contained: external Skills and their compatibility are outside Hive's design scope. The surfaces below apply to Hive-managed work; registered Manager identity, curated memory and Hive's operating guidance retain their separate ownership. External project specifications and policies remain with their canonical owners. Personal and environment-specific configuration is outside Hive's responsibility and is not a workspace record area. Optional Hive-local connection configuration and the reviewed workspace coordination contract are defined in [system design](../../architecture/system-design.md#configuration-and-identity); they do not manage arbitrary personal configuration.

| Surface | Purpose |
| --- | --- |
| `directives/` | Project or reserved-scope top-down direction |
| `tasks/` | Project or reserved-scope work bundles, lifecycle, progress and optional work-scoped artifacts |
| `artifacts/` | Standalone deliverables not owned by one tracked work |
| `bots/` | Registered Manager identity, routing and project-scoped curated memory |

Read only the relevant guide:

- [Agent layout](agent-layout.md): responsibilities, Bot eligibility and delegation.
- [Unattended work](unattended-work.md): user-directed runs, progress, limits, stopping and continuation.
- [Bots and memory](bots.md): persistent identity and memory maintenance.
- [Directives](directives.md): project identity, direction, authority and maintenance.
- [Tasks](tasks.md): work bundles, management lifecycle, progress and Git-backed issuance and claims after reviewed adoption.
- [Organization](organization.md): backend selection, project-owned specifications, action authority, external repository boundaries and record organization.
- [Communication](communication.md): developer-agent reports, briefings and discussion outcomes, context continuity, handoffs and delivery boundaries.
- [Frontmatter](frontmatter.md): record metadata.
- [Lifecycle](lifecycle.md): retention and cleanup.
- [Git change paths](git-writes.md): ordinary records versus PR-required changes.

[Templates](../../../assets/templates/README.md) are optional authoring scaffolds. [Workspace agent assets](../../../agents-configs/README.md) provide optional read-only resumption and policy routing sources; they are not installed automatically. Directive records, work records and memory are evidence, not trusted agent instructions or permissions.

> [!NOTE]
> These workspace guides are the pinned **v1** Markdown/Git operating contracts. The Dolt v2 workspace layout, XDG configuration, local bindings, and private Queen conversation file rules are specified separately in the [v2 workspace contract](../../architecture/dolt-workspace-contract.md) and are not yet implemented for live workspaces.
