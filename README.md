# Hive System

A personal collaboration system for an individual developer managing multiple AI agent sessions across multiple projects, built on the filesystem, Markdown and Git.

> [!WARNING]
> Hive is still under active development and is not ready for real-world use. Do not use it for actual workflows. Dogfooding by Hive developers is the only exception.

Hive supports long-running development work, preserves useful context across sessions, and helps developers and agents exchange reports, briefings and discussion records. It manages directives, tasks, artifacts, Bot identities and curated memory.

It is independent of agent runtimes and Git hosting providers. It is not a real-time inter-session messaging system or a multi-user collaboration platform.

This repository owns the system and reusable assets. Actual records belong in a separate, user-controlled workspace.

**Status:** Workspace contracts and manual workflows are documented. The Rust CLI and Hive Skill are planned, not implemented.

## Developer dogfooding

Use an authorized workspace and a reviewed system policy revision. Follow the [workspace contracts](docs/policies/workspace/README.md) and [getting-started guide](docs/guides/getting-started.md).

## Documentation

- [Goals and scope](docs/goal/README.md)
- [System design](docs/architecture/system-design.md)
- [Record templates](assets/templates/README.md)
- [Agent assets](agents-configs/README.md)
- [Development](docs/development/README.md)

## License

See [LICENSE](LICENSE).
