---
description: "Use when implementing the Dolt v2 Area hierarchy, purpose-based Projects, Issue work graph and workspace storage."
---

# Dolt v2 MVP Architecture

Dolt v2 is the adopted MVP development baseline in the [#22 decision](https://github.com/mols3131d/hive-system/issues/22#issuecomment-7198810176), reconciled with the product identity, role, authority, and continuity direction in [#34](https://github.com/mols3131d/hive-system/issues/34). Implement against the [MVP contract](dolt-mvp-contract.md) and [relational schema](dolt-schema.md), preserving the pinned initial migration, stable IDs, and domain guards. Existing workspaces continue under their [pinned v1 contracts](README.md#v1-compatibility). Live migration and an authority switch require separate explicit operator approval.

## Product boundary

Hive is an agent-native, user-owned personal work management system for one individual working across projects, organizations, collaboration services, and execution environments. Keep user-owned data, local-first operation, vendor/runtime independence, optional synchronization, and non-interference with external projects. External team workspaces (such as Notion, Jira, or GitHub) retain their own canonical owners; Hive may reference external work without mirroring lifecycles, duplicating organizational data, or requiring synchronization. Use each host's native orchestration rather than building a competing orchestration engine. Hive is not a multi-user project management service, live messaging service, scheduler, IAM/sandbox, or generalized execution-log archive.

## Domain direction

| Concept | Responsibility |
| --- | --- |
| Workspace | Ownership and operating boundary; one database initially |
| Area | Arbitrary logical management and context scope |
| Project | Purpose-based collection of issues |
| Issue | Independent unit of work or resolution |
| Resource | Reference to a repository, directory, document, dataset or other asset |
| Claim | Current exclusive execution assignment at execution/work scope, not a permission grant |
| Directive | Applicable direction (including natural-language external activity policy) and provenance |
| Artifact | Intentional deliverable, evidence, decision or handoff |
| Bot (Queen) | Registered persistent Queen identity with portable `SOUL.md` and multiple independent sessions; curated Memory operations proposed deferred for MVP while preserving `bot_memories` compatibility |

Area and Sub-area are the same entity. A nullable parent reference forms a hierarchy; a stable ID survives renaming and moving. No required Area type, repository association or project lifecycle. A Project is not an Area, repository or parent Issue. It may contain issues from multiple Areas; membership does not duplicate the issue or its execution state. Closing or removing a Project must not close or delete its issues.

| Relationship | Cardinality |
| --- | --- |
| Area to child Area | 1:N |
| Area to owned Issue | 1:N (one owning Area per Issue) |
| Project to Issue | N:M |
| Area to Resource | N:M |
| Issue to Resource | N:M when necessary |
| Issue to child Issue | 1:N |
| Issue to prerequisite Issue | N:M |
| Issue to active Claim | 0:1 |

Use explicit domain tables and foreign keys, not a generic entity/attribute/value store. Resolve stable IDs separately from names, paths and external addresses. Define uniqueness, root sibling slug collisions, deletion restrictions, cross-area references and cycle rejection. Area ancestry and issue parentage do not imply execution dependencies or automatic context/authority inheritance. Validate the combined completion graph (prerequisite and parent-completion obligations) to prevent otherwise acyclic graphs from deadlocking acceptance.

## Work graph

The Issue types are epic, feature, task and bug. Use labels for research, documentation and other classification. The lifecycle uses backlog, todo, in-progress, review, done and cancelled. Preserve acceptance, verified execution stop and safe claim recovery semantics. Keep project planning fields separate from global Issue state. The [MVP contract](dolt-mvp-contract.md) defines lifecycle, ID allocation, priority, dependency outcome and parent completion semantics.

Readiness is derived: selected work, accepted prerequisites, no incompatible constraint and no active claim. Readiness is not execution authorization. A claim operation must re-evaluate authoritative state, prerequisite acceptance, assignment authority and expected revision. A stale read or duplicate request cannot establish ownership. Require evidence of former execution stop before recovery; no automatic lease reassignment in initial scope.

## Storage and runtime

In v2, Dolt owns structured collaboration records, including Area, Project, Issue, relationships, labels, Claims, and Directive/textual Artifact Markdown bodies. The initial 24-table migration also includes `bot_memories`, which is preserved unchanged for SQL and v1 migration compatibility while `#34` proposes deferring Bot-specific curated Memory operations for MVP dogfooding. Reference large or binary assets at their existing owners and keep user-owned `SOUL.md` in Git-tracked Markdown. Preserve user-owned Queen conversation history through a minimal explicit import and read path for user-selected UTF-8 Markdown stored in a separate private local data root outside Git worktrees and the Git-tracked Soul asset tree (keyed by portable workspace and Queen identity with separate sessions, excluded from context resolution and ordinary paired Dolt/Soul backups by default, with optional explicit export or backup inclusion for portability); do not add implicit host capture, raw host session reads, or a premature session table. Follow the ownership and cross-store recovery rules in the [MVP contract](dolt-mvp-contract.md). Never make DB and Markdown competing originals of the same record.

Use versioned v2 XDG configuration and data locations alongside the current v1 paths. Select the contract before workspace resolution and require explicit initialization or migration. Keep Dolt internal files and private Queen conversation files out of ordinary Git tracking. Native Git may remain for file assets and software development. Remote sync is optional and explicit; validate ordinary Git remotes for Dolt on disposable data, not an existing workspace.

Use one shared authoritative local DB per workspace for coordination. An independent clone cannot guarantee exclusive claims across replicas. Distinguish SQL transactions, version commits and remote publishing. Test contention, constraint enforcement, graph races, version-commit failure/retry, ambiguous outcomes, merge validation and recovery. Specify what a successful mutation receipt guarantees; do not infer global ownership from local commits. No unreviewed automatic conflict resolution. DB and filesystem writes need a recoverable cross-store protocol if both are involved.

Use the Rust CLI with a local Dolt SQL Server. The [MVP contract](dolt-mvp-contract.md#local-runtime-and-authority) describes the runtime boundary and conditions for revisiting alternatives. Keep DB access local by default and avoid credentials in logs or tracked configuration.

## Context, roles, and continuity

Target v2 replaces Manager–Coordinator–Executor with **Queen**, **Overlord**, and **Drone** as logical responsibilities and policy categories distinguishing three concerns: internal Hive work authority, external representation, and durable session retention. They are not mandatory process types, model configurations, or a fixed runtime hierarchy, and no request must traverse every role. Use the host's native delegation, messaging, parallel execution, and session controls; Hive owns work contracts, records, and acceptance invariants rather than a competing orchestration engine.

- **Queen:** Human-facing work management, discussion, delegation, review, and limited external representation. Multiple registered Queens are permitted in one workspace with no one-to-one Queen–Project mapping, and one Queen may run multiple concurrent independent sessions. A registered Bot in v2 is a persistent Queen identity defined by `assets/bots/<slug>/SOUL.md` (globally unique immutable slug locator, DB UUID identity). Queen identity is distinct from session identity: track execution responsibility and Claims at execution/work scope rather than treating all sessions of one Queen as one holder.
- **Overlord and Drone:** Task-scoped coordination (Overlord) and execution/verification (Drone) responsibilities. Neither requires a registered persistent Bot identity, `SOUL.md`, or durable session archival. A Queen may coordinate or execute directly without changing Queen identity.
- **External representation:** Only Queen may perform authorized external collaboration writes on the human's behalf, guided by natural-language policies through existing Directives (no new policy DSL or approval engine). External write authority must not be delegated to Overlord or Drone, which have authorized external reads only and prepare proposed external changes or drafts inside Hive for Queen review. Authorized local implementation (code edits, tests, local commits) is distinct from external publication. Separate policy from technical enforcement: natural-language policy is not a security boundary; use host/tool/account permissions and credential separation where supported, and document actual host-enforced versus procedural limits.
- **Continuity boundaries:** Distinguish three mechanisms: (1) **preserved Queen conversations** via a minimal explicit user-selected UTF-8 Markdown import and read path stored in a private local data root outside Git worktrees and Git Soul assets (keyed by portable workspace and Queen identity, keeping sessions separate, excluded from context resolution and ordinary backups by default, with optional explicit export or backup inclusion; no implicit host capture or raw host session reads); (2) **native session resume** provided by the host where available (no cross-provider internal model-state restoration required); and (3) **worker continuity** for Overlord and Drone through Issues, claims, progress/history, artifacts, reports, decisions, verification evidence, and handoffs rather than durable worker-session archival.
- **No autonomous Bot memory requirement:** Hive does not maintain autonomous AI personalities that independently learn and accumulate private memories. Preserved Queen conversations are shared collaboration history, not private Bot memory. Keep `SOUL.md`, and propose deferring Bot-specific curated Memory operations (`bot_memories` mutations, logical `memories/LATEST.md`, `memories/core/`, `memories/references/`, and acceptance `A17`) for MVP dogfooding while preserving the pinned `bot_memories` SQL table and v1 file-memory compatibility/migration prerequisites.

Optional management README/AGENTS must not enter runtime Bot context; verify host auto-discovery behavior or refuse that activation path. Make deterministic operations hook-callable: optional host lifecycle hooks invoke the same local CLI/domain contract as Skill and manual use, with read-only defaults and no generalized execution-log capture, implicit mutation, required host adapter, or automatic host configuration. Scope selection does not confer access or instruction authority. Agent/provider/harness authentication, permission enforcement, sandboxing, and process isolation remain outside Hive. Hive validates collaboration-domain scope, ownership, workflow delegation and review evidence, transitions, revisions, and storage integrity; these checks do not authenticate a caller or provide a security sandbox. See the detailed [execution defaults and decision gates](dolt-mvp-contract.md#execution-defaults-and-decision-gates). No raw session logs or private user data in public source or test fixtures.

## CLI direction

Keep machine-readable JSON, stable errors, explicit scope selection and safe mutation receipts. Candidate domains: area, project, issue, resource, artifact, directive, bot, conversation, context, validate, doctor and backup. Bot curated memory commands and remote sync are proposed deferred beyond the local MVP. Include filtered issue lists, ready/blocked, dependency traversal, guarded claim/release and context resolution. Prefer bounded, read-only SQL queries where useful; domain writes must enforce lifecycle and ownership invariants. See the [v2 command and compatibility contract](dolt-mvp-contract.md).

## Review and transition gates

The adoption decision follows independent design review and pinned Dolt/Rust experiments on Linux. The recovery fixture covers writer death before commit and after Dolt acknowledges SQL COMMIT, before the caller receives the outcome. Implemented recovery, supported hosts, compatibility and migration rehearsal remain acceptance work. [Design review (#23)](https://github.com/mols3131d/hive-system/pull/23#issuecomment-7198009441) · [Synthetic evidence (#24)](https://github.com/mols3131d/hive-system/pull/24)

1. Implement and verify the reviewed baseline in the delivery order in [#15](https://github.com/mols3131d/hive-system/issues/15). Coordinate the v2 workspace semantics, schema, CLI, templates, tests and distributable assets with their implementation. Preserve reusable work and pinned v1 compatibility.
2. Inventory legacy records read-only. Map old project/reserved-scope IDs to explicit Areas and Projects without assuming one repository equals one Area. Preserve work IDs or explicit mappings, relationships, lifecycle, claims, memory, references and evidence. Validate disposable copies and rollback before seeking live cutover approval.
3. Obtain separate explicit approval for live migration, policy-pin changes and an authority switch. Confirm old execution has stopped or been safely handed off before switching authority.

See the [relational schema](dolt-schema.md) for candidate DDL, ready queries and concurrency invariants.
