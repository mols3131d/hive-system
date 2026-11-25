---
description: "Read when designing or implementing the standalone Rust Hive CLI and separating system development from workspace data."
---

# Hive System Design

**Status: design proposal, not implemented.** Hive is an independent Rust system, separate from user-controlled data repositories. The proposed initial product is a local CLI with a Hive-owned Skill, operating on Markdown and Git workspaces. The storage access method, command contracts and deployment details remain proposals pending review.

Hive is an agent collaboration workspace, not a general project management application or an agent execution platform. Preserve the [workspace operating contracts](../policies/workspace/README.md), including project-owned specifications, human and agent authority, selective retention and non-interference with external developers. Existing records and workflows remain valid until a reviewed change explicitly replaces them.

## Product boundary

| Surface | Responsibility |
| --- | --- |
| Markdown and Git | Canonical workspace records, references and revision history |
| Hive Skill | Progressive discovery of governing policy, choosing applicable actions and interpreting results |
| Rust CLI | Deterministic discovery, parsing, validation, scoped record operations and useful diagnostics |
| Existing Git and authentication | Repository access, credentials and revision-safe publishing |
| Agent or human | Goal, priority, delegation, actual acceptance, permission and retention decisions |

Own Hive-specific semantics directly; do not integrate Backlog.md, Beads or another task manager as a second task owner. Reuse established Rust libraries and native tools for generic parsing, serialization, Git and HTTP where they reduce maintenance and risk.

CLI-first is the proposed initial interface. The Skill links to canonical policies and provides usage guidance rather than copying policies or requiring a particular runtime. Shell-capable clients can invoke the CLI; authorized clients without it can still use existing GitHub tools and Markdown. A future MCP adapter is a separate decision justified by real consumers (for example, a client without Shell access). Do not implement MCP, remote services or a general adapter framework now.

No database, alternate state store, backend router, automatic synchronization, task scheduler, agent spawning, session control, distributed claims or leases, automatic memory learning, automatic artifact deletion, TUI or web application is in initial scope. The existing explicit Notion designation policy remains unchanged; this CLI operates on Hive records only and must not silently select, migrate to or write to another backend.

## Repository separation

Separate reusable software in `hive-system` from the explicitly selected, user-controlled data repository. The proposed CLI binary name is `hive`. A data workspace can be named independently of the system; preserve existing repository identity, record paths, Git history and stable scope IDs during any authorized migration. Do not create a second workspace as an implicit replacement or discover one by routing.

| Ownership | Hive system repository | User-controlled data repository |
| --- | --- | --- |
| Software | Rust source, Cargo configuration, tests, synthetic fixtures, CI and release tooling | No system implementation, build tools, tests or development CI |
| Shared contracts | Canonical operating policies, record format, templates and native Rulesync product sources in `agents-configs/` | Minimal entrypoints, selected Rulesync consumer sources and configuration for direct record management |
| Workspace-specific context | Generic examples without private data | Workspace directions, configuration and applicable policy references |
| Live records | No real workspace records or credentials | Tasks, artifacts, directives, registered Bot identity and curated memory |
| Personal configuration | No user-specific contents | Existing private configurations, with their distinct ownership |

The split separates software development permissions, releases and history from day-to-day record changes. The public system must work without access to any particular private workspace. Keep private history, actual records, private identifiers, credentials and raw conversation material out of system source, fixtures, documentation, configuration and logs. Publication of software does not authorize disclosure of workspace contents.

A data repository is a data-only workspace: keep actual records, private configurations and the minimal documentation, Git metadata and directly used agent assets required to manage those records. A small workspace-local `.rulesync/` and `rulesync.jsonc` may select or contain reviewed copies of system-owned product assets; they are not a second canonical owner. Keep system development tooling and generated harnesses out of its tracked tree. Machine-specific settings and generated projections belong to the consumer environment. The system owns shared validation and future CLI tooling; direct authorized Markdown and Git use remains available. Do not claim a CLI validator exists before implementation.

Shared policies and record formats belong to the system. During an authorized cutover, preserve workspace-specific direction in the data repository and update references and entrypoints together. Retain the existing source until the replacement is verified and authorized. Pin or otherwise identify the reviewed compatible policy version rather than relying on an unversioned moving default. If required policy is unavailable or compatibility is uncertain, refuse affected writes. An installed binary is not proof that an agent loaded the applicable policy.

## Rust implementation

Start with one Cargo package and one executable. Separate CLI argument and output handling from domain behavior and concrete storage access. Add modules only as functionality is implemented; do not introduce a public Core package, multiple crates or generic plugin abstractions solely for potential future MCP support.

| Responsibility | Initial design |
| --- | --- |
| CLI | Parse explicit commands and scope, choose output format and expose failures |
| Record model | Parse and validate project-qualified IDs, lifecycle areas and existing frontmatter |
| Domain operations | Resolve, list, inspect and perform eligible task, artifact, directive, bot and memory operations |
| Storage | Read the chosen repository at a known revision; apply a scoped change with a guarded write |
| Verification | Test observable contracts with synthetic records and isolated repositories |

Candidate libraries include `clap`, `serde`, `serde_json`, a maintained YAML parser, and, where required, a Markdown parser, date library and Git or HTTP client. `thiserror`, `anyhow`, `assert_cmd` and `tempfile` are candidates for errors and tests. Select and review actual dependencies when needed; no versions, licenses, compatibility or performance claims are established here. Prefer existing Git or `gh` integration over reimplementing Git, credential handling or HTTP transport when sufficient.

The existing TypeScript scripts and verification commands are not replaced by this design. Evaluate any migration of development tooling independently against existing callers, tests and maintenance cost. Development Agent Assets under `.rulesync/` remain separate from the [distributable Hive Agent Assets](../../agents-configs/README.md). Product releases must select only product sources, not the development harness; distribution mechanics remain to be designed.

## Repository access and writes

Choose one initial access implementation after testing the expected deployment:

| Option | Tradeoff |
| --- | --- |
| GitHub API | Recommended candidate for multiple sessions or machines sharing one remote repository; use explicit repository, ref and authorized account, consistent revision reads and guarded commits |
| Local Git | Candidate for direct editing and offline inspection; avoid sharing a mutable checkout between writers, preserve dirty files and verify the remote state before publishing |

Do not implement both merely for flexibility. Decide whether a mutation prepares a local change for explicit publishing or commits an authorized remote change directly. Make the effect explicit in command help and results; a read must never trigger a commit, push or synchronization. For remote changes, require a revision precondition that prevents a stale writer from replacing another writer's work; a check followed by an unguarded ref update is insufficient. Re-resolve and revalidate on conflict, and do not blindly retry a possibly applied mutation. Apply a multi-file lifecycle change as one coherent Git change, or stop safely without leaving inconsistent records.

Keep credentials with their existing owner. Require explicit workspace and account selection, reuse applicable verified context, and never switch accounts or repositories to evade a denial. No registry, automatic backend selection or fallback is proposed. Preserve [Git change paths](../policies/workspace/git-writes.md): ordinary records and approved routing metadata may use the allowed direct path; directives, identity, curated memory, governing documents and tooling require a reviewed PR. The CLI must not merge, bypass protections, publish external-project changes or grant authority merely because credentials exist.

## CLI surface

The examples below are provisional, not executable commands in the current repository. Define exact arguments, exit codes and output fields during implementation.

```sh
hive task list --project example --json
hive task show example/fix-auth --json
hive task create --project example --file task.md
hive task select example/fix-auth
hive task update example/fix-auth --status review
hive task archive example/fix-auth
hive artifact list --task example/fix-auth
hive artifact add --task example/fix-auth report.md
hive artifact expired
hive directive list --project example
hive bot list
hive memory show --bot example-bot --project example
hive validate
hive doctor
```

- Provide concise human output and stable machine-readable JSON where useful. Keep diagnostics separate from machine output; distinguish empty, absent, invalid, stale, unauthorized and unavailable results. Bound query scope and output; cross-project operations require explicit scope.
- Reuse existing identifiers, record ownership and lifecycle semantics from [tasks](../policies/workspace/tasks.md), [organization](../policies/workspace/organization.md), [frontmatter](../policies/workspace/frontmatter.md) and [lifecycle](../policies/workspace/lifecycle.md). Preserve unknown metadata and Markdown body content when making a targeted update. Reject duplicate IDs, invalid states, ambiguous paths and path escapes; never recreate a stale path.
- Validate that a requested transition is structurally permissible, but do not infer readiness, authority, acceptance or actual execution ownership from metadata alone. A task archive operation needs the accepted outcome or authorized cancellation and closure evidence. Preserve useful work-scoped artifacts and references when moving a bundle.
- Treat artifact expiration as a read-only eligibility report, not a deletion command. Apply existing retention, open-work evidence, canonical absorption and archive exceptions. Unknown retention evidence must remain unknown. Git history can retain earlier versions even after a file is removed.
- Use [directives](../policies/workspace/directives.md), [bots](../policies/workspace/bots.md) and [communication](../policies/workspace/communication.md) for applicable read scope and evidence. A handoff is not a delivery or execution trigger; memory is evidence, not permission. Do not expose unrelated Bot memories, secrets or raw runtime context in output or logs.
- Do not wrap generic Git operations without a distinct Hive contract. Keep writes explicit and non-interactive when possible; high-impact actions still require the relevant authorization. Provide a preview when it materially reduces write risk. Do not add a separate task, run or session state model.

## Implementation and verification

Proceed in independently reviewable increments after applicable storage and implementation decisions are approved.

| Increment | Outcome and acceptance |
| --- | --- |
| 1. Bootstrap | Separate system development from live data; establish Rust toolchain, Cargo, a small CLI entrypoint and synthetic fixtures. Existing data and history remain unchanged. |
| 2. Read and validate | Resolve existing records by project and work ID, parse metadata without changing files, and return scoped deterministic results. Test malformed, missing, duplicate and inaccessible records. |
| 3. Scoped mutations | Implement selected task and artifact operations with explicit effects, valid transitions and guarded writes. Test conflicting revisions, partial failure, path escapes, retained metadata and unauthorized operations. |
| 4. Agent-facing usage | Supply a Hive-owned Skill and command help; show actual invocation, appropriate policy loading and no fallback to another backend. Keep direct Git access possible. |
| 5. Release and compatibility | Test representative existing formats with redacted or synthetic fixtures, verify read/write compatibility and document installation, version selection and recovery. Do not claim runtime behavior without actual bounded tests. |

Use the existing workspace schema first. Decide how compatibility is signaled before the first incompatible change; do not auto-migrate data on startup or binary upgrade. Any approved migration needs a validated source revision, a recoverable backup or Git reference, a reviewable change and a compatibility/rollback check. No database migration framework is required initially.

## Decisions before implementation

- Define how a user explicitly selects a data workspace and compatible system revision. Preserve stable scope IDs and a single owner for work records across any authorized migration.
- Choose the initial storage approach, mutation publication semantics and credential mechanism. Establish how stale reads, write conflicts and uncertain results are reported.
- Define compatibility and policy reference mechanics for data workspaces, including which common documents and templates move, and how existing direct Git consumers continue to work.
- Review the smallest initial command set, Rust dependency and distribution choices, and the relationship to current TypeScript development tooling.
- Confirm implementation acceptance and authorization before implementation, data migration, privileged automation or release.

The design preserves the collaboration backend and agent policy boundaries. It does not assert that the proposed CLI, Skill or data-workspace compatibility has been implemented or tested.
