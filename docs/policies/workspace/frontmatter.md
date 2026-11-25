---
description: "Choose required, optional and conditional YAML fields for each Hive record or directory entrypoint."
---

# Workspace Frontmatter

This document owns metadata for Hive Markdown records and entrypoints. Use the record's path and purpose to select its contract; do not add fields merely because another area uses them. Follow [ordinary documentation metadata](../../documentation/frontmatter.md) for docs, and native formats for runtime-owned agent assets, tool configuration and binary or structured files.

## Common fields

All new or updated workspace Markdown entrypoints and records under `tasks/`, `directives/`, `bots/` and `artifacts/` use the following common fields. This includes every directory README, even historical directory entrypoints. Retained historical non-README evidence need not be migrated solely for metadata. The repository root README is exempt under the [documentation contract](../../documentation/frontmatter.md).

| Field | Requirement | Type and meaning |
| --- | --- | --- |
| `description` | Required | Nonempty string describing the directory or bundle for README files; purpose and selection context for other records |
| `title` | Optional | Nonempty human-readable title, only when useful beyond the filename or heading |
| `tags` | Optional | List of distinct nonempty strings used for meaningful classification |

Use quoted strings for descriptions and titles. Tags may use quoted colon-separated paths such as `"domain:identity:tokens"` without empty segments. Omit empty, unknown and unused optional fields; do not use placeholders, null or empty lists in finished records. Order common fields as description, title, area-specific fields, then tags. Do not duplicate project, bot or work IDs already encoded by paths, or add universal timestamps merely to duplicate Git history.

## Area-specific fields

Common fields apply in every row. A dash means no additional metadata is required or defined for that record type.

| Record or entrypoint | Additional required fields | Additional optional fields |
| --- | --- | --- |
| Area README; project or reserved-scope README | — | — |
| `tasks/<project-id>/backlog/<work-id>/README.md` | — | `priority` |
| `tasks/<project-id>/active/<work-id>/README.md` | `status`; `claim` for `in-progress` under the adopted coordination contract | `priority`, `claim` on eligible `todo` or `review` under that contract |
| `tasks/<project-id>/archive/<work-id>/README.md` | `status` | — |
| `directives/<project-id>/<slug>.md` or `<slug>/README.md` | `status` | — |
| Supporting Markdown within a directive bundle | — | — |
| Standalone or work-scoped artifact file or bundle README | — | `kind`, `status` |
| Bot README, SOUL.md, memory navigation, scoped MEMORY.md and topic notes | — | — |

A directory README's description identifies the purpose and scope of its directory, not its README file. A scope README identifies its verified project or non-project purpose in the body. Directory navigation is not a task, directive or artifact record; omit status, priority and kind there. Identity, routing, provenance, evidence and source references belong in the appropriate body sections. Do not introduce unowned metadata fields without updating this contract.

### Tasks

| Area | Allowed status |
| --- | --- |
| Backlog | Omit |
| Active | `todo`, `in-progress`, `review`, `done` |
| Archive | `done`, `cancelled` |

Optional `priority` is one of `low`, `normal`, `high` for backlog or active work. Omission means unspecified, not normal. Priority is relative to the scope and does not establish scheduling, readiness or authority. Omit priority when archiving; retain relevant historical context in the body if needed. Record blockers, goals, acceptance criteria, dependencies and assignments in the body when needed. See [task lifecycle](tasks.md).

```yaml
---
description: "Continue API authentication improvements."
status: in-progress
priority: high
tags:
  - "domain:identity:tokens"
---
```

The adopted [Git-backed coordination contract](tasks.md#git-backed-issuance-and-claims) uses an optional active-task `claim` mapping with exactly two required nonempty string fields:

| Field | Meaning |
| --- | --- |
| `id` | Fresh opaque identifier for this assignment, distinct from any earlier assignment of this task; generated on acquisition or transfer |
| `holder` | Authorized task-scoped execution responsibility reference; a stable label for the assignment, not a Git email, credential, mandatory Bot or device ID |

Omit `claim` for unclaimed work, backlog, archived work and active `done`. An active `in-progress` record requires a claim under the adopted contract. Example:

```yaml
---
description: "Continue API authentication improvements."
status: in-progress
claim:
  id: "5d9eb872-8b4e-4cba-97bf-83df96eab245"
  holder: "worker-a"
---
```

The mapping records only current assignment; Git history retains prior assignments. Keep recovery, transfer or closure evidence in the task body when needed. Do not add claim timestamps, expiry, credentials, raw runtime context or a separate session registry. Structural validation does not establish holder authority or runtime termination. Earlier pinned workspaces need no claim fields until reviewed adoption.

### Directives

Require `status` on the directive entrypoint to identify applicability at selection time. Supporting documents use common fields only; do not repeat or independently override the entrypoint's applicability.

| Value | Meaning |
| --- | --- |
| `proposed` | Direction not yet established as applicable |
| `active` | Current direction, subject to source, authority, scope and validity verification |
| `superseded` | Replaced by another direction |
| `withdrawn` | No longer applicable without a replacement |

Keep issuer, source/approval reference, applicable scope, date, conditions, expiry and any replacement reference in the body. An active label does not prove approval or grant permission. Expired, conflicting or unverified direction is not actionable. Follow [directive rules](directives.md).

### Artifacts

Optional `kind` classifies purpose for discovery: `report`, `research`, `review`, `handoff`, `message`, `checkpoint`, `work-note`, `manifest`. Omit it for unclassified records or general bundle navigation. A bundle's kind describes its purpose, not its file format.

Optional `status` describes temporary artifact usefulness: `active`, `parked`, `closed`. Omission means unclassified. It is not task progress, review approval, delivery, verification or cleanup permission. An artifact inside a task does not inherit the task's status. Follow [ownership](organization.md) and [retention](lifecycle.md).

### Bots and memory

Use common fields only. The path determines the bot and memory scope; identity and purpose live in SOUL.md, verified routing in the bot README, and scoped learning and per-entry source/review dates in memory. Do not add task or artifact status, priority or kind. See [bot rules](bots.md).

## Templates and verification

Choose a [template](../../../assets/templates/README.md) matching the record type. Required fields must be present; optional fields are omitted by default except a template's known artifact kind. Replace placeholders and remove authoring comments and unused sections. Ordinary work record body sections use H2.

Use the applicable workspace validator when available and review each actual workspace record against this table and the relevant area contract. System repository documentation checks validate system documentation and their explicitly configured inputs, not an external data workspace or its full record schema. A Hive CLI validator is proposed in the [system design](../../architecture/system-design.md), not implemented by this contract. Do not claim that a format check proves authority, applicability, actual task acceptance or retention eligibility.
