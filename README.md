# Hive System

A personal collaboration system for an individual developer managing multiple AI agent sessions across multiple projects. Dolt v2 is the adopted MVP development baseline. Existing Markdown/Git workspaces follow their pinned v1 contracts.

> [!WARNING]
> Hive is still under active development and is not ready for real-world use. Do not use it for actual workflows. Dogfooding by Hive developers is the only exception.

Hive supports long-running development work, preserves useful context across sessions, and helps developers and agents exchange reports, briefings and discussion records. The v2 design covers Areas, Issues, Directives, Artifacts, registered Queens and private conversation history. Curated Bot Memory operations are deferred for MVP.

Hive is independent of IDEs, coding-agent runtimes and vendors, model providers, and Git hosting providers. It does not require changes to external projects. It is not a real-time inter-session messaging system or a multi-user collaboration platform.

This repository owns the system and reusable assets. Actual records belong in a separate, user-controlled workspace.

**Status** The Rust CLI provides v1 configuration and context inspection plus read-only committed Dolt validation under contract 2. The v2 read and write workflows and Hive Skill remain in development. See [development setup](docs/development/README.md) to build and test the CLI.

## Developer dogfooding

Use an authorized workspace and a reviewed system policy revision. Follow the [workspace contracts](docs/policies/workspace/README.md) and [getting-started guide](docs/guides/getting-started.md).

## Documentation

- [Goals and scope](docs/goal/README.md)
- [Architecture](docs/architecture/README.md)
- [Record templates](assets/templates/README.md)
- [Agent assets](agents-configs/README.md)
- [Development](docs/development/README.md)

## License

See [LICENSE](LICENSE).
