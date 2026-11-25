---
description: "Read when retaining, promoting, moving or cleaning up work bundles and standalone artifacts."
---

# Temporary Workspace Lifecycle

Work bundles and standalone artifacts support execution, review, delivery and intentional knowledge exchange; they are not the canonical archive of external project policies or specifications.

| Surface | Retention boundary |
| --- | --- |
| Work-scoped artifacts | Default three months, then delete, subject to the exceptions below |
| Root `artifacts/` | Default three months, then delete, subject to the exceptions below |
| Work entrypoints and bundles | Retain while work or handoff is open; apply the closure and cleanup rules below |
| Bot routing metadata | Retain while relevant; verify actual runtime availability separately |

[Project directives](directives.md) follow their own validity and supersession rules; retain currently applicable direction and required traceability. They are not retired merely because a related task closes.

[Bot identity and curated memory](bots.md) and private configuration are durable sources outside temporary cleanup. Repository operating docs and reusable assets are likewise not temporary records.

## Cleanup

Before removing any record, confirm actual completion or disuse, consumer needs, outstanding blockers, validation and incoming references. Transfer durable project findings to the real project owner and bot-specific reusable learning to reviewed memory when eligible. A transfer is not an approval or automatic completion.

### Artifact retention

The default is to retain an artifact for **three months**, then delete it from the current workspace. This applies to both work-scoped and standalone artifacts, including temporary specifications. A supervising agent or human may decide to absorb useful content into its canonical owner's documentation or explicitly archive an artifact for continued retention. Record the decision and relevant reference in a record that survives cleanup, such as the owning task, retained artifact or canonical document as appropriate; follow the canonical owner's policy and approval requirements. Verify an approved transfer before deleting its source. An artifact does not become canonical merely because it is archived.

Use the existing retention reference point: the latest of creation, substantive update and, for a work-scoped artifact, actual work completion or cancellation. A move, rename or metadata-only change does not restart retention. Do not delete evidence needed by open work or handoffs. If age, ownership or a material dependency is uncertain, preserve the affected artifact pending a decision rather than guessing. Archiving a closed task does not itself exempt its artifacts from the three-month default. No automatic cleanup job, new archive layout or universal metadata field is required by this policy.

### Work records

Record the outcome and closure date in the work README when closing. The existing minimum seven-day retention for a closed work entrypoint remains separate from artifact retention: count from the latest of creation, substantive update and actual completion or cancellation. A whole-bundle deletion must also satisfy the retention requirements of its artifacts and preserve any explicitly archived content. Setting `status: done` or moving a work bundle to its archive area does not authorize disposal. Do not create redundant permanent copies.

Never commit secrets, personal data, raw private conversations, reasoning traces or unreviewed sensitive outputs. Git history may retain earlier versions after deletion.
