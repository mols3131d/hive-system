---
description: "Read when selecting, updating or retiring Agent Assets and validating their source, projection and runtime availability."
---

# Agent Asset Lifecycle

Hive has two separate Agent Asset scopes. Choose the owner by intended consumer, not by file format.

## Ownership

| Scope | Source location | Consumer |
| --- | --- | --- |
| Development | `.rulesync/` and root [`rulesync.jsonc`](../../../rulesync.jsonc) | Agents assisting development of this repository |
| Distribution | [`assets/agent/`](../../../assets/agent/README.md) | Agents using Hive in a separate data workspace |

The development scope includes repository-local definitions and selected upstream assets installed by Rulesync. Upstream sources retain their ownership. The distribution scope owns Hive product assets, including the future Hive Skill. Maintain each asset in its intended scope; do not copy product definitions into the development `.rulesync/` directory or package development assets as Hive product assets.

[Rulesync](../../tools/rulesync.md) owns development installation and projection. The [distribution source guide](../../../assets/agent/README.md) owns consumer usage and compatibility. [Harness verification](harness.md) owns runtime evidence.

## Select and change development assets

1. Identify the needed behavior and runtime targets. Prefer a verified reusable asset over a local duplicate.
2. Check the source's provenance, scope, compatibility and trust before changing the selection. [`rulesync.jsonc`](../../../rulesync.jsonc) owns external Skill sources and targets; the generated `rulesync.lock` records resolved revisions after an authorized install.
3. Add, refresh or remove the selection in its canonical declaration. Only with appropriate authorization, regenerate projections using [Rulesync tooling](../../tools/rulesync.md); inspect the lockfile, generated output and managed ignore changes. Do not hand-edit vendor projections or silently refresh unrelated upstream sources.
4. Review downstream references and supported runtimes before retiring an asset. If a derived project takes ownership of a local asset, establish a single maintained source rather than preserving competing upstream and local copies.

## Verify what changed

Distinguish a **declared selection** from a **generated projection**, a **runtime-reachable asset**, and **observed behavior**. A matching manifest, lockfile or generated directory cannot establish that the target runtime loaded and followed the asset. Use [Harness verification](harness.md) for representative checks and report unavailable runtimes as unverified.
