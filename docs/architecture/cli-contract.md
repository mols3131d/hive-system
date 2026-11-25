---
description: "Use when implementing Hive domain APIs, configuration resolution, CLI commands, arguments, options, results and failure contracts."
---

# Hive CLI and Domain Contract

**Status: MVP proposal, not implemented.** This document owns the proposed command interface and internal Rust operation contracts. [System design](system-design.md) owns architecture; [workspace policies](../policies/workspace/README.md) own record semantics and authority. API means typed in-process operations called by the CLI, not HTTP, MCP, a public Rust SDK or a separate service. Keep the binary name `hive`; naming conflicts and aliases are deferred.

## Invocation and scope

```text
hive [GLOBAL OPTIONS] <domain> <command> [ARGUMENTS] [OPTIONS]
hive [GLOBAL OPTIONS] context
hive [GLOBAL OPTIONS] validate
hive [GLOBAL OPTIONS] doctor
```

Global options are accepted before the domain or after the leaf command; repeated conflicting values are errors. Validate the common option values and mutually exclusive combinations across these positions before any side effect. A parser's local conflict annotation alone is insufficient: a synthetic `clap` 4.6.10 probe accepted `--at` before a subcommand together with `--authority` after it. Explicit post-parse validation rejected that combination in both orders. Collect value-bearing common options separately at the root and leaf positions, preserving repeated occurrences before resolution. A synthetic `clap` 4.6.10 probe using position-specific flattened argument groups and explicit equality/conflict checks passed repeated-value tests at the root, leaf and across the boundary. Identical repeated values are allowed; conflicting values fail with `INVALID_ARGUMENT` without echoing the supplied values. A recognized JSON request still produces a JSON failure when common output options conflict. Reuse this mechanism in the product and test all actual command depths. View options are read-only and rejected on mutations. All other options are command-specific unless explicitly listed as global. Help and version do not require a workspace. Unknown arguments, unknown options, missing required arguments and incompatible combinations fail before side effects.

| Global option | Type | Default and boundary |
| --- | --- | --- |
| `--workspace <path>` | Path | Applicable project binding, then user default; never guess a data repository from the external project |
| `--project <id>` | Project ID | Applicable project binding; short task IDs require resolved scope |
| `--project-root <path>` | Path | Current Git project's top level if available; explicit path for another verified project; not a workspace selector |
| `--format <text\|json>` | Enum | User preference, otherwise `text` |
| `--json` | Switch | Shorthand for JSON; conflicts with `--format text` |
| `--at <commit>` | Local full commit object ID | Read-only immutable snapshot; otherwise inspect the working tree |
| `--authority` | Switch | Explicit read-only authority check, not a fetch or synchronization; conflicts with `--at` |

Use stable IDs from [organization](../policies/workspace/organization.md#ids-and-file-naming). Task arguments accept `<work-id>` within a resolved project or `<project-id>/<work-id>`. A qualified ID may supply the scope when no scope has been selected; it must agree with an explicit or applicable project binding. For another project, explicitly select the intended workspace/project pair. Reserved scopes are not ordinary project names. Resolve identities before constructing paths. Do not infer scope from Git email, account, branch, repository URL, status or current folder name. `--all-projects` is available only on task and project list and validation where specified below; never use it as a fallback for a missing project.

### Configuration resolution

Use the [three configuration owners](system-design.md#configuration-and-identity). The user path on Unix is `$XDG_CONFIG_HOME/hive/config.yml` when that variable is an absolute path; otherwise `$HOME/.config/hive/config.yml`. On Windows the proposed equivalent is `%APPDATA%/hive/config.yml` (support only after platform tests). No environment variable overrides the selected workspace, project, claim or Git authority in MVP. Missing optional files are not errors; an existing malformed file is not silently ignored. Project config is read only at the resolved project root, not an arbitrary ancestor or a different checkout. Relative paths in YAML resolve against the file's directory; CLI paths resolve against the invocation directory. A leading `~/` may expand to the verified user's home; no other expansion, shell evaluation, executable YAML tags or remote config loading.

| Source | Allowed fields | Defaults |
| --- | --- | --- |
| User `config.yml` | `workspace` (path), `output.format` (text or json) | No workspace assumed; text output |
| Project `.local/hive/config.yml` | `project_id` (required), `workspace` (optional path) | Use verified user workspace when absent |
| Workspace `hive.yml` | `version: 1`, `coordination.mode: local\|remote`, `coordination.remote` (required only in remote mode) | No implicit coordination mode or remote; main ref fixed |
| Workspace `AGENTS.md` | Reviewed system policy reference | No implicit latest-policy fallback |

Validate types, duplicate keys, unknown fields, required values, versions, path safety and selected project/workspace pairing. Existing invalid or unverified selection is not repaired or skipped by choosing a different target. CLI overrides may change the pair only with explicit verified selection; they do not overwrite files. Use the actual Git root, effective fetch/push targets and repository-local authentication, not the external project's Git configuration. Missing or incompatible policy blocks affected writes. Legacy workspaces without adopted coordination can be inspected with compatible policy support; coordinated commands fail with `INCOMPATIBLE_POLICY` until reviewed adoption. Never infer local mode from missing config.

## Shared operation and data types

Use one Cargo package with modules only as needed. CLI parsing converts strings into validated domain types; domain functions return `Result<T, HiveError>` and do not print, interpret LLM instructions or invoke external agents. The following operation signatures describe responsibility and typed inputs/outputs; all operation returns are wrapped in Result, omitted from the shorthand below. Exact borrowing, module visibility and implementation may evolve without changing the CLI contract.

| Type | Meaning |
| --- | --- |
| `ResolvedContext` | Verified workspace root, optional project, config provenance, policy revision, coordination target if adopted; no stored credentials |
| `ReadView` | Working tree with observed HEAD and dirty state, or immutable commit; authority freshness is separate |
| `TaskKey` | Validated project and work IDs; no lifecycle directory in the identity |
| `ArtifactKey` | Explicit owning task or standalone scope, plus safe relative record identifier |
| `Observation` | Source view, full revision when known, content token for mutable file reads, freshness and diagnostics |
| `ExpectedRevision` | Full native Git commit object ID, checked against the selected authoritative ref |
| `ContentToken` | Opaque digest of exact file bytes, e.g. sha256 followed by the digest; not a Git revision or authorization |
| `ClaimId` | Opaque assignment ID, distinct on acquisition/transfer; not a credential |
| `MutationReceipt` | Resulting scope, changed relative paths, local or confirmed outcome, base/candidate/confirmed revisions as applicable, claim ID when confirmed |
| `HiveError` | Stable code, message, relevant scope/path and safe diagnostic details |

Resolve once per invocation. Pass context and typed operation requests to the domain module; no generic repository abstraction or framework is required. Authority-sensitive operations also receive observed preconditions and verified caller/assignment context where available. CLI-supplied identifiers or evidence are not authentication or proof of permission; the authorized caller must establish actual user, role and execution authority. Validate deterministic invariants independently. No persistent session or claim registry is introduced.

## Configuration and workspace

| Command | Arguments and options | Result / side effect |
| --- | --- | --- |
| `config show` | `--scope <user\|project\|workspace\|resolved>` (resolved default) | Read fields, source paths, presence and provenance; redact secrets and credential-bearing URLs |
| `config validate` | `--scope <user\|project\|workspace\|resolved>` (resolved default) | Read-only diagnostics; unresolved required fields fail |
| `config bind` | Explicit `--workspace`, `--project`; optional `--project-root` | Create only the absent, ignored project binding for a verified existing scope; no overwrite, ignore/config changes or harness projection |
| `context` | Global scope and view options | Resolved context, selected view, policy compatibility and coordination freshness |
| `doctor` | Global scope options, optional `--authority` | Read-only environment, Git, configuration, policy, record access and optional authority checks |
| `project list` | `--all-projects`, list options below | Scope entries from task/directive directories; no unrelated memory recall |
| `project show` | `<project-id>` or resolved project | Canonical scope reference, existing entrypoints and counts; read-only |

The config show/validate source scopes can inspect that source without requiring a complete resolved context, but report missing required context when the requested scope depends on it. Config bind is an explicit opt-in operation, not an automatic init. It supports Git project roots only in MVP; non-Git projects can use explicit workspace/project arguments. Verify an existing applicable project identity and repository-local ignore rule before creating the untracked file; a tracked file, unignored path or existing binding is a conflict. Edit existing local YAML manually for MVP. Bootstrap or change tracked workspace configuration through the applicable reviewed workspace procedure, not a CLI config setter. No automatic initialization of a repository or policy migration.

For a new project without scope entrypoints, issue its first ticket using explicit workspace/project selection and a verified canonical project reference; create only the required scope entrypoint and lifecycle directories in the same confirmed change. No empty project registry. Project listing is a derived view, not an independent identity database. A scope existing only in authorized Bot memory is not listed by default; resolve such identity explicitly when necessary. The CLI create/select split is intentionally narrower than the policy's permitted direct active creation.

Internal operations: `config::resolve(Selection) -> ResolvedContext`, `config::inspect(Selection, ConfigScope) -> ConfigReport`, `config::validate(Selection, ConfigScope) -> ValidationReport`, `config::bind(BindRequest) -> MutationReceipt`, `workspace::inspect(ctx, view) -> WorkspaceReport`, `workspace::doctor(ctx, checks) -> DiagnosticReport`, `project::list(ctx, query) -> Page<ProjectSummary>`, `project::show(ctx, id) -> ProjectDetail`. Bind performs only the narrow local creation described above.

## Task

Read [task lifecycle and claims](../policies/workspace/tasks.md) before implementing transitions. The lifecycle and claim fields remain in the same task entrypoint. Do not create a duplicate ticket or separate owner record.

### Read commands

| Command | Arguments and options | Result |
| --- | --- | --- |
| `task list` | `--area <backlog\|active\|archive\|all>` (active default), `--status <status>`, `--priority <low\|normal\|high>`, `--all-projects`, list options | Project-qualified summaries with area, applicable metadata, recorded claim and freshness |
| `task show` | `<task>` | Entry-point metadata/body, current resolved area/path, observation and content token |
| `task context` | `<task>`, `--include-artifact <id>` (repeatable), `--max-bytes <n>` | Bounded resume context and references; explicitly requested artifact content only |

List filters must be compatible with the selected lifecycle area; backlog has no status and archive has no priority. No synthetic readiness or ownership is inferred. Default list limit is 50, maximum 500; `--after <key>` resumes after the last stable key, using deterministic ID order. Return `items`, `truncated` and optional `next_after`; for consistent multi-page reads reuse the same `--at` commit. A working-tree page is not an immutable snapshot. An empty list is successful, not a missing task.

Context includes the task entrypoint, known policy revision and relevant policy links, scoped directive candidates and task artifact summaries/references. Bound each directive and artifact reference list to the first 50 stable keys, report omitted counts or unknown completeness, and use domain list commands for more. The agent determines relevance and actual applicability; the CLI does not semantically summarize or authorize them. Do not load all artifact bodies or unrelated memory. A default 32768-byte content budget (maximum 262144) bounds included record text, not the entire JSON envelope. Return truncation/omission reasons and reference IDs. If essential task content exceeds the budget, fail with `CONTEXT_LIMIT` rather than returning a misleading complete context. Explicitly requested but missing artifacts are errors. These initial limits are proposed defaults, subject to representative usage measurements.

### Coordinated mutations

The following commands explicitly request an authoritative Git change, not a local-only draft or a runtime action. They require an adopted compatible workspace, a fresh authoritative base, actual permission and successful conditional publication. All except a fresh create require `--expected-revision <oid>` from the caller's authoritative observation. Fresh create may omit it because the operation checks nonexistence at the current base and still performs an atomic expected-old-ref update; supply it when acting on a prior observation. Check the current authoritative commit's coordination contract and policy pin against the reviewed local selection; if they changed or cannot be verified, stop rather than silently redirecting the operation. A claimed task's owner-sensitive operations require `--claim-id <id>` matching the current assignment. These options never substitute for actual permission or termination verification.

| Command | Required arguments / options | Transition |
| --- | --- | --- |
| `task create` | `<task> --file <path>`; `--project-source <reference>` only for first ticket in a new ordinary scope | Issue backlog ticket from validated full Markdown, no status/claim; reject duplicate ID across all areas |
| `task select` | `<task> --expected-revision <oid>` | Move backlog bundle to active todo, no claim |
| `task claim` | `<task> --holder <reference> --expected-revision <oid>` | Acquire eligible unclaimed active work; preserve todo/review until actual execution state changes |
| `task release` | `<task> --claim-id <id> --status <todo\|review> --expected-revision <oid> --note-file <path>` | After verified stop, remove claim and record continuation; never unclaimed in-progress |
| `task transfer` | `<task> --claim-id <id> --to <holder> --status <todo\|review> --expected-revision <oid> --note-file <path>` | After verified former stop and accepted new assignment, atomically replace claim with fresh ID |
| `task recover` | `<task> --claim-id <id> --status <todo\|review> --expected-revision <oid> --evidence-file <path>` | Developer/authorized Manager clears abandoned claim only after verified termination or equivalent evidence; no force/expiry |
| `task accept` | `<task> --expected-revision <oid> --outcome-file <path>`; claim ID if claimed | Record verified acceptance and outcome, set active done and release claim in the same change after verified stop |
| `task archive` | `<task> --expected-revision <oid>` | Move accepted unclaimed active done bundle to archive; record closure date, retain all artifacts |
| `task cancel` | `<task> --expected-revision <oid> --outcome-file <path>`; claim ID if claimed | Authorized cancellation with reason and verified stop if claimed; move whole bundle to archive cancelled |
| `task reopen` | `<task> --expected-revision <oid> --note-file <path>` | Authorized reopen of archived bundle as active todo, without claim; retain history |

Files passed to create contain complete Markdown entrypoints with allowed frontmatter. Note, evidence and outcome files contain deliberate Markdown body text, not executable input; append a purposeful record to the existing task body, preserving earlier evidence. Archive/cancel removes active-only metadata and records the actual closure date. A release/recovery report is evidence, not proof of stop; the caller verifies actual execution and authority. No automatic reassignment, claim expiry, approval or delivery. No `--force`, `--skip-validation`, `--remote`, `--ref` or `--claim-any` override. Use separate explicit operations instead of a generic patch that can alter lifecycle or claim fields.

### Ordinary progress changes

`hive task update <task> --if-match <token> [--file <path>] [--status <todo|in-progress|review>] [--claim-id <id>]`

At least one file or status change is required. `--file` supplies the intended replacement entrypoint Markdown; validate the replacement and preserve historical/unknown metadata unless an applicable reviewed migration explicitly changes it. The replacement may change the body and permitted ordinary metadata, but must not change status or claim; status changes use only the explicit status option. Status may be set only for active non-done work; done/cancelled cannot be set through update, and an active done record cannot be reopened or rewritten via update. In-progress requires a current confirmed claim; owner-sensitive changes require its ID and verified ownership. Updating archived records or accepting work uses the applicable review/lifecycle rules, not update. Unclaimed work may have authorized planning metadata changes. Before owner-sensitive writes, explicitly check current authoritative assignment and applicable policy, including in remote mode; if the authority or ownership is unavailable or uncertain, stop. No local draft becomes a substitute for this check. A local content token from show must still match the exact file. Re-resolve the task location and relevant claim, and reject conflicting changes under the [local replacement contract](#native-git-execution-and-recovery).

Update changes only the local working tree, does not stage, commit or push, and returns local outcome plus the resulting content token. A draft is not authoritative. Publishing ordinary changes uses an explicit authorized native Git procedure, current ref/claim checks and transition validation. Do not introduce generic publish/sync commands in MVP. Coordinated changes never implicitly include a dirty draft; if affected paths are dirty, reject and require explicit preservation/reconciliation first.

Internal operations: `task::list(ctx, view, TaskQuery) -> Page<TaskSummary>`, `task::show(ctx, view, TaskKey) -> TaskDetail`, `task::context(ctx, view, TaskKey, ContextOptions) -> TaskContext`, `task::update(ctx, UpdateRequest) -> MutationReceipt`. Use separate typed requests for `create`, `select`, `claim`, `release`, `transfer`, `recover`, `accept`, `archive`, `cancel`, `reopen`, each returning a confirmed receipt only after the shared coordination operation succeeds. Claim and lifecycle validation is shared with validate; do not use a generic public transition endpoint.

## Artifact

Choose exactly one owner: `--task <task>` or `--standalone`. A task owner is resolved across lifecycle areas. An artifact ID is a safe relative identifier within that owner, not an arbitrary filesystem path. Existing Markdown files and directory bundle entrypoints can be read; first MVP writes create/update a single Markdown file, not arbitrary binary trees or session logs.

| Command | Arguments and options | Result / side effect |
| --- | --- | --- |
| `artifact list` | Owner, `--kind <kind>`, `--status <active\|parked\|closed>`, list options | Scoped artifact summaries and references |
| `artifact show` | `<id>`, owner | Markdown entrypoint, metadata, content token and path |
| `artifact add` | `<id> --file <path>`, owner; claim ID when owner-sensitive | Create new validated Markdown artifact locally; fail if destination exists |
| `artifact update` | `<id> --file <path> --if-match <token>`, owner; claim ID when owner-sensitive | Update only owned existing Markdown file locally; no implicit move or overwrite of another writer |

Use [artifact metadata](../policies/workspace/frontmatter.md#artifacts), [ownership](../policies/workspace/organization.md#ownership) and [retention](../policies/workspace/lifecycle.md#artifact-retention). Do not write to a closed task merely because its files remain; follow authorized review and retention rules. No automatic delete, cleanup, promotion, message delivery or attachment synchronization. A read-only expiration eligibility report is a follow-up, not required for initial write support. Preserve existing artifact bundles via direct authorized Git/Markdown where needed.

Internal operations: `artifact::list(ctx, view, ArtifactQuery) -> Page<ArtifactSummary>`, `artifact::show(ctx, view, ArtifactKey) -> ArtifactDetail`, `artifact::add(ctx, AddArtifact) -> MutationReceipt`, `artifact::update(ctx, UpdateArtifact) -> MutationReceipt`. The two mutations use scoped content and ownership guards, not a second artifact database. For owner-sensitive writes on a claimed task, require the same current authoritative assignment check as task update; a standalone artifact has no task claim.

## Directive, Bot and Memory

These are read-focused domains in MVP. The existing policies own reviewed creation, modification, supersession, identity and memory consolidation. Do not introduce CLI write commands that bypass review or make records into instructions, and do not hard-code a hosting provider's PR API.

| Command | Arguments and options | Result |
| --- | --- | --- |
| `directive list` | Resolved project, `--status <proposed\|active\|superseded\|withdrawn\|all>` (active default), list options | Scoped directive metadata/references; active does not prove applicability |
| `directive show` | `<id>`, resolved project | Directive entrypoint and source references |
| `bot list` | List options | Registered Manager identity summaries/routing references only |
| `bot show` | `<bot-id>` | SOUL and routing/navigation metadata; no automatic memory load or activation |
| `memory list` | `<bot-id>`, resolved project, list options | Authorized scoped memory entrypoint/topic references |
| `memory show` | `<bot-id>`, optional `<topic>`, resolved project | Scoped MEMORY.md when topic omitted; otherwise one selected topic |

No cross-Bot memory aggregation, implicit `_general` recall or session context persistence. General memory/directives require explicit applicable scope. Bot lists do not prove runtime availability. Directives require verified issuer and applicability, not a status alone. See [directives](../policies/workspace/directives.md) and [Bot/memory](../policies/workspace/bots.md).

Internal operations: `directive::list/show` with validated project and directive query/key; `bot::list/show` with Bot query/key; `memory::list/show` with verified Bot and project/topic key. Return typed pages/details with observations and reference metadata. No provider adapters or public write endpoints. Reviewed changes remain possible through the authorized direct document/Git workflow.

## Validation and Git coordination

| Command | Arguments and options | Behavior |
| --- | --- | --- |
| `validate` | Resolved project, or `--task <task>`, or explicit `--all-projects`; optional `--against <base-oid>` | Read-only structural, identity, path, compatibility and reference checks; compare transitions when a base is supplied |
| `context --authority` | Global target options | Explicitly observe authority revision and whether its object is present locally |
| `task show <task> --authority` | Resolved task | Read the observed authoritative commit only if the commit object is available locally |

Project-scope validation covers relevant task/directive records and their artifacts, not unrelated memory; explicit all-projects validation covers the workspace record layout and metadata, without printing record bodies. Task scope and all-projects are mutually exclusive. `--against` selects a local base commit; compare the selected candidate view (working tree or `--at`) against it. A snapshot-only check cannot establish transition validity. Unknown external evidence is not an invalid local reference. No repair or deletion.

In remote mode, the explicitly requested authority check uses the configured remote's current ref (for example native Git ls-remote), not a tracking branch. It does not fetch objects, change local refs or imply ongoing freshness. For a domain read with `--authority`, check the observed snapshot's reviewed policy and coordination contract before treating it as applicable. If the required commit is not present locally, return `OBJECT_UNAVAILABLE` and a safe explanation to explicitly obtain that object using authorized Git before retrying. Local mode resolves the shared actual repository ref. Default reads stay offline. A coordinated mutation explicitly accesses authority and may obtain needed objects in isolated preparation; it must not synchronize the user's checkout as a side effect.

Internal operations:

- `validation::inspect(ctx, view, ValidationScope) -> ValidationReport` and `validation::transition(ctx, base, candidate, scope) -> ValidationReport`; deterministic checks only, never approval.
- `git::read_view(ctx, selector) -> ReadView` and `git::observe_authority(ctx) -> AuthorityObservation`; concrete native Git operations with isolated repository context.
- `coordination::apply(ctx, expected, TaskMutation) -> MutationReceipt`: check current authoritative base, claim/lifecycle and candidate; prepare coherent scoped change, require expected-old-ref comparison and fast-forward ancestry, publish once, confirm or return unknown outcome. No automatic retries, rebase, merge or non-fast-forward changes.
- `storage::replace_file(ctx, path, expected_token, bytes) -> LocalReceipt`: safe replacement preserving unrelated content and permissions; reject stale location/content, unsafe paths and incompatible dirty state.

### Native Git execution and recovery

Use native Git in the verified workspace context. The following implementation recipe specifies the expected behavior, not a currently available command implementation. Verify every affected precondition and the exact Git operation through synthetic integration tests before enabling writes.

- Isolate repository-selection and configuration overrides: `git -C` alone does not isolate inherited `GIT_DIR`, `GIT_WORK_TREE` or injected `GIT_CONFIG_COUNT` entries. Reject or explicitly sanitize inherited repository-local variables using [rev-parse local-env-vars](https://git-scm.com/docs/git-rev-parse) and relevant config/identity overrides; never replace trusted native authentication settings or retry with different credentials. Verify the resulting root, common Git directory and effective author/committer. For remote mode, inspect all effective fetch and push URLs after Git rewriting. Reject multiple or mismatched destinations unless equivalence to the explicitly authorized single authority is established. Redact URLs and environment details in diagnostics.
- Observe the exact authoritative `refs/heads/main`; resolve the candidate from that immutable base, not a tracking ref or dirty checkout. Use an isolated index or worktree and stage only the affected bundle; retain the candidate commit and base OID for verification and recovery. Never include unrelated staged/unstaged files. If affected paths are dirty, stop rather than overwrite.
- Local mode: check all registered worktrees with [worktree list --porcelain](https://git-scm.com/docs/git-worktree). For the first implementation, refuse coordinated ref writes if the authority branch is checked out in any worktree or the worktree state is uncertain; require a separately prepared non-authority working branch or detached checkout. Do not switch, reset or move another worktree automatically. This conservative rule assumes compliant writers keep the authority branch unoccupied during coordination; the list check itself is not an atomic lock against arbitrary checkout changes. Verify fast-forward ancestry using `git merge-base --is-ancestor <base> <candidate>`, then [update-ref](https://git-scm.com/docs/git-update-ref) with the exact expected old OID: `git update-ref refs/heads/main <candidate> <base>`.
- Remote mode: verify the same ancestry precondition, and request exactly one update with the candidate OID and explicit expected lease, for example `git push --porcelain --no-follow-tags --recurse-submodules=no --force-with-lease=refs/heads/main:<base> <verified-remote> <candidate>:refs/heads/main`. See [push](https://git-scm.com/docs/git-push). Reject any configuration that would add destinations or ref updates or bypass required protections. The lease is not permission to rewrite history: the explicit ancestry check is mandatory. Do not use an implicit tracking-ref lease, unconditional force, automatic merge, rebase or synchronization.
- Distinguish a definitive stale/rejected update from a failed acknowledgement. Confirm the actual update and relevant current task/claim before execution. If confirmation cannot be established, return `UNKNOWN_WRITE_OUTCOME` with the base and candidate IDs; preserve the candidate in the isolated local preparation context until reconciled, without writing a second authoritative record or publishing temporary refs. An explicit authority observation may establish that the candidate was accepted, including when subsequent commits have advanced the ref, but historical acceptance alone does not prove the current claim. Check the current record and observed authoritative revision before starting or resuming work; do not retry a mutation merely because its acknowledgement was lost.
- For ordinary file edits, read exact content, resolve the current owner and path, and validate the candidate. Prepare a temporary file on the same filesystem and preserve file permissions. Serialize the final path, ownership and content checks through replacement across cooperating Hive processes that edit the same record in the same working tree. If the supplied content token no longer matches, return `CONCURRENT_CHANGE` and preserve the current file. Test two processes with distinct edits and the same starting token. Exactly one update must succeed and its bytes must remain. Use no-clobber creation for new records and bindings. If replacement is interrupted or its result is ambiguous, inspect the actual bytes and expected content token, preserve recoverable data and stop. The serialization guarantee covers cooperating Hive writers. Test platform-specific recovery and report the limits for arbitrary editors and crash durability. Lifecycle moves use a coherent Git change.

Use the native Git result, exact revision/claim observation and shared validation as evidence; do not expose a generic Git wrapper or a separate task/operation registry. Platform-specific file recovery, hook behavior, identity isolation, and lost-acknowledgement failure injection remain executable verification gates.

## Results and failures

Text is for humans; JSON is the automation contract. With a recognized JSON output selection, write one UTF-8 JSON object to stdout, including on domain failure. No progress messages or logs in stdout; diagnostics may use stderr without exposing credentials. Help/version remain human-readable; command-line parse failures before output selection can be plain usage on stderr. JSON v1 success contains `schema_version`, `ok`, `command`, `context`, `data`, and `warnings`; failure contains `schema_version`, `ok: false`, `command`, available safe context, `error` and `warnings`. Fields not available are omitted rather than invented; an unknown outcome is an error, not success.

```json
{
  "schema_version": 1,
  "ok": true,
  "command": "task list",
  "context": {
    "project_id": "example-project",
    "view": "working-tree",
    "freshness": "local"
  },
  "data": {
    "items": [],
    "truncated": false
  },
  "warnings": []
}
```

The context also includes available workspace/source revision, dirty state and policy revision. Use `view: working-tree|commit` for reads, and freshness `local|authority-observed|unknown`; authority observation is point-in-time. Local mutations return `outcome: local`; coordinated successes return `outcome: confirmed` with base, candidate and confirmed revisions. Never return a confirmed claim ID for an unknown or rejected write. A validation failure includes structured diagnostics (code, severity, path, message), not an empty list.

| Exit | Error codes | Meaning |
| --- | --- | --- |
| 0 | — | Successful read or confirmed requested operation; warnings allowed |
| 1 | `INTERNAL_ERROR` | Unexpected internal failure |
| 2 | `INVALID_ARGUMENT`, `INVALID_CONFIG`, `AMBIGUOUS_SCOPE`, `CONTEXT_LIMIT` | Input or resolution problem; fix input |
| 3 | `NOT_FOUND` | Requested record missing in resolved scope |
| 4 | `INVALID_RECORD`, `INVALID_TRANSITION`, `INCOMPATIBLE_POLICY` | Record or policy contract not satisfied |
| 5 | `ALREADY_EXISTS`, `ALREADY_CLAIMED`, `CLAIM_MISMATCH`, `STALE_REVISION`, `CONCURRENT_CHANGE`, `DIRTY_CONFLICT` | Conflict; re-read and reconcile, never blind retry |
| 6 | `UNAUTHORIZED` | Verified permission/authority denied; no alternate account |
| 7 | `AUTHORITY_UNAVAILABLE`, `OBJECT_UNAVAILABLE`, `POLICY_UNAVAILABLE`, `IO_ERROR` | Required source or operation unavailable |
| 8 | `UNKNOWN_WRITE_OUTCOME` | Write may have occurred; reconcile actual authority/local state before retry |

Error details may include safe expected/observed IDs, affected relative paths, candidate revision and diagnostic array; never return secrets, credential URLs, unrelated repository state or raw session history. These are proposed stable v1 meanings; add codes only for materially distinct caller decisions. Tests must assert both exit and JSON contract, including absence, invalid data, conflicts, partial/unknown outcomes and valid empty results.

## MVP implementation boundary

Deliver configuration/workspace resolution, record model/validator and scoped read/context first, then guarded local edits, then authoritative task coordination, then Skill and representative integration. A narrow config bind and artifact update may be implemented alongside ordinary local writes; they are not prerequisites for read-only use. Defer public SDK, MCP/HTTP, generic publishing wrappers, interactive UI, aliases, configuration registries, provider-specific integration, automatic cleanup, automatic migration and Directive/Bot/Memory write commands. Do not implement every possible operation before the core task flow works.

Use synthetic fixtures to verify CLI parsing/defaults, precedence and wrong-target failures, read-only isolation, stable output, duplicate IDs and claim races, stale tokens and claims, dirty/unsafe paths, recovery and different native Git identities. Integration must exercise actual native Git and representative agent environments. No live policy adoption or project changes as a test. The command contract is a proposal until reviewed and implemented; passing documentation checks do not establish runtime behavior.
