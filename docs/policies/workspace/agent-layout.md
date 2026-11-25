---
description: "Read when assigning Manager, Coordinator or Executor roles, deciding whether to delegate work, or clarifying responsibility and authority."
---

# Agent Layout

Hive defines three logical layers of agent responsibility: **Manager, Coordinator and Executor**. Layers describe the role performed in a work unit, not a particular model, runtime or permanent bot identity.

| Layer | Role | Primary responsibility |
| --- | --- | --- |
| 1 | Manager | Human-facing agent: clarify intent, manage goals, priorities, acceptance criteria and communicate results. |
| 2 | Coordinator | Plan and decompose work, delegate tasks, manage dependencies, coordinate verification and integrate results. |
| 3 | Executor | Perform scoped work, make implementation-level decisions, verify results and report evidence or blockers. |

## Bot eligibility and continuity

**Only the Manager layer is eligible for registration as a persistent [Bot](bots.md).** Registration is not a requirement for every Manager. Coordinators and Executors are task-scoped agents, not registered Bots; do not provision separate bot identities or long-term bot memory for them.

Task-scoped agents may span multiple sessions. Resume from delegated context, task state, project sources and artifact checkpoints or handoffs rather than a separate persistent bot memory. The Manager supplies relevant, authorized context when delegating; do not load its entire memory into delegated agents.

## Manager execution

**A Manager may perform simple tasks directly. This is permission, not a requirement or default.** The Manager may instead delegate even a simple task. Choose based on complexity, risk, available capabilities and the cost of coordination; never bypass applicable authority or verification requirements.

The Manager remains responsible for the human-facing outcome whether the work is performed directly or delegated.

## Delegation and accountability

- Use only the layers needed for the task. Do not require every request to traverse all three layers. One agent may fulfill multiple responsibilities when appropriate; identify the responsibility being exercised. This does not change Bot registration eligibility.
- When delegating, communicate a self-contained goal, scope, acceptance criteria, authority boundaries, relevant source and work references, expected output and verification method as needed. Coordinators may adapt the plan within the delegated scope; Executors may choose implementation details within their assigned work. An Executor may delegate a narrower part when useful and supported, taking Coordinator responsibility for that part without losing accountability for its original assignment. Do not multiply agents without a concrete benefit.
- The assigning agent retains responsibility for its delegated outcome. Inspect actual deliverables at the relevant revision, reconcile conflicting findings and check acceptance before integrating or accepting results. A worker's completion report, majority agreement or missing result is not evidence of acceptance. Use independent verification when the risk or uncertainty warrants it; do not require another agent for every task.
- Report results, evidence, uncertainties and blockers to the assigning role. Escalate material changes to the goal, scope or authority to the appropriate decision owner. The Manager communicates decisions and outcomes to the human as needed. Delegation returns results to the assigning role; a handoff transfers a specified execution responsibility only when the new owner and authority are established.
- Roles do not grant permissions. Follow [decision and action authority](organization.md#decision-and-action-authority) when delegating or acting, [task management](tasks.md#selection-and-decomposition) for work selection, and [concurrent work](communication.md#concurrent-work) for isolation.

## User-directed unattended work

For autonomous execution, read [unattended work](unattended-work.md). It owns initiation, progress and exit conditions; the delegation responsibilities above still apply.

## Workspace integration

The Manager selects relevant verified directives from the [user-authorized collaboration backend](organization.md#collaboration-backend) and, when applicable, [scoped bot memory](bots.md). For Hive-managed work, use [directives](directives.md), [tasks](tasks.md) and [artifact-based communication](communication.md). Do not create Hive records for explicitly Notion-managed work. Bot records identify registered Managers and routing references; actual availability, capabilities and permissions must be verified. Route task-scoped agents through work and session references.

This is a responsibility model, not a requirement for separate processes, automatic scheduling or agent orchestration. Hive records do not themselves activate another agent.
