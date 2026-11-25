---
description: "Read when sharing a deliberate handoff or other artifact, resuming work, or verifying agent communication and delivery."
---

# Artifact Exchange and Handoffs

Agents may communicate through their authorized runtime or integration. Hive retains selected artifacts only when a durable deliverable, useful handoff or evidence of an outcome is needed. It is not a general mailbox, session database or transcript archive.

## Runtime and persistence

Agents may use communication and delegation capabilities already available in their authorized execution environment. Those capabilities are not task or artifact ownership. Hive does not implement runtime adapters, session-control services, schedulers or automatic wakeups, and has no dependency on Paseo or any other specific runtime. Resolve the [selected collaboration backend](organization.md#collaboration-backend) before delegating or resuming a work unit. Keep task state and intentional artifacts there, independent of runtime session IDs. The following publishing procedure is for Hive-managed work; use the selected backend's native records for other work.

Prefer sending a concise goal, project-qualified work ID, artifact reference and source revision, expected action and authority boundary instead of copying the full context into messages. The receiver resolves current state and reads the relevant records. Use runtime delivery/status evidence to distinguish an actual handoff from a saved artifact. Confirm the recipient's assignment and acceptance of the execution responsibility before treating a handoff as transferred; delivery alone does not prove acceptance or authority. If runtime access is unavailable, retain only an intentional handoff in the selected backend and report delivery as unverified.

## Concurrent work

The assigning Manager or Coordinator confirms each work unit's execution owner before delegation or resumption. Prefer independent writable outputs and non-overlapping implementation scopes; for code, use separate worktrees or equivalent isolation when appropriate under project policy. Parallel research or independent review of the same source is allowed; competing mutations of the same logical change are not the default. Designate a responsible writer for shared task state and integrated results. When a shared write target is necessary, coordinate sequential writes or use existing guarded revision checks rather than relying on instructions alone. Verify semantic compatibility, dependencies and acceptance after integration; a clean Git merge is not proof of correctness.

Recheck existing execution and current revisions before reassigning; if ownership or termination is uncertain, do not start competing execution or writes. A work status, Git commit or message is not an exclusive claim or execution approval. Hive does not implement distributed claims, leases or automatic reassignment. Follow [user-directed unattended work](unattended-work.md) for autonomous execution boundaries.

## Publish and resume

1. Identify the task and intended recipient or reader. Locate its current [work bundle](tasks.md) by project-qualified work ID. Save any needed artifact under its `artifacts/` directory, or root `artifacts/` for a genuinely independent deliverable.
2. A handoff can be a single concise document with the relevant goal, source repository/ref, confirmed progress and checks, material decisions, blockers, next actions and intended recipient. Include originating session or agent reference only when useful for provenance. Link to existing evidence instead of copying it. A separate message or checkpoint is optional; do not generate both by default.
3. Re-read the actual bundle location, Hive ref and external source revision before continuing. Reconstruct what already landed and what remains from the work record, actual changes and relevant evidence; revalidate scope, authority and inherited claims against the current artifacts. Reuse still-valid work rather than repeating it. If the bundle moved or the observed head changed, re-resolve its current state before publishing. Distinguish verified facts from assumptions, stale information and unconfirmed delivery. Update an owned record with guarded writes, or create a distinct artifact for a different writer; never overwrite another agent's work.
4. Use the [publishing rules](git-writes.md) for authorized writes. A Git commit is a durable record, **not** proof of receipt, notification, runtime activation or approval. Use an authorized delivery mechanism when actual delivery is required.

Retain material decisions, verification results, failed approaches that affect the next decision, and unresolved scope or authority boundaries only when useful for review or resumption. Reference actual evidence and distinguish verified, failed and inconclusive checks. Use the existing task or intentional artifact; no mandatory decision-log format, per-iteration log or checkpoint file is required. Apply the [artifact retention policy](lifecycle.md#artifact-retention).

Keep routine communication and ephemeral context out of Hive. Do not retain raw private conversations, hidden reasoning, credentials, logs or unreviewed sensitive output. Memory and permanent project policy remain with their respective owners. See [organization](organization.md), [optional templates](../../../assets/templates/README.md) and [lifecycle](lifecycle.md).
