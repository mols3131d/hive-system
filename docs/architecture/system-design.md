---
description: "Read when designing or implementing the standalone Rust Hive CLI and separating system development from workspace data."
---

# Hive System Design

**Status: design proposal, not implemented.** Hive is an independent Rust system, separate from user-controlled data repositories. The proposed initial product is a local CLI with a Hive-owned Skill, operating on Markdown and Git workspaces. The foundation is the local filesystem and native Git; detailed storage semantics, command contracts and deployment details remain proposals pending review.

Hive is a personal collaboration workspace for one developer working across multiple AI agent sessions and projects, not a multi-user collaboration service, general project management application or agent execution platform. Optimize for long-running development, selective context persistence, low setup and maintenance cost, simple local operation and durable developer-agent communication. Reports, briefings, discussion outcomes and handoffs are records for later reading, not live messages between agent sessions. See [communication](../policies/workspace/communication.md) for the record and delivery boundary. One project has one active development environment (device, VM, container or equivalent); multiple agent sessions may work within it and multiple projects may share an environment. Sequential environment moves are supported after stopping or safely handing off work and preserving source and Hive state. Simultaneous development of the same project from multiple independent environments is outside initial support. Different projects may still share a Hive data repository, so preserve scoped write safety and explicit Git-backed ticket issuance and execution claims without device registration, enterprise identity management or distributed orchestration. Preserve the [workspace operating contracts](../policies/workspace/README.md), including project-owned specifications, human and agent authority, selective retention and non-interference with external developers. Existing records and workflows remain valid until a reviewed change explicitly replaces them.

## Product boundary

| Surface | Responsibility |
| --- | --- |
| Filesystem, Markdown and Git | Canonical workspace records, references and revision history |
| Hive Skill | Progressive discovery of governing policy, choosing applicable actions and interpreting results |
| Rust CLI | Configuration resolution, deterministic validation, scoped records and context, guarded task coordination |
| Existing Git and authentication | Repository access, credentials and revision-safe publishing |
| Agent or human | Goal, priority, delegation, actual acceptance, permission and retention decisions |

Own Hive-specific semantics directly; do not integrate Backlog.md, Beads or another task manager as a second task owner. Reuse established Rust libraries and native tools for generic parsing, serialization and Git where they reduce maintenance and risk.

Core records and operations must be independent of IDEs, coding-agent vendors, model providers, runtime session formats and Git hosting providers. Do not require vendor SDKs, model API keys, IDE plugins or vendor-specific metadata for core operation. Optional projections and integration conveniences belong at the consumer boundary; they must not alter record semantics or require a particular vendor. CLI-first is the proposed initial interface. The Skill links to canonical policies and provides usage guidance rather than copying policies or requiring a particular runtime. Shell-capable clients can invoke the CLI; authorized clients without it can still use available Git and Markdown tools, including a hosting connector where appropriate. A future MCP adapter is a separate decision justified by real consumers (for example, a client without Shell access). Do not implement MCP, remote services or a general adapter framework now.

No database, alternate state store, backend router, automatic synchronization, real-time inter-session messaging, message broker, notification service, task scheduler, agent spawning, session control, distributed lock services, automatic leases or runtime enforcement, automatic memory learning, automatic artifact deletion, TUI or web application is in initial scope. The existing explicit Notion designation policy remains unchanged; this CLI operates on Hive records only and must not silently select, migrate to or write to another backend.

## Repository separation

Keep the Hive system, the developer's Hive data workspace and external project worktrees logically separate. Hive-managed collaboration does not imply project adoption of Hive. Resolve the data workspace explicitly, independently of the external project's current directory, and limit Hive record writes to it. Keep optional Hive harness installation in the explicitly authorized user or Hive workspace environment, not in unrelated projects. The sole default-supported project-local opt-in is the untracked connection file described below; it is not a harness installation. Follow [external repository boundaries](../policies/workspace/organization.md#external-development-repositories); project-native implementation changes and canonical specifications remain owned by the project.

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
| Configuration | Resolve explicit workspace, project and coordination contract; report provenance and conflicts |
| Record model and validation | Parse existing records and enforce structural, lifecycle and claim invariants |
| Domain operations | Resolve, list, inspect and perform eligible record operations and task coordination |
| Context collection | Collect bounded continuation context and pinned policy references |
| Storage | Read local files and Git snapshots; safely prepare changes and conditionally publish critical task changes |
| Verification | Test observable contracts with synthetic records and isolated repositories |

### Initial implementation choices

Use Rust edition 2024 and a tested stable toolchain, one package and one binary named `hive`. Bootstrap records the tested toolchain and minimum supported Rust version in the package/toolchain configuration after resolving actual dependencies. Commit `Cargo.lock` for repeatable application builds. The following are selected dependency families for the MVP proposal, added only when their functionality is implemented; the exact resolved versions, feature set, licenses, advisories and toolchain compatibility must be checked at Bootstrap before accepting the build.

| Dependency | First use | Decision |
| --- | --- | --- |
| [clap](https://docs.rs/clap/latest/clap/_derive/) 4, derive | CLI | Typed argument and subcommand parsing |
| [serde](https://docs.rs/serde/latest/serde/) 1, derive; [serde_json](https://docs.rs/serde_json/latest/serde_json/) 1 | CLI | Typed results and JSON v1 |
| [thiserror](https://docs.rs/thiserror/latest/thiserror/) 2 | CLI | Typed domain errors; one error-code mapping |
| [serde-saphyr](https://docs.rs/serde-saphyr/2.2.0/serde_saphyr/) 2 | Configuration | Maintained Serde YAML parser; disable default features and explicitly enable `deserialize` for read-only parsing, reject duplicate keys and unsupported tags, enforce resource limits and strict scalar types |
| [sha2](https://docs.rs/sha2/latest/sha2/) | Read and mutation | SHA-256 content token over exact bytes |
| [tempfile](https://docs.rs/tempfile/latest/tempfile/struct.NamedTempFile.html) 3 | Mutation and tests | Same-directory temporary files and isolated synthetic repositories; not a concurrency or durability guarantee |
| [uuid](https://docs.rs/uuid/latest/uuid/) 1, v4 | Coordination | Fresh assignment ID, not a credential |
| [time](https://docs.rs/time/latest/time/) 0.3 | Coordination | Closure date in documented UTC convention |

Do not add an HTTP client, Git library, async runtime, database, generic error framework or dedicated Markdown renderer. Use `std::process::Command` with explicit arguments for native Git and `std::fs` for scoped file operations. Standard Rust test harness and process invocation suffice initially; add a test library only if demonstrated necessary. Frontmatter is a delimited YAML mapping with an opaque Markdown body, not a Markdown AST rewriting task. The selected YAML parser can coerce values when targeting Rust strings directly; validate the actual scalar types and keys before typed conversion, reject duplicates, non-mapping documents and unsupported shapes, and use bounded parsing without interpolation or include features. Test strict types, malformed input and metadata preservation; do not silently round-trip unknown or unsupported content through a lossy serializer.

Keep the initial source layout small: root Cargo manifest, `src/main.rs`, `src/cli.rs`, `src/output.rs` and `src/error.rs` at Bootstrap; add `config`, `workspace`, `record`, `validation`, `task`, `context`, `artifact`, `directive`, `bot`, `memory`, `storage`, `git` and `coordination` modules when their responsibilities become executable. Split files only as necessary. Use one internal application boundary rather than a public SDK or a workspace of crates. The [CLI contract](cli-contract.md) owns operation signatures and failure semantics.

Use the standard Cargo build, format, lint and test path, plus the existing repository checks; synthetic fixtures contain no real workspace records. A hosted CI runner may provide the Rust toolchain and execute the exact reviewed source revision when the coordinating agent has no local Rust installation. Keep jobs bounded and read-only with no private data or publishing credentials. Resolve and retain the tested lockfile and toolchain evidence; verify with `--locked` and inspect actual job outcomes and failure logs. CI is execution evidence, not acceptance or authorization. A disposable probe may validate design choices separately from the production package; use its findings rather than incorporating the probe as another shipped implementation. Validate a minimal installation from a reviewed checkout with [Cargo install](https://doc.rust-lang.org/cargo/commands/cargo-install.html) using `cargo install --locked --path . --root <isolated-install-root>`, then exercise the installed binary. No forced replacement, public registry publication or user-environment install is needed for MVP acceptance. Native Git must be present for Git operations; confirm supported versions by actual tests, not by the version observed in one environment. No Git hosting provider or network access is required for local operation.

The existing TypeScript scripts and verification commands are not replaced by this design. Evaluate any migration of development tooling independently against existing callers, tests and maintenance cost. Development Agent Assets under `.rulesync/` remain separate from the [distributable Hive Agent Assets](../../agents-configs/README.md). Product releases must select only product sources, not the development harness; distribution mechanics remain to be designed.

## Configuration and identity

Use YAML with the `.yml` extension for Hive configuration. Only expose values that vary between environments; do not turn safety rules into settings. These are proposed version-one contracts, not installed configuration or implemented commands.

| Source | Ownership and purpose |
| --- | --- |
| User configuration (e.g. `~/.config/hive/config.yml` on XDG systems, respecting the platform config directory) | Optional local default workspace and output preference (`text` or `json`); not tracked in the data repository |
| Project `.local/hive/config.yml` | Optional, explicitly authorized, untracked binding to a Hive project and workspace; no records or runtime instructions |
| Workspace root `hive.yml` | Reviewed, tracked coordination contract and version; no machine paths, credentials or personal settings |
| Workspace `AGENTS.md` | Existing reviewed system policy revision; do not duplicate it in YAML |

Example user configuration:

```yaml
workspace: /example/hive-data
output:
  format: text
```

Example project configuration:

```yaml
project_id: example-project
workspace: /example/hive-data
```

The project binding requires `project_id`; omit `workspace` when a verified user default applies. Resolve an explicit project root or the current Git project root; verify the binding against the authorized workspace and project before using it. Treat project-local configuration as data, not an authority grant; do not traverse unrelated repositories or automatically create files. Use existing ignore rules or a separately authorized local exclusion; never modify tracked ignore rules or Git configuration as a side effect. Refuse installation if untracked exclusion cannot be established. No project binding is required for explicit CLI use.

Example tracked workspace coordination configuration:

```yaml
version: 1
coordination:
  mode: remote
  remote: origin
```

For a single shared local Git repository use `coordination: {mode: local}` instead, omitting `remote`. The authoritative ref in both modes is `refs/heads/main`, following [Git change paths](../policies/workspace/git-writes.md); do not expose a separate configurable ref or automatic fallback. Remote mode requires every writer to use the same explicitly authorized Git repository and ref. Validate the effective fetch and push targets and the reviewed coordination contract; an alias such as `origin` is not proof of identity. Local mode is supported only when every writer shares the same actual Git repository, not independent clones on different machines. Moving or changing the authority requires a reviewed, coordinated cutover with existing claims reconciled; do not silently switch modes.

Resolve configuration by field ownership, not arbitrary deep merging: explicit CLI target, applicable project binding, verified user default for workspace, and already verified operation context only within its scope. An explicit override that would change the project/workspace pair must identify the intended pair and verify its canonical project binding; do not mix unrelated defaults. Read the selected workspace's tracked configuration and policy before coordinated changes. Reject malformed YAML, duplicate keys, unknown config fields, missing required values, unsupported versions, path escapes and conflicting or unverified targets. Resolve relative local paths against the file that declares them; do not evaluate shell expressions or executable config. Output format may be overridden independently. Provide read-only context and config diagnostics with value provenance, without printing credentials or unrelated local settings.

A single developer can have multiple Git author emails, signing identities and hosting accounts. Hive project IDs and Bot IDs are stable logical identities; task claim IDs and holder references identify an execution assignment, not a Git identity, authentication token or registered Bot. Never derive these identifiers or authority from Git email, host username or remote URL. Preserve IDs through repository rename, host migration or authorized environment moves. Use the relevant repository's existing Git author configuration, conditional includes, SSH, credential helper and signing settings. Run workspace Git operations in the selected workspace's repository context, not the external project's working directory; prevent inherited repository-selection/config overrides from redirecting writes or credentials. Verify effective destination and identity where material; never change Git identity, store credentials, or retry with another account. Hosted connectors remain optional and must target the authorized account and repository.

## Local filesystem and Git

Use the local filesystem and native Git as the initial access model. Markdown files own the records; Git supplies immutable snapshots, history and coordination. No Git hosting provider, HTTP API, vendor SDK or remote URL is required for local inspection. Offline validation requires applicable reviewed policy and a compatible validator locally. A workspace may have no remote, a self-hosted remote or a hosted Git service. Do not add a storage adapter framework or a hosting-specific client merely for portability.

- Resolve the workspace root and target explicitly, independently of the project being edited. Scope file access to the selected workspace and reject traversal or symlink escapes.
- Distinguish immutable commit reads from mutable working-tree inspection; report source, revision and freshness. A dirty checkout is not an immutable snapshot or proof of current authoritative state.
- Reads never fetch objects, commit, push or synchronize; an explicit authority observation may query the configured Git ref without modifying local state. Ordinary local changes and explicit Git publication are distinct; coordination operations explicitly access the configured authority because their success depends on it.
- Authorized file or Git tools, including a hosting connector, remain valid access paths to the same records. They may perform coordination only if they can meet the same atomic preconditions and confirm the result; otherwise report the blocker.

## Deterministic validation

Use the existing [frontmatter](../policies/workspace/frontmatter.md), [task lifecycle](../policies/workspace/tasks.md), [organization](../policies/workspace/organization.md) and [retention](../policies/workspace/lifecycle.md) contracts as the source of semantics. Do not introduce a parallel schema or duplicate the task state model. Implement shared validation logic for both read-only inspection and eligible CLI mutations.

| Check | Required behavior |
| --- | --- |
| Structure | Validate record location, entrypoints, required metadata, types and permitted values for the record kind and lifecycle area. |
| Identity and paths | Reject duplicate project-qualified task IDs across lifecycle areas, ambiguous locations, invalid IDs, path escapes and unsafe symlink traversal. |
| References | Resolve in-scope task references and required local paths when the applicable contract defines them; distinguish unresolved external or inaccessible evidence from a confirmed invalid reference. |
| Transition | Compare the known base and proposed change; reject structurally forbidden transitions, inconsistent bundle moves and missing required closure information, invalid claim transitions and loss of a current claim. A snapshot alone cannot prove transition history. |
| Compatibility | Identify the reviewed policy revision and supported record contract. Report unsupported or unavailable compatibility evidence; stop affected mutations rather than guessing. |

Return stable, actionable diagnostic codes, affected paths and scope; distinguish errors from warnings and unknown or unavailable evidence. A standalone validator must be read-only. Before a CLI mutation, validate its base, applicable transition and proposed result using the same logic; reject relevant blocking errors without publishing a partial result. Keep any permitted historical exceptions from the canonical contracts. Do not silently repair, normalize or delete records.

The validator cannot establish user intent, delegated authority, execution ownership, acceptance quality, external delivery or retention eligibility solely from record contents. Those decisions remain with authorized humans and agents; validation is not approval. Direct Markdown and Git editing remains supported. A standalone validation check can inspect direct edits; local hooks in the Hive workspace may invoke it as an optional safeguard. Neither the CLI nor a local hook guarantees enforcement against a writer bypassing it. Any mandatory repository-side enforcement is a separate decision, not a requirement for a local workspace.

## Safe changes and task coordination

Keep ordinary local editing simple: use scoped content preconditions and safe file replacement, preserve unrelated dirty files, and validate the proposed result. For a multi-file lifecycle operation, define a coherent staged change and recoverable interrupted state before enabling the operation. Never publish a partial bundle or change a ref behind an unrelated checked-out worktree. Use an atomic expected-old-revision check for Git ref updates, not a read followed by an unconditional update. The exact safe local file and Git operations remain implementation choices.

Task issuance and execution ownership are more consequential than ordinary record edits. The [task contract](../policies/workspace/tasks.md#git-backed-issuance-and-claims) owns their lifecycle; [frontmatter](../policies/workspace/frontmatter.md#tasks) owns the claim shape. Use one authoritative Git ref and one record owner, not a second claim database, ticket service, distributed lock or runtime scheduler.

| Operation | Confirmation boundary |
| --- | --- |
| Read, validate, context | Local snapshot or working tree; report freshness |
| Ordinary artifact or progress draft | Validated local change; publication separate |
| Issue or select a task, acquire, release, transfer or recover a claim, accept a task as done, close or reopen work | Validated coherent change accepted at the configured authoritative ref |
| Publish other task changes | Validate against the current authoritative state; preserve current claim and lifecycle, reject stale changes |

For a coordinated mutation:

1. Resolve the authorized workspace and reviewed coordination configuration. In remote mode explicitly read the current configured remote ref; in local mode resolve the common Git repository's authoritative ref. An inaccessible or ambiguous authority is a blocking error, not a reason to switch modes.
2. Resolve the project-qualified task across all lifecycle areas at that commit. Check authorization, the current claim and requested transition. For issuance, reject an existing ID; for a claim, require eligible active work with no current claim. Validate the entire candidate, including relevant paths and unique IDs.
3. Build one coherent Git change from that base in an isolated index, worktree or equivalent safe operation. Update the exact authority with an atomic expected-old-revision condition and only a fast-forward change. In remote mode use native Git with an explicit expected revision and verify the proposed commit descends from it; never perform a non-fast-forward update or rely on a preliminary remote check alone. A native Git candidate is an explicit expected-revision push lease combined with a separate ancestry check; a lease alone can permit non-fast-forward changes and is insufficient. Local commit success is not remote confirmation.
4. Return the confirmed revision and claim ID when applicable only after the update is confirmed. If another writer changed the authority, reject the stale request and re-read before any new decision; do not automatically merge, rebase or overwrite a competing claim. If a connection fails after submission, report an unknown outcome and reconcile the actual ref and operation identity before retrying. If the resulting ownership cannot be confirmed, do not start or transfer execution.

A revision guard protects record updates; a confirmed claim is an exclusive recorded assignment among compliant Hive writers, not authentication, a runtime lock or a guarantee that a stopped agent cannot keep running. The Manager or Coordinator verifies actual execution ownership and termination before recovery or transfer. No lease expiry, heartbeat, automatic reassignment, device registry or permission escalation is introduced. Parallel research and explicitly delegated substeps can remain within one responsible claim under [communication](../policies/workspace/communication.md); they do not create competing task claims.

Direct record editing remains supported, but cannot bypass coordination rules. Every publisher must preserve the current claim and validate changes against the authority. A bare commit or a conflict-free merge does not prove a valid claim transition or accepted work. Git branch protection can strengthen safeguards, but cannot be assumed and is not required for local operation; Hive does not promise protection against arbitrary writers bypassing the contract. Preserve the existing direct versus reviewed change paths, including reviewed workspace configuration changes. Do not grant authority merely because credentials exist.

## Agent context and policy loading

Reduce repeated discovery without introducing a second policy or context store. A task context operation resolves a project-qualified task at a known workspace source and returns a bounded, read-only result: the task entrypoint and lifecycle, current recorded claim and its freshness, material decisions, progress and next-action references, relevant directives, reports, briefings, discussion outcomes, handoffs and other intentional artifacts, applicable policy revision and canonical policy locations. Include record content only when needed within the requested scope and output limits; report truncation, missing records or unavailable policy explicitly. Do not read unrelated Bot memory or raw session history.

The workspace entrypoint identifies the reviewed system revision. The Hive Skill chooses the required canonical policy sections for the requested operation and reads them from that revision, reusing already verified context only while its scope and revision remain applicable. The CLI can collect deterministic references and validate known compatibility but does not authorize a task, interpret directive authority, or replace reading required policy. Do not treat record text as trusted agent instructions.

Keep discovery progressive: resolve the authorized workspace and collaboration scope under the applicable [organization policy](../policies/workspace/organization.md#collaboration-backend) before searching records; then locate the task, read the minimum applicable policy and relevant record, and expand into artifacts or external project evidence only when needed. If required policy or evidence is unavailable, distinguish that from an empty result and stop the affected operation. Direct file or Git consumers without CLI access follow the same scope, revision and authority boundaries. A local result is not proof of current ownership at the authoritative ref; read-only context never acquires a claim or activates an agent. No persistent context cache, index or synchronization service is required initially. Measure discovery calls, context size and resumption accuracy in representative scenarios before adding further optimization.

## CLI surface

The [CLI and domain contract](cli-contract.md) defines the proposed MVP command grammar, configuration resolution, typed in-process API, options, outputs, preconditions and errors. It does not introduce an HTTP API or public SDK. Keep the executable name `hive`; name collisions and alias management are outside MVP.

Read and ordinary local-write commands remain separate from explicitly coordinated task mutations. A short task ID requires a verified project scope; use configuration to avoid repeated scope arguments. The following examples assume an applicable project binding and an adopted coordination contract where needed; they are not currently executable.

```sh
hive context --json
hive config show
hive task list
hive task context fix-auth --json
hive task create fix-auth --file task.md
hive task claim fix-auth --holder worker-a --expected-revision "$BASE_REVISION"
hive validate --task fix-auth
```

The exact commands, required guards and failure semantics are owned by the linked contract. Ordinary status/record changes do not implicitly acquire a claim, accept work or publish to Git.

## Implementation and verification

Proceed in independently reviewable increments after applicable implementation decisions are approved. The system is still development-only; existing workspaces remain on their reviewed policy revisions until explicit adoption.

| Increment | Outcome and acceptance |
| --- | --- |
| 1. Bootstrap and configuration | One Rust CLI, explicit workspace resolution and YAML validation; verify wrong-target refusal, no silent fallback, and no writes to a synthetic non-adopting external project. |
| 2. Read, validate and resume | Scoped offline reads and deterministic diagnostics; test malformed metadata, duplicate IDs, invalid paths, dirty snapshots, missing policy and direct file edits. Reconstruct useful progress, decisions, reports and next actions without full session context. |
| 3. Safe ordinary changes | Validate local edits, transitions and coherent bundle moves; test content conflicts, dirty files, interrupted changes and preservation of unrelated content. |
| 4. Git-backed task coordination | Test two writers issuing the same ID and acquiring the same task, stale claim changes, transfer/release/recovery, rejection after a remote race, uncertain write outcome, local mode, remote mode and unavailable authority. Verify no competing operation can be confirmed through the compliant path. |
| 5. Agent-facing usage | Supply a Hive-owned Skill and command help; verify distinct agent environments, plain Git/Markdown access, bounded context and pinned policy loading. Measure discovery calls and context size against manual resumption. Verify that ordinary Hive use and optional setup leave a non-adopting project's tracked/untracked files, Git settings and hooks unchanged; test explicit ignored project binding separately. |
| 6. Release and compatibility | Use synthetic fixtures and generic local/bare Git remotes, distinct Git identities and no vendor service. Verify environment moves, policy adoption/rollback and safe recovery. Document installation, compatibility, remaining restrictions and actual test results before real use. |

Use the existing Markdown layout and stable identifiers. The new coordination metadata and tracked configuration require a reviewed policy-version adoption: verify compatible tooling or a direct Git path satisfying the contract, quiesce old-policy writers, reconcile current execution owners and in-flight tasks, agree on one authoritative ref, and validate records before enabling coordinated writes. No automatic migration on startup, binary upgrade or policy merge. Existing pinned workspaces keep their previous contracts until explicitly migrated; a workspace adopting this contract must not treat missing configuration or uncertain claims as permission to fall back. Preserve the original revision and a recoverable backup for rollback; do not roll back over active ownership without reconciliation.

## Decisions before implementation

- Review the proposed [CLI and domain contract](cli-contract.md), implement platform path resolution and verify exact command/error behavior. Finalize safe Git compare-and-update mechanics and recovery evidence through actual Git tests before enabling writes.
- Specify non-invasive optional asset installation and test repository/credential isolation, including differing project and Hive Git authors and remotes.
- Finalize compatibility signaling and the reviewed adoption/rollback procedure for the new coordination contract. Preserve stable scope and work IDs and one record owner across migration.
- Review the selected dependency families, source layout and installation approach above; at Bootstrap resolve and review the actual lockfile/toolchain, run Cargo checks and preserve the existing TypeScript development tooling.
- Confirm implementation acceptance and authorization before implementation, data migration, privileged automation or release.

The design preserves the collaboration backend and agent policy boundaries. It does not assert that the proposed CLI, Skill or data-workspace compatibility has been implemented or tested.
