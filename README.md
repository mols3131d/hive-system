# Hive System

A persistent collaboration system for AI agents working across projects.

Hive lets agents coordinate work, share intentional artifacts, resume tasks across sessions, and retain scoped knowledge without depending on a particular agent runtime. Humans and registered Manager Bots guide work; task-scoped agents carry it out and return evidence for review.

Workspaces use Markdown and Git as their canonical storage. Hive keeps agent collaboration separate from the external projects that own their code, specifications, and policies.

**Status:** The workspace contracts, record templates, and manual workflows are documented. The standalone Rust CLI and Hive Skill are planned, not yet implemented.

## Collaboration model

| Component | Purpose |
| --- | --- |
| Directives | Goals, priorities, and constraints provided by humans or authorized Managers |
| Tasks | Project-scoped work bundles with goals, acceptance criteria, progress, and outcomes |
| Artifacts | Research, plans, reviews, reports, and handoffs worth retaining |
| Bots | Persistent Manager identity and routing information |
| Memory | Curated, project-scoped knowledge for registered Bots |

The [workspace contracts](docs/policies/workspace/README.md) define record ownership, agent responsibilities, authority, lifecycle, and retention.

## Architecture

Hive separates reusable software from user-controlled data:

- **System:** This public repository owns the planned Rust CLI, shared policies, record templates, and reusable agent assets.
- **Workspace:** A separate user-controlled Git repository owns actual tasks, artifacts, directives, Bot identities, memory, and private configuration.

The planned CLI will provide scoped queries, record validation, and safe updates. A Hive Skill will guide agents in applying the shared policies and using the CLI. Direct authorized Git and file access remains supported.

Hive is a collaboration and persistence system, not a task execution platform. It does not manage agent runtimes, schedule execution, or replace external project management. See the [system design](docs/architecture/system-design.md) for the proposed interfaces and implementation boundaries.

## Getting started

Until the CLI is available, use Hive through its document-first workflow:

1. Select an authorized data workspace and an applicable system policy revision.
2. Read the relevant [workspace contracts](docs/policies/workspace/README.md).
3. Follow the [work bundle guide](docs/guides/getting-started.md) to create, resume, or hand off work using Git and Markdown. Use the [record templates](assets/templates/README.md) as needed.

Keep actual workspace records out of this public repository.

## Documentation

- [Goals and scope](docs/goal/README.md)
- [System design](docs/architecture/system-design.md)
- [Workspace agent assets](assets/agent/README.md)
- [Contributing and development](docs/development/README.md), including [development environment setup](docs/tools/initial-setup.md)

## License

See [LICENSE](LICENSE).
