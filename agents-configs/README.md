---
description: "Select portable Hive agent assets for an explicitly authorized data workspace, separate from the system development harness."
---

# Distributable Hive Agent Assets

This directory is the canonical [Rulesync source tree](https://rulesync.dyoshikawa.com/guide/separate-input-root) for Hive product Agent Assets. Author and maintain the distributed definitions using Rulesync's native [file formats](https://rulesync.dyoshikawa.com/reference/file-formats), not a Hive-specific asset schema or generated runtime files. Place rules, commands and subagents directly in `rules/`, `commands/` and `subagents/`; put future Skills in `skills/<name>/SKILL.md` with their required companion files. The directory itself is the input root, so no nested `.rulesync/` or `rulesync/` directory is needed.

The root `.rulesync/` directory and root `rulesync.jsonc` are for agents developing Hive, not for product distribution. Do not automatically load product sources into the system development harness, include development-only assets in a product distribution, or maintain duplicate canonical definitions across the two scopes. The [workspace policies](../docs/policies/workspace/README.md) own behavior; product assets route to relevant policy rather than duplicate it.

| Source | Purpose |
| --- | --- |
| [hive-records](rules/hive-records.md) | Path-scoped policy routing for workspace records |
| [hive-resume](commands/hive-resume.md) | Explicit read-only work resumption |
| [hive-context-reader](subagents/hive-context-reader.md) | Bounded read-only work context collection |

## Use in a data workspace

With a Rulesync version supporting separate input roots, select `agents-configs/` directly as an input root in the consuming workspace's Rulesync configuration. For example, the following configuration fragment assumes a reviewed system checkout next to the workspace:

```jsonc
{
  "inputRoots": ["../hive-system/agents-configs"]
}
```

Keep the consumer's own target and feature selection; do not reuse the system development configuration. If combining roots, review their order and name collisions, since later roots can override earlier assets. Preserve existing consumer-local definitions and preview generated writes and deletions before authorized generation. Alternatively, copy the selected definitions into the appropriate workspace-local `.rulesync/` directories at a reviewed system revision. Do not both copy and select the same source definitions.

Keep the data workspace's `AGENTS.md` as its entrypoint to the reviewed system policy revision; the paths named inside these assets are relative to that system revision, not to the data checkout. The agent must be able to read the actual policy from a verified source, including when it has only local read tools. Do not grant network or additional tool access merely to make an asset work.

Use the existing [asset ownership](../docs/development/agents/assets.md) and [harness verification](../docs/development/agents/harness.md) guidance for review. Review the product sources, supported target formats and compatible policy revision before an explicit consumer installation or release. No Hive-specific install command, packaging mechanism, automatic asset sync or runtime adapter is provided. A data workspace can operate directly through its instructions and Git tools without these optional assets. Until actual projection and representative runtime checks are observed, activation remains unverified.
