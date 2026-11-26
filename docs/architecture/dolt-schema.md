---
description: "Review the proposed relational tables, constraints, readiness queries and concurrent mutation design for the Dolt migration."
---

# Proposed Dolt Relational Schema

Status: candidate design under [#22](https://github.com/mols3131d/hive-system/issues/22) and [architecture redesign](dolt-redesign.md). This is not a migration, adopted schema, or tested implementation.

## Relational conventions

One database per workspace. Do not partition by Area, Project or repository. Use immutable application-generated UUIDs (UUIDv7 candidate; CHAR(36) initially) as PKs; paths, names and display numbers are not identities. Workspace-wide display issue numbering is optional; if added, allocate through authoritative writes, not MAX+1 or independent clones. Use UTC timestamps and explicit schema migrations. No automatic deletion cascades on business records. Prefer archive/close to delete. Stable IDs survive moves. A human or session holder reference is not a registered Bot ID or credential.

## Core DDL candidate

Illustrative MySQL-compatible SQL; verify against a pinned Dolt release. Root sibling slug uniqueness needs additional domain validation because nullable parent IDs do not enforce it through a composite unique key. All mutations affecting those invariants use the same guarded authority.

```sql
CREATE TABLE areas (
  id CHAR(36) PRIMARY KEY,
  parent_id CHAR(36),
  slug VARCHAR(100) NOT NULL,
  name VARCHAR(255) NOT NULL,
  description TEXT,
  archived_at DATETIME(6),
  UNIQUE (parent_id, slug),
  FOREIGN KEY (parent_id) REFERENCES areas(id),
  CHECK (parent_id IS NULL OR parent_id <> id)
);

CREATE TABLE projects (
  id CHAR(36) PRIMARY KEY,
  name VARCHAR(255) NOT NULL,
  description TEXT,
  status VARCHAR(16) NOT NULL,
  CHECK (status IN ('active', 'completed', 'archived'))
);

CREATE TABLE issues (
  id CHAR(36) PRIMARY KEY,
  area_id CHAR(36) NOT NULL,
  parent_id CHAR(36),
  title VARCHAR(255) NOT NULL,
  body LONGTEXT,
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
  CHECK (type IN ('epic','feature','task','bug')),
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
  uri TEXT
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

CREATE TABLE mutation_guard (
  id TINYINT PRIMARY KEY,
  token CHAR(36) NOT NULL,
  CHECK (id = 1)
);
```

Initialize exactly one mutation_guard row at explicit workspace bootstrap. This is a conservative candidate to test, not a proven Dolt lock. The MVP proposes required issue Area ownership (an explicitly chosen general Area for unclassified work, not an implicit fallback), one parent at most, optional project memberships, and a globally defined 0–3 priority (0 highest, NULL unspecified). Issue types, lifecycle and priority are reviewable decisions. Project-specific priority/order is deferred to membership fields if justified. No implicit dependency from parentage or project inclusion. Archive policy and ancestry-based eligibility must be made explicit before final readiness contract.

## Additional tables and ownership

| Table | Key and semantics |
| --- | --- |
| labels | id PK; unique normalized name within workspace |
| issue_labels | composite PK (issue_id, label_id), two FKs |
| bots | id PK, unique stable role-qualified locator; role immutable under normal operations; no duplicate Soul/Memory content |
| directives | id PK, explicit applicable scope relation, Markdown body, provenance and validity |
| artifacts | id PK, kind, title, provenance, body or file locator with content digest; choose canonical ownership before DDL |
| artifact_issues / artifact_projects / artifact_areas | explicit FK join tables only when needed; no generic polymorphic foreign key |
| schema_migrations | applied version and migration identity; compatibility check before writes |

Bot and asset storage, actor registry, directive scope/validity and artifact retention still require separate decisions. Do not use a generic entity-reference table to evade FK constraints. Do not store raw sessions, credentials or private paths in shared records. Environment-specific resource path bindings stay outside shared data.

## Read model

Ready candidate view (execution permission and archived Area handling remain domain checks):

```sql
CREATE VIEW ready_issues AS
SELECT i.id, i.area_id, i.title, i.priority
FROM issues i
WHERE i.status = 'todo'
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

Only accepted done satisfies a prerequisite; cancelled does not. Blocked reasons, children, dependents, project progress and recursive Area paths are derived. Query membership with joins, not copied arrays. Recursive traversal and its limits must be tested. Ready is not a claim or authority.

## Transaction and merge safety

Dolt supports FK, CHECK, indexes and SQL transactions, but its merge can combine independent cell updates. The transaction docs describe Read Committed while the vendor's 2026-02-17 concurrency article describes Repeatable Read and warns of lost updates. Do not assume serializable isolation or ordinary row locking. Test the installed release.

Candidate MVP protocol: every authoritative domain mutation reads the singleton mutation_guard token, updates it to a distinct random token in the same transaction, then validates the complete affected graph/state and applies changes. A conflicting token must cause one concurrent transaction to fail at commit; a failure retries from a fresh snapshot only after reconciliation. Avoid identical increment values as conflict signals. The guard covers dependency changes, Area moves, readiness/claim acquisition, acceptance and release. Treat missing guard or bypassed SQL writers as invalid authority. This intentionally serializes successful writes for a personal workspace; benchmark and test the exact Dolt behavior before adoption. If not guaranteed, use a single-writer service or proven interprocess serialization; never claim safety from this sketch alone.

Claim acquisition inserts a unique fresh claim_id into the issue-keyed claims table and changes issue status/revision in the same guarded transaction. Recheck current state, prerequisites, authority and expected revision; no upsert to replace an owner. Acceptance records outcome and clears claim in the same mutation after verified execution stop. Reopening a prerequisite with running dependents requires explicit safe reconciliation. Reject cycles in Area, issue parent and dependency graphs, including concurrent edge additions; FK and self-edge checks alone do not detect cycles.

Use one authoritative shared main branch. Validate the precise SQL transaction/version-commit policy and mutation receipt before announcing success. No independent-clone global claim guarantee, direct SQL writes to operational tables, force commit with constraint violations, or unreviewed remote merge. Import/merge needs full domain validation, including graphs and claims. Dolt history is evidence, not proof of runtime stop or acceptance.

References: [transactions](https://www.dolthub.com/docs/concepts/dolt/sql/transaction/), [concurrency](https://www.dolthub.com/blog/2026-02-17-dolt-concurrency/), [constraints](https://www.dolthub.com/docs/concepts/dolt/sql/constraints/), [Dolt procedures](https://www.dolthub.com/docs/sql-reference/version-control/dolt-sql-procedures/).

## Required spike

Pin Dolt version and test DDL, root uniqueness, cyclic references, duplicate claims, claim-versus-dependency changes, same-value writes, competing guard changes, cancellation/reopen, transaction rollback, version commit failure/recovery and invalid merge. Test offline operation and restoration on synthetic data only. This design has not been executed.
