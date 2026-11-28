---
description: "Use when reviewing or implementing v1-to-v2 workspace migration mapping, quarantine holds, ID/counter rules, reports, and rollback."
---

# Dolt v2 Migration Mapping Design

This document defines the reviewable v1-to-v2 migration mapping, hold (quarantine), reporting, and rollback contract for Hive [#22](https://github.com/mols3131d/hive-system/issues/22) and acceptance gate `A15` in [#15](https://github.com/mols3131d/hive-system/issues/15). It extends the migration summary in the [MVP contract](dolt-mvp-contract.md#backup-sync-and-migration) against the pinned 24-table [relational schema](dolt-schema.md) ([`0001_initial.sql`](../../assets/schema/dolt/0001_initial.sql)) and the pinned [v1 workspace policies](../policies/workspace/README.md).

**Boundary:** This is a design and dry-run specification, not authorization for live migration, schema activation, policy-pin changes, or an authority switch. Existing v1 workspaces remain under their reviewed pins. The v2 workspace descriptor, user configuration, local resource bindings, and private Queen conversation metadata formats are owned by the separate v2 workspace-layout contract (`#18` / `#20`) and are not defined here.

## Principles and Non-Goals

- **Preserve originals read-only:** Inventory the v1 Git repository at one clean committed `HEAD` without modifying files, branches, refs, `AGENTS.md`, `hive.yml`, or XDG config.
- **No automatic repair or fabrication:** Never synthesize missing timestamps, reviewer approvals, outcomes, submission transitions, stop evidence, or scope ownership. Hold incomplete or ambiguous records for explicit operator reconciliation.
- **No automatic activation or live switch:** Dry-runs and staged imports target a new, isolated, inactive v2 database and Git asset tree. No live cutover occurs without separate explicit approval after old writers stop.
- **No dual canonical originals:** During migration rehearsal, v1 remains canonical and the v2 target is disposable/inactive. After an approved cutover, Dolt owns structured records and textual bodies, Git owns `assets/bots/<slug>/SOUL.md`, and the preserved v1 snapshot stays read-only for audit and rollback.
- **No private data or host leakage:** Reports, ID maps, and fixtures use portable workspace/record identifiers and digests, never credentials, host-local paths, or raw private content.

## Evidence Tiers

Distinguish three verification levels so design rehearsal is never mistaken for product acceptance or live readiness:

| Tier | Scope | Exact artifact / owner | What it proves | What remains outside it |
| --- | --- | --- | --- | --- |
| 1. Task-only synthetic design rehearsal | 8 synthetic v1 task bundles under `tasks/synthetic/` | [`tests/spikes/dolt_migration.py`](https://github.com/mols3131d/hive-system/blob/025b80fc749d06b6cc624bb85cee3c5b7c88f923/tests/spikes/dolt_migration.py) at `025b80fc749d06b6cc624bb85cee3c5b7c88f923` ([CI run](https://github.com/mols3131d/hive-system/actions/runs/45861306783), repeated on Linux/Dolt 2.7.6 in [#22](https://github.com/mols3131d/hive-system/issues/22#issuecomment-7214863736)) | Qualified `(<scope>, <work-id>)` task mapping (2 mapped, 6 held), SQL `ROLLBACK` leaving target empty, versioned `COMMIT`, `NOT NULL` rejection of undated `done`, unchanged read-only v1 Git source | Reserved scopes, multi-Area/Project/Resource links, directives, artifacts, Bot `SOUL.md`, historical Memory inventory, policy pins, `workspace_counters`, report/hash generation, production CLI |
| 2. Additional disposable synthetic verification | Remaining v1 surfaces and negative/failure cases on synthetic fixtures | Disposable synthetic fixtures under `#10`, `#12`, `#18`, `#20`, `#22` | Full mapping/hold rules across scopes, directives, artifacts, `SOUL.md`, deferred Memory inventory, policy pins, counters, report hashes/counts, and rollback on pinned Dolt 2.7.6 | Full end-to-end installed CLI workflow, multi-host verification, and disposable copy of real workspace structure |
| 3. Implemented product dry-run (`A15`) | Installed Rust CLI (`hive --contract 2`) alongside v1 in one XDG environment | Owned by [#15](https://github.com/mols3131d/hive-system/issues/15) (with `#10`–`#14`, `#18`, `#20`) | Contract-isolated v1/v2 XDG coexistence, production dry-run importer, report/ID-map/hash/rollback checks, and supported-host acceptance | Live workspace cutover and authority switch (separate explicit operator approval) |

## Surface Mapping and Hold Rules

All rules below read from one verified, clean v1 Git commit (`source_commit`). Any record that fails a rule is excluded from operational import, recorded in the hold ledger with its exact v1 path and `source_commit`, and left intact in the v1 source.

**Title selection (Tasks, Directives, and Artifacts):** V1 frontmatter `title` is optional ([frontmatter](../policies/workspace/frontmatter.md#common-fields)), whereas `0001_initial.sql` requires non-null `title` (`VARCHAR(255)`) on `issues`, `directives`, and `artifacts`. Resolve `title` deterministically from actual source inputs without making optional v1 `title` mandatory:

1. Use frontmatter `title` when present, non-empty, within 255 characters, and free of control characters.
2. Otherwise, use the record's first top-level Markdown H1 heading (`# ...`) when present, non-empty, within 255 characters, and free of control characters (after trimming surrounding whitespace).
3. Otherwise, use frontmatter `description` when non-empty and within 255 characters, or an explicit non-empty title in the reviewed migration manifest when source candidates are unsuitable or exceed 255 characters.
4. **Hold** (`missing_or_conflicting_title`) if no valid candidate exists or if inputs contradict each other (for example, frontmatter `title` and a reviewed manifest title conflict without an explicit reviewed override, or multiple conflicting H1 headings appear with no frontmatter `title` or reviewed selection).

### 1. Legacy Scopes, Projects, and Resource Links

V1 organizes records by `<project-id>` under `tasks/<project-id>/`, `directives/<project-id>/`, and `bots/<bot-id>/memory/<project-id>/`, with scope identity documented in each scope's `README.md` (or scoped `MEMORY.md`) and two reserved scopes: `_hive-userdata` and `_general` ([organization](../policies/workspace/organization.md#project-identity)).

| v1 source | Target v2 entity | Mapping rule | Hold / exclusion rule |
| --- | --- | --- | --- |
| Ordinary `tasks/<project-id>/README.md`, `directives/<project-id>/README.md` | `areas` (required), optional `projects`, optional `resources` + `area_resources` | Require an explicit reviewed scope-mapping manifest entry mapping `<project-id>` to a target Area (`id`, `slug`, `name`, optional `parent_id`). Never assume one repository equals one Area or infer Area/Project from directory or repo names. | **Hold** (`ambiguous_scope_or_collision`) if `<project-id>` is absent from the reviewed mapping, has conflicting canonical project references across `tasks/`, `directives/`, or `bots/*/memory/`, or collides on root/sibling Area slug. |
| Reserved `_general` | Explicit general `areas` row (for tasks/directives) | Map explicitly to a reviewed general Area (for example slug `general`); never use it as an implicit fallback for unresolved scopes. | **Hold** if any record under `_general` lacks explicit mapping confirmation. |
| Reserved `_hive-userdata` | Explicit workspace-operations `areas` row | Map explicitly to a dedicated Area distinct from the ordinary `hive-system` product Area. | **Hold** if conflated with `hive-system`. |
| Legacy pre-split `_hive` (older pinned workspaces) | None directly | Must be split by ownership (`_hive-userdata` vs `hive-system`) per [reserved scopes](../policies/workspace/organization.md#reserved-scopes) before migration. | **Hold** (`unsplit_legacy_scope`); never treat `_hive` as a current alias or fallback. |
| Notion-designated scopes ([collaboration backend](../policies/workspace/organization.md#collaboration-backend)) | None | External team/Notion backends retain their own canonical owners. | **Exclude** (`external_backend_owned`); do not mirror or import into Dolt. |
| Canonical repo/doc references in scope `README.md` | `resources` + `area_resources` (and `issue_resources` where explicitly linked) | Register portable `kind`, `name`, and canonical `uri` only. Never copy host-local paths or `.local/hive/config.yml` bindings into Dolt. | **Hold** if the canonical reference is ambiguous or local-path-only. |

Project membership (`projects` + `project_issues`) is created only when explicitly declared in the reviewed scope-mapping manifest. Removing or completing a v2 Project never closes or alters its member Issues.

### 2. Qualified v1 Tasks and Issue Provenance

V1 task bundles live at `tasks/<project-id>/backlog/<work-id>/README.md`, `tasks/<project-id>/active/<work-id>/README.md`, and `tasks/<project-id>/archive/<work-id>/README.md` ([tasks](../policies/workspace/tasks.md), [frontmatter](../policies/workspace/frontmatter.md#tasks)). Because `<work-id>` is unique only within `<project-id>`, every task is keyed by the qualified pair `(<project-id>, <work-id>)`.

| v1 path and state | Target v2 `issues.status` | Required provenance to import | Hold / quarantine rule |
| --- | --- | --- | --- |
| `tasks/<project-id>/backlog/<work-id>/README.md` (status omitted, no `claim`) | `backlog` | Explicit mapped `area_id`, resolved `title`, `created_at`, `updated_at` | **Hold** (`incomplete_historical_provenance`) if `created_at` or `updated_at` is missing/unparseable; **Hold** (`invalid_legacy_state`) if `status` or any `claim:` is present. |
| `tasks/<project-id>/active/<work-id>/README.md` with `status: todo`, **no `claim`** | `todo` | Explicit mapped `area_id`, resolved `title`, `created_at`, `updated_at`, and non-empty acceptance criteria (from task body or reviewed mapping) | **Hold** (`incomplete_historical_provenance`) if timestamps or required acceptance criteria are missing. |
| `tasks/<project-id>/active/<work-id>/README.md` with `status: in-progress`, OR any `claim:` on active `todo` or `review` | None (not imported operationally) | N/A | **Hold** (`execution_needs_reconciliation`). Never create an active `issue_claims` row during migration or silently drop a claim; confirm execution stopped and reconcile first. |
| `tasks/<project-id>/active/<work-id>/README.md` with `status: review` (unclaimed) | None (not imported operationally) | N/A | **Hold** (`missing_typed_submission_evidence`). V2 `review` requires a typed `issue_updates` (`kind = 'transition'`) submission record binding Claim/holder/stop/result evidence, which v1 free-form prose does not provide. |
| `tasks/<project-id>/active/<work-id>/README.md` with `status: done` (awaiting closure) or `archive/<work-id>/README.md` with `status: done` or `cancelled`, **no `claim`** | `done` or `cancelled` | Explicit mapped `area_id`, resolved `title`, `created_at`, `updated_at`, `closed_at`, `closed_by_ref`, `resolution_note` (required by `0001_initial.sql` `CHECK` constraint), and, for `done`, satisfied completion obligations below | **Hold** (`invalid_legacy_state`) if any `claim:` is present on active `done` or any `archive` terminal record (never silently strip a claim); **Hold** (`incomplete_historical_provenance`) if any required timestamp, `closed_by_ref`, or `resolution_note` is absent. Retain unknown dates as unknown; never manufacture closure fields. |
| Duplicate `(<project-id>, <work-id>)` across `backlog/`, `active/`, `archive/` | None | N/A | **Hold** (`duplicate_legacy_work_id`) for all conflicting bundles until resolved. |

Additional task-field mapping rules:

- **`type`:** V1 tasks do not carry an Issue type. Default to `task` unless the reviewed mapping manifest explicitly assigns `epic`, `feature`, `task`, or `bug`. Never assign `epic` automatically without verifying epic constraints (no `in-progress` status, no active claim).
- **`priority`:** Omit (`NULL`) when v1 `priority` is omitted or archived. Map `high` to `0`, `normal` to `1`, and `low` to `2` on backlog and active tasks to preserve their relative order. The imported priority carries the v2 scheduling semantics and does not establish authority.
- **`tags` -> `labels` / `issue_labels`:** Apply v2 label canonicalization (trim leading and trailing Unicode whitespace only; require non-empty `<= 100` chars with no control characters; preserve spelling, accents, and interior spaces). **Hold** malformed tags or SQL collation collisions rather than silently rewriting or dropping them.
- **Parentage, prerequisites, and `done` completion obligations (`issues.parent_id`, `issue_dependencies`):** Do not auto-parse free-form Markdown prose into graph edges. Populate only from reviewed mapping input, and validate that the combined completion-obligation graph (prerequisites plus parent-to-child completion) is acyclic. Every imported `done` Issue requires all direct prerequisites to be imported as `done` and all immediate children to be imported in a terminal state (`done` or `cancelled`). Distinguish **committed snapshot checks** from **historical scope proof**: a committed Dolt snapshot verifies structural graph acyclicity, `done` direct prerequisites, and terminal immediate children, whereas importing a `done` parent with a `cancelled` immediate child additionally requires explicit historical acceptance evidence of the changed scope (in v1 closure/review provenance or the reviewed mapping manifest). Never silently drop a prerequisite or parent/child link to make an Issue pass import: if a prerequisite or child is held/quarantined, violates the `done`/terminal requirement, or lacks cancelled-child scope acceptance proof, **hold** (`unresolved_completion_obligation`) the affected dependent or parent Issue together with its dependent links.
- **Closure retention:** Preserve v1 minimum 7-day closed-record retention; migration never deletes closed bundles.

### 3. Directives and Artifacts

#### Directives

V1 directives live at `directives/<project-id>/<slug>.md` or as a bundle at `directives/<project-id>/<slug>/README.md` with optional supporting Markdown files ([directives](../policies/workspace/directives.md), [frontmatter](../policies/workspace/frontmatter.md#directives)).

- **Navigation entrypoints:** `directives/README.md` and `directives/<project-id>/README.md` are navigation files, not directive records; exclude them from `directives` rows and retain only their scope metadata for inventory.
- **Scope mapping:** Map `<project-id>` via the reviewed scope table to exactly one v2 scope (`scope_kind = 'area'` with `include_descendants = FALSE` by default, or `'workspace'` / `'project'` only when explicitly designated in the reviewed mapping). Never auto-enable `include_descendants`.
- **Status and provenance:** Map v1 `status` (`proposed`, `active`, `superseded`, `withdrawn`) 1:1. `0001_initial.sql` requires `title` (per the title selection rule above), `issuer_ref`, `source_ref`, `created_at`, `updated_at`, and non-null `approval_ref` whenever `status = 'active'`, plus `expires_at > valid_from` when both are set and `supersedes_id IS NULL OR supersedes_id <> id`.
- **Supersession reversal and edge validation:** In v1, a `superseded` directive may name its replacement ([directives](../policies/workspace/directives.md#maintenance)); in v2, `directives.supersedes_id` is stored on the **replacement** directive and names the **replaced** directive. Reverse that edge direction during mapping when translating v1 replacement declarations into v2 `supersedes_id`. Validate the target for **every** non-null `supersedes_id` edge (without inventing extra v2 status restrictions beyond `0001_initial.sql`): **hold** (`invalid_supersession_chain`) any directive or chain with a missing, unresolved, held, self-referential, or cyclic endpoint, or with conflicting replacement declarations (such as mismatched old/new declarations or ambiguous multiple replacements for the same edge).
- **Hold rules:** **Hold** (`missing_directive_approval` / `incomplete_historical_provenance`) any `active` directive lacking `approval_ref`, any directive missing `issuer_ref`/`source_ref`/timestamps, or any expired `active` directive. Never silently downgrade an unverified `active` directive to `proposed`.
- **Bundle supporting files:** Supporting Markdown inside `directives/<project-id>/<slug>/` is detail/evidence, not an independently applicable directive. Preserve in the source Git ref or map explicitly as a linked Artifact; never promote it to a separate active directive.

#### Artifacts

V1 artifacts live under work-scoped `tasks/<project-id>/<area>/<work-id>/artifacts/` or standalone root `artifacts/` ([organization](../policies/workspace/organization.md#ownership), [frontmatter](../policies/workspace/frontmatter.md#artifacts), [lifecycle](../policies/workspace/lifecycle.md#artifact-retention)).

- **Textual Markdown artifacts:** Import valid UTF-8 Markdown into `artifacts` (`id`, `kind`, `title`, `body`, `author_ref`, `source_ref`, `revision`, `created_at`, `substantive_updated_at`, optional `archived_at` + `archive_reason`), resolving `title` per the title selection rule above.
- **`author_ref` provenance:** `0001_initial.sql` requires a non-null `artifacts.author_ref`. Free-form prose may not establish `author_ref`, and Git author email alone does not establish a Hive actor; **hold** (`incomplete_historical_provenance`) any artifact lacking verified structured author provenance unless supplied through the reviewed migration manifest.
- **Scope joins (`artifact_areas`, `artifact_projects`, `artifact_issues`, `artifact_resources`):**
  - Work-scoped artifacts link to their mapped `issue_id` (and its owning `area_id` only when explicitly specified). If the owning v1 task is held/quarantined, **hold** its work-scoped artifacts together with the task (`parent_task_quarantined`) so work-scoped evidence is not orphaned or stripped of task context.
  - Standalone `artifacts/` link only to explicitly mapped scopes/resources, or remain standalone with zero joins. Never widen joins during import, preserving v2's multi-scope body-disclosure gate.
- **`kind` vocabulary:** Map explicit v1 `kind` values (`report`, `research`, `review`, `handoff`, `message`, `checkpoint`, `work-note`, `manifest`) to the v2 bounded domain vocabulary (`research`, `design`, `decision`, `report`, `review`, `handoff`, `evidence`, `work-note`, plus retained legacy mappings `message`, `checkpoint`, `manifest` as documented in [relational schema](dolt-schema.md#additional-ddl-candidate)). **Hold** (`unclassified_artifact_kind`) any artifact with an omitted v1 `kind` unless a reviewed explicit kind mapping exists in the migration manifest, because `0001_initial.sql` requires a valid non-null `artifacts.kind`. Also **hold** unknown or conflicting `kind` values rather than guessing.
- **Binary or externally owned files:** V2 MVP has no managed binary blob store. Do **not** inline binary files or large external assets into `artifacts.body`. Reference them via `resources` + `artifact_resources`/`issue_resources` when their canonical owner is verified, or **hold** (`unresolved_binary_or_external_owner`) while keeping the file intact in the read-only v1 snapshot.
- **Retention preservation:** Preserve v1's 3-month retention clock via `substantive_updated_at` (derived from actual v1 creation, substantive update, or owning work closure/cancellation — never reset to migration time). **Hold** (`incomplete_historical_provenance`) if `created_at` or `substantive_updated_at` cannot be established without fabrication. Map v1 `status` (`active`, `parked`, `closed`) as provenance metadata only; never delete expired artifacts during migration.

### 4. Bot `SOUL.md` and Historical Memory (Curated Ops Deferred)

V1 bot records live under `bots/<bot-id>/` with `README.md`, `SOUL.md`, root navigation `MEMORY.md`, and scoped memory files `memory/_general/MEMORY.md`, `memory/_hive-userdata/MEMORY.md`, `memory/<project-id>/MEMORY.md`, and `memory/<project-id>/<topic>.md` ([bots](../policies/workspace/bots.md)).

#### Registered Queens and `SOUL.md`

- **Identity mapping:** Each eligible v1 Manager Bot maps to one `bots` row (`id` UUIDv7, immutable lowercase kebab-case `slug` = `<bot-id>`, `display_name`, `revision`, `retired_at`) and one Git-tracked file at `assets/bots/<slug>/SOUL.md` in the target v2 workspace's native Git asset tree. No role column or role directory is added.
- **Commit ordering:** Stage, validate (UTF-8, safe relative path, no traversal or symlink escape, identity agreement with registry), and commit `assets/bots/<slug>/SOUL.md` in the target asset Git tree **before** inserting the `bots` row, recording the exact asset Git commit in migration provenance.
- **Management-context exclusion:** `bots/<bot-id>/README.md` (routing and runtime bindings) and any `AGENTS.md` are **never** merged into `assets/bots/<slug>/SOUL.md` or runtime Queen context. Retain them read-only in the v1 snapshot (or as non-runtime management notes outside the Soul activation path).
- **Hold rules:** **Hold** (`invalid_soul_or_slug`) if `<bot-id>` is not valid lowercase kebab-case, collides with another active or retired slug, is missing `SOUL.md`, fails UTF-8/path safety, represents a non-Manager worker record, or contains unresolved conflicting identity fields.

#### Historical Memory (Curated Operations Deferred)

Under [#34](https://github.com/mols3131d/hive-system/issues/34) and [#42](https://github.com/mols3131d/hive-system/pull/42), Bot-specific curated Memory operations (`memory` CLI commands, logical `memories/LATEST.md`, `memories/core/`, `memories/references/`, and `A17`) are **proposed deferred beyond MVP dogfooding**, while the pinned `bot_memories` table in `0001_initial.sql` and v1 file-memory compatibility/migration prerequisites remain intact.

- **Default MVP migration posture:** **Inventory read-only and hold/defer (`curated_memory_deferred`)** all v1 `bots/<bot-id>/memory/**` files. Do **not** populate `bot_memories`, do **not** write physical memory files into v2 `assets/bots/<slug>/`, and do **not** import v1 memory into private Queen `conversations/`. Preserve all v1 memory files untouched in the read-only v1 Git snapshot.
- **Navigation vs facts:** Classify a root or scoped `MEMORY.md` as `navigation_only` only when the pinned policy and file contents establish that it serves navigation. Inventory substantive or ambiguous content as held and preserve it in the source snapshot.
- **Preserved post-MVP mapping prerequisites (only if curated Memory is enabled in a later reviewed adoption):**
  - Scope mapping: `memory/_general/` -> `area_id = NULL` (global); `memory/_hive-userdata/` and `memory/<project-id>/` -> explicit mapped `area_id`.
  - Provenance and dates: require non-empty `source_ref` and `author_ref`; preserve unknown observation dates as `observed_at = NULL` (never fabricate a date); set `authorization_mode = 'reviewed'` only with real `reviewed_by_ref` and `reviewed_at` (never fabricate a reviewer).
  - Uniqueness and quarantine: enforce live `(bot_id, area_id, topic_key)` uniqueness including `NULL` global scope; **quarantine** ambiguous scopes, duplicate topic keys, conflicting facts, missing sources, or sensitive/instruction-like payloads.

### 5. Workspace Policy Pins and Configuration Boundaries

V1 workspaces record their pinned system policy revision in workspace `AGENTS.md`, their coordination contract in tracked root `hive.yml` (`version: 1`, `coordination.mode`), and local environment bindings in `~/.config/hive/config.yml` and optional project-local `.local/hive/config.yml` ([system design](system-design.md#configuration-and-identity)).

- **Inventory capture:** Read and record the v1 `source_commit`, the pinned system policy revision from `AGENTS.md`, the `hive.yml` schema version and coordination mode (if present), and any `.rulesync/` / `rulesync.jsonc` presence.
- **Isolation:** Dry-runs and staged imports never edit v1 `AGENTS.md`, `hive.yml`, `~/.config/hive/config.yml`, or `.local/hive/config.yml`. V1 and v2 keep separate versioned XDG config/data/state roots (`hive --contract 2` vs v1).
- **No invented v2 formats:** V2 workspace descriptor, user config, local binding, and private conversation metadata schemas are owned by the separate v2 workspace-layout contract (`#18` / `#20`). Migration tooling must consume that reviewed contract rather than defining ad hoc descriptor or binding files.
- **Cutover gate:** Updating a workspace's policy pin or switching active authority from v1 to v2 is never performed by dry-run or import; it requires separate explicit operator approval after old writers and executions are verified stopped.

#### Source preflight

On Linux, run the source preflight against a disposable, authorized v1 copy with a standalone local Git object store. Supply the absolute worktree root and its full lowercase commit ID explicitly.

```sh
hive --contract 2 --json workspace migration-preflight \
  --source "$source_root" --source-commit "$source_commit"
```

The command verifies that HEAD matches the selected commit and checks for staged, modified, and untracked entries before and after inventory. It refuses linked worktrees, alternate object stores, submodules, tracked symlinks, and index flags that can hide worktree changes. It reads the policy pin and coordination mode through the v1 compatibility rules from the selected commit. `hive_yml` is null for the supported older pin that predates tracked coordination configuration. A committed remote mode describes the configuration only.

Preflight checks a private copy of the source index against the verified commit. It requires a standalone Git index of at most 16 MiB and refuses indexes that depend on a separate shared index file. The only ignore rules for untracked files come from per-directory `.gitignore` files and a private copy of `.git/info/exclude`. Git gives per-directory rules priority over the copied exclude file. When present, `.git/info` must be a directory and `.git/info/exclude` must be a regular file of at most 64 KiB. Both paths must be free of symlinks.

Preflight walks the selected commit's snapshot. It verifies each tree and blob against its Git object ID through an isolated local reader before reporting success. It rejects an unavailable, unreadable, or mismatched object and supports loose and packed SHA-1 and SHA-256 objects.

A successful `source_preflight` report gives the Git tree ID, SHA-256 of the raw root tree, committed file and directory counts, total blob size, and committed Rulesync presence. The catalog SHA-256 covers path-sorted entries encoded as path, mode, object ID, and size (or `-` for a directory), each terminated by a NUL byte. The catalog limits are 4 MiB of encoded entries and 10,000 paths. Inventory accepts at most 1 MiB per tree and 64 MiB of decoded tree and blob contents combined. Reused trees count once per path and reused blobs count once per object ID toward the verification budget. The reported total blob size counts each file path. The report omits source paths, record bodies, and remote aliases.

This slice covers source selection and inventory. Per-record raw digests, mapping, holds, target validation, rollback, and staged import remain required for A15.

### 6. Issue Counters and Number Non-Reuse

V1 identifies tasks by `(<project-id>, <work-id>)` with no workspace-wide numeric sequence. V2 assigns each imported Issue a fresh lowercase UUIDv7 (`issues.id`) and an immutable positive workspace-wide `issues.number` (`BIGINT UNSIGNED > 0`), backed by `workspace_counters` (`name = 'issue_number'`, `next_value`).

- **Deterministic allocation:** Sort qualified, non-held v1 tasks deterministically (by reviewed manifest order, defaulting to lexicographical `(<project-id>, <work-id>)`) and allocate contiguous numbers `1..N` inside the single guarded import transaction.
- **Counter synchronization:** In the same transaction, set `workspace_counters.next_value = N + 1` (or `1` when `N = 0`). Never derive numbers from `MAX(number) + 1` or allocate offline on replicas.
- **Quarantined tasks:** Do **not** allocate `issues.number` to held/quarantined tasks during initial import. When an operator later reconciles and imports a held task through a normal guarded operation, it receives the next counter value from `workspace_counters`.
- **Non-reuse invariant:** Once an import target is promoted or accepts subsequent writes, an assigned `issues.number` is never reused—even if an Issue is cancelled or reconciled later. If a pre-cutover dry-run rolls back cleanly before promotion, the disposable target is discarded and no numbers are published.

## Minimum Migration Report, Content Hashes, ID Map, and Rollback

Every dry-run and staged import must emit a machine-readable JSON report (plus optional human summary) containing **only nonsecret metadata, counts, IDs, and SHA-256 digests** (no credentials, host-local paths, or raw private bodies).

### Minimum Report Fields

1. **Provenance header:**
   - `source`: logical workspace label, `source_commit` (exact v1 Git SHA), `source_clean` (`true` required), pinned v1 policy revision (`AGENTS.md`), `hive.yml` version and coordination mode (or `absent`).
   - `target`: `workspace_id`, Dolt version, `schema_version` (`1`), `migration_digest` (SHA-256 of `0001_initial.sql`), `base_dolt_commit`, `confirmed_dolt_commit` (or `null` on rollback), `asset_git_commit` (for `assets/bots/<slug>/SOUL.md`), `issue_number_next_value`.
   - `mode`: `dry_run` or `staged_import`, `observed_at` (UTC).
2. **Surface counts (reconciled inventory):** For each surface (`scopes`, `tasks`, `directives`, `artifacts_work_scoped`, `artifacts_standalone`, `bots_soul`, `bot_memories_historical`, `navigation_only`), report `inventoried`, `imported`, `held`, and `excluded_non_record`, satisfying `inventoried == imported + held + excluded_non_record`. Break down `tasks` by v1 lifecycle directory (`backlog`, `active`, `archive`) and v2 target status (`backlog`, `todo`, `done`, `cancelled`).
3. **Content hashes:** For every imported record (`issues`, `directives`, `artifacts`, `assets/bots/<slug>/SOUL.md`), record:
   - `source_path` and `source_git_blob_sha`,
   - `source_raw_sha256` (exact v1 UTF-8 file bytes),
   - `target_body_sha256` over the exact Dolt body bytes after the documented metadata extraction. For Soul record the SHA-256 of the committed file and compare it with the source file bytes. Keep the original Soul frontmatter in storage and strip it only during runtime context rendering.
4. **Legacy ID map:** Deterministic table mapping each qualified v1 key to its v2 target identifier(s):
   - Scope `<project-id>` -> `area_id` (and optional `project_id` / `resource_id` list),
   - Task `(<project-id>, <work-id>)` -> `issue_id` (UUIDv7) and `issue_number`,
   - Directive `directives/<project-id>/<slug>` -> `directive_id`,
   - Artifact `<v1-artifact-path>` -> `artifact_id` (or `resource_id` for external/binary reference),
   - Bot `<bot-id>` -> `bot_id`, `slug`, and `assets/bots/<slug>/SOUL.md` blob SHA.
5. **Hold (quarantine) ledger:** Exact `source_path`, `source_commit`, qualified legacy key, typed `reason_code`, and required operator reconciliation action.
6. **Post-import invariant checks:** Schema/constraint validation pass, combined completion-obligation graph acyclicity pass, `done` Issue completion-obligation checks (all direct prerequisites `done`, all immediate children terminal, plus recorded historical cancelled-child scope proof), `issue_claims` count `== 0`, `workspace_counters.next_value == max(issues.number) + 1` (or `1`), retention timestamps preserved (`substantive_updated_at`, `closed_at`), and source Git `HEAD`/clean-status unchanged. Full `#10` read-only Dolt validation inspects **committed** snapshots only, not uncommitted SQL transaction state: run it on a staged import only after committing the staged transaction to the isolated, inactive copy and before any promotion decision (an open rolled-back dry-run transaction runs SQL/transaction-level checks only and requires a corresponding committed disposable copy for full `#10` validation).

### Synthetic Report Excerpt (Illustrative)

```json
{
  "mode": "dry_run",
  "source": {
    "source_commit": "0000000000000000000000000000000000000001",
    "source_clean": true,
    "policy_pin": "v1-synthetic-pin",
    "hive_yml_mode": "local"
  },
  "target": {
    "workspace_id": "01900000-0000-7000-8000-000000000001",
    "schema_version": 1,
    "issue_number_next_value": 3,
    "active_claims": 0
  },
  "counts": {
    "tasks": { "inventoried": 8, "imported": 2, "held": 6, "excluded_non_record": 0 },
    "bot_memories_historical": { "inventoried": 3, "imported": 0, "held": 2, "excluded_non_record": 1 }
  },
  "id_map": [
    {
      "kind": "task",
      "legacy_key": "synthetic/planned",
      "target_id": "01900000-0000-7000-8000-000000000011",
      "issue_number": 1,
      "source_raw_sha256": "<sha256>",
      "target_body_sha256": "<sha256>"
    }
  ],
  "holds": [
    {
      "legacy_key": "synthetic/running",
      "source_path": "tasks/synthetic/active/running/README.md",
      "reason_code": "execution_needs_reconciliation"
    }
  ]
}
```

### Rollback Contract

| Phase | Required rollback behavior |
| --- | --- |
| Pre-cutover dry-run or failed staged import | Roll back the SQL transaction (leaving target Dolt `HEAD` at `base_dolt_commit` with zero imported rows) and discard or isolate the disposable target directory. Verify v1 `source_commit` and clean working tree are untouched. |
| Post-cutover, **zero** v2 operational writes after promotion | Stop v2 writers under lock, verify target Dolt `HEAD` and asset Git `HEAD` match the cutover commits with no post-cutover `operation_receipts`, mark v2 target inactive, and re-enable v1 at the preserved read-only `source_commit` with explicit operator approval. |
| Post-cutover, **after** any v2 operational write | **Refuse silent rollback.** Stop v2 writers, export/reconcile all new or modified v2 records and confirmed receipts against v1, and require explicit operator approval for any reverse transition. |

Never delete the v1 source snapshot, rewrite Git history, or auto-promote an inactive restored or migrated target.

## Remaining Synthetic Coverage Matrix

Tier 1 (`tests/spikes/dolt_migration.py` at `025b80fc749d06b6cc624bb85cee3c5b7c88f923`) covers only the 8 synthetic task records under `tasks/synthetic/`. The remaining coverage required before closing `A15` is:

| Coverage item | Synthetic fixture scenario | Tier | Expected owner |
| --- | --- | --- | --- |
| Reserved scopes, multi-Area/Project/Resource mapping, and collisions | Synthetic `_general`, `_hive-userdata`, ordinary project, unsplit `_hive` (held), Notion-backed scope (excluded), root slug collision (held), and portable `resources` links | Tier 2 -> Tier 3 | `#22` (design), `#10`/`#12` (validation/import), `#15` (`A15`) |
| Task claims in invalid states and deterministic title selection | Legacy `claim:` on `backlog`, `active/done`, and `archive` terminal records blocked as `invalid_legacy_state`; claimed active `todo`/`review`/`in-progress` held as `execution_needs_reconciliation`; source-backed title resolution across optional `title`, H1 heading, `description`, and reviewed override, plus hold on missing/over-length/contradictory title inputs without requiring optional v1 `title` | Tier 2 -> Tier 3 | `#10`/`#12`, `#15` (`A15`) |
| Terminal `done` completion obligations and committed `#10` validation | Imported `done` Issue with non-`done` or held prerequisite held; `done` parent with non-terminal or held child held; `done` parent with `cancelled` child held without explicit historical scope-acceptance proof and imported when proof is present; dependent links/records held rather than dropped; full `#10` read-only validation run against the committed isolated staged copy prior to promotion (not uncommitted transaction state) | Tier 2 -> Tier 3 | `#10`/`#12`, `#15` (`A15`) |
| Directives approval, scope, and supersession reversal | `active` with `approval_ref`, `active` missing `approval_ref` (held), expired `active` (held), `proposed`/`withdrawn`; v1 `superseded` naming replacement reversed onto replacement's v2 `supersedes_id`; non-null `supersedes_id` target validated for every status; missing/held/self/cyclic endpoints and conflicting replacement declarations held; navigation `README.md` excluded; bundle supporting file not promoted | Tier 2 -> Tier 3 | `#10`/`#12`, `#15` (`A15`) |
| Artifacts (`kind`, `author_ref`, scope, binary, retention) | Omitted v1 `kind` held as `unclassified_artifact_kind` unless mapped in reviewed manifest; prose-only or Git-email-only `author_ref` held; work-scoped artifact linked to mapped Issue vs held with quarantined parent task; standalone multi-scope artifact preserving disclosure boundary; binary asset refused from `artifacts.body`; `substantive_updated_at` preserved (never reset to import time) | Tier 2 -> Tier 3 | `#10`/`#12`, `#15` (`A15`) |
| Bot `SOUL.md` and management-file exclusion | Valid Queen `bots/<slug>/SOUL.md` -> committed `assets/bots/<slug>/SOUL.md` + `bots` row; `README.md`/`AGENTS.md` excluded; duplicate/invalid slug or symlink escape held | Tier 2 -> Tier 3 | `#10` (Queen registry), `#20` (committed Soul & exclusion), `#15` (`A15`) |
| Historical Memory read-only inventory (curated ops deferred) | Classify a verified root navigation file as `navigation_only`. Hold a substantive root file and scoped memory while curated Memory is deferred. Verify `bot_memories` remains empty | Tier 2 -> Tier 3 | `#20` (v1 memory compatibility), `#15` (`A15`) |
| Policy pins and v1/v2 XDG coexistence | One XDG environment with both v1 (`config.yml`, `hive.yml`, `AGENTS.md`) and v2 workspaces; `hive --contract 2` resolves v2 without mutating v1 pins or config | Tier 3 | `#18` (XDG/layout), `#15` (`A15`) |
| Issue counters and number non-reuse | Deterministic `1..N` allocation, `workspace_counters.next_value == N + 1`, quarantined tasks consume no numbers, post-import create/cancel never reuses an issued number | Tier 2 -> Tier 3 | `#10`/`#12`, `#15` (`A15`) |
| Minimum report, content hashes, ID map, and rollback | JSON report accounting identity, raw vs stored SHA-256 match, deterministic ID map, rollback on injected failure leaving target empty and v1 unchanged, post-write rollback refusal | Tier 2 -> Tier 3 | `#12`/`#15` (`A15`) |

## Importer handoff

Use a versioned nonsecret scope mapping input that binds the exact source commit and policy pin to the selected inactive target workspace ID. Include explicit legacy scope keys, target Areas and any selected Project or Resource mappings. Require the operator to review any authorized override for an ambiguous record. The importer work under #12 and #15 owns the final wire encoding and parser checks.

Hold an unclaimed legacy todo without acceptance criteria by default. A reviewed mapping can explicitly place it in backlog for later selection. Hold an artifact without valid required author provenance or with an omitted v1 `kind` unless explicitly mapped. Record a verified author reference and explicit kind through the reviewed mapping before importing it. Free-form prose and Git author email alone do not establish `author_ref`.
