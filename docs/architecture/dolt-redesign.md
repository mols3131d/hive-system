---
description: "Use when implementing the Dolt v2 Area hierarchy, purpose-based Projects, Issue work graph and workspace storage."
---

# Dolt v2 MVP Architecture

Dolt v2 is the adopted MVP development baseline in the [#22 decision](https://github.com/mols3131d/hive-system/issues/22#issuecomment-7198810176). Implement against the [MVP contract](dolt-mvp-contract.md) and [relational schema](dolt-schema.md). Existing workspaces continue under their [pinned v1 contracts](README.md#v1-compatibility). Live migration and an authority switch require separate explicit operator approval.

## Product boundary

Hive is a local-first collaboration and context system for one developer and multiple agent sessions. Keep user-owned data, selective durable context, vendor/runtime independence, optional synchronization and non-interference with external projects. Hive is not a general multi-user project management service, live messaging service, scheduler or execution harness.

## Domain direction

| Concept | Responsibility |
| --- | --- |
| Workspace | Ownership and operating boundary; one database initially |
| Area | Arbitrary logical management and context scope |
| Project | Purpose-based collection of issues |
| Issue | Independent unit of work or resolution |
| Resource | Reference to a repository, directory, document, dataset or other asset |
| Claim | Current exclusive execution assignment, not a permission grant |
| Directive | Applicable direction and provenance |
| Artifact | Intentional deliverable, evidence, decision or handoff |
| Bot | Registered persistent Manager agent identity with Soul and selectively recalled memory |

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

In v2, Dolt owns structured collaboration records, including Area, Project, Issue, relationships, labels, Claims and curated Bot Memory. Dolt also owns Directive and textual Artifact Markdown bodies. Reference large or binary assets at their existing owners and keep user-owned `SOUL.md` in Git-tracked Markdown. Follow the ownership and cross-store recovery rules in the [MVP contract](dolt-mvp-contract.md). Never make DB and Markdown competing originals of the same record.

Use versioned v2 XDG configuration and data locations alongside the current v1 paths. Select the contract before workspace resolution and require explicit initialization or migration. Keep Dolt internal files out of ordinary Git tracking. Native Git may remain for file assets and software development. Remote sync is optional and explicit; validate ordinary Git remotes for Dolt on disposable data, not an existing workspace.

Use one shared authoritative local DB per workspace for coordination. An independent clone cannot guarantee exclusive claims across replicas. Distinguish SQL transactions, version commits and remote publishing. Test contention, constraint enforcement, graph races, version-commit failure/retry, ambiguous outcomes, merge validation and recovery. Specify what a successful mutation receipt guarantees; do not infer global ownership from local commits. No unreviewed automatic conflict resolution. DB and filesystem writes need a recoverable cross-store protocol if both are involved.

Use the Rust CLI with a local Dolt SQL Server. The [MVP contract](dolt-mvp-contract.md#local-runtime-and-authority) describes the runtime boundary and conditions for revisiting alternatives. Keep DB access local by default and avoid credentials in logs or tracked configuration.

## Context model

Retain Manager, Coordinator and Executor responsibilities. Bot is a persistent Manager agent by definition, not a configurable role. Coordinator and Executor are task-scoped responsibilities without independent Bot identities, Soul or memory. A Manager may coordinate or execute directly without changing Bot identity. The Soul asset is `assets/bots/<slug>/SOUL.md`; the globally unique immutable Bot slug is a locator and the DB UUID is identity. Memory lives in scoped Dolt records. The logical Memory namespace relative to a Bot is `memories/LATEST.md`, `memories/core/` and `memories/references/`. Latest is the compact recent-memory view; durable memory is divided into core (default recall) and reference (explicit recall). Inject identity and scoped latest/core by default within context limits; retrieve reference bodies explicitly. These paths are logical names for Dolt-backed content, not additional canonical files, required filesystem directories or automatic exports. A Bot may curate its own latest, core and reference records under explicit verified user delegation without per-edit approval. Hive deterministically checks scope, size, keys, authorization and revisions and renders bounded views; the Bot decides meaning and value. No automatic semantic promotion, eviction, learning or background maintenance. Make deterministic operations hook-callable: optional host lifecycle hooks invoke the same local CLI/domain contract as Skill and manual use. Hooks are thin triggers, not an execution harness or permission source; no transcript capture, implicit mutation, required host adapter or automatic host configuration. Optional management README/AGENTS must not enter runtime Bot context; test host auto-loading. Registration never selects a role; a role describes execution responsibility, not permission. Sessions remain host-owned. Scope selection does not confer access or instruction authority. Agent/provider/harness authentication, permission enforcement, sandboxing and process isolation are outside Hive. Hive validates collaboration-domain scope, ownership, workflow delegation and review evidence, transitions, revisions and storage integrity; these checks do not authenticate a caller or provide a security sandbox. Consume explicit operator/host-provided workflow evidence without introducing an identity provider, independent security grant registry or required security model. See the detailed [execution defaults and decision gates](dolt-mvp-contract.md#execution-defaults-and-decision-gates). No raw session logs or private user data in public source or test fixtures.

## CLI direction

Keep machine-readable JSON, stable errors, explicit scope selection and safe mutation receipts. Candidate domains: area, project, issue, resource, artifact, directive, bot, memory, context, validate, doctor and backup. Remote sync is deferred beyond the local MVP. Include filtered issue lists, ready/blocked, dependency traversal, guarded claim/release and context resolution. Prefer bounded, read-only SQL queries where useful; domain writes must enforce lifecycle and ownership invariants. See the [v2 command and compatibility contract](dolt-mvp-contract.md).

## Review and transition gates

The adoption decision follows independent design review and pinned Dolt/Rust experiments on Linux. The recovery fixture covers writer death before commit and after Dolt acknowledges SQL COMMIT, before the caller receives the outcome. Implemented recovery, supported hosts, compatibility and migration rehearsal remain acceptance work. [Design review (#23)](https://github.com/mols3131d/hive-system/pull/23#issuecomment-7198009441) · [Synthetic evidence (#24)](https://github.com/mols3131d/hive-system/pull/24)

1. Implement and verify the reviewed baseline in the delivery order in [#15](https://github.com/mols3131d/hive-system/issues/15). Coordinate the v2 workspace semantics, schema, CLI, templates, tests and distributable assets with their implementation. Preserve reusable work and pinned v1 compatibility.
2. Inventory legacy records read-only. Map old project/reserved-scope IDs to explicit Areas and Projects without assuming one repository equals one Area. Preserve work IDs or explicit mappings, relationships, lifecycle, claims, memory, references and evidence. Validate disposable copies and rollback before seeking live cutover approval.
3. Obtain separate explicit approval for live migration, policy-pin changes and an authority switch. Confirm old execution has stopped or been safely handed off before switching authority.

See the [relational schema](dolt-schema.md) for candidate DDL, ready queries and concurrency invariants.
