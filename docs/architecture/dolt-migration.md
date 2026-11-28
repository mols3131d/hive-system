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

Preflight checks a private copy of the source index against the verified commit. It preserves the index modification time on the copy so Git detects same-size edits made within the index timestamp window. It requires a standalone Git index of at most 16 MiB and refuses indexes that depend on a separate shared index file. The only ignore rules for untracked files come from per-directory `.gitignore` files and a private copy of `.git/info/exclude`. Git gives per-directory rules priority over the copied exclude file. When present, `.git/info` must be a directory and `.git/info/exclude` must be a regular file of at most 64 KiB. Both paths must be free of symlinks.

Preflight walks the selected commit's snapshot. It verifies each tree and blob against its Git object ID through an isolated local reader before reporting success. It rejects an unavailable, unreadable, or mismatched object and supports loose and packed SHA-1 and SHA-256 objects.

A successful `source_preflight` report gives the Git tree ID, SHA-256 of the raw root tree, committed file and directory counts, total blob size, and committed Rulesync presence. The catalog SHA-256 covers path-sorted entries encoded as path, mode, object ID, and size (or `-` for a directory), each terminated by a NUL byte. The catalog limits are 4 MiB of encoded entries and 10,000 paths. Inventory accepts at most 1 MiB per tree and 64 MiB of decoded tree and blob contents combined. Reused trees count once per path and reused blobs count once per object ID toward the verification budget. The reported total blob size counts each file path. The report omits source paths, record bodies, and remote aliases.

This slice covers source selection and inventory. Per-record raw digests, mapping, holds, target validation, rollback, and staged import remain required for A15.

#### Read-only scope plan

On Linux, pass a reviewed scope mapping file and the intended inactive workspace ID alongside the same explicit v1 source selection.

```sh
hive --contract 2 --json workspace migration-plan \
  --source "$source_root" --source-commit "$source_commit" \
  --workspace-id "$workspace_id" --mapping-file "$mapping_file"
```

The command repeats source preflight and plans directly from its verified inventory. The mapping is a regular UTF-8 JSON file of up to 64 KiB. Version 1 requires the fields shown below, with the exact selected Git commit, the policy pin found in the committed source, and the explicitly selected lowercase workspace UUID. Use named JSON objects for the mapping, each scope and decision, each Area, and each project evidence entry. Positional arrays in place of these objects, unknown fields, repeated JSON members, unsupported versions, mismatched bindings, and mapping keys absent from the verified scope inventory are errors.

```json
{
  "version": 1,
  "source_commit": "1111111111111111111111111111111111111111",
  "policy_pin": "0e584830a7eff314d85b7af8c6f8cd69e735730a",
  "workspace_id": "00000000-0000-7000-8000-000000000001",
  "scopes": [
    {
      "scope": "example-app",
      "decision": {
        "kind": "area",
        "area": {
          "id": "00000000-0000-7000-8000-000000000002",
          "slug": "applications",
          "name": "Applications"
        },
        "project_evidence": [
          {
            "path": "tasks/example-app/README.md",
            "canonical_ref": "https://forge.example/projects/example-app"
          }
        ]
      }
    },
    {
      "scope": "example-external",
      "decision": {"kind": "external"}
    }
  ]
}
```

- Scope keys come from committed directories under `tasks/`, `directives/`, and `bots/*/memory/`. Ordinary scopes require one reviewed `project_evidence` entry for the committed `README.md` or scoped `MEMORY.md` in every such directory. A nonportable Bot ID or Memory scope component holds the affected Area mapping. Supply the same literal `canonical_ref` from each verified UTF-8 entrypoint of up to 64 KiB. The Markdown scan accepts standalone HTTPS URLs in prose, inline links and standalone autolinks. It skips frontmatter, comments, images and code. A second distinct URL in the scanned prose causes a hold even if it may serve another purpose.
- A raw HTML block can enclose later Markdown. The scanner skips comment-only blocks and literal `<div>` or `<div hidden>` wrappers whose text has no nested markup and whose exact `</div>` terminator is in the same Markdown block. Other raw HTML blocks hold the entrypoint.
- The scanner skips a prose block containing an exact inline `<code>` or `<span hidden>` pair when its matching closer appears in that block and the pair crosses no Markdown element boundary or nested raw tag. Other raw inline tags and incomplete pairs hold the entrypoint.
- Portable references in this planner use a dotted ASCII DNS hostname with at most 63 octets per label and 253 octets in full, plus a project path. Omit a terminal dot, credentials, queries and fragments. Each percent escape must contain two hexadecimal digits. Host-local URLs, missing evidence, unrecognized reference forms and conflicting entrypoints hold the scope. V1 prose and entrypoints stay intact.
- Confirm `_general` and `_hive-userdata` with explicit Area decisions and omit `project_evidence` for them. Supply a distinct Area for `_hive-userdata` and `hive-system`. The planner always holds `_hive` as `unsplit_legacy_scope`. Use an `external` decision for an ordinary scope with an explicit external ownership designation. This excludes the scope as `external_backend_owned`.
- Areas require a lowercase UUIDv7, a lowercase kebab-case slug of up to 100 bytes, and a nonblank name of up to 255 characters without control characters. Omit `parent_id` or use null for a root, or supply a distinct lowercase UUIDv7. Missing or repeated scope decisions, missing or conflicting project evidence, duplicate Area IDs, and duplicate root or sibling slugs hold the affected scopes as `ambiguous_scope_or_collision`.

A completed plan returns `mode: scope_plan`, the verified source and inventory summary, the declared target workspace ID, the mapping version and its raw SHA-256, and scope outcomes ordered by legacy key. Each outcome lists committed scope directories and either the proposed Area's ID, slug and parent ID or a hold or exclusion reason. Exit status zero means classification completed, including when some scopes are held. The report omits record bodies, canonical references, free-form Area names and host-local file paths.

The report emits portable legacy scope keys in `scope` and their portable relative directories in `source_paths`. A malformed scope key appears as `scope_sha256`. Bot IDs and ordinary Memory scope components use lowercase kebab-case of up to 100 bytes and exclude Windows device names. Memory scopes also allow `_general` and `_hive-userdata`. The report lists paths with a nonportable component in `source_path_sha256s` in original path order. Each locator is the lowercase SHA-256 of the exact UTF-8 scope key or full relative directory path, with no separator, newline or normalization added. To resolve a locator locally, enumerate scope directories from the report's exact `source_commit` in the authorized v1 repository. Hash the matching kind of field from the committed tree and compare it with the locator.

The workspace ID identifies the intended target for review. Actual target identity, Area ancestry, and target constraints require validation at staging.

##### Optional Resource bindings

Add the following top-level `resource_bindings` object to a version 1 scope mapping to prepare an explicit `resources` row and its `area_resources` link. Omit the object to leave Resource preparation out of the plan.

```json
{
  "version": 1,
  "bindings": [
    {
      "scope": "example-app",
      "area_id": "00000000-0000-7000-8000-000000000002",
      "resource": {
        "id": "00000000-0000-7000-8000-000000000003",
        "kind": "git-repo",
        "name": "Example App Repository"
      },
      "evidence_path": "tasks/example-app/README.md",
      "evidence_blob_oid": "2222222222222222222222222222222222222222"
    }
  ]
}
```

Version 1 accepts the JSON string `"git-repo"` for `kind`. Supply the exact lowercase UUIDv7 of the reviewed Resource and the UUIDv7 of its mapped Area. Use a reviewed display name of 1–255 characters with no control characters or surrounding whitespace. Use the exact README path and committed Git blob OID from a `tasks/` or `directives/` entrypoint in that scope's verified project evidence. The same source commit, policy pin, and destination workspace bind both decisions. The planner uses the literal canonical HTTPS reference already verified for the scope and checks that it fits the schema's `TEXT` field. The binding accepts only the evidence locator. It performs no network access, URL rewriting, or link inference.

The section requires a `bindings` array of objects. Each entry requires a `resource` object. The section rejects nulls, unknown fields, repeated members, unsupported versions or kinds, invalid IDs or names, and unknown scopes. A binding to a held or external scope, a different Area ID, or a missing or mismatched verified README or blob is held. The planner holds otherwise eligible bindings whose Resource ID or Unicode-lowercased name appears more than once in the section. Submit unique bindings. Resource deduplication, additional kinds, and evidence from Memory or record links are outside version 1.

The report lists Resource outcomes in manifest order. A prepared outcome contains the Resource ID, Area ID and SHA-256 of the exact URI bytes. A held outcome contains the Resource ID and a reason code. Prepared rows remain internal, including the Resource name, kind, and URI. Preparing an Area link creates no Project membership, Issue link, or standalone Artifact link provenance.

##### Optional Projects and Issue memberships

Add the top-level `project_membership` object to a version 1 scope mapping to select reviewed Projects and their explicit Issue memberships. The same mapping header binds these decisions to the verified Git commit, v1 policy pin, and target workspace ID.

```json
{
  "project_membership": {
    "projects": [
      {
        "id": "00000000-0000-7000-8000-000000000004",
        "name": "Example Delivery",
        "status": "active"
      }
    ],
    "memberships": [
      {
        "project_id": "00000000-0000-7000-8000-000000000004",
        "legacy_key": {"scope": "example-app", "work_id": "verify-release"},
        "source_git_blob_sha": "2222222222222222222222222222222222222222",
        "decision": "link"
      }
    ]
  }
}
```

Each Project has a distinct lowercase UUIDv7, a reviewed name of 1–255 characters with no control characters or surrounding whitespace, and an explicit `active`, `completed`, or `archived` status. An optional description must be nonblank, fit the 65,535-byte SQL `TEXT` limit, and use only tab and line endings as control characters. Both arrays take named objects. Use JSON strings for Project `status` and membership `decision`. A null section, positional arrays in place of objects, duplicate JSON members, unknown fields, malformed identifiers, and unsupported decisions fail as invalid mapping input. Omit the section to leave Projects outside the plan or stage. Use an empty `memberships` array to review Projects independently.

Use the Task intake's exact qualified key and committed README blob ID for each membership. Select `link` to import it when both endpoints are prepared, `unresolved` to hold an undecided relationship, or `exclude` to hold an explicitly excluded relationship. Tasks can join more than one Project through separately reviewed rows. A Project can span Areas through its member Issues. Directory names and Area or Resource mappings supply no membership decisions.

Pass `--tasks` to evaluate membership endpoints. Without it, Projects can prepare and membership endpoints remain unresolved. Held or absent Projects and Tasks, missing README evidence, and undecided links hold as `unresolved_project_membership`. An explicitly excluded link or an externally owned Task holds as `excluded_project_membership` once its source binding is verified. A stale blob holds as `project_membership_binding_mismatch`. Duplicate Task records hold a membership as `ambiguous_project_member_task`. Repeated Project IDs or repeated membership pairs hold every row in that group. The reason distinguishes duplicate declarations from conflicting definitions, blob IDs, or decisions.

The Project section reports counts and outcomes in manifest order. It uses Project IDs, digests of names and descriptions, and the same qualified-key digest as staged Tasks. A prepared membership only receives its Issue ID in staging from the existing qualified Task assignment. The decision does not change Task eligibility, numbering, status, or Area. A Task held by Task preparation receives no Issue ID or number.

#### Read-only Task intake and preparation

Add `--tasks` to the scope plan command to classify Task bundles in the same verified snapshot using the same scope decisions and mapping file. The result adds `task_intake` with `coverage: partial`, counts of inventoried, prepared, held, and excluded bundles, and outcomes sorted by `(scope, work_id, source_path)`. Prepared, held, and excluded counts sum to the inventoried count. It inventories immediate bundle directories in the three v1 lifecycle directories. Scope navigation and supporting files stay in the verified source inventory. An absent regular `README.md` yields the bundle path, null blob digests, and a `missing_task_entrypoint` hold when the bundle otherwise qualifies for inspection.

Every outcome uses the report's exact `source.source_commit` and the entrypoint's source-relative path, Git blob ID, and raw SHA-256 when present. For a malformed scope or work ID, the Task row carries the exact path's UTF-8 SHA-256 in `source_path_sha256` and omits `legacy_key` and `source_path`. Row counts and ordering still use the full verified inventory.

External ownership excludes the Task. For all other scopes, duplicate qualified keys across lifecycle directories hold every conflicting bundle, and unresolved scope decisions carry through to their Tasks even when a duplicate hold also applies. When the status and claim are independently readable under the committed pin, the planner retains an execution reconciliation hold alongside unrelated metadata errors, including repeated unrelated keys. A repeated status, claim, claim ID, or claim holder makes the execution evidence ambiguous. The planner supports both preflight policy pins and applies claim rules according to the committed pin. Titles, body and criteria text, claim details, and review or closure text stay in the source.

The Task planner uses a valid frontmatter title first. With that field omitted it can use a single leading unindented ATX H1 before any other Markdown block or reference definition or the description. An invalid string in the frontmatter title field, conflicting headings, and ambiguous heading structure require explicit reviewed override. Malformed frontmatter, including a null or non-string title, still yields `unsupported_task_metadata` and cannot be rescued by a title decision.

To prepare backlog, unclaimed todo, or terminal Tasks supported by intake, add an optional top-level `task_reviews` array to the version 1 scope mapping. First run intake to get each qualified key and its committed README blob ID. Each review uses the following fields.

| Field | Reviewed input |
| --- | --- |
| `legacy_key` | Object with `scope` and `work_id` exactly as shown in intake. |
| `source_git_blob_sha` | Full lowercase Git blob ID for that Task's committed README. |
| `area_id` | The exact UUIDv7 from that scope's resolved Area mapping. The manifest header binds the reviews to the verified source commit, policy pin, and intended workspace ID. |
| `issue_type` | Optional canonical JSON string, one of `epic`, `feature`, `task`, or `bug`. Omission selects `task`. Reviewed type decisions preserve all lifecycle and Claim holds. |
| `title` | Optional named JSON object with `mode` and `value`. Use `"fallback"` when the source has no usable title and has neither an invalid frontmatter title nor ambiguous headings. Use `"override"` to explicitly replace a source title or resolve an ambiguous or invalid source title. The value must be nonblank, at most 255 characters, and free of control characters. |
| `relations` | Use the reviewed relation object below for terminal Tasks and for any Task with edges. For independent backlog and todo Tasks, `"no_edges"` remains available after reviewing all incident relations across the snapshot. Use `"unresolved"` or omit the field when obligations are uncertain or still under review. |
| `created_at`, `updated_at` | Objects with integer `start` and `end` selecting the respective date values in the body. Use the exact RFC 3339 literals with a known offset and up to six fractional digits. Dates must fit the target UTC date range and the update must be at or after creation. Missing, malformed, unrepresentable, or reversed dates hold as `incomplete_historical_provenance`. |
| `acceptance_criteria` | The same range format, selecting the exact nonblank criteria text. Required for todo and optional otherwise. A required but missing range or an invalid range holds as `unverified_acceptance_criteria`. The selected text must fit the target SQL `TEXT` byte limit. |
| `closure` | Required for active or archived done and archived cancelled Tasks. Supply `closed_at`, `closed_by_ref`, and `resolution_note` as body ranges. Select a recorded closure date at or after creation, an explicit actor reference of at most 255 characters, and the exact nonblank outcome text within the SQL `TEXT` byte limit. Preserve a later recorded update after closure. A missing or invalid closure field holds as `incomplete_historical_provenance`. |

Ranges are zero-based, half-open UTF-8 byte offsets from the first byte after the closing YAML delimiter line, including its newline when present. Count bytes in the exact source body and preserve its line endings. The reviewer selects evidence that actually establishes the timestamps and criteria, and checks for conflicts in the source. The planner verifies the selected bytes, types, and chronology. It uses no Git commit dates, timestamp defaults, or prose-derived graph edges.

Review the entire snapshot for relations, including links across scopes and to held Tasks. The relation object requires `parent` (a qualified key or null), `prerequisites` (an array of qualified keys), and `children` (an array of objects with `legacy_key`) for preparation. Use empty arrays after reviewing that there are no obligations in those directions. The child and parent must agree on parentage. Each dependent declares its own prerequisites, and an incoming prerequisite reference conflicts with a `"no_edges"` declaration on its target.

For each cancelled immediate child of a done parent, add a `scope_acceptance` range to that child entry in the parent's review. Select the complete standalone paragraph in the parent's committed README, excluding its line ending. The supported historical statement is ``Cancellation of `<scope>/<work_id>` was accepted as a scope change.`` with the exact qualified child key. The paragraph must appear at the top level of the Markdown body. Raw HTML blocks follow the same rules as scope entrypoints. Unrecognized or ambiguous evidence remains held. The reviewer verifies that the source records historical acceptance and checks for conflicting statements. Preserve the original v1 prose. Other child entries omit this field.

The planner checks acyclicity across the combined dependent-to-prerequisite and parent-to-child graph. Every referenced endpoint must itself be prepared. Done requires done prerequisites and terminal children, plus the historical scope acceptance for cancelled children. A review matching the Task's qualified key and verified README blob contributes relationship declarations even when the Task is held for scope, duplicate records or reviews, metadata, or missing provenance. Missing, contradictory, cyclic, or held obligations keep the affected done Task held as `unresolved_completion_obligation`. Other affected statuses hold as `unresolved_task_relations`. Holds propagate to dependents and parents and also to children that would otherwise lose their parent reference.

Both decisions are bound by the Task review's exact key, committed blob, Area, source commit, policy pin, and intended workspace. Omit optional fields instead of supplying null. A structurally valid title decision with an invalid value or an inapplicable fallback holds as `missing_or_conflicting_title`. A title or type selection supplies no date, execution, closure, criteria, label, or relation evidence.

An omitted review keeps the original provenance holds. Repeated reviews for the same key hold as `conflicting_task_review`, and a stale blob or different mapped Area holds as `task_review_binding_mismatch`. Unsupported keys and malformed review identifiers invalidate the mapping input. The read-only plan holds tagged Tasks as `unverified_task_labels` pending the target collation check in disposable staging.

A `prepared` outcome records the candidate status and mapped Area ID, exact timestamp literals and their body ranges, the extracted body SHA-256, the resolved title's source and SHA-256, any non-default Issue type, and the mapped priority for backlog and active records when present. Archive priority remains null. It records qualified parent, prerequisite, and child references, sorted by qualified key within each list. Criteria, closure actor, resolution, and cancellation scope acceptance selections appear only as body ranges and SHA-256. Digests cover the exact UTF-8 bytes of the extracted values. The report records preparation and assigns no Issue identity, operational status, or number. The planner reads the verified Git source and the manifest without opening a target workspace or modifying or deleting any legacy archive.

Execution and unclaimed review holds still apply even when a Task has a review entry. Reconcile execution by verifying stop and ownership and review by supplying typed submission evidence.

##### Optional Issue Resource bindings

Supply the top-level `issue_resource_bindings` array in the same version 1 mapping and pass `--tasks` to either `migration-plan` or `migration-stage`. Each object binds a qualified legacy Task and its exact committed README Git blob ID to a Resource UUIDv7 selected in `resource_bindings`.

```json
[
  {
    "legacy_key": {"scope": "example-app", "work_id": "review-release"},
    "source_git_blob_sha": "3333333333333333333333333333333333333333",
    "resource_id": "00000000-0000-7000-8000-000000000003"
  }
]
```

The Task must be eligible for Issue import, the blob must match the verified Task entrypoint, and the prepared Resource must belong to the Task's mapped Area. Missing and held endpoints, mismatched blobs, and Resources from a different Area hold the link. Duplicate Task and Resource pairs and malformed declarations are invalid. Omit the array to leave Issue links unselected. URLs in Task prose, Area membership, and Task relationships never select a link.

The plan reports the qualified Task key digest, Resource ID and hold reason when applicable. Staging adds the mapped Issue ID for each link exercised inside the private transaction. It checks the exact `issue_resources` pairs within the transaction and at the committed snapshot, or verifies zero retained links after dry-run rollback. Stage counts reconcile the declared links with imported and held outcomes.

#### Read-only Directive and Artifact intake

Add `--include-directives-artifacts` to `workspace migration-plan` to produce the partial `directive_artifact_intake` report. The CLI uses the same version 1 scope mapping and reads selected Markdown by verified Git blob ID during the same source checks. Each row gives a workspace-relative source locator, surface, Git blob ID, byte size, and outcome. `source_raw_sha256` is populated for blobs read for metadata inspection. Without reviewed records, each surface count reconciles `inventoried` with `held` and `excluded`. Exit status zero means the requested classification completed.

Portable `source_path` and `parent_task_path` locators use lowercase kebab-case components, known reserved scope IDs and fixed entrypoint names, and lowercase alphanumeric file extensions. For a locator with any malformed component, the report emits `source_path_sha256` or `parent_task_path_sha256` in place of the path. Each digest covers the exact UTF-8 bytes of the full workspace-relative path. Held and excluded records follow the same rules.

- Intake inventories committed files below `directives/`, `artifacts/`, and `tasks/<scope>/<backlog|active|archive>/<work-id>/artifacts/`. Surface navigation and recognized Directive bundle support files are excluded. Single-file and bundle Directives with the same scope and slug are held. Artifact bundle members beyond the entrypoint need explicit review. Reviewed external scope exclusions and holds for unresolved or unsplit scopes apply to records under those scopes.
- Candidate metadata comes from a direct Markdown file or a bundle `README.md`. Intake limits each inspected Markdown blob to 256 KiB and its YAML frontmatter to 64 KiB under the existing bounded YAML parser. Status and kind labels must be string scalars. Optional metadata fields may be omitted, and explicit nulls trigger holds. Title discovery uses Markdown structure to find top-level ATX and Setext H1 headings. It holds conflicting heading text when a valid frontmatter title is unavailable. Non-Markdown Artifact files, non-text Markdown, and Git LFS pointers need reviewed owner or Resource mappings. Intake uses only the committed snapshot and leaves referenced files and URLs unresolved.
- Directive candidates report the bounded v1 status and reviewed Area proposal without descendant inheritance. All require reviewed historical provenance. Active candidates also require verified approval and validity. Superseded candidates need graph reconciliation. The complete graph, including possible replacement endpoints of any status, needs a reviewed migration manifest before readiness can be established.
- Artifact candidates report an explicit v1 kind and any bounded lifecycle label. Review still needs author and source provenance and the original retention clock. Work-scoped rows include the legacy parent bundle path and require a verified Task outcome before they can link to an Issue. Standalone candidates carry no inferred joins.

Without reviewed records, the report contains only source locators, digests, bounded labels, proposed Directive Area IDs, and hold or exclusion codes. All candidates remain held for review.

#### Preparing explicitly reviewed records

Add a `reviewed_records` array to the version 1 mapping and pass `--include-directives-artifacts`. Each entry binds a committed `source_path` and its `source_git_blob_sha` to a distinct lowercase UUIDv7 `target_id` in the mapping's selected workspace. The mapping header binds this review to the exact verified source commit and policy pin. A mismatched path or blob, repeated path or target ID, or malformed declaration fails with `INVALID_CONFIG`.

An entry can include a `title` object alongside `provenance`. Supply a `mode` of `fallback` when the source has no usable title, no unsuitable frontmatter title string, and no conflicting H1 headings. Supply `override` to explicitly replace a usable title or resolve an unsuitable or conflicting title. Both modes require a string `value` that is nonblank, at most 255 characters, and free of control characters. Omit `title` to use the existing source selection. A malformed title declaration fails with `INVALID_CONFIG`. An inapplicable fallback or a well-typed decision with an unsuitable value holds as `missing_or_conflicting_title`. Source metadata and all provenance and relationship requirements apply to both modes.

The following example shows the `reviewed_records` array for a synthetic unlinked proposal and standalone Artifact.

```json
[
  {
    "source_path": "directives/_general/proposal.md",
    "source_git_blob_sha": "0000000000000000000000000000000000000001",
    "target_id": "01900000-0000-7000-8000-000000000411",
    "provenance": {
      "kind": "proposed_directive",
      "area_id": "01900000-0000-7000-8000-000000000101",
      "issuer_ref": "human:synthetic-operator",
      "source_ref": "https://forge.example/synthetic/review/1",
      "created_at": "2024-02-29T12:00:00Z",
      "updated_at": "2024-03-01T12:00:00Z",
      "supersession": "none",
      "approval_and_validity": "none"
    }
  },
  {
    "source_path": "artifacts/report.md",
    "source_git_blob_sha": "0000000000000000000000000000000000000002",
    "target_id": "01900000-0000-7000-8000-000000000511",
    "provenance": {
      "kind": "standalone_artifact",
      "artifact_kind": "report",
      "author_ref": "bot:synthetic-author",
      "source_ref": "https://forge.example/synthetic/review/2",
      "created_at": "2024-02-29T12:00:00Z",
      "substantive_updated_at": "2024-03-01T12:00:00Z",
      "links": "none",
      "archive": "none"
    }
  }
]
```

- A single-file proposed Directive requires the Area ID returned by scope planning and keeps `include_descendants` false. Set `supersession` to `none` only after reviewing possible incoming and outgoing edges in the exact snapshot, including references from other Directives. Set `approval_and_validity` to `none` only after confirming that approval and validity fields have no historical values to preserve. `unresolved` holds the record for either field.
- A root-level standalone Markdown Artifact requires a reviewed `artifact_kind`. When the committed frontmatter omits `kind`, this path- and blob-bound review can supply any supported v2 kind. When frontmatter declares a kind, it must be a supported v1 scalar and agree with the review. An omitted kind without review, an invalid declaration, or a conflict holds the Artifact. Set `links` to `none` after confirming that the Artifact needs zero Area, Project, Issue, or Resource joins and no reference rewrites. `unresolved` holds the record.
- Set `archive` to `none` after confirming that there is no historical archival decision to preserve, or use `unresolved` to hold the record. To preserve a recorded decision, supply an `archive` object with `archived_at` and `archive_reason`. Each field is a named half-open UTF-8 byte range (`start` and `end`) within the committed Artifact body, excluding frontmatter. Select an RFC 3339 timestamp with a known offset and at most six fractional digits, at or after `substantive_updated_at`, and a nonblank reason within the SQL `TEXT` byte limit. Hive stores the UTC instant and exact reason bytes and reports the timestamp and reason SHA-256. The v1 lifecycle label remains provenance metadata.
- Each review supplies an actual historical source reference and the actor and timestamps it supports. Use a portable HTTPS reference with the same syntax as scope planning, and an actor identifier of up to 255 bytes in lowercase kebab-case with an optional `namespace:` prefix. Supply UTC RFC 3339 timestamps with a `T` or `t` separator and a final `Z`, with zero to six fractional digits and years 1000–9999. Update and retention timestamps must be at or after creation. The CLI validates these fields and their binding to the source snapshot. The operator verifies historical facts and any referenced evidence during review.

A qualifying row reports `outcome.status: prepared`, the selected target ID and scoped candidate metadata, the reviewed dates, and the SHA-256 digests of the source and actor references. `source_raw_sha256` covers the entire committed blob. `target_body_sha256` covers every byte after the closing frontmatter delimiter and its line ending, including leading blank lines, CRLF, trailing spaces, and any final newline. The content stays intact. `target_title_sha256` covers the exact UTF-8 title selected from the source or review. The report adds a `prepared` count per surface so `inventoried = prepared + held + excluded`.

The preparation report is partial and read-only. Unreviewed candidates and Artifact bundles remain subject to the intake hold rules. Target identity and constraints still require a later inactive-target staging check.

#### Preparing standalone Artifact joins

Keep `links` set to `none` for a standalone Artifact with zero joins. To retain a reviewed multi-scope set, supply a named object instead. The object accepts optional `area_ids`, `project_ids`, `resource_ids`, and `issues` arrays. Each Issue entry identifies one qualified Task by `legacy_key` (`scope` and `work_id`), `area_id`, exact committed `source_path` for its `README.md`, and `source_git_blob_sha`.

The planner verifies the entire set under the same source commit, policy pin, and target workspace. Area IDs must match prepared scopes, Resources must be selected by prepared `resource_bindings`, and Project IDs must match prepared `project_membership` Projects. Issues must match eligible Tasks by their qualified key, path, Git blob, and mapped Area. An explicit set must contain between 1 and 100 joins across all endpoint types. An out-of-range set or any missing, held, stale, repeated, or ambiguous endpoint holds the entire Artifact as `unresolved_artifact_links`. A requested join cannot be omitted during staging. Related Area, Project, Resource, and Issue relationships do not supply additional Artifact joins.

Use `--tasks` to make the required Issues eligible during staging. The private transaction inserts the exact reviewed join set after all endpoints exist. Issue links resolve to IDs assigned in that transaction. The report records IDs and digests, including the final Issue ID set in staging. The committed validator checks all endpoints. Runtime body disclosure continues to require coverage of all joined scopes.

#### Preparing work-scoped Artifacts

Use `provenance.kind` of `work_scoped_artifact` for a reviewed Markdown file directly under `tasks/<scope>/<lifecycle>/<work-id>/artifacts/`. Bind the review to the Artifact path and Git blob, and supply its reviewed kind, author and source references, and historical archival decision as for a standalone Artifact.

Supply `parent_task` with the qualified `legacy_key` (`scope` and `work_id`), the Task's committed `README.md` path in `source_path`, `source_git_blob_sha`, `area_id`, and `workspace_id`. The binding must match the actual owning Task and a prepared Task outcome under the same verified source and target selection. A held, unprepared, missing, or mismatched parent yields `parent_task_quarantined`. Supply `issue_link` of `retain`. An omitted or unresolved decision holds the Artifact. Omit `area_link` for an Issue-only join or name the same reviewed owning Area ID to retain the Area join as well.

Select `created_at` and `substantive_updated_at` using half-open UTF-8 byte ranges within the committed Artifact body. Each range selects an actual RFC 3339 timestamp with a known offset and at most six fractional digits. The plan reports the original timestamp literal, its range, and the source and body digests. The importer stores the equivalent UTC instant. Current preparation covers ordinary unclaimed backlog or todo Tasks. A terminal parent's closure contribution to retention still requires reviewed handling.

#### Preparing Directive history and supersession

Use `provenance.kind` of `directive_history` for reviewed Directive history. The same `source_path`, `source_git_blob_sha`, and `target_id` fields bind the review to a committed single file or bundle entrypoint. The provenance has `status`, `area_id`, `issuer_ref`, `source_ref`, `created_at`, and `updated_at` fields. The declared status must match committed v1 frontmatter, and the Area must match the scope outcome. All prepared Directives in this plan have `include_descendants` set to false.

Supply `approval_and_validity` as an object with `state` of `reviewed` and optional `approval_ref`, `valid_from`, and `expires_at`. `reviewed` means the operator verified each value against the exact source and its historical evidence, including the absence of omitted fields. An approval reference uses a portable HTTPS URL or the same syntax as an actor identifier. The report hashes the reference. Use `state` of `unresolved` to hold a Directive whose historical approval or validity is uncertain.

An `active` Directive requires an approval reference and `directive_observed_at` in the mapping header. The planner validates and reports that declared UTC timestamp and uses it for every active Directive in the plan. Validity includes `valid_from` and excludes `expires_at`. A verified absent bound leaves that end open. The observation must be at or after the source's reviewed update. A missing approval, unavailable or invalid validity, or an expired active Directive holds the record. Withdrawn and superseded Directives retain their reviewed historical fields, including past validity dates. The report supplies migration provenance. Runtime activation authority requires its own evidence.

Supply `supersession` as an object with `state` of `reviewed` after reviewing possible edges across the committed snapshot. A superseded Directive can declare `replacement` as the legacy key `directives/<scope>/<slug>` without a filename extension or bundle entrypoint suffix. The replacement can optionally declare `replaces` as the predecessor's legacy key to confirm the same edge. Both keys resolve to exact entrypoints bound by their own reviews. Use `directive_history` for either endpoint, including a proposed replacement. A review with `state` of `unresolved` holds the record.

The planner writes the predecessor's target ID into the replacement's reported `supersedes_id`. Replacement status does not affect edge direction. A missing, unreviewed, held, ambiguous, or self-referential endpoint, a cycle, or a conflicting or multiple edge holds the affected chain as `invalid_supersession_chain`. Excluded source records stay excluded. Counts reflect the holds after graph validation.

#### Read-only Queen Soul and historical Memory intake

To include Bot assets in the same plan, add a `queen_assets` array to the version 1 mapping. Each entry binds a reviewed Bot identity to the committed Soul blob.

```json
{
  "queen_assets": [
    {
      "slug": "example-queen",
      "source_git_blob_sha": "2222222222222222222222222222222222222222"
    }
  ]
}
```

Review the full committed Soul and its identity before listing the slug and blob ID. An empty array inventories the surfaces with no reviewed Souls. The planner accepts candidates with a single matching review, common v1 frontmatter, and the v1 Bot template's explicit identity and purpose sections. The ID must match the Bot directory and the Layer must be `1 — Manager`. Additional explicit identity declarations in headings or lists cause a hold. The planner inspects up to 64 KiB per Soul or root Memory entrypoint. Absent, stale, or repeated reviews, missing or invalid Souls, and ambiguous source identities are held as `invalid_soul_or_slug`. A symlink or other unsupported Git entry invalidates source preflight.

Bot routing README files and Bot-tree `AGENTS.md` files are excluded as `management_only`. Root `MEMORY.md` qualifies as `navigation_only` when its frontmatter consists of a single quoted description, `Select memory navigation.` or `Memory navigation.`, and its body consists of a Memory heading followed by a plain list of links to existing scoped `MEMORY.md` files with labels matching their scopes. Other root Memory and files inside `memory/` are held as `curated_memory_deferred`, including scoped entrypoints.

The plan reports candidates, holds, and exclusions under `queen_assets`. A candidate includes its Git blob ID, SHA-256 of the exact source bytes including frontmatter, and proposed asset path. The plan emits repository-relative paths when every component is portable. It retains the path digest for every record and uses it as the locator when any component is unportable. The disposable staging command verifies prepared asset commits and registry rows in an empty inactive target. Broader A15 coverage remains open under [#98](https://github.com/mols3131d/hive-system/issues/98).

#### Read-only Queen preparation

Add the optional `queen_preparation` array alongside the intake reviews to supply reviewed registry identities for positive Soul candidates. Both arrays use the manifest's binding to the verified source commit, policy pin, and selected workspace UUID.

```json
{
  "queen_preparation": [
    {
      "slug": "example-queen",
      "source_git_blob_sha": "2222222222222222222222222222222222222222",
      "source_raw_sha256": "3333333333333333333333333333333333333333333333333333333333333333",
      "bot_id": "018f0000-0000-7000-8000-000000000001",
      "display_name": "Example Queen"
    }
  ]
}
```

The immutable slug must equal the portable legacy Manager Bot ID. Supply the reviewed display name and a registry UUIDv7. Preparation requires a positive intake candidate with both matching digests. A stale digest, missing candidate, repeated slug, or repeated registry UUID holds each affected entry as `invalid_soul_or_slug`. The report lists prepared identifiers and digests or hold reasons.

The private preparation result retains the exact committed Soul bytes and the reviewed registry fields. Staging commits the Soul at `assets/bots/<slug>/SOUL.md` before inserting the Bot row and records the asset commit in migration provenance.

#### Disposable migration staging

On Linux, use the installed CLI with the workspace initialization policy revision and the reviewed mapping. The target must be an absent absolute path under an existing local parent, separate from the source. The command refuses occupied paths, the active root for the selected workspace ID, the resolved v1 and v2 user configuration and v2 bindings paths, the v2 XDG default workspace path, and overlaps with the v2 runtime state directory.

```sh
hive --contract 2 --json workspace migration-stage \
  --source "$source_root" --source-commit "$source_commit" \
  --workspace-id "$workspace_id" --mapping-file "$mapping_file" \
  --target "$target_root" --policy-revision "$v2_policy_revision" --mode dry-run
```

The command classifies scopes from the verified commit and reviewed mapping. It omits held and excluded scopes. Every mapped Area parent must itself be eligible. Missing or held parents and cycles refuse either mode before any target database is created.

Select additional surfaces in the same reviewed mapping against that source commit, policy pin, and intended target UUID.

- Pass `--tasks` to stage prepared Task rows. A mapping with Task reviews requires this flag for Area-only staging. Project membership and Artifact reviews with Task endpoints can keep their Task links held when staging omits Tasks. Issue numbers follow lexicographic `(scope, work_id)` order even when parent placement requires a different insertion order. The transaction inserts Issues in parent order, then their prerequisites, and advances the Issue counter. It preserves reviewed timestamps at UTC microsecond precision, and stores titles, body, criteria, closure actor and resolution from their reviewed source bytes. The transaction accepts up to 64 MiB of Task body bytes including repeated blobs.
- Task tags use the v2 label name rules. Staging trims outer Unicode whitespace and shares a label row across Issues with the same exact result. Before Issue allocation, it compares otherwise eligible spellings under the initialized `labels.name` collation. Each Task with a distinct colliding spelling holds as `task_label_collation_collision`, and required graph endpoints propagate holds. The transaction writes and verifies label rows and Issue links. Task ID map entries expose linked label IDs.
- Supply `resource_bindings` to stage prepared Resources and their explicit Area links. With `--tasks`, supply `issue_resource_bindings` to stage the separately reviewed Issue links after both endpoints.
- Supply `project_membership` to stage prepared Projects. Add `--tasks` for reviewed membership links. The transaction inserts the links after their Issue and Project rows and verifies both tables against the committed staged snapshot or verifies emptiness after dry-run rollback.
- Supply `queen_assets` and `queen_preparation` to stage prepared Queen Souls and Bot registry rows. The CLI commits the exact reviewed Soul bytes to the paired asset repository and verifies the commit before inserting any Bots. Historical Memory stays held and `bot_memories` remains empty.
- Supply `reviewed_records` to stage prepared Directives in supersession order and prepared standalone Artifacts. The review preserves historical fields, the selected title bytes, and committed body bytes. `--include-directives-artifacts` selects this intake in the read-only plan. Use `--tasks` to stage reviewed work-scoped Artifacts whose owning Tasks are eligible in the same transaction. The importer resolves the Issue join through that transaction's assigned Task IDs and writes an Area join only for an explicit `area_link`. It verifies the exact committed Artifact join tables and checks their absence after rollback. Standalone Artifacts retain either their explicit reviewed set or zero joins.

Both modes insert the selected rows inside one guarded Dolt transaction on a private disposable copy. Dry-run checks the exercised rows and rolls back, then verifies the original clean Dolt baseline and empty import tables. Staged import creates one Dolt commit extending that baseline, compares the staged surfaces to their committed rows and validates the committed schema and records using the paired asset commit. The CLI verifies the exact clean source again after preparation. Staged import syncs and publishes the verified payload atomically to the inactive target. Neither mode registers or starts a workspace or changes either contract's XDG configuration.

The report preserves the existing `coverage` value for Area-only and single-surface staging. Combined staging uses `coverage: mixed` and lists the selected surfaces in `included_surfaces`. It includes provenance, the mapping SHA-256, balanced counts, IDs, field digests and typed holds and exclusions. Queen asset holds include SHA-256 source locators in `queen_inventory`. Scope, Task and record paths appear as digests resolvable against the exact source commit. The Task key digest covers the scope, a NUL byte and the work ID. The report omits raw record payloads, approval references, and paths. In dry-run, `imported` describes the rows exercised inside the rolled-back transaction. The reported target row counts are zero, the retained Issue counter is 1, and `projected_issue_number_next_value` gives the exercised counter when Tasks are selected.

The `reconciliation` section identifies whether counts and ID mappings came from the rolled-back transaction or the confirmed commit. It also identifies the verified disposable or published asset commit and lists surfaces that were inventoried without being selected for staging. Hold entries carry reason codes and reconciliation actions. Dry-run and staged import allocate their own Issue and Label UUIDs.

The wider acceptance gate includes terminal-parent Artifact retention, further source-evidence review, and post-import counter and rollback scenarios. See [#98](https://github.com/mols3131d/hive-system/issues/98) and the [A15 acceptance matrix](dolt-mvp-contract.md#acceptance-matrix-and-evidence).

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

The installed Linux mixed migration fixture checks the public `validate --all` result at the exact migrated Dolt commit. It backs up that stopped synthetic target, then starts a runtime scoped to the disposable fixture and exercises Issue create, cancel and create. The cancelled Issue retains number `N+1`, the next Issue receives `N+2`, and the counter advances to `N+3`. The fixture checks all three receipts and an unchanged v1 snapshot. It stops the runtime and verifies that staged import and backup restore each refuse the occupied post-write target while preserving the written Dolt and asset state and the pre-write backup manifest. For the rollback boundary, the fixture exercises destination refusal. Live cutover and reverse transitions are outside its scope.

With Git and Dolt 2.7.6 on absolute `PATH` entries, use a fresh install root and disposable local scratch to run the fixture on Linux with procfs, pidfds and loopback TCP.

```sh
cargo install --locked --path . --root "$install_root"
HIVE_STAGING_INSTALLED_BIN="$install_root/bin/hive" HIVE_DOLT_BIN="$dolt_bin" \
  TMPDIR="$scratch_tmp" cargo test --locked --test dolt_validation \
  migration::mixed_staging::installed_cli_stages_mixed_reviewed_surfaces_in_one_inactive_target \
  -- --exact --ignored
```

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
