---
root: false
description: "Read when working with Hive directives, work bundles, standalone artifacts or bot records."
globs:
  - "directives/**"
  - "tasks/**"
  - "bots/**"
  - "artifacts/**"
---

Resolve the explicitly authorized Hive data workspace, account and ref, independently of the system repository. Read that workspace's `AGENTS.md` to identify its applicable reviewed system policy revision. Read the actual required policies at that revision; if any required source or read capability is unavailable, report the blocker instead of assuming the policy or selecting a different workspace. Consult `docs/policies/workspace/README.md` in the selected system revision and its applicable owner. Project directives require verified applicability and authority. Tasks own work bundles and their intentional artifacts; root artifacts are standalone. Do not persist routine LLM context or logs. Bot identity and memory have separate rules.

Follow `docs/policies/workspace/git-writes.md` at that same system revision before writing. This rule routes to governing documentation and grants no additional permissions.
