---
description: "Read when configuring opt-in local Git hooks and their compatibility checks."
---

# Lefthook

[Lefthook](https://lefthook.dev/) runs optional local Git hooks configured in [lefthook.yml](../../lefthook.yml). Staged Markdown and supported Biome file types are checked without rewriting or re-staging files. Review the staged diff before committing. Local hooks do not run in CI or for GitHub API commits.

`mise run check` validates the hook configuration; it does **not** install Git hooks. Before using `lefthook install`, inspect existing hooks and `git config --get core.hooksPath`, then explicitly authorize changes to that local setup.

## Version management — loose

Follow the [major-prefix policy](README.md#version-management). Run `lefthook validate` and check staged-file behavior on upgrades.
