---
description: "How Rulesync is used in this repository."
---

# Rulesync

Hive uses [Rulesync](https://rulesync.dyoshikawa.com/) for its development agents. Root [`rulesync.jsonc`](../../rulesync.jsonc) declares external sources and targets; `.rulesync/` holds development asset definitions and installed sources. Setup and synchronization run through [mise tasks](mise.md) and generate runtime-specific projections.

Product assets under [`assets/agent/`](../../assets/agent/README.md) are separate distribution sources. The repository's development setup does not install or project them. Do not select development-only assets for product distribution.

[Agent Asset Lifecycle](../development/agents/assets.md) owns the two scopes and development asset maintenance.
