---
description: "Use when implementing Dolt v2 tables, constraints, readiness queries and guarded mutations."
---

# Dolt v2 Relational Schema

This relational design supports the adopted Dolt v2 development baseline. The MVP contract governs domain invariants. Existing Markdown/Git workspaces operate under their reviewed v1 pins. Live schema activation during migration or an authority switch requires separate explicit operator approval. [Dolt v2 architecture](dolt-redesign.md), [MVP contract](dolt-mvp-contract.md), [v1 compatibility](README.md#v1-compatibility).

The system-owned initial migration implements the 24 reviewed tables on Dolt 2.7.6. Storage schema version 1 records the lowercase SHA-256 of that SQL file in `schema_migrations`. Keep the migration bytes fixed once applied. An explicit bootstrap supplies the singleton workspace identity, ledger row and initial Issue counter. The illustrative SQL below retains the reviewed design and its spike inputs. [Initial migration](../../assets/schema/dolt/0001_initial.sql), [read-only validation and verification](../development/README.md#dolt-record-validation).

The validator checks the committed column, key, foreign key and CHECK definitions against SHA-256 fingerprints generated from the migration with the Dolt 2.7.6 renderer. It omits the trailing table-default `utf8mb4` collation token since the migration inherits that setting. The charset, column-level collation overrides and constraint names and definitions are part of the fingerprint. [Schema shape manifest](../../assets/schema/dolt/0001_schema.sha256).

## Relational conventions

One database per workspace. Do not partition by Area, Project or repository. Use immutable application-generated UUIDs (UUIDv7 candidate; CHAR(36) initially) as PKs; paths, names and display numbers are not identities. The recommended baseline uses immutable workspace-wide display issue numbers; allocate through a counter under the authoritative lock, not MAX+1 or independent clones. Use UTC timestamps and explicit schema migrations. No automatic deletion cascades on business records. Prefer archive/close to delete. Stable IDs survive moves. A human or session holder reference is not a registered Bot ID or credential.

## Core DDL candidate

Verify the illustrative MySQL-compatible SQL against a pinned Dolt release. Root sibling slug uniqueness needs additional domain validation because nullable parent IDs do not enforce it through a composite unique key. All mutations affecting those invariants use the same guarded authority.

```sql
CREATE TABLE areas (
  id CHAR(36) PRIMARY KEY,
  parent_id CHAR(36),
  slug VARCHAR(100) NOT NULL,
  name VARCHAR(255) NOT NULL,
  description TEXT,
  archived_at DATETIME(6),
  revision CHAR(36) NOT NULL,
  UNIQUE (parent_id, slug),
  FOREIGN KEY (parent_id) REFERENCES areas(id),
  CHECK (parent_id IS NULL OR parent_id <> id)
);

CREATE TABLE projects (
  id CHAR(36) PRIMARY KEY,
  name VARCHAR(255) NOT NULL,
  description TEXT,
  status VARCHAR(16) NOT NULL,
  revision CHAR(36) NOT NULL,
  CHECK (status IN ('active', 'completed', 'archived'))
);

CREATE TABLE issues (
  id CHAR(36) PRIMARY KEY,
  number BIGINT UNSIGNED NOT NULL UNIQUE,
  area_id CHAR(36) NOT NULL,
  parent_id CHAR(36),
  title VARCHAR(255) NOT NULL,
  body LONGTEXT,
  acceptance_criteria TEXT,
  type VARCHAR(16) NOT NULL,
  status VARCHAR(16) NOT NULL,
  priority TINYINT,
  hold_reason TEXT,
  revision CHAR(36) NOT NULL,
  created_at DATETIME(6) NOT NULL,
  updated_at DATETIME(6) NOT NULL,
  closed_at DATETIME(6),
  closed_by_ref VARCHAR(255),
  resolution_note TEXT,
  FOREIGN KEY (area_id) REFERENCES areas(id),
  FOREIGN KEY (parent_id) REFERENCES issues(id),
  CHECK (parent_id IS NULL OR parent_id <> id),
  CHECK (number > 0),
  CHECK (type IN ('epic','feature','task','bug')),
  CHECK (type <> 'epic' OR status <> 'in-progress'),
  CHECK (status IN ('backlog','todo','in-progress','review','done','cancelled')),
  CHECK (priority IS NULL OR priority BETWEEN 0 AND 3),
  CHECK (
    (status IN ('done','cancelled') AND closed_at IS NOT NULL
      AND closed_by_ref IS NOT NULL AND resolution_note IS NOT NULL)
    OR
    (status NOT IN ('done','cancelled') AND closed_at IS NULL
      AND closed_by_ref IS NULL AND resolution_note IS NULL)
  ),
  INDEX issues_area_status (area_id, status, priority)
);

CREATE TABLE project_issues (
  project_id CHAR(36) NOT NULL,
  issue_id CHAR(36) NOT NULL,
  PRIMARY KEY (project_id, issue_id),
  FOREIGN KEY (project_id) REFERENCES projects(id),
  FOREIGN KEY (issue_id) REFERENCES issues(id),
  INDEX project_issues_reverse (issue_id, project_id)
);

CREATE TABLE issue_dependencies (
  issue_id CHAR(36) NOT NULL,
  prerequisite_id CHAR(36) NOT NULL,
  PRIMARY KEY (issue_id, prerequisite_id),
  FOREIGN KEY (issue_id) REFERENCES issues(id),
  FOREIGN KEY (prerequisite_id) REFERENCES issues(id),
  CHECK (issue_id <> prerequisite_id),
  INDEX dependency_reverse (prerequisite_id, issue_id)
);

CREATE TABLE resources (
  id CHAR(36) PRIMARY KEY,
  kind VARCHAR(32) NOT NULL,
  name VARCHAR(255) NOT NULL,
  uri TEXT,
  revision CHAR(36) NOT NULL
);

CREATE TABLE area_resources (
  area_id CHAR(36) NOT NULL,
  resource_id CHAR(36) NOT NULL,
  PRIMARY KEY (area_id, resource_id),
  FOREIGN KEY (area_id) REFERENCES areas(id),
  FOREIGN KEY (resource_id) REFERENCES resources(id),
  INDEX area_resources_reverse (resource_id, area_id)
);

CREATE TABLE issue_resources (
  issue_id CHAR(36) NOT NULL,
  resource_id CHAR(36) NOT NULL,
  PRIMARY KEY (issue_id, resource_id),
  FOREIGN KEY (issue_id) REFERENCES issues(id),
  FOREIGN KEY (resource_id) REFERENCES resources(id),
  INDEX issue_resources_reverse (resource_id, issue_id)
);

CREATE TABLE issue_claims (
  issue_id CHAR(36) PRIMARY KEY,
  claim_id CHAR(36) NOT NULL UNIQUE,
  holder_ref VARCHAR(255) NOT NULL,
  acquired_at DATETIME(6) NOT NULL,
  FOREIGN KEY (issue_id) REFERENCES issues(id)
);

CREATE TABLE workspace_counters (
  name VARCHAR(32) PRIMARY KEY,
  next_value BIGINT UNSIGNED NOT NULL
);

CREATE TABLE operation_receipts (
  request_id CHAR(36) PRIMARY KEY,
  kind VARCHAR(64) NOT NULL,
  fingerprint CHAR(64) NOT NULL,
  actor_ref VARCHAR(255) NOT NULL,
  authorization_ref TEXT NOT NULL,
  workflow_evidence_json LONGTEXT NOT NULL,
  base_commit VARCHAR(128) NOT NULL,
  result_json LONGTEXT NOT NULL,
  created_at DATETIME(6) NOT NULL
);
```

Initialize the issue_number counter at explicit bootstrap. Allocate and increment it in the same guarded transaction as issue creation. The OS-held workspace write lock is the recommended MVP candidate; the prior mutation_guard is only an alternative spike. The MVP proposes required issue Area ownership (an explicitly chosen general Area for unclassified work, not an implicit fallback), one parent at most, optional project memberships, and a globally defined 0–3 priority (0 highest, NULL unspecified). Issue types, lifecycle and priority are recommended review candidates; see the [MVP design baseline](dolt-mvp-contract.md) for transition and ownership rules. Project-specific priority/order is deferred to membership fields if justified. No implicit dependency from parentage or project inclusion. Archive policy and ancestry-based eligibility must be made explicit before final readiness contract.

## Additional DDL candidate

The following tables complete the first-pass relational layout. Metadata is relational; Markdown remains opaque text. Validate all SQL and migration ordering against the selected Dolt release. The system is the canonical schema owner; the workspace descriptor must match the database identity, not override it.

```sql
CREATE TABLE workspace_meta (
  singleton_id TINYINT PRIMARY KEY,
  workspace_id CHAR(36) NOT NULL UNIQUE,
  CHECK (singleton_id = 1)
);

CREATE TABLE schema_migrations (
  version INT UNSIGNED PRIMARY KEY,
  migration_digest CHAR(64) NOT NULL,
  applied_at DATETIME(6) NOT NULL
);

CREATE TABLE labels (
  id CHAR(36) PRIMARY KEY,
  name VARCHAR(100) NOT NULL UNIQUE,
  description TEXT,
  revision CHAR(36) NOT NULL
);

CREATE TABLE issue_labels (
  issue_id CHAR(36) NOT NULL,
  label_id CHAR(36) NOT NULL,
  PRIMARY KEY (issue_id, label_id),
  FOREIGN KEY (issue_id) REFERENCES issues(id),
  FOREIGN KEY (label_id) REFERENCES labels(id),
  INDEX issue_labels_reverse (label_id, issue_id)
);

CREATE TABLE issue_updates (
  id CHAR(36) PRIMARY KEY,
  issue_id CHAR(36) NOT NULL,
  kind VARCHAR(16) NOT NULL,
  body LONGTEXT NOT NULL,
  author_ref VARCHAR(255) NOT NULL,
  created_at DATETIME(6) NOT NULL,
  transition_json LONGTEXT,
  FOREIGN KEY (issue_id) REFERENCES issues(id),
  CHECK (kind IN ('progress','review','handoff','evidence','comment','transition')),
  CHECK (
    (kind = 'transition' AND transition_json IS NOT NULL)
    OR (kind <> 'transition' AND transition_json IS NULL)
  ),
  INDEX issue_updates_order (issue_id, created_at, id)
);

CREATE TABLE bots (
  id CHAR(36) PRIMARY KEY,
  slug VARCHAR(100) NOT NULL UNIQUE,
  display_name VARCHAR(255) NOT NULL,
  revision CHAR(36) NOT NULL,
  retired_at DATETIME(6)
);

CREATE TABLE bot_memories (
  id CHAR(36) PRIMARY KEY,
  bot_id CHAR(36) NOT NULL,
  area_id CHAR(36),
  kind VARCHAR(16) NOT NULL,
  recall_tier VARCHAR(16),
  topic_key VARCHAR(100) NOT NULL,
  title VARCHAR(255) NOT NULL,
  body LONGTEXT NOT NULL,
  source_ref TEXT NOT NULL,
  author_ref VARCHAR(255) NOT NULL,
  authorization_mode VARCHAR(16) NOT NULL,
  authorization_ref TEXT NOT NULL,
  reviewed_by_ref VARCHAR(255),
  observed_at DATETIME(6),
  reviewed_at DATETIME(6),
  revision CHAR(36) NOT NULL,
  created_at DATETIME(6) NOT NULL,
  updated_at DATETIME(6) NOT NULL,
  retired_at DATETIME(6),
  retirement_reason TEXT,
  FOREIGN KEY (bot_id) REFERENCES bots(id),
  FOREIGN KEY (area_id) REFERENCES areas(id),
  CHECK (
    (kind = 'latest' AND recall_tier IS NULL)
    OR (
      kind = 'durable'
      AND recall_tier IS NOT NULL
      AND recall_tier IN ('core','reference')
    )
  ),
  CHECK (
    (authorization_mode = 'reviewed'
      AND reviewed_by_ref IS NOT NULL AND reviewed_at IS NOT NULL)
    OR (authorization_mode = 'delegated'
      AND reviewed_by_ref IS NULL AND reviewed_at IS NULL)
  ),
  CHECK (
    (retired_at IS NULL AND retirement_reason IS NULL)
    OR (retired_at IS NOT NULL AND retirement_reason IS NOT NULL)
  ),
  INDEX bot_memories_recall (bot_id, area_id, retired_at, kind, recall_tier, id),
  INDEX bot_memories_topic (bot_id, area_id, topic_key)
);

CREATE TABLE directives (
  id CHAR(36) PRIMARY KEY,
  scope_kind VARCHAR(16) NOT NULL,
  area_id CHAR(36),
  project_id CHAR(36),
  include_descendants BOOLEAN NOT NULL DEFAULT FALSE,
  title VARCHAR(255) NOT NULL,
  body LONGTEXT NOT NULL,
  status VARCHAR(16) NOT NULL,
  issuer_ref VARCHAR(255) NOT NULL,
  source_ref TEXT NOT NULL,
  approval_ref TEXT,
  valid_from DATETIME(6),
  expires_at DATETIME(6),
  supersedes_id CHAR(36),
  revision CHAR(36) NOT NULL,
  created_at DATETIME(6) NOT NULL,
  updated_at DATETIME(6) NOT NULL,
  FOREIGN KEY (area_id) REFERENCES areas(id),
  FOREIGN KEY (project_id) REFERENCES projects(id),
  FOREIGN KEY (supersedes_id) REFERENCES directives(id),
  CHECK (
    (scope_kind = 'workspace' AND area_id IS NULL AND project_id IS NULL)
    OR (scope_kind = 'area' AND area_id IS NOT NULL AND project_id IS NULL)
    OR (scope_kind = 'project' AND area_id IS NULL AND project_id IS NOT NULL)
  ),
  CHECK (scope_kind = 'area' OR include_descendants = FALSE),
  CHECK (status IN ('proposed','active','superseded','withdrawn')),
  CHECK (status <> 'active' OR approval_ref IS NOT NULL),
  CHECK (valid_from IS NULL OR expires_at IS NULL OR expires_at > valid_from),
  CHECK (supersedes_id IS NULL OR supersedes_id <> id)
);

CREATE TABLE artifacts (
  id CHAR(36) PRIMARY KEY,
  kind VARCHAR(32) NOT NULL,
  title VARCHAR(255) NOT NULL,
  body LONGTEXT NOT NULL,
  author_ref VARCHAR(255) NOT NULL,
  source_ref TEXT,
  revision CHAR(36) NOT NULL,
  created_at DATETIME(6) NOT NULL,
  substantive_updated_at DATETIME(6) NOT NULL,
  archived_at DATETIME(6),
  archive_reason TEXT,
  CHECK (
    (archived_at IS NULL AND archive_reason IS NULL)
    OR (archived_at IS NOT NULL AND archive_reason IS NOT NULL)
  )
);

CREATE TABLE artifact_areas (
  artifact_id CHAR(36) NOT NULL,
  area_id CHAR(36) NOT NULL,
  PRIMARY KEY (artifact_id, area_id),
  FOREIGN KEY (artifact_id) REFERENCES artifacts(id),
  FOREIGN KEY (area_id) REFERENCES areas(id),
  INDEX artifact_areas_reverse (area_id, artifact_id)
);

CREATE TABLE artifact_projects (
  artifact_id CHAR(36) NOT NULL,
  project_id CHAR(36) NOT NULL,
  PRIMARY KEY (artifact_id, project_id),
  FOREIGN KEY (artifact_id) REFERENCES artifacts(id),
  FOREIGN KEY (project_id) REFERENCES projects(id),
  INDEX artifact_projects_reverse (project_id, artifact_id)
);

CREATE TABLE artifact_issues (
  artifact_id CHAR(36) NOT NULL,
  issue_id CHAR(36) NOT NULL,
  PRIMARY KEY (artifact_id, issue_id),
  FOREIGN KEY (artifact_id) REFERENCES artifacts(id),
  FOREIGN KEY (issue_id) REFERENCES issues(id),
  INDEX artifact_issues_reverse (issue_id, artifact_id)
);

CREATE TABLE artifact_resources (
  artifact_id CHAR(36) NOT NULL,
  resource_id CHAR(36) NOT NULL,
  PRIMARY KEY (artifact_id, resource_id),
  FOREIGN KEY (artifact_id) REFERENCES artifacts(id),
  FOREIGN KEY (resource_id) REFERENCES resources(id),
  INDEX artifact_resources_reverse (resource_id, artifact_id)
);
```

Normalize identifiers (canonical lowercase UUIDv7) and label names (trim leading and trailing Unicode whitespace only, requiring nonempty names within `VARCHAR(100)` with no control characters while preserving spelling, accents, and interior spaces without case-folding or Unicode rewriting) before writes; rely on the `labels.name` unique key and installation collation for uniqueness, and validate collation and root slug uniqueness. In v2, `bots` stores registered persistent Queen identities; no role column, role table or role selector is needed for Bot identity, and multiple registered Queens are permitted in one workspace. The workspace-wide unique Bot slug is immutable in normal operations, including after retirement; the path `assets/bots/<slug>/SOUL.md` is derived, not a second stored locator. UUID remains the stable relational identity. Queen identity is distinct from session identity: a Queen may have multiple concurrent sessions, and `issue_claims.holder_ref` tracks execution/work scope rather than conflating all sessions of one Queen. Overlord and Drone are logical responsibilities without separate `bots` records. Retired Bots cannot be selected.

The pinned initial migration (`0001_initial.sql`, 24 tables) retains `bot_memories` unchanged to preserve schema compatibility and verified `#10` validation work while `#34` and `#20` reassess whether Bot-specific Curated Memory is required for MVP (Hive has no autonomous Bot memory requirement). Where `bot_memories` is used, the guarded domain operation validates live topic-key uniqueness per Bot and exact Area (including NULL global scope), verified approval or delegation and source, allowed scope, byte limits and current revision. Authorization fields describe the last mutation, not a persistent permission; verify the actual current grant under the lock. Never fabricate a reviewer for delegated writes. Workflow delegation and review references are validated against explicit operator/host-provided evidence; identity authentication and execution permissions remain outside Hive. No mandatory Hive security grant registry. Test evidence resolution and scope matching before enabling delegated mutations. Test NULL-aware uniqueness and concurrent changes in the spike. Latest has NULL recall_tier; durable requires core or reference. The logical views are `memories/LATEST.md`, `memories/core/`, and `memories/references/`, scoped to the selected Bot and authorized Area. Transitions between these categories edit one record rather than duplicating it. Check final aggregate byte totals and unique live topic keys for all touched scopes inside the guarded transaction; an explicitly requested multi-record curation batch may edit, classify and retire atomically, but never auto-evict. Read-only candidate reports and views cannot mutate records. Test all valid and invalid kind/tier combinations (including SQL NULL behavior) on the pinned release. Exclude retired records, retired Bots and archived Area ancestry from operational recall; historical inspection is separate. Do not add a Queen conversation table to `0001_initial.sql` prematurely; exact Queen conversation storage format, capture/read boundaries, and whether any relational index is warranted remain open implementation questions pending host evidence under `#34`.

Directive status or approval_ref alone is not permission; verify provenance and applicability. Supersession is a guarded domain transition, reject cycles and invalid scope changes. Artifact kind vocabulary is bounded by the domain (research, design, decision, report, review, handoff, evidence, work-note and retained legacy mappings). Artifacts can have no joins; any linked open Issue protects retention. For linked work use the latest applicable substantive/closure event; never infer cleanup eligibility from timestamps alone. Issue updates are append-only in normal use; corrections are new notes. Only guarded lifecycle commands create transition updates. Their bounded typed transition_json stores request ID, old/new states and Issue revisions, former/new Claim ID and holder as applicable, submitted result and exact stop/review/evidence references as applicable. Each review cycle retains the submission transition ID and submitter/holder references after Claim removal. Do not infer acceptance or former holder from free-form prose. No raw logs. Do not use polymorphic pseudo-FKs. The [MVP baseline](dolt-mvp-contract.md) owns remaining semantic constraints.

A mutation may change several rows. Each existing versioned row whose own fields or directly incident relationship change receives a new opaque revision, including both endpoints of join edits, old/new parents for a move, owning Issue for note or Claim changes, and owning Bot plus Memory for memory changes. Require expected revisions for these existing aggregates; do not increment unrelated transitive ancestors or dependents merely because a derived view changes. Snapshot identity plus fresh graph validation covers derived readiness and ancestry. Receipt result_json is bounded canonical metadata (IDs, revisions, outcome kind; no bodies/secrets), validated by the domain. Store original base_commit, actor, authorization reference and the minimal canonical nonsecret workflow evidence snapshot actually used (workflow_evidence_json) in the same SQL transaction. This is historical provenance, not an active grant or authentication proof. Find the confirmed commit from retained authoritative history; do not claim a self-referential hash in the row. No history rewriting or automatic receipt removal. Update the affected aggregate's revision on relationship changes as well as field edits; enforce request ID idempotency and expected revision in the domain layer. Record closure, claim, graph, archive and retention invariants across tables with guarded domain validation. The schema migration ledger is authoritative; test DDL transaction semantics instead of assuming transactional DDL. Keep failed migration state isolated and require explicit recovery.

## Read model

Ready candidate view (not the complete readiness predicate; archived Area ancestry and workflow conditions remain required domain checks; external execution permission is a separate host concern):

```sql
CREATE VIEW ready_issue_candidates AS
SELECT i.id, i.area_id, i.title, i.priority
FROM issues i
WHERE i.status = 'todo'
  AND i.type <> 'epic'
  AND i.hold_reason IS NULL
  AND NOT EXISTS (
    SELECT 1 FROM issue_claims c WHERE c.issue_id = i.id
  )
  AND NOT EXISTS (
    SELECT 1 FROM issue_dependencies d
    JOIN issues p ON p.id = d.prerequisite_id
    WHERE d.issue_id = i.id AND p.status <> 'done'
  );
```

Only accepted done satisfies a prerequisite; cancelled does not. The public ready operation filters candidate rows against archived Area ancestry and the remaining domain rules before returning them. Blocked reasons, children, dependents, project progress and recursive Area paths are derived. Query membership with joins, not copied arrays. Recursive traversal and its limits must be tested. Ready is not a claim or authority.

## Transaction and merge safety

Dolt supports FK, CHECK, indexes and SQL transactions, but its merge can combine independent cell updates. The transaction docs describe Read Committed while the vendor's 2026-02-17 concurrency article describes Repeatable Read and warns of lost updates. Do not assume serializable isolation or ordinary row locking. Test the installed release.

Recommended MVP protocol: a verified OS-held exclusive workspace lock serializes all cooperating authoritative writers and maintenance on one local machine. Under the lock, resolve a matching existing receipt before checking original expected revisions for a new mutation; otherwise replay incorrectly fails after its own successful revision change. For a new request validate affected state and graph, persist a bounded local pending-request recovery marker before attempting SQL commit, write one transaction and receipt, confirm version commit, clear marker, then release. The marker identifies the pending operation and its earlier server session for recovery. An absent receipt or unchanged visible history cannot prove rollback before the earlier transaction is known unable to publish. Direct administrative SQL is offline maintenance only; independent clones and bypassed writers are not supported. The previous singleton random mutation_guard token is a possible experimental additional check, not required schema. Test process contention, crash and ambiguous commit recovery; no safety claim until evidence. See [MVP design baseline](dolt-mvp-contract.md).

Claim acquisition inserts a unique fresh claim_id into the issue-keyed claims table and changes issue status/revision in the same guarded transaction. Recheck current state, prerequisites, authority and expected revision; no upsert to replace an owner. Submission records outcome and evidence, verifies execution stopped, and clears the claim atomically on transition to review. Check accepted prerequisites on submission and separate authorized acceptance, including for epics. Reopening a prerequisite with running dependents requires explicit safe reconciliation. Reject cycles in Area, issue parent and dependency graphs, including concurrent edge additions. Also reject cycles in the combined completion-obligation graph, where an Issue points to prerequisites and a parent points to each child for acceptance. The combined graph is for validation only; it does not add prerequisite rows or change readiness. FK and self-edge checks alone do not detect cycles.

Use one authoritative shared main branch. Validate the precise SQL transaction/version-commit policy and mutation receipt before announcing success. No independent-clone global claim guarantee, direct SQL writes to operational tables, force commit with constraint violations, or unreviewed remote merge. Import/merge needs full domain validation, including graphs and claims. Dolt history is evidence, not proof of runtime stop or acceptance.

References: [transactions](https://www.dolthub.com/docs/concepts/dolt/sql/transaction/), [concurrency](https://www.dolthub.com/blog/2026-02-17-dolt-concurrency/), [constraints](https://www.dolthub.com/docs/concepts/dolt/sql/constraints/), [Dolt procedures](https://www.dolthub.com/docs/sql-reference/version-control/dolt-sql-procedures/).

## Schema verification

Use the [synthetic spike evidence (#24)](https://github.com/mols3131d/hive-system/pull/24) when verifying the implementation. Pin Dolt version and test DDL, root uniqueness, cyclic references, duplicate claims, claim-versus-dependency changes, same-value writes, competing lock acquisition, crash and ambiguous receipt recovery, cancellation/reopen, transaction rollback, version commit failure/recovery and invalid merge. Test offline operation and restoration on synthetic data only.
