# Hive System

Hive is a document-first collaboration system for humans and AI agents. It keeps work, intentional artifacts, directives, and registered Manager Bot identity and curated memory in a separate user-controlled Markdown and Git workspace. This public repository owns reusable system software, shared workspace contracts and templates; it must not contain real workspace records.

The planned application is a standalone Rust CLI with a Hive-owned Skill. The CLI is not implemented yet. See [Goals](docs/goal/README.md), [system design](docs/architecture/system-design.md), [workspace contracts](docs/policies/workspace/README.md), and [templates](assets/templates/README.md). For manual workspace use, read [Start a Work Bundle](docs/guides/getting-started.md).

## Develop the system

Review [Initial Tool Setup](docs/tools/initial-setup.md) and authorize required downloads, generated files and local Git changes before installing the inherited development tooling. Install [mise](https://mise.jdx.dev/) if needed, then run from this repository:

```sh
mise install
mise run setup
mise run check
```

`setup` resolves the first upstream asset lock (or reuses an existing one), generates enabled runtime projections and ignore entries, and configures the clone-local [Git commit template](docs/tools/git/README.md). Inspect and commit the resulting `rulesync.lock` only after reviewing its resolved sources. Follow [Development](docs/development/README.md) and the retained [Project Setup](docs/guides/project-setup.md) guide for toolchain verification and GitHub settings. Rust application setup and build commands will be documented when implemented.

## Development foundation

| Area | Provisioning / entry |
| --- | --- |
| Upstream Agent Skills | [rulesync.jsonc](rulesync.jsonc) selects Skills from [mols-agent-assets](https://github.com/mols3131d/mols-agent-assets) and [Rulesync](https://github.com/dyoshikawa/rulesync) |
| Agent asset projections | Claude Code, Codex CLI, GitHub Copilot, Copilot CLI, Antigravity IDE/CLI and AgentsSkills; [configured features](rulesync.jsonc) and [asset ownership](docs/development/agents/assets.md) vary per target |
| General tooling | [mise.toml](mise.toml) selects rumdl, Biome, Lefthook, Rulesync and its Node runtime |
| Documentation | [Documentation map](docs/README.md) routes goals, architecture, development and tools; [Tools](docs/tools/README.md) owns shared tool guidance |
| Project scripts | [scripts/](scripts/README.md) is reserved for project-owned executable helpers when native tool tasks are insufficient |
| Working records | [Inbox](.inbox/README.md) is inherited development scaffolding; never store private or live workspace records in this public repository |

## Everyday commands

| Action | Command |
| --- | --- |
| Format Markdown and supported code/config files | `mise run format` (writes across repository) |
| Check Markdown, Biome and Lefthook configuration | `mise run check` |
| Check Rulesync configuration | `mise run agents-check` |
| Preview Rulesync projections (after setup) | `mise run agents-preview` |
| Restore accepted upstream revisions (requires lockfile) | `mise run agents-restore` |
| Update upstream revisions deliberately | `mise run agents-refresh` |
| Preview/apply removal of retired runtime outputs | `mise run agents-retire -- <targets> --features <features> [--apply]` |

Git hook installation is **optional**, not part of the default setup. Follow [Tools](docs/tools/README.md) for scoped formatting, hook safety and version updates. `mise run check` does not run an application test suite, and an asset selection does not prove runtime activation.

The included Node.js documentation index is a shared repository utility, **not** a required application language. Rust application tooling, tests and CI will be added separately. Existing CI workflows remain development-tool checks, not proof of Hive runtime behavior.
