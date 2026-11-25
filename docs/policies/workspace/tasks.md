---
description: "Read when creating, locating, selecting, updating, completing or archiving a work bundle."
---

# File-Based Tasks

These file-based rules apply only to scopes for which Hive is the [selected collaboration backend](organization.md#collaboration-backend). Use the designated backend's native work records for other scopes; do not create a parallel Hive bundle. The project directory scopes work and its lifecycle areas; the work entrypoint's frontmatter identifies progress. Kanban is a possible view, not the storage layout.

| Directory | Meaning |
| --- | --- |
| `tasks/<project-id>/backlog/` | Captured work not yet selected for active management |
| `tasks/<project-id>/active/` | Selected work, including waiting, execution, review and accepted outcomes pending closure |
| `tasks/<project-id>/archive/` | Closed work retained for reference, with a recorded outcome |

## Work bundle

Use the shared [project identity and reserved scopes](organization.md#project-identity). Each project or reserved scope has a README identifying its project or purpose and three lifecycle directories. The combination of project/scope ID and work ID identifies exactly one bundle across its three areas. Create ordinary project directories only when work exists; retain the three empty lifecycle areas with `.gitkeep` as needed for Git. Do not create empty work or artifact bundles. Cross-project work with a confirmed owner stays with that owner; reference other projects rather than duplicating the bundle. Genuinely non-project work belongs to `_general`, not an arbitrary project.

```text
tasks/<project-id>/
├── README.md
├── backlog/
├── active/
│   └── <work-id>/
│       ├── README.md
│       └── artifacts/
│           ├── research.md
│           └── review.md
└── archive/
```

Only `README.md` is required inside a work bundle. Record the goal, applicable acceptance criteria, source references and relevant progress or blockers. Link applicable verified [project directives](directives.md) rather than copying them. Create `artifacts/` only for intentional deliverables. Use the [work template](../../../assets/templates/task.md) and [frontmatter rules](frontmatter.md).

## Selection and decomposition

The Manager sets work priorities within user intent; a Coordinator may select and assign work within its delegation, and an Executor may select further work only if explicitly authorized to do so. Check dependencies, blockers, acceptance criteria, current ownership and authority before selecting an executable item. Do not start dependent execution until its required predecessor outcome is accepted; independent work may continue within the authorized scope. A backlog entry, priority or status alone does not grant authority. The assigning agent remains responsible for results under [delegation and accountability](agent-layout.md#delegation-and-accountability).

Distinguish a tracked Task, which has its own goal, acceptance and lifecycle, from a temporary execution unit such as a research slice, implementation step or verification pass. Keep units within the existing task unless independent priority, acceptance, ownership or lifecycle genuinely needs a separate task. Record the minimal useful plan and assignment in the existing work record when needed for coordination; do not create mandatory child bundles or a separate state model for sessions. New findings outside authorized execution may be recorded as backlog candidates when useful and within record-writing authority; do not automatically select or execute them.

## Progress and lifecycle

The active work entrypoint requires a `status`. Backlog or active entries may specify a priority as defined in [frontmatter](frontmatter.md); priority is not readiness or execution authority.

| Status | Meaning |
| --- | --- |
| `todo` | Selected, not yet started or ready to resume when prerequisites allow |
| `in-progress` | Execution underway |
| `review` | Awaiting review, verification or decision |
| `done` | Acceptance met and outcome recorded |

Record blockers in the body rather than implying that every `todo` item is executable. After a run stops, reflect the actual work state: use `todo` for unfinished work awaiting execution or resumption and `review` for a result awaiting verification or decision. If an execution may still be active, record that uncertainty and do not treat the work as available for reassignment. A run ending is not a terminal task outcome. Backlog entries omit status; archived entries retain terminal status `done` or `cancelled` with the outcome or cancellation reason. Do not infer acceptance from cancellation.

- Resolve the project or reserved scope first, then the work ID across its three areas before creating or updating a bundle. Never duplicate a work ID within the scope or overwrite a concurrent writer's changes.
- Select work by moving the whole bundle from backlog to active and setting status in the same Git change. Active progress changes update README frontmatter without moving the bundle. A Manager may create directly in active when work is already selected.
- Close accepted work only after the responsible agent or human verifies the relevant acceptance criteria against actual results and required evidence, and any needed handoff is complete. Respect project-specific review or approval requirements; an Executor's report alone is not acceptance. Archive the whole bundle with its terminal outcome and closure date; explicit cancellation may move backlog or active work to archive with its reason. Apply the [retention boundary](lifecycle.md) after closure. Reopen archived work only with appropriate authority and a new active status; preserve its history.
- Move only at lifecycle boundaries, preserve IDs and internal relative paths, and recheck affected external references. Re-resolve the current location and revision before writing; use a guarded write against the observed head and never recreate a stale path. If the bundle moved, read its current state and coordinate the artifact update with the owner; do not treat a merge or Git conflict resolution as acceptance.
- The Manager owns the outcome and priorities; the Coordinator tracks delegated work and acceptance, or the Manager fulfills that responsibility directly. Follow [Agent Layout](agent-layout.md) and actual approval requirements.

By default, inspect tasks only within the relevant project or reserved scope. For an explicitly cross-project active-work query, discover `tasks/*/active/*/README.md` and select by metadata. A Kanban, list or ready-work view may be derived from the same task records. Do not maintain a second state owner or treat a view as a scheduling or permission mechanism. Archiving does not authorize disposal; follow [lifecycle](lifecycle.md).
