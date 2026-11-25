---
description: "Read when choosing record ownership, project and work IDs, reserved scopes, file names or bundles."
---

# Workspace Organization

## Ownership

The record locations below apply to Hive-managed scopes. Resolve the [collaboration backend](#collaboration-backend) before choosing a task, directive or artifact location.

| Surface | Owns |
| --- | --- |
| `directives/<project-id>/` | Project-scoped top-down direction; see [directives](directives.md) |
| `tasks/<project-id>/<area>/<work-id>/` | A tracked work unit, its management lifecycle, progress, entrypoint and optional work-scoped artifacts |
| `artifacts/` | Intentional standalone artifacts not owned by one tracked work unit |
| `bots/` | Registered Manager identity, routing and per-project curated memory |
| `private-configs/` | Canonical personal and environment-specific configurations |
| System `assets/` | Reusable Hive assets, including optional templates; not live records |
| System `docs/` | Canonical shared Hive operating guidance |

An artifact is a deliberately retained deliverable with a purpose and an audience or future use: research, a plan, a review, a report, an evidence summary, a focused handoff or a useful generated file. Do not persist every interaction, LLM context, raw transcript, reasoning trace, routine tool output or session log. Normal execution context stays with its runtime. Permanent project specifications and policies belong to their actual owner, not an artifact store.

Place a work-scoped artifact in `tasks/<project-id>/<area>/<work-id>/artifacts/`; place independent research, analysis or a cross-work deliverable under root `artifacts/`. A report follows the same ownership rule. No separate work ID or task bundle is needed solely to store an independent artifact. Link rather than duplicate. If an independent artifact becomes exclusive to a tracked work unit, move it into that bundle after checking references; shared artifacts remain standalone.

## Collaboration backend

Use Hive by default. Use Notion only under an explicit, applicable user instruction. Apply that instruction directly; do not introduce or use a backend routing system, registry, mapping, automated selection or discovery mechanism. An already available, verified user instruction may be applied within its stated scope. Use only one backend for task state, directives and intentional work or standalone artifacts. If the explicitly designated target is ambiguous, inaccessible or conflicts with existing ownership, stop the affected management operation; do not silently fall back to another backend or account.

Resolve the exact repository/ref or workspace/database and authorized connection before writing. Do not create parallel work queues, duplicate artifacts, automatic synchronization or a second state owner. Changing the backend requires an explicit user decision and a controlled handoff of existing work and references; do not silently migrate or switch during a run. Hive's own operating records, registered Bot identity and curated memory, and eligible private configurations remain separate responsibilities; they are not alternative task or artifact stores for a Notion-managed project.

## Project-owned specifications

Check the actual project's instructions, existing specifications and specification-driven development (SDD) policy before proposing or writing a specification. The project owns its canonical requirements, specifications, architectural decisions and their change and verification procedure, whether stored in a repository or another location designated by the project. Follow that policy and reference the authoritative source and revision from the work record. The chosen collaboration backend does not acquire specification ownership by managing the task.

Only when the absence of a project-level SDD policy has been established and the work needs a specification for execution or verification may the agent retain a temporary specification as a work-scoped artifact in the selected backend. Label it as temporary and non-canonical, scope it to the work, and record its assumptions and acceptance or verification conditions. It must not override existing project requirements, become an independent permanent specification, or be promoted automatically. Follow the project's policy for any authorized transfer and [retention](lifecycle.md) for temporary artifacts. An unavailable or unverified policy is not evidence of its absence.

## Decision and action authority

An agent may act only within the intersection of the user's authorization, applicable project and workspace policies, and actual tool/account permissions. A role, directive, task record or available credential does not itself grant authority. A verified user instruction or authorized delegation may establish authority within its actual scope; another agent's request alone is insufficient. Delegated authority is a subset of the assigning agent's own authority and the delegated scope; a supervisor cannot approve an action beyond its own authority.

Within an authorized goal or task, proceed with reasonable, reversible technical decisions without unnecessary human approval. Investigate and test resolvable uncertainties; explain material choices and evidence. Ask the human for product direction, material preference or goal changes, new authority, or consequential decisions that cannot be resolved within the existing mandate. If a material decision is unresolved, stop the affected action and continue other authorized work when safe.

| Action | Boundary |
| --- | --- |
| Read, analyze, plan, isolated changes and tests | Proceed within authorized scope and project constraints |
| Hive task and artifact writes | Follow Hive ownership, retention and [Git change paths](git-writes.md) |
| External repository writes, notifications and CI | Apply the checks below; a general goal does not automatically authorize them |
| Merge, release, deploy, destructive data changes, shared history or security changes | Require explicit authority covering the action and target, including any separate approval required by project policy |

Reversibility alone is insufficient: consider effects on other people, shared resources, cost, disclosure and recovery. Use already-granted, clearly applicable authority rather than asking for redundant approval. Do not infer wider authority from unattended execution or a technically possible action.

## External development repositories

Treat development repositories, including GitHub, as project-owned collaboration and implementation surfaces, not as an extension of Hive's task or artifact store. Follow each project's actual instructions, permissions and contribution workflow. Keep agent-internal tasks, handoffs, research, plans and temporary specifications in the selected collaboration backend, not in the target repository.

Prefer authorized read-only inspection and isolated local worktrees or equivalent environments for development; preserve existing work and changes by other developers. Before publishing a branch, opening or updating an issue or pull request, posting a comment or review, requesting reviewers or triggering CI, confirm that the action is necessary, covered by the user's actual mandate and permitted by the applicable project policy and tool permissions, including collaboration conventions and impact limits. Avoid unnecessary notifications, duplicate issues or PRs, speculative pushes and CI runs. Do not modify others' branches, shared history, protections or unrelated work without explicit authority. Approval for work in Hive does not authorize writes to an external project, and the [Hive Git write paths](git-writes.md) apply to the authorized Hive data workspace and its maintained files only; system development follows its own repository's policy.

## Project identity

Use the same stable project ID under `tasks/`, `directives/` and each bot's `memory/`. For a new repository-backed scope, normalize its name to lowercase kebab-case (replace runs of other characters with a hyphen and trim edge hyphens); qualify with the normalized owner when names collide. A project may also have a verified canonical reference outside GitHub. A candidate ID is not proof of identity: check matching scope entrypoints in tasks, directives and relevant bot memory before assigning it. If an existing ID maps to a different project or remains ambiguous, resolve the collision explicitly before writing. Do not read unrelated memory contents merely to discover an ID. Reuse an established ID after a repository rename; update its verified project reference instead of silently changing IDs.

### Reserved scopes

| ID | Scope |
| --- | --- |
| `_hive-userdata` | Operation and maintenance of the current Hive data workspace |
| `_general` | Genuinely non-project-specific work, direction or bot learning |

The leading underscore distinguishes reserved scope IDs from ordinary project IDs. These are the only reserved exceptions to kebab-case; `_general` is not a project or a fallback for an unresolved project. Verify ambiguous ownership before writing. Cross-project work with a clear owner stays in that project's scope; use `_general` only when no project owns it. General direction or memory does not automatically apply to every project.

Use `_hive-userdata` for managing the data workspace itself, including its record organization and workspace-specific maintenance. Use `hive-system` as the ordinary repository-backed project ID for development of the Hive product, including shared contracts, CLI and distributable assets. Keep both scopes in the authorized data workspace; do not store live records in the public system repository. A split of the former `_hive` scope requires an explicit, reviewed migration of records by ownership, preserving work IDs, lifecycle, evidence and references. Historical references may remain at their original revisions; do not use the former scope as a current alias or fallback. The project or scope entrypoint identifies its canonical project reference or non-project purpose: `README.md` for tasks and directives, scoped `MEMORY.md` for bot memory. Initialize the reserved scopes as needed and create ordinary project directories only when they have actual records. The surfaces do not have to contain identical sets of project directories. This convention applies to project-scoped workspace records, not the exact repository-name roots and eligibility rules of private configuration or the independent root `artifacts/` store.

## IDs and file naming

- Use a stable lowercase kebab-case work ID, unique across all lifecycle areas within its project or reserved scope. Refer to work by `<project-id>/<work-id>` outside its scope; the management area is not part of the identifier. Do not encode an agent, mutable status or display title in the ID.
- Name documents and optional subdirectories by purpose in lowercase kebab-case, for example `research.md`, `review-api-contract.md` or `benchmark-results/`. Keep native file extensions. Established entrypoint and identity files such as `README.md`, `AGENTS.md`, `SOUL.md` and `MEMORY.md` are exceptions.
- Use an optional two-digit ordering prefix (`00-`, `01-`) only when a real reading or execution order matters. Retain assigned numbers. Use dates or other distinguishing identifiers only when necessary to distinguish recurring or concurrent records; avoid encoding changeable metadata in filenames.
- A standalone artifact may be one file or a bundle with a `README.md` entrypoint. Create subdirectories only for real contents and use shallow structures by default. A work bundle may contain a structured artifact at `artifacts/<slug>/` with its own `README.md` when its files need a shared entrypoint; do not require standard `messages/`, `sessions/` or `outputs/` subdirectories.

## References and metadata

Use relative links inside a bundle. Refer to other moving work bundles by project-qualified work ID and resolve their current management area within that project; use a revision-pinned URL when citing historical evidence. Revalidate cross-bundle and external links when moving a record. Use revisioned source references for canonical project evidence; keep a reference instead of copying large or sensitive data.

Apply [workspace frontmatter](frontmatter.md) to Markdown records. Use [Tasks](tasks.md) for lifecycle and progress, [communication](communication.md) for selective handoff and [lifecycle](lifecycle.md) for retention. Historical filenames and layouts need not be normalized solely for style.
