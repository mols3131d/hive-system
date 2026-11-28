---
description: "Select portable Hive agent assets for an explicitly authorized data workspace, separate from the system development harness."
---

# Distributable Hive Agent Assets

This directory is the canonical [Rulesync source tree](https://rulesync.dyoshikawa.com/guide/separate-input-root) for Hive product Agent Assets. It contains the Dolt v2 workflow Skill and the legacy Markdown/Git v1 sources. Use Rulesync's native [file formats](https://rulesync.dyoshikawa.com/reference/file-formats). The directory itself is the input root. Skills live in `skills/<name>/SKILL.md` and rules, commands and subagents are under their respective feature directories.

The root `.rulesync/` directory and root `rulesync.jsonc` belong to the development harness. Select product sources from this directory. Follow the [v2 workspace contract](../docs/architecture/dolt-workspace-contract.md) or the [pinned v1 policies](../docs/policies/workspace/README.md) for workspace behavior.

| Source | Contract | Purpose |
| --- | --- | --- |
| [hive-workflow](skills/hive-workflow/SKILL.md) | v2 | Scoped read-only resumption and explicitly authorized Dolt workflows |
| [hive-records](rules/hive-records.md) | v1 | Path-scoped policy routing for workspace records |
| [hive-resume](commands/hive-resume.md) | v1 | Explicit read-only work resumption |
| [hive-context-reader](subagents/hive-context-reader.md) | v1 | Bounded read-only work context collection |

## Install the v2 Skill

Pair the Skill with a reviewed [contract 2 CLI installation](../docs/guides/getting-started.md#install-the-v2-cli) from the same system revision. Keep a locally readable checkout at the workspace descriptor's policy pin for `context resolve`. In an explicitly authorized consumer directory, outside the external project and outside the data record tree if host discovery would inject management files, select only `skills`. For example, with Rulesync 43.12.0 and a consumer-local config:

```jsonc
{
  "inputRoots": ["/absolute/path/to/reviewed/hive-system/agents-configs"],
  "targets": ["codexcli"],
  "features": ["skills"],
  "outputRoots": ["."],
  "delete": false
}
```

Save this as `rulesync.jsonc` in the chosen consumer directory or apply the selection to its reviewed configuration. Choose only the intended target. Rulesync also provides `antigravity-ide` and `antigravity-cli` Skill projections. Run `rulesync generate --config rulesync.jsonc --dry-run` there, review the proposed paths and any replacement of existing Skills, then explicitly run `rulesync generate --config rulesync.jsonc` for the approved output. Keep projections local and untracked. Check discovery and management-file exclusion in the intended host before using the Skill for execution. Projection alone establishes generated content. [Harness verification](../docs/development/agents/harness.md)

The Skill defaults to read-only inspection and context resumption. The host and operator control tool permissions, process isolation and external accounts. To enforce the Queen-only external publication boundary, give workers only the external read permissions they need and verify that the host isolates them from write credentials. Use the [manual v2 workflow](../docs/guides/getting-started.md#inspect-or-resume-v2-work) if the host cannot load the Skill or isolate the required context.

## Use pinned v1 assets in a data workspace

Hive's core is independent of IDE, coding-agent vendor and model provider. Rulesync and runtime-specific projections are optional consumer conveniences, not product runtime requirements. Install or project Hive assets only into an explicitly authorized Hive workspace or appropriate user-local environment. An optional, explicitly authorized ignored project `.local/hive/config.yml` is only a connection binding, not a runtime projection. Do not generate into an external project, change its instructions, settings or ignore files, or require it to adopt Hive merely because its developer uses Hive. See [external project boundaries](../docs/policies/workspace/organization.md#external-development-repositories).

With a Rulesync version supporting separate input roots, select `agents-configs/` directly as an input root in the consuming agent environment's local Rulesync configuration. A data workspace may track a minimal `.rulesync/` source selection and `rulesync.jsonc` for direct record management. Keep generated projections and machine-specific settings untracked. For example, the following local configuration fragment assumes a reviewed system checkout next to the consumer working directory:

```jsonc
{
  "inputRoots": ["../hive-system/agents-configs"]
}
```

Keep the consumer's pinned system revision and target and feature selection. When using this mixed source tree for v1 projection, select only the compatible `rules`, `commands` or `subagents` features supported by the chosen targets. Rulesync's separate input roots apply to `generate`. If combining roots, review their order and name collisions, since later roots can override earlier assets. Preview generated writes and deletions before authorized generation; protect tracked workspace instructions and records from overwrite. Alternatively, a data workspace may retain only the selected product definitions as revision-pinned local consumer copies in `.rulesync/`, with a minimal native `rulesync.jsonc` for the relevant targets and features. Review copies against the selected system revision when updating; the system remains their canonical owner. Do not select both the source tree and its copies. Keep workspace-specific rules narrowly scoped to record management; do not commit system development tooling or generated harness files. Disable automatic deletion for this minimal consumer configuration and preserve its tracked `AGENTS.md` when projecting.

Select `commands` to project `hive-resume` even for targets that render the command as a Skill. Select `subagents` for `hive-context-reader`.

Keep the data workspace's `AGENTS.md` as its entrypoint to the reviewed system policy revision; the paths named inside these assets are relative to that system revision, not to the data checkout. The agent must be able to read the actual policy from a verified source, including when it has only local read tools. Do not grant network or additional tool access merely to make an asset work.

Use the existing [asset ownership](../docs/development/agents/assets.md) and [harness verification](../docs/development/agents/harness.md) guidance for review. Review the product sources, supported target formats and compatible policy revision before an explicit consumer installation or release. No Hive-specific install command, packaging mechanism, automatic asset sync or runtime adapter is provided. A data workspace can operate directly through its instructions and Git tools without these optional assets. Until actual projection and representative runtime checks are observed, activation remains unverified.
