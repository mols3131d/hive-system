---
description: "Read when deciding whether a Hive change may be committed directly to main or must go through a PR, or when managing branches."
---

# Workspace Git Change Paths

A Git-backed Hive data workspace has **two write paths**. The public Hive system repository follows its own [development branch policy](../../tools/git/branches.md); live workspace records are not system repository content. Select by the meaning of the changes, not only the file extension. Confirm the intended repository, account, target ref, actual permissions and current state before any write.

| Change | Path |
| --- | --- |
| Ordinary `tasks/` work bundles and their artifacts, independent `artifacts/` records | **Direct commit/push to `main` permitted** |
| Ordinary `bots/` routing metadata that does not change identity, agent behavior or permissions | **Direct commit/push to `main` permitted** |
| [Project directive records](directives.md) creation, changes, supersession and withdrawal | **Topic branch and PR required** |
| [Bot identity (`SOUL.md`) and curated memory](bots.md) creation, changes and consolidation | **Topic branch and PR required** |
| Hive operating guides under `docs/`, scripts, code, tests, workflow files, configuration and templates in `assets/templates/` | **Topic branch and PR required** |
| Root or directory-level *governing* `README.md`, every `AGENTS.md`, runtime instructions, conventions, schemas, tooling or validation changes | **Topic branch and PR required** |

Files such as `docs/INDEX.tsv`, `scripts/`, `tests/`, `.github/`, `mise.toml`, `rulesync.jsonc`, `package.json` and all repository-level tooling/configuration are repository-maintained changes, **not ordinary records**. A Markdown file is not direct-push eligible just because it ends in `.md`. A work or artifact bundle `README.md` that only indexes ordinary work records may be committed directly to `main`; one defining repository or directory-wide instructions requires a PR. Creating or editing an agent's actual definition, privileges, automation or execution contract is a PR change even if placed under `bots/`. When a change mixes both categories, use a PR for the entire change or split independent records from maintained changes.

## Direct-to-main working records

Direct commits are permitted only for ordinary work records within the explicitly allowed areas. This is an authorization to use that publishing route, **not** permission to commit sensitive data, merge PRs, modify someone else's records, bypass an existing review requirement or perform unrelated actions.

- Verify that every path and the actual content are eligible. Follow [artifact ownership](organization.md); save only intentional artifacts, not routine session records.
- Read the current `main` ref and check concurrent work before publishing. Re-resolve the project, current management area and revision before writing or moving a bundle; do not overwrite another writer's artifact; handle a non-fast-forward or conflicting update by re-reading current state and retrying safely, never with a forced push.
- Use small, reviewable commits; confirm that writes reached the intended account/repository/ref. Do not infer message receipt or bot execution from Git commit success.
- Follow [lifecycle](lifecycle.md) and project authority boundaries before removing or substantially rewriting existing records.

## PR-required repository changes

For documentation, scripts, configurations, templates or changes to how Hive operates, create a dedicated topic branch from the intended base, submit a PR and inspect its diff and **actual verification evidence** before integration. For Hive data repositories, keep repository automation manual-only when configured; use relevant local checks or explicitly authorized manual runs, and do not assume an absent CI result is a pass. This restriction does not redefine the separate system repository's development CI. **Do not write these changes directly to `main`**, even if GitHub technically accepts the push.

- Inspect local `git status --short` and current branch before switching; preserve unrelated changes.
- Use a descriptive branch name, such as `docs/git-workflow` or `fix/index-validation`.
- Keep each branch focused; use the [commit guide](../../tools/git/commits.md) for related, reviewable commits.
- Confirm remote and base before pushing; do not infer permission to modify another contributor's branch.
- Do not merge, delete branches containing useful unintegrated work, or rewrite shared history without the required authorization.

This is a **repository operating convention**. GitHub-hosted branch protection or rulesets may impose stricter controls; a text instruction alone does not enforce a server-side path restriction. Do not change hosted protections or use another account to bypass them. For merging and conflict handling see [Integration](../../tools/git/integration.md); GitHub PRs have a separate [template owner](../../../.github/templates/README.md).
