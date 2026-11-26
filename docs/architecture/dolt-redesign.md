---
description: "Review the proposed Dolt-backed architecture, Area hierarchy, purpose-based Projects and issue work graph before extending the MVP."
---

# Proposed MVP Architecture Redesign

Status: proposed, tracked in [#22](https://github.com/mols3131d/hive-system/issues/22). This is a design baseline for review and a technical spike, not an adopted workspace policy, implementation contract, or migration authorization. The existing [system design](system-design.md), [CLI contract](cli-contract.md), and [workspace policies](../policies/workspace/README.md) remain authoritative for existing workspaces until reviewed replacement and explicit adoption.

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
| Bot | Persistent identity with fixed role and selectively recalled memory |

Area and Sub-area are the same entity. A nullable parent reference forms a hierarchy; a stable ID survives renaming and moving. No required Area type, repository association or project lifecycle. A Project is not an Area, repository or parent Issue. It may contain issues from multiple Areas; membership does not duplicate the issue or its execution state. Closing or removing a Project must not close or delete its issues.

Proposed relationships:

| Relationship | Cardinality |
| --- | --- |
| Area to child Area | 1:N |
| Area to owned Issue | 1:N; required vs optional ownership to decide |
| Project to Issue | N:M |
| Area to Resource | N:M |
| Issue to Resource | N:M when necessary |
| Issue to child Issue | 1:N |
| Issue to prerequisite Issue | N:M |
| Issue to active Claim | 0:1 |

Use explicit domain tables and foreign keys, not a generic entity/attribute/value store. Resolve stable IDs separately from names, paths and external addresses. Define uniqueness, root sibling slug collisions, deletion restrictions, cross-area references and cycle rejection. Area ancestry and issue parentage do not imply execution dependencies or automatic context/authority inheritance.

## Work graph

Proposed issue types: epic, feature, task, bug. Use labels for research, documentation and other classification. Candidate lifecycle: backlog, todo, in-progress, review, done, cancelled. Preserve acceptance, verified execution stop and safe claim recovery semantics. Keep project planning fields separate from global issue state. Decide exact lifecycle, ID allocation, priority, dependency outcome and parent completion semantics before final DDL.

Readiness is derived: selected work, accepted prerequisites, no incompatible constraint and no active claim. Readiness is not execution authorization. A claim operation must re-evaluate authoritative state, prerequisite acceptance, assignment authority and expected revision. A stale read or duplicate request cannot establish ownership. Require evidence of former execution stop before recovery; no automatic lease reassignment in initial scope.

## Storage and runtime

Dolt is the proposed canonical store for structured collaboration records, replacing native Git-tracked task bundles and frontmatter as the structured storage model. Area, Project, Issue, relationships, labels and claims are relational records. Directive and textual Artifact Markdown bodies are DB candidates; binary/large assets and Bot context remain file candidates. Decide the canonical ownership and reference integrity boundary before implementation. Never make DB and Markdown competing originals of the same record.

Use XDG configuration and data locations; preserve explicit target resolution and no implicit initialization or migration. Keep Dolt internal files out of ordinary Git tracking. Native Git may remain for file assets and software development. Remote sync is optional and explicit; validate ordinary Git remotes for Dolt on disposable data, not an existing workspace.

Prefer a shared authoritative local DB for coordination. An independent clone cannot guarantee exclusive claims across replicas. Distinguish SQL transactions, version commits and remote publishing. Test contention, constraint enforcement, graph races, version-commit failure/retry, ambiguous outcomes, merge validation and recovery. Specify what a successful mutation receipt guarantees; do not infer global ownership from local commits. No unreviewed automatic conflict resolution. DB and filesystem writes need a recoverable cross-store protocol if both are involved.

Retain Rust as a candidate and compare a local Dolt SQL Server integration, CLI subprocess integration and Go embedded integration using measured synthetic probes. No language switch or daemon assumption before review. Keep DB access local by default and avoid credentials in logs or tracked configuration.

## Context model

Retain Manager, Coordinator and Executor responsibilities. Bot role is fixed; proposed paths are `bots/<role>/<bot-id>/SOUL.md`, `MEMORY.md`, and `memory/<area-id>/`. Inject identity, latest curated memory and explicitly selected long-term context only. Optional management README/AGENTS must not enter runtime Bot context; test host auto-loading. Existing Manager-only registration remains until explicitly changed. Scope selection does not confer access or instruction authority. No raw session logs or private user data in public source or test fixtures.

## CLI direction

Keep machine-readable JSON, stable errors, explicit scope selection and safe mutation receipts. Candidate domains: area, project, issue, resource, artifact, directive, bot, memory, context, validate, doctor and sync. Include filtered issue lists, ready/blocked, dependency traversal, guarded claim/release and context resolution. Prefer bounded, read-only SQL queries where useful; domain writes must enforce lifecycle and ownership invariants. Exact commands, JSON compatibility and schema versions remain to be specified.

## Review and transition gates

1. Confirm domain semantics and cardinalities, lifecycle, stable IDs and schema ownership.
2. Run a synthetic Dolt spike: installation/offline, SQL connectivity, FK/CHECK, concurrent claims and graph writes, rollback, version history and failure recovery, remote backup/restore and conflicting changes. Record versions and actual results.
3. Decide language/runtime, canonical DB/file boundary, synchronization and migration protocol.
4. Revise product acceptance, architecture, workspace semantics, schema, CLI, templates, tests and distributable assets together. Reconcile #9–#15, #18, #20 and PRs #19/#21; preserve independent work. Do not treat existing file-based implementation as the new contract.
5. Inventory legacy records read-only. Map old project/reserved-scope IDs to explicit Areas and Projects without assuming one repository equals one Area. Preserve work IDs or explicit mappings, relationships, lifecycle, claims, memory, references and evidence. Validate disposable copies and rollback; do not migrate live data or change policy pins until authorized.
6. Implement and verify the reviewed baseline. Confirm old execution has stopped or been safely handed off before switching authority.

No runtime, concurrency, migration or remote verification is claimed by this document.

See the [proposed relational schema](dolt-schema.md) for candidate DDL, ready queries and concurrency invariants.
