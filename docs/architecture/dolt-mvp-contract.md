---
description: "Review the recommended MVP lifecycle, storage ownership, context selection, local runtime, CLI and migration contracts for the Dolt redesign."
---

# Proposed MVP Design Baseline

Status: recommended decisions for review under [#22](https://github.com/mols3131d/hive-system/issues/22). Read with the [redesign](dolt-redesign.md) and [schema](dolt-schema.md). These are proposed replacements, not active workspace policy. User direction establishes Dolt, generic Area/Sub-area, purpose-based Projects and fixed Bot roles; other decisions below need review and technical evidence.

## Domain and identity

One authoritative local database per workspace. Area is an untyped recursive scope with a stable UUIDv7 (application-generated, CHAR(36), normalized lowercase). Sub-area is an Area with a parent. Require one owning Area per Issue; an explicitly selected general Area is allowed, never an implicit fallback. Enforce sibling slug uniqueness including roots and prohibit cycles. Moving an Area preserves all IDs. Archive an Area only with no active claims in its subtree; archived ancestry excludes execution and new work until explicitly restored. No automatic archival of children or related Projects.

Project is an independent purpose-based collection of Issues. Membership is optional and many-to-many across Areas. Removing a membership or completing a Project never closes an Issue. Explicitly completing a Project is a planning decision, not acceptance of all members. MVP has no custom project fields, milestones, iterations or project-specific execution status.

Use UUIDv7 primary keys and an immutable workspace-wide issue number for human references (#42). Allocate under the authoritative write lock using a counter row in the same transaction; no MAX+1, reuse or offline allocation on replicas. Import preserves a qualified legacy ID mapping, not necessarily legacy numbers. On restore prohibit concurrent old authority before writes. Area slugs and Bot slugs are locators, not identity. Priority 0–3 (0 highest), NULL unspecified, ordering by priority then creation then ID/number; no priority grants authority. Labels are workspace-wide, normalized and unique. Epic is a grouping Issue, not an executable claim target. Feature, Task and Bug may execute. Parent hierarchy and prerequisite DAG are independent; cross-Area references are allowed with explicit authority. Related/discovered-from links may be deferred; do not misrepresent either as a dependency.

## Work lifecycle and accountability

| Command / transition | Invariant |
| --- | --- |
| create -> backlog | Capture goal; no Claim |
| select backlog -> todo | Verified scope, actionable acceptance criteria, authority; epic may be selected for planning |
| claim todo -> in-progress | Non-epic, ready, fresh expected revision, no active claim; allocate unique claim and holder |
| release/recover in-progress -> todo | Exact claim and verified execution stop; recovery requires authorization |
| submit in-progress -> review | Verified execution stop; evidence and result recorded, remove claim atomically |
| request-changes review -> todo | Record reviewer feedback; new claim required |
| accept review -> done | Authorized acceptance, recorded outcome, required children resolved; no claim |
| submit-epic todo -> review | Explicit proposal after required children resolved; no execution claim |
| cancel nonterminal -> cancelled | Explicit authority and outcome; current claim must first be safely reconciled or released in same guarded change |
| reopen done/cancelled -> todo | Explicit reason/authority; reset closure fields; reconcile downstream and parent outcomes |

Only in-progress may have an active execution Claim; it must have exactly one. Holder is an opaque authorized execution reference, not necessarily a persistent Bot. Planned assignment and reviewer assignment are not exclusive claims; defer a separate assignee field. Claim is not runtime control, credential, or execution permission. No lease, heartbeat, automatic expiry or reassignment in MVP. Stop uncertainty blocks release/recovery/acceptance rather than granting ownership. Epic acceptance requires all children terminal; cancelled children require explicit documented acceptance of the changed scope. The same child-resolution check applies to any parent with children; parentage does not itself create prerequisite edges. Only accepted done satisfies dependencies. Reopen or dependency edits that invalidate accepted or executing dependents require explicit reconciliation; reject otherwise. Reject adding unmet prerequisites to a running issue; no implicit cascade or reopening. Keep progress, evidence, handoff and review notes as intentional typed Issue updates, not logs.

Ready = todo + executable type + no hold + no current Claim + accepted prerequisites + no archived Area ancestry. Blocked = executable todo not ready, with reason codes; epics are planning-only. Do not store ready/blocked status. Readiness does not grant permission. Claim re-evaluates the predicate in the write transaction.

## Canonical ownership and files

| Data | Canonical owner |
| --- | --- |
| Structured collaboration records, Issue body and updates | Dolt |
| Directive and textual Artifact Markdown body | Dolt |
| Bot registry, immutable role and Area/resource relations | Dolt |
| Bot Soul, latest memory, curated long-term memory | Native Git tracked Markdown files |
| Large/binary external assets | Existing authorized owner; Resource reference with optional immutable revision/digest |
| Machine config, resource path bindings, local credentials | XDG local environment; never shared |

MVP does not require a managed binary blob store. Reference externally owned files without copying; optional immutable managed blob storage and its recovery protocol are later features. For text artifacts, store Markdown once in Dolt, export on request as a noncanonical copy. Artifact has optional explicit Area, Project, Issue and Resource joins, can be standalone, and does not inherit issue status. Preserve existing artifact retention default (three months from applicable substantive event) and work record minimum retention (seven days after relevant event), with retention exceptions, open references and claims protected. No automated deletion or history erasure. Explicit durable archive or verified transfer to canonical owner is a separate decision.

Directive has exactly one scope: workspace, Area or Project. Area descendant applicability is an explicit flag, default false. A project directive applies only when that project is explicitly selected and the issue is its member; membership alone is not applicability. Workspace/Area ancestry, selected Project, and current Issue are query dimensions, not trust or authority grants. Check source, issuer, validity interval and approval provenance. Conflicting applicable directions stop the affected action for resolution; specificity is not an automatic permission override.

Bot registration is DB metadata; the locator derives from immutable role and stable slug: bots/<role>/<slug>/SOUL.md, MEMORY.md, memory/<area-id>/. Existing Manager-only eligibility retained; Coordinator and Executor are session roles unless later authorized as persistent Bots. Role policy remains system-owned. SOUL body contains only identity/purpose/persona for injection; strip management frontmatter from runtime payload. Latest memory is curated; long-term memory read only for explicitly relevant Area(s), no automatic parent/descendant sweep. Do not load README/AGENTS as Bot runtime context; verify host auto-discovery behavior or refuse that activation path. Registry validation checks safe relative paths, expected files and no symlink escape. Registration: prepare and validate asset files and Git commit first, then record registry; failure leaves unregistered files not loadable as a Bot. File edits remain reviewable and independent; capture exact file revision for context. No atomic SQL+Git claim. No secret, raw conversation or personal data capture.

## Local runtime and authority

Recommended Rust CLI + local Dolt SQL Server (MySQL protocol) first. Preserve reusable CLI/config/JSON code. Go embedded is a fallback only if measured setup, latency or reliability warrants a rewrite; CLI subprocess access is a bootstrap/diagnostic candidate, not the normal per-query data path. Pin Dolt and Rust dependency versions after a synthetic spike; verify supported versions at doctor/start. One local server per authorized workspace, explicit start/stop; no auto-install, background scheduler, service registration, external bind or remote exposure. Prefer a private Unix socket and restrictive permissions where supported; otherwise authenticated loopback with unique endpoint. Disable or secure default root access, use limited writer/read-only credentials, redact diagnostics. No credential in Git, DB records, command argv or logs. No Hive HTTP daemon required.

Use an OS-held exclusive advisory lock per canonical workspace for all Hive mutations, maintenance, backup/restore and sync; process death releases it. Local filesystem only, no independent clone or multi-host writers. Resolve lock identity from verified workspace root and DB identity; no deleting lock files or stealing based on age. All participating writers must use the lock; direct Dolt administrative writes are explicit offline maintenance with full validation, not normal operation. Raw DB access is not a security boundary against the same OS user. Use read-only access for UI and analysis; do not expose arbitrary mutation SQL. If lock tests or process ownership fail, refuse writes; prefer a verified single writer rather than assuming Dolt merge semantics provide serializable graph invariants. The previous mutation_guard token remains an alternative experimental conflict check, not an MVP dependency.

With lock held, verify authority, clean/compatible branch main and request identity; begin one SQL transaction, revalidate affected state, apply all changes, store an operation receipt, commit, verify the committed result before success. Prototype session dolt_transaction_commit=1 with explicit transactions, force-commit disabled; test actual durability/rollback and version commit behavior. Record request ID, operation kind, request fingerprint (no raw confidential payload), result IDs and new revision in the same transaction. On ambiguous result, reconcile by request ID at the authoritative version; same request + fingerprint returns prior result, different fingerprint conflicts. Do not blindly retry or assign execution until verified. Version commit hash is discovered after commit, not written into that same commit. Hold lock through confirmation. If SQL and version commits separate in the tested release, define recoverable pending-publication state before adopting; block subsequent mutations until reconciled. No global ownership from independent replicas.

## XDG and configuration

User configuration: XDG_CONFIG_HOME/hive/config.yml (default ~/.config/hive/config.yml). Workspace data: XDG_DATA_HOME/hive/userdata/ (default ~/.local/share/hive/userdata/), with db/ (Dolt), assets/ (Native Git Bot files), and a nonsecret workspace descriptor with stable workspace ID and schema compatibility. State: XDG_STATE_HOME/hive/ (default ~/.local/state/hive/) for lock, migration/recovery state and redacted diagnostics; socket under valid private XDG_RUNTIME_DIR when available. Never track Dolt internal files in ordinary Git. No secret or host-specific mapping in shared files. Absolute XDG paths only; fallback per specification; explicit invalid selection is an error. Resolve explicit CLI workspace/Area/Project > explicitly configured applicable local binding > user default > validated XDG candidate. A Resource may map to several Areas; return candidates, never silently choose. Project bindings become Area/resource bindings with optional explicit Project; non-Git resources work without Git root. No automatic writes to external projects.

## CLI and API

Commands proposed for MVP; preserve JSON v1 semantics only where compatible, introduce explicit new API/schema version for breaking changes. No silent task-to-issue reinterpretation. Human and machine outputs from shared domain operations.

| Domain | MVP operations |
| --- | --- |
| workspace | init (explicit empty target), status, start, stop, doctor |
| area | create, list, tree, show, move, archive, restore |
| project | create, list, show, add-issue, remove-issue, complete, archive |
| issue | create, list, show, select, ready, blocked, claim, release, recover, submit, request-changes, accept, cancel, reopen, note |
| issue relation | set-parent, add-dependency, remove-dependency, label, unlabel |
| resource | register, list, show, link-area, link-issue |
| directive / artifact | create, list, show, update with revision, link or supersede as applicable |
| bot / memory | list, show, context; explicit curated file editing; no automatic learning |
| context | bounded resolved issue/area/project context and provenance; no mutation |
| validate | full domain diagnostics |
| backup / restore | explicit consistent snapshot; restore only to separate empty target |

Query through bounded typed filters, sort, pagination and named views. Arbitrary SQL CLI and custom view language deferred; optional read-only SQL client only after permissions and procedure restrictions tested. Global flags: --workspace, --area, --project, --json; issue ID or #number (shell quote if needed). List return observed revision and pagination; context returns selected source refs and excludes unrelated records. Mutations take expected revision where editing existing Issue/record, request ID, explicit target and actor/holder evidence. Stable errors: unresolved_scope, ambiguous_scope, schema_incompatible, invalid_transition, blocked, claim_conflict, stale_revision, authority_unverified, lock_busy, commit_uncertain, constraint_violation. No invented success.

## Backup, sync and migration

Local-first; no remote required or per-change push. MVP consistent backup: quiesce writers and asset edits, verify clean committed DB and asset Git revisions, export to isolated destination with a manifest containing workspace ID, schema version, Dolt commit, asset Git commit and digest of snapshot files; omit credentials and host paths. Validate restore on a fresh target before promoting, verify graph/claims/files and prohibit overlapping execution authority. Ordinary Git remote is a candidate for Dolt remote; do not conflate Git refs with database refs. Optional remote backup only after synthetic restore and privacy checks. Paired DB/file remote publishing is not atomic; combined remote sync/pull/merge deferred until manifest publication and partial failure recovery are proven. No automatic conflict merge, force push, or independent offline claim coordination.

Migration: inventory legacy files read-only, stop/handoff affected agents, preserve exact old Git ref and policy pin. Produce explicit mapping old scopes -> Areas, Project collections and Issues; preserve references and historical revision evidence. Backlog -> backlog, active todo/in-progress/review/done -> corresponding state, archived -> terminal, but active/uncertain claims require reconciliation; do not fabricate acceptance, silently downgrade or transfer a claim. Preserve legacy date uncertainty; quarantine invalid records rather than inventing values. Map reserved general/userdata scopes explicitly, never infer from repository name. Import directives, text artifacts, bot metadata and file memories separately. Use a new DB/asset tree, dry run on a copy, compare counts, hashes, semantic relationships, missing references, retention and context boundaries; produce import report and legacy ID map. Stop old authority before cutover, obtain approval, update policy and config atomically at the authority-selection boundary. Keep source snapshot read-only for rollback; once new writes occur, rollback requires reconciliation, not silent switch.

## Acceptance and explicit deferrals

Test DDL, unique IDs/numbers/root slugs, FK and graph constraints, all lifecycle transitions, claim races, lock crash/recovery, commit ambiguity/retry, forbidden mutations, backup/restore, safe paths and host context exclusion on synthetic fixtures. Compare Rust SQL connectivity and operational cost before selecting production dependencies. No live data migration, production schema activation, automatic server launch or remote push implied. Later: multi-host authority, background leases/scheduling, generalized SQL query UI, binary store, custom Project fields, broader persistent Bot roles. Canonical legacy policies continue until reviewed replacement.

References: [Dolt SQL server](https://www.dolthub.com/docs/sql-reference/server/), [configuration](https://www.dolthub.com/docs/sql-reference/server/configuration/), [access management](https://www.dolthub.com/docs/sql-reference/server/access-management/), [transactions](https://www.dolthub.com/docs/concepts/dolt/sql/transaction/), [Git remotes](https://www.dolthub.com/docs/sql-reference/version-control/remotes/), [backup](https://www.dolthub.com/docs/concepts/dolt/rdbms/backups/).
