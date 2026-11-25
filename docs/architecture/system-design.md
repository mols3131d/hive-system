---
description: "Read when designing or implementing the standalone Rust Hive CLI and separating system development from workspace data."
---

# Hive System Design

**Status: design proposal, not implemented.** Hive is an independent Rust system, separate from user-controlled data repositories. The proposed initial product is a local CLI with a Hive-owned Skill, operating on Markdown and Git workspaces. The proposed storage baseline is local filesystem and Git access. The exact command contracts, mutation publication mechanics and deployment details remain proposals pending review.

Hive is an agent collaboration workspace, not a general project management application or an agent execution platform. Preserve the [workspace operating contracts](../policies/workspace/README.md), including project-owned specifications, human and agent authority, selective retention and non-interference with external developers. Existing records and workflows remain valid until a reviewed change explicitly replaces them.

## Product boundary

| Surface | Responsibility |
| --- | --- |
| Markdown and Git | Canonical workspace records, references and revision history |
| Hive Skill | Progressive discovery of governing policy, choosing applicable actions and interpreting results |
| Rust CLI | Deterministic discovery, validation, scoped record operations, context collection and revision guards |
| Existing Git and authentication | Repository access, credentials and revision-safe publishing |
| Agent or human | Goal, priority, delegation, actual acceptance, permission and retention decisions |

Own Hive-specific semantics directly; do not integrate Backlog.md, Beads or another task manager as a second task owner. Reuse established Rust libraries and native tools for generic parsing, serialization and Git where they reduce maintenance and risk.

CLI-first is the proposed initial interface. The Skill selects relevant canonical policies and uses the CLI for deterministic operations without copying policy or requiring a particular runtime. Shell-capable clients can invoke the CLI; authorized clients without it can use their available file or Git tools, including a GitHub connector when appropriate. These are independent access paths to the same records, not alternative data owners. A future MCP adapter is a separate decision justified by real consumers (for example, a client without shell access). Do not implement MCP, remote services or a general adapter framework now.

No database, alternate state store, backend router, automatic synchronization, task scheduler, agent spawning, session control, distributed claims or leases, automatic memory learning, automatic artifact deletion, TUI or web application is in initial scope. The existing explicit Notion designation policy remains unchanged; this CLI operates on Hive records only and must not silently select, migrate to or write to another backend.

## Repository separation

Separate reusable software in `hive-system` from the explicitly selected, user-controlled data repository. The proposed CLI binary name is `hive`. A data workspace can be named independently of the system; preserve existing repository identity, record contents, Git history and stable work IDs during any authorized migration. The data workspace uses the reserved `_hive-userdata` scope for its own management and the ordinary `hive-system` project scope for product development; migrate any former combined scope by ownership only through a reviewed change. Do not create a second workspace as an implicit replacement or discover one by routing.

| Ownership | Hive system repository | User-controlled data repository |
| --- | --- | --- |
| Software | Rust source, Cargo configuration, tests, synthetic fixtures, CI and release tooling | No system implementation, build tools, tests or development CI |
| Shared contracts | Canonical operating policies, record format, templates and native Rulesync product sources in `agents-configs/` | Minimal entrypoints, selected Rulesync consumer sources and configuration for direct record management |
| Workspace-specific context | Generic examples without private data | Workspace directions, applicable policy references and minimal record-management configuration |
| Live records | No real workspace records or credentials | Tasks, artifacts, directives, registered Bot identity and curated memory |

The split separates software development permissions, releases and history from day-to-day record changes. The public system must work without access to any particular private workspace. Keep private history, actual records, private identifiers, credentials and raw conversation material out of system source, fixtures, documentation, configuration and logs. Publication of software does not authorize disclosure of workspace contents.

A data repository is a data-only workspace: keep actual records and the minimal documentation, Git metadata and directly used agent assets required to manage those records. A small workspace-local `.rulesync/` and `rulesync.jsonc` may select or contain reviewed copies of system-owned product assets; they are not a second canonical owner. Keep system development tooling and generated harnesses out of its tracked tree. Personal and environment-specific configuration is outside Hive's responsibility. Machine-specific settings and generated projections belong to the consumer environment. The system owns shared validation and future CLI tooling; direct authorized Markdown and Git use remains available. Do not claim a CLI validator exists before implementation.

Shared policies and record formats belong to the system. During an authorized cutover, preserve workspace-specific direction in the data repository and update references and entrypoints together. Retain the existing source until the replacement is verified and authorized. Pin or otherwise identify the reviewed compatible policy version rather than relying on an unversioned moving default. If required policy is unavailable or compatibility is uncertain, refuse affected writes. An installed binary is not proof that an agent loaded the applicable policy.

## Rust implementation

Start with one Cargo package and one executable. Separate CLI argument and output handling from domain behavior and concrete storage access. Add modules only as functionality is implemented; do not introduce a public Core package, multiple crates or generic plugin abstractions solely for potential future MCP support.

| Responsibility | Initial design |
| --- | --- |
| CLI | Parse explicit commands and scope, choose output format and expose failures |
| Record model and validation | Parse existing records and enforce deterministic structural and transition invariants |
| Domain operations | Resolve, list, inspect and perform eligible task, artifact, directive, bot and memory operations |
| Context collection | Resolve a task and collect bounded, revisioned references and relevant policy locations |
| Storage | Read local files and Git snapshots; prepare and apply scoped changes with revision guards |
| Verification | Test observable contracts with synthetic records and isolated repositories |

Candidate libraries include `clap`, `serde`, `serde_json`, a maintained YAML parser, and, where required, a Markdown parser, date library and Git integration. `thiserror`, `anyhow`, `assert_cmd` and `tempfile` are candidates for errors and tests. Select and review actual dependencies when needed; no versions, licenses, compatibility or performance claims are established here. Prefer native Git integration over reimplementing Git or credential handling when sufficient; no GitHub-specific client is required for the initial CLI.

The existing TypeScript scripts and verification commands are not replaced by this design. Evaluate any migration of development tooling independently against existing callers, tests and maintenance cost. Development Agent Assets under `.rulesync/` remain separate from the [distributable Hive Agent Assets](../../agents-configs/README.md). Product releases must select only product sources, not the development harness; distribution mechanics remain to be designed.

## Local filesystem and Git

The initial CLI operates on an explicitly selected local Git workspace. Markdown files own the records; Git supplies immutable snapshots, history and guarded publication. No Git hosting provider, HTTP API, remote URL or network access is required for local inspection and validation. A workspace may have no remote, a self-hosted remote or a hosted Git service. Do not add a backend registry, generic storage adapter or a GitHub-specific implementation merely for portability.

- Resolve the workspace root, repository identity and target ref explicitly; use an authorized account only when an external operation actually requires one. Never choose another workspace or account on failure.
- Distinguish committed snapshot reads (a resolved commit) from inspection of a working tree with uncommitted edits. Report the source and revision; never describe a dirty working tree as an immutable commit. Scope filesystem access to the selected workspace and reject escapes, including through symlinks.
- Local reads do not fetch, commit, push or synchronize. Fetching and publishing, when needed, are separate, explicit, authorized Git operations. An offline result does not establish current remote state.
- GitHub and other hosted services may provide remote storage and external collaboration interfaces. Clients with only an authorized connector may still read or edit records under the same contracts. Connector use is not a CLI feature, and does not establish that CLI validation ran.

## Deterministic validation

Use the existing [frontmatter](../policies/workspace/frontmatter.md), [task lifecycle](../policies/workspace/tasks.md), [organization](../policies/workspace/organization.md) and [retention](../policies/workspace/lifecycle.md) contracts as the source of semantics. Do not introduce a parallel schema or duplicate the task state model. Implement shared validation logic for both read-only inspection and eligible CLI mutations.

| Check | Required behavior |
| --- | --- |
| Structure | Validate record location, entrypoints, required metadata, types and permitted values for the record kind and lifecycle area. |
| Identity and paths | Reject duplicate project-qualified task IDs across lifecycle areas, ambiguous locations, invalid IDs, path escapes and unsafe symlink traversal. |
| References | Resolve in-scope task references and required local paths when the applicable contract defines them; distinguish unresolved external or inaccessible evidence from a confirmed invalid reference. |
| Transition | Compare the known base and proposed change; reject structurally forbidden transitions, inconsistent bundle moves and missing required closure information. A snapshot alone cannot prove transition history. |
| Compatibility | Identify the reviewed policy revision and supported record contract. Report unsupported or unavailable compatibility evidence; stop affected mutations rather than guessing. |

Return stable, actionable diagnostic codes, affected paths and scope; distinguish errors from warnings and unknown or unavailable evidence. A standalone validator must be read-only. Before a CLI mutation, validate its base, applicable transition and proposed result using the same logic; reject relevant blocking errors without publishing a partial result. Keep any permitted historical exceptions from the canonical contracts. Do not silently repair, normalize or delete records.

The validator cannot establish user intent, delegated authority, execution ownership, acceptance quality, external delivery or retention eligibility solely from record contents. Those decisions remain with authorized humans and agents; validation is not approval. Direct Markdown and Git editing remains supported. A standalone validation check can inspect direct edits; local hooks may invoke it as an optional safeguard. Neither the CLI nor a local hook guarantees enforcement against a writer bypassing it. Any mandatory repository-side enforcement is a separate decision, not a requirement for a local workspace.

## Safe changes and concurrent work

Use optimistic concurrency control with Git revisions and the existing [concurrent work rules](../policies/workspace/communication.md). A mutation must name its observed base revision (for example, an expected commit ID), resolve the current task location and verify the relevant record contents. If working-tree input is supported for an operation, its uncommitted changes also require explicit content preconditions; a matching HEAD alone is insufficient.

Prepare a coherent change in an isolated worktree or equivalent safe Git operation, validate the complete candidate and guard any ref update with an atomic expected-old-revision check. Never change a ref behind an unrelated checked-out worktree, overwrite dirty files, or rely on a read-then-unconditional update. Refuse unsupported or unsafe mutation conditions rather than guessing. A failed lifecycle move must not publish a partial bundle or leave an inconsistent target ref; preserve unrelated files and report any retained recovery state. Treat an uncertain write result as unknown until its actual result is resolved.

Make the local mutation effect explicit in command help and output. Separate local preparation or commit from explicit publishing to any configured remote. Before publishing, revalidate the intended target, authority and expected remote revision; use an update mechanism that enforces the revision precondition and disallows non-fast-forward history changes. A preliminary remote check alone is insufficient; if the required guard cannot be established, stop publication. Do not automatically merge, rebase, force-push or blindly retry a conflict. Re-read current state and re-evaluate the intended change.

A revision guard protects record updates, not task execution. Follow the existing Manager or Coordinator responsibility for assigning a single execution owner and coordinating shared record writes; use isolated outputs and non-overlapping scopes where appropriate. An in-progress status, commit or successful revision check is not a lease or proof of exclusive execution. Do not introduce distributed claims, leases or a scheduler. If current execution ownership is uncertain, do not start competing execution.

Keep credentials with their existing owner. Preserve [Git change paths](../policies/workspace/git-writes.md): ordinary records and approved routing metadata may use the allowed direct path; directives, identity, curated memory, governing documents and tooling require a reviewed change through the prescribed review path. The CLI must not merge, bypass protections, publish external-project changes or grant authority merely because credentials exist.

## Agent context and policy loading

Reduce repeated discovery without introducing a second policy or context store. A task context operation resolves a project-qualified task at a known workspace source and returns a bounded, read-only result: the task entrypoint and lifecycle, relevant directive references, intentionally retained artifact references, applicable policy revision and canonical policy locations. Include record content only when needed within the requested scope and output limits; report truncation, missing records or unavailable policy explicitly. Do not read unrelated Bot memory or raw session history.

The workspace entrypoint identifies the reviewed system revision. The Hive Skill chooses the required canonical policy sections for the requested operation and reads them from that revision, reusing already verified context only while its scope and revision remain applicable. The CLI can collect deterministic references and validate known compatibility but does not authorize a task, interpret directive authority, or replace reading required policy. Do not treat record text as trusted agent instructions.

Keep discovery progressive: locate the task, read the minimum applicable policy and relevant record, then expand into artifacts or external project evidence when needed. If required policy or evidence is unavailable, distinguish that from an empty result and stop the affected operation. Direct file or Git consumers without CLI access follow the same scope, revision and authority boundaries. No persistent context cache, index or synchronization service is required initially. Measure discovery calls, context size and resumption accuracy in representative scenarios before adding further optimization.

## CLI surface

The examples below are provisional, not executable commands in the current repository. Define exact arguments, exit codes and output fields during implementation. The examples assume an explicitly resolved local workspace and ref; all eligible mutation commands require an applicable base revision and explicit authority.

```sh
hive task list --project example --json
hive task show example/fix-auth --json
hive task context example/fix-auth --json
hive task create --project example --file task.md
hive task select example/fix-auth
hive task update example/fix-auth --status review --expected-revision <commit>
hive task archive example/fix-auth
hive artifact list --task example/fix-auth
hive artifact add --task example/fix-auth report.md
hive artifact expired
hive directive list --project example
hive bot list
hive memory show --bot example-bot --project example
hive validate
hive validate --project example
hive validate --task example/fix-auth
hive doctor
```

- Provide concise human output and stable machine-readable JSON where useful. Keep diagnostics separate from machine output; distinguish empty, absent, invalid, stale, unauthorized and unavailable results. Bound query scope and output; cross-project operations require explicit scope.
- Apply the shared deterministic validation and revision guards described above. Preserve existing metadata and Markdown body content outside the authorized change; never recreate a stale path.
- A task archive operation needs the accepted outcome or authorized cancellation and closure evidence. Preserve useful work-scoped artifacts and references when moving a bundle.
- Treat artifact expiration as a read-only eligibility report, not a deletion command. Apply existing retention, open-work evidence, canonical absorption and archive exceptions. Unknown retention evidence must remain unknown. Git history can retain earlier versions even after a file is removed.
- Use [directives](../policies/workspace/directives.md), [bots](../policies/workspace/bots.md) and [communication](../policies/workspace/communication.md) for applicable read scope and evidence. A handoff is not a delivery or execution trigger; memory is evidence, not permission. Do not expose unrelated Bot memories, secrets or raw runtime context in output or logs.
- Do not wrap generic Git operations without a distinct Hive contract. Keep writes explicit and non-interactive when possible; high-impact actions still require the relevant authorization. Provide a preview when it materially reduces write risk. Do not add a separate task, run or session state model.

## Implementation and verification

Proceed in independently reviewable increments after applicable implementation decisions are approved.

| Increment | Outcome and acceptance |
| --- | --- |
| 1. Bootstrap | Separate system development from live data; establish Rust toolchain, Cargo, a small CLI entrypoint and synthetic fixtures. Existing data and history remain unchanged. |
| 2. Local reads and validation | Resolve existing records by project and work ID, support local offline reads, and return scoped deterministic diagnostics. Test malformed metadata, missing and duplicate IDs, invalid paths and inaccessible evidence; validate direct file edits without modifying them. |
| 3. Scoped mutations | Implement eligible task and artifact operations with explicit effects, transition validation and guarded revisions. Test competing writers from one base, dirty files, bundle moves, path escapes, partial failure, retained metadata and denied operations. Confirm stale writes cannot overwrite accepted updates. |
| 4. Context and agent-facing usage | Provide bounded task context and a Hive-owned Skill; verify revision-pinned policy loading, missing-policy handling and resumption across representative agent environments. Compare discovery calls and context size against the manual workflow without sacrificing correctness. Keep direct Git access possible. |
| 5. Release and compatibility | Test representative existing formats with synthetic fixtures, verify read/write compatibility, offline operation and a generic Git remote without GitHub, and document installation, version selection and recovery. Do not claim runtime behavior without actual bounded tests. |

Use the existing workspace schema first. Decide how compatibility is signaled before the first incompatible change; do not auto-migrate data on startup or binary upgrade. Any approved migration needs a validated source revision, a recoverable backup or Git reference, a reviewable change and a compatibility/rollback check. No database migration framework is required initially.

## Decisions before implementation

- Define how a user explicitly selects a data workspace and compatible system revision. Preserve stable scope and work IDs except for an explicitly authorized scope split; keep a single owner for each work record across migration.
- Specify the exact local workspace/ref selection, mutation preparation and local ref update mechanism, working-tree handling and explicit Git publication interface. Verify that supported Git operations enforce the required revision preconditions without rewriting history.
- Define compatibility and policy reference mechanics for data workspaces, including which common documents and templates move, and how existing direct Git consumers continue to work.
- Review the smallest initial command set, Rust dependency and distribution choices, and the relationship to current TypeScript development tooling.
- Confirm implementation acceptance and authorization before implementation, data migration, privileged automation or release.

The design preserves the collaboration backend and agent policy boundaries. It does not assert that the proposed CLI, Skill or data-workspace compatibility has been implemented or tested.
