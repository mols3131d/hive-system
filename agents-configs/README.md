---
description: "Select portable Hive agent assets for an explicitly authorized data workspace, separate from the system development harness."
---

# Distributable Hive Agent Assets

This directory is the canonical [Rulesync source tree](https://rulesync.dyoshikawa.com/guide/separate-input-root) for Hive product Agent Assets. The current product sources target the pinned **v1** Markdown/Git workspace contract; Dolt v2 workspace layout and Skill updates are specified separately in the [v2 workspace contract](../docs/architecture/dolt-workspace-contract.md) and remain unimplemented. Author and maintain the distributed definitions using Rulesync's native [file formats](https://rulesync.dyoshikawa.com/reference/file-formats), not a Hive-specific asset schema or generated runtime files. Place rules, commands and subagents directly in `rules/`, `commands/` and `subagents/`; put future Skills in `skills/<name>/SKILL.md` with their required companion files. The directory itself is the input root, so no nested `.rulesync/` or `rulesync/` directory is needed.

The root `.rulesync/` directory and root `rulesync.jsonc` are for agents developing Hive, not for product distribution. Do not automatically load product sources into the system development harness, include development-only assets in a product distribution, or maintain duplicate canonical definitions across the two scopes. The [workspace policies](../docs/policies/workspace/README.md) own behavior; product assets route to relevant policy rather than duplicate it.

| Source | Purpose |
| --- | --- |
| [hive-records](rules/hive-records.md) | Path-scoped policy routing for workspace records |
| [hive-resume](commands/hive-resume.md) | Explicit read-only work resumption |
| [hive-context-reader](subagents/hive-context-reader.md) | Bounded read-only work context collection |

## Use in a data workspace

Hive's core is independent of IDE, coding-agent vendor and model provider. Rulesync and runtime-specific projections are optional consumer conveniences, not product runtime requirements. Install or project Hive assets only into an explicitly authorized Hive workspace or appropriate user-local environment. An optional, explicitly authorized ignored project `.local/hive/config.yml` is only a connection binding, not a runtime projection. Do not generate into an external project, change its instructions, settings or ignore files, or require it to adopt Hive merely because its developer uses Hive. See [external project boundaries](../docs/policies/workspace/organization.md#external-development-repositories).

With a Rulesync version supporting separate input roots, select `agents-configs/` directly as an input root in the consuming agent environment's local Rulesync configuration. A data workspace may track a minimal `.rulesync/` source selection and `rulesync.jsonc` for direct record management. Keep generated projections and machine-specific settings untracked. For example, the following local configuration fragment assumes a reviewed system checkout next to the consumer working directory:

```jsonc
{
  "inputRoots": ["../hive-system/agents-configs"]
}
```

Keep the consumer's own target and feature selection; do not reuse the system development configuration. Rulesync's separate input roots apply to `generate`, not to every Rulesync command. If combining roots, review their order and name collisions, since later roots can override earlier assets. Preview generated writes and deletions before authorized generation; protect tracked workspace instructions and records from overwrite. Alternatively, a data workspace may retain only the selected product definitions as revision-pinned local consumer copies in `.rulesync/`, with a minimal native `rulesync.jsonc` for the relevant targets and features. Review copies against the selected system revision when updating; the system remains their canonical owner. Do not select both the source tree and its copies. Keep workspace-specific rules narrowly scoped to record management; do not commit system development tooling or generated harness files. Disable automatic deletion for this minimal consumer configuration and preserve its tracked `AGENTS.md` when projecting.

Keep the data workspace's `AGENTS.md` as its entrypoint to the reviewed system policy revision; the paths named inside these assets are relative to that system revision, not to the data checkout. The agent must be able to read the actual policy from a verified source, including when it has only local read tools. Do not grant network or additional tool access merely to make an asset work.

Use the existing [asset ownership](../docs/development/agents/assets.md) and [harness verification](../docs/development/agents/harness.md) guidance for review. Review the product sources, supported target formats and compatible policy revision before an explicit consumer installation or release. No Hive-specific install command, packaging mechanism, automatic asset sync or runtime adapter is provided. A data workspace can operate directly through its instructions and Git tools without these optional assets. Until actual projection and representative runtime checks are observed, activation remains unverified.
