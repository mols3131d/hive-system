---
description: "Select portable Hive agent assets for an explicitly authorized data workspace, separate from the system development harness."
---

# Distributable Hive Agent Assets

This directory owns Hive product Agent Asset sources intended for distribution to agents operating a Hive data workspace. Keep product Skills, Rules, Commands and Subagents here as they are implemented. The current Rulesync-format sources live under `rulesync/`.

The root `.rulesync/` directory and root `rulesync.jsonc` are for agents developing Hive, not for product distribution. Do not automatically load product sources into the system development harness, include development-only assets in a product distribution, or maintain duplicate canonical definitions across the two scopes. The [workspace policies](../../docs/policies/workspace/README.md) own behavior; product assets route to relevant policy rather than duplicate it.

| Source | Purpose |
| --- | --- |
| [hive-records](rulesync/rules/hive-records.md) | Path-scoped policy routing for workspace records |
| [hive-resume](rulesync/commands/hive-resume.md) | Explicit read-only work resumption |
| [hive-context-reader](rulesync/subagents/hive-context-reader.md) | Bounded read-only work context collection |

The files preserve the existing Rulesync source formats. For an authorized workspace that uses Rulesync, place or reference the reviewed source definitions under the appropriate workspace-local `.rulesync/` directories, after reviewing the target's declared settings and supported features. Keep the data workspace's `AGENTS.md` as its entrypoint to the reviewed system policy revision; the paths named inside these assets are relative to that system revision, not to the data checkout. The agent must be able to read the actual policy from a verified source, including when it has only local read tools. Do not grant network or additional tool access merely to make an asset work.

Use the existing [asset ownership](../../docs/development/agents/assets.md) and [harness verification](../../docs/development/agents/harness.md) guidance for review. Review the product sources, supported target formats and compatible policy revision before an explicit consumer installation or release. No install command, packaging mechanism, automatic asset sync, runtime adapter or new integration is provided. A data workspace can operate directly through its instructions and Git tools without these optional assets. Until actual projection and representative runtime checks are observed, activation remains unverified.
