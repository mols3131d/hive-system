---
description: "Use when implementing or verifying Dolt v2 workspace layout, descriptor, XDG configuration, local bindings, runtime state, or private Queen conversation storage."
---

# Dolt v2 Workspace Layout and Local File Contract

This document specifies the Dolt v2 local filesystem layout, workspace descriptor, XDG configuration, local Area/resource bindings, canonical asset root, runtime state, and private Queen conversation storage for `#22`, `#18`, and `#20` against approved baseline `c66b3c50150e18426485d134de92c6b311dac125`. Existing workspaces continue under their [pinned v1 contracts](README.md#v1-compatibility). Migration mapping, live migration, and an authority switch are out of scope and require separate explicit approval.

The Rust CLI supports [descriptor inspection](../development/README.md#dolt-workspace-descriptor-inspection), [inactive workspace initialization](../development/README.md#dolt-workspace-initialization), and [offline workspace status](#workspace-status).

Read this with the [Dolt v2 architecture](dolt-redesign.md), [MVP contract](dolt-mvp-contract.md), and [relational schema](dolt-schema.md).

## Contract selection and XDG layout

Select the CLI contract (`hive --contract 2`) **before** reading configuration or resolving a workspace. Never fall back silently between v1 (`hive/`) and v2 (`hive/v2/`).

V2 reuses the verified Unix XDG and environment rules from the v1 configuration loader:

- Honor `HOME`, `XDG_CONFIG_HOME`, `XDG_DATA_HOME`, `XDG_STATE_HOME`, and `XDG_RUNTIME_DIR` **only when absolute**.
- Treat unset, empty, or relative values for `XDG_CONFIG_HOME`, `XDG_DATA_HOME`, and `XDG_STATE_HOME` as absent and fall back to `$HOME/.config`, `$HOME/.local/share`, and `$HOME/.local/state`.
- Never guess `HOME`. If a fallback or `~/` expansion requires `HOME` and `HOME` is unset, empty, or relative, fail closed with `INVALID_CONFIG`.
- Never fabricate a fallback directory (such as `/tmp`) for `XDG_RUNTIME_DIR`. If `XDG_RUNTIME_DIR` is unset, empty, or relative, omit the Unix socket path and use an authenticated loopback endpoint (`127.0.0.1`).
- Keep `XDG_CACHE_HOME/hive/v2/` reserved for future disposable caches; do not create or require it in MVP.

| Surface | Path | Default fallback | Permissions | Shared / tracked |
| --- | --- | --- | --- | --- |
| User configuration | `$XDG_CONFIG_HOME/hive/v2/config.yml` | `$HOME/.config/hive/v2/config.yml` | `0700` dir / `0600` file | Never |
| User-local bindings | `$XDG_CONFIG_HOME/hive/v2/bindings.yml` | `$HOME/.config/hive/v2/bindings.yml` | `0700` dir / `0600` file | Never |
| Default workspace candidate | `$XDG_DATA_HOME/hive/v2/userdata/` | `$HOME/.local/share/hive/v2/userdata/` | `0700` dir | Workspace-owned |
| Local state and locks | `$XDG_STATE_HOME/hive/v2/` | `$HOME/.local/state/hive/v2/` | `0700` dir / `0600` file | Never |
| Local runtime socket | `$XDG_RUNTIME_DIR/hive/v2/` | None (use loopback) | `0700` dir / `0600` sock | Never |

### Canonical workspace directory layout

A v2 workspace root (`<workspace-root>/`) separates the nonsecret descriptor, authoritative Dolt store, Git-tracked Soul asset repository, and private local Queen conversations:

```text
<workspace-root>/
├── hive-workspace.yml          # Versioned nonsecret v2 workspace descriptor
├── db/                         # Authoritative local Dolt database (never tracked in ordinary Git)
├── assets/                     # Canonical Git repository for Queen Soul files
│   └── bots/
│       └── <slug>/
│           ├── SOUL.md         # Required committed Queen identity
│           ├── README.md       # Optional management-only entrypoint (never runtime context)
│           └── AGENTS.md       # Optional management-only instructions (never runtime context)
└── conversations/              # Private local Queen conversation Markdown (0700/0600, outside Git/Dolt)
    └── bots/
        └── <bot_id>/
            └── <conversation_id>.md
```

- `db/` and `conversations/` must never be tracked in ordinary Git worktrees or placed inside `assets/`.
- Credentials, local endpoints, PIDs, runtime locks, runtime recovery markers, and machine-local bindings must never be stored inside `<workspace-root>/` or any shared/tracked file.

## Shared YAML and filesystem validation

V2 configuration, binding, and descriptor files reuse the verified v1 strict parsing and safe-open guarantees:

- **File format:** UTF-8 YAML (`.yml`), maximum size `64 KiB`, single YAML document.
- **Parser bounds:** `max_depth: 8`, `max_nodes: 128`, `max_anchors: 0`, `max_aliases: 0`, duplicate keys rejected, merge keys (`<<`) rejected, strict booleans and indentation, no custom or unsupported tags, no tagged field names, and `deny_unknown_fields`. Present optional fields must have their declared type rather than `null`.
- **Redacted diagnostics:** Error messages identify the file role and failure category without echoing raw input values, unknown field names, or secrets.
- **Safe opens:** Open user-level files with `O_NOFOLLOW | O_NONBLOCK` and verify a regular file. Open workspace-scoped and resource-scoped files component-by-component with descriptor-relative `openat(..., O_NOFOLLOW | O_CLOEXEC)` so symlink replacement cannot escape the root.
- **Path values:** Reject empty strings, URI schemes (`://`), and control characters. Expand `~/` only against verified absolute `HOME`. Resolve relative paths against the containing YAML file's parent directory (or invocation `cwd` for CLI `--workspace`).

## Workspace descriptor (`hive-workspace.yml`)

`<workspace-root>/hive-workspace.yml` is the required nonsecret marker for a v2 workspace. Using a distinct filename prevents confusion with v1's `hive.yml`.

```yaml
version: 2
workspace_id: 0195f1b2-3c4d-7e8f-9a0b-1c2d3e4f5a6b
schema_version: 1
policy_revision: c66b3c50150e18426485d134de92c6b311dac125
```

| Field | Type | Requirement |
| --- | --- | --- |
| `version` | integer | Required; must equal `2`. Any other integer fails with `SCHEMA_INCOMPATIBLE`; non-integer fails with `INVALID_CONFIG`. |
| `workspace_id` | string | Required immutable canonical lowercase UUID. Match the exact committed `workspace_meta.workspace_id` in `db/`. |
| `schema_version` | integer | Required; must equal `1` for the pinned initial migration and match committed `schema_migrations.version` in `db/`. |
| `policy_revision` | string | Required; 40-character lowercase hexadecimal `hive-system` commit SHA pinning the reviewed v2 policy revision. |
| `bootstrap_layout` | string | Optional. New installations write `dolt-native-v1`. A missing field preserves descriptor-only inspection behavior. Other values return `SCHEMA_INCOMPATIBLE`. |

- Reject `hive-workspace.yml` if it contains credentials, machine-local paths, endpoints, bindings, or unknown fields.
- Workspace initialization creates a private, nonsecret `.hive-init-publication-incomplete` sentinel before publishing workspace entries. V2 readers refuse any entry with this name in the root, including a symlink. Init clears the sentinel before final verification and installs the descriptor only after the checks and payload syncs pass. The descriptor move is the readiness event. For a descriptor with `bootstrap_layout`, readers take a nonblocking shared lock on the same directory, re-read the descriptor, and verify the layout. An active initializer at this point returns `CONCURRENT_CHANGE`.
- The native layout check requires real directories and nonempty regular files at `db/.dolt/repo_state.json`, `db/.dolt/noms/manifest`, and `assets/.git/HEAD`, plus real Git `objects/` and `refs/` directories. A missing or unsafe layout returns `INVALID_CONFIG`. The check confirms the storage layout. Committed database identity and committed asset contents require separate verification.
- `workspace init --target EMPTY_DIR --register-runtime` opts the fresh workspace UUID into machine-local runtime registration. Init verifies the moved database UUID and exact initial commit, then installs the bounded `active-root.json` under the stable workspace-ID lock before publishing the descriptor. Init without the flag leaves local runtime state untouched. An existing UUID state directory is refused, including an incomplete one. If init is interrupted after reserving the UUID but before publishing the descriptor, the local state is preserved and the root remains unselectable.
- A v2 workspace has `hive-workspace.yml` and no v1 `hive.yml`. V2 selection returns `SCHEMA_INCOMPATIBLE` for a v1-only root or a root with both markers. A standalone v2-only root lacks the reviewed v1 workspace policy. V1 keeps its existing Git and policy checks, including its legacy rules for `hive.yml`.
- A mismatch between `hive-workspace.yml` `workspace_id` and `workspace_meta.workspace_id` in `db/` returns `AUTHORITY_UNVERIFIED` and leaves all targets untouched.

## User configuration and local Area/resource bindings

### User configuration (`config.yml`)

Location: `$XDG_CONFIG_HOME/hive/v2/config.yml` (default `~/.config/hive/v2/config.yml`). Absent file is valid.

```yaml
version: 2
workspace: ~/work/hive-v2-userdata
output:
  format: json
```

| Field | Type | Requirement |
| --- | --- | --- |
| `version` | integer | Required; must equal `2`. |
| `workspace` | string | Optional local path to the default v2 workspace root. |
| `output.format` | string | Optional; `text` or `json`. |

### Local Area and resource bindings

V2 reads local Area and Resource bindings from the optional machine-local `$XDG_CONFIG_HOME/hive/v2/bindings.yml`. A binding maps a canonical resource directory to a registered Resource and optional scope. This also supports non-Git directories and keeps the binding file outside external projects. V1 continues to use its pinned project binding rules.

Example `$XDG_CONFIG_HOME/hive/v2/bindings.yml`:

```yaml
version: 2
bindings:
  - path: /example/non-git-docs
    workspace: /example/hive-v2-userdata
    workspace_id: 0195f1b2-3c4d-7e8f-9a0b-1c2d3e4f5a6b
    resource_id: 0195f1b2-4000-7000-8000-000000000010
    area_id: 0195f1b2-4000-7000-8000-000000000001
    project_id: 0195f1b2-4000-7000-8000-000000000020
```

| Field | Type | Requirement |
| --- | --- | --- |
| `version` | integer | Required; must equal `2`. |
| `path` | string | Required canonical local directory path. Reject duplicate paths after normalization with `INVALID_CONFIG`. |
| `workspace` | string | Optional local path to the v2 workspace root; if omitted, resolves through `config.yml` `workspace` or fails if none applies. |
| `workspace_id` | string | Optional canonical lowercase workspace UUID. When supplied, match the resolved workspace ID exactly. |
| `resource_id` | string | Required lowercase UUIDv7 matching a registered row in `resources`. |
| `area_id` | string | Optional lowercase UUIDv7 matching an Area linked to `resource_id` in `area_resources`. |
| `project_id` | string | Optional lowercase UUIDv7 matching an existing row in `projects`. |

- Resolve a supplied resource root by exact canonical path. Otherwise choose the longest registered directory path containing the canonical invocation directory. Compare path components and refuse an ambiguous match. Obtain bindings only from the machine-local file.
- A Resource may be linked to multiple Areas in `area_resources`. If neither CLI `--area` nor the binding specifies `area_id` and `resource_id` maps to more than one Area, return `AMBIGUOUS_SCOPE` with the bounded candidate Area IDs and slugs — **never silently choose**.
- Local bindings are convenience data, never an authority grant or harness installation.

### Lookup precedence, refusal rules, and provenance

Resolve workspace and scope in strict order:

1. Explicit CLI flags (`--workspace`, `--area`, `--project`, `--resource`).
2. Applicable verified local binding in `bindings.yml`.
3. User configuration default (`$XDG_CONFIG_HOME/hive/v2/config.yml`).
4. Validated XDG default candidate (`$XDG_DATA_HOME/hive/v2/userdata/`).

Refusal rules (fail closed):

- **No fallback on explicit error:** An invalid or incompatible explicit CLI target fails immediately without consulting bindings, user defaults, or the XDG candidate.
- **Verify the binding when used:** Reject an invalid selected binding or an incompatible bound workspace. Report the selected input source and a typed failure.
- **Explicit workspace and scope:** A valid `--workspace` selects that workspace even when the invocation directory has a binding elsewhere. Ignore scope IDs from bindings for a different workspace. Explicit Area and Project IDs take precedence over matching-workspace defaults and must validate in the selected workspace. If the command still requires a scope and has no applicable selection, return a typed scope error.
- **XDG candidate is read-only discovery:** Presence of `$XDG_DATA_HOME/hive/v2/userdata/` requires a valid `hive-workspace.yml` and compatible `db/`; it never authorizes initialization, migration, or policy adoption.
- **Hook invocation boundary:** Optional `hook dispatch` requires an explicit or prebound target and must not fall back to the user default or XDG candidate.

Every config, context, and doctor report records explicit source provenance and omits unavailable fields rather than inventing values:

| Provenance field | Allowed values |
| --- | --- |
| `sources.workspace` | `cli`, `binding`, `user`, `xdg-default` |
| `sources.area` | `cli`, `binding`, `issue` |
| `sources.project` | `cli`, `binding` |
| `sources.output_format` | `cli`, `user`, `default` |

Reports also include `workspace_id`, canonical `workspace_root`, `policy_revision`, and the nonsecret source file paths that contributed to resolution (`user_config`, `binding`, `descriptor`).

## Canonical asset root and reserved runtime/state layout

### Canonical asset root (`<workspace-root>/assets/`)

`<workspace-root>/assets/` is a dedicated native Git repository owned by the user and separate from `db/` and `conversations/`.

- **Required Queen Soul:** `assets/bots/<slug>/SOUL.md`, where `<slug>` is a workspace-wide unique immutable lowercase kebab-case identifier matching `bots.slug` in Dolt. No role subdirectory and no reuse of retired slugs.
- **Committed snapshot only:** Activation and `context resolve` read `SOUL.md` from an explicit clean committed Git tree (`asset_commit`). Dirty, staged-only, or untracked files are never activation inputs.
- **Management-file exclusion:** Optional `assets/bots/<slug>/README.md` and `assets/bots/<slug>/AGENTS.md` are management-only files. They are never required for registration or validation, malformed optional files must not enter identity context, and `context resolve` strictly excludes them. Never use `assets/bots/<slug>/` as a host working directory when the host auto-discovers `AGENTS.md`.
- **Content and path guards:** Require valid UTF-8, strip management frontmatter before injecting the Soul body into runtime context, reject path traversal and symlinks/reparse points escaping `assets/`, and require any identity fields explicitly present in `SOUL.md` to match the `bots` registry row.
- **Deferred curated memory:** Logical `memories/LATEST.md`, `memories/core/`, and `memories/references/` views over `bot_memories` are proposed deferred for MVP and have no required physical files in `assets/`.

### Reserved state and runtime layout

All machine-local operational state lives under `$XDG_STATE_HOME/hive/v2/` and `$XDG_RUNTIME_DIR/hive/v2/`, keyed by stable lowercase `workspace_id` so two workspaces cannot collide and a copied workspace at another path cannot silently acquire active writer authority.

```text
$XDG_STATE_HOME/hive/v2/
├── workspaces/
│   └── <workspace_id>/
│       ├── lock                  # OS-held exclusive advisory lock (stable inode; never unlink/steal)
│       ├── active-root.json      # Canonical registered active workspace_root and DB identity
│       ├── pending-request.json  # Durable pending-commit marker (no payload body or credentials)
│       ├── stopped-request.json  # Verified exit evidence bound to a pending marker
│       └── server.json           # Owned Dolt SQL Server PID, start identity, nonce, endpoint ref
└── diagnostics/                  # Optional redacted local diagnostics only

$XDG_RUNTIME_DIR/hive/v2/         # Used only when XDG_RUNTIME_DIR is absolute and private
└── workspaces/
    └── <workspace_id>/
        └── dolt.sock             # Mode 0600 private Unix socket (when supported by pinned Dolt)
```

- **Active-root verification:** Under the `workspace_id`-keyed lock, writers verify that the target canonical `workspace_root` matches `active-root.json` and `db/` `workspace_meta.workspace_id`. A restored copy at a different path remains inactive until explicitly promoted after verifying the old authority stopped and preserving final history.
- **Pending-request marker:** Records the format version, exact workspace, request, base commit, bounded launch and session identity, payload fingerprint and receipt digest. Version 1 denotes an Issue note and records `area_id` and `issue_id`. Version 2 also records a kind for Issue create, update, dependency add, or dependency remove. Version 3 uses `artifact_id` for Artifact create and update and includes `area_id` only for an Area-linked Artifact. Version 4 records `area_id` and the optional `parent_id` for Area creation, or `project_id` and the member Issue's `area_id` when applicable for Project planning. Reconcile before any replay or new write.
- Keep runtime state and local config outside `db/`, `assets/` and backup manifests. Obtain credentials through the explicitly selected machine-local runtime mechanism. Preserve the existing environment-based reader credentials for validation.

#### Workspace status

`hive --contract 2 --json workspace status [--workspace PATH]` uses the descriptor selector with explicit workspace, applicable local binding, user configuration, and XDG candidate precedence. It opens the versioned state directory and selected UUID directory without following symlinks. Missing state is valid. Status reports the descriptor UUID, canonical root, selection source, policy pin, and storage schema version. It makes no network connection by default.

Status and the lifecycle commands consume two strict UTF-8 JSON records, each bounded to 64 KiB, with integer `version: 2` and no extra or duplicate fields.

| Record | Required fields in addition to `version` |
| --- | --- |
| `active-root.json` | `workspace_id` (canonical lowercase UUID used for both descriptor and committed DB identity), `workspace_root` (exact canonical absolute path string) |
| `server.json` | The same `workspace_id` and `workspace_root`, positive integer `pid`, Linux boot UUID `boot_id`, positive integer `start_ticks` (field 22 of `/proc/<pid>/stat`), per-launch canonical lowercase UUID `nonce`, and `endpoint` with an integer TCP `port` (1–65535) and explicit Dolt `database` name |

The server record requires a matching registration. Its endpoint denotes TCP on `127.0.0.1`. Registration matching establishes agreement with local configuration. On Linux, `pid_start_verified` establishes matching boot, PID and process start ticks for a live process in the caller's procfs view. Without readable process evidence the process remains `recorded`. A different root or UUID, stale boot or process start, or a dead or missing recorded PID fails closed. The nonce is a launch record identifier.

Supply all of `--workspace-id UUID --database NAME --port PORT` to inspect committed identity using the existing SELECT-only reader and `HIVE_DOLT_USER` and `HIVE_DOLT_PASSWORD`. The expected UUID must match the descriptor, and the target must match the recorded endpoint when one exists. A successful read verifies the selected database, Dolt version, committed main snapshot, supported schema, and workspace UUID. With a server record it upgrades the endpoint observation to `committed_identity_verified`. This verifies the SQL identity returned through the endpoint. Binding that endpoint to the recorded process and on-disk root requires further evidence. Status reports endpoint ownership as `unverified` and operational state as `unknown`.

A safely readable `pending-request.json` is enough to report `recovery_required`, even if its contents are incomplete. Status does not interpret its payload. Status observes files without taking the writer lock, so lifecycle commands must reverify under that lock. These observations grant no mutation authority and provide no evidence about worker execution, claims or stop.

#### Workspace start, stop and recovery

```sh
hive --contract 2 --json --workspace PATH workspace start --dolt-bin PATH --port PORT
hive --contract 2 --json --workspace PATH workspace stop
hive --contract 2 --json --workspace PATH workspace recover --dolt-bin PATH
```

Use an existing bootstrapped workspace and an already registered canonical root. The Linux lifecycle requires a workspace state directory owned by the caller with mode `0700`, Dolt 2.7.6, procfs, pidfds, and a local filesystem exposing the Dolt storage lock. All three commands acquire the stable workspace-ID lock. Start requires a free loopback port from 1024 through 65535. A pending request blocks start and is preserved by stop.

Start provisions launch-specific credentials in a private directory beneath the workspace state directory. The reader receives database SELECT. The initial writer receives database SELECT and UPDATE on `workspace_counters` for the scoped transaction verification. The running response identifies the reader account and private password file for use with the existing reader environment variables.

Start writes and syncs `run-<nonce>/server-config.yaml` (mode `0600`) beneath the private workspace state directory. Dolt receives the same bytes through an inherited sealed Linux file descriptor with `--config /proc/self/fd/<fd>` and `--skip-root-user-initialization`. The config contains the loopback listener and local paths for the selected database and private Dolt state. Credentials stay in the existing private files. Hive leaves any prior `db/config.yaml` in place.

Start and stop verify the boot and process start identity, the launched executable and arguments, the process's exclusive kernel lock on the selected `db/.dolt/noms/LOCK` inode, and its exact loopback listener. The launch-specific reader then verifies the committed workspace UUID and schema, bracketed by process, store, and listener checks. Stop sends SIGTERM through the verified pidfd and waits for process exit before removing the local server record. An interrupted launch or stale server record requires inspection.

For launches using `--config`, ownership verification checks the process's inherited descriptor against the synced private `config-id` record and verifies its seals and exact bytes for the selected database, launch directory and endpoint. A running legacy launch remains eligible for verified stop when its arguments exactly match the launcher that supplied all settings as command-line flags and all existing ownership checks pass.

If an owned launch has a safely readable pending marker, `workspace stop` durably records the pidfd-verified exit before removing the launch directory and server record. The proof binds the registered workspace lock and state directory, exact launch, canonical store, executable identity and digest, and exact marker identity and digest. Recovery can finish interrupted launch cleanup when the surviving records match that proof.

`workspace recover` supports v1 Issue note, v2 Issue create, update, and dependency edits, v3 Artifact create and update, and v4 Area create, Project create and Project membership markers. It requires the absolute path to the same Dolt executable used by that launch. It holds the registered workspace and canonical Dolt storage locks while a temporary loopback server reads a private copy of the store and working set with a fresh SELECT-only account. It stops the temporary server before clearing the exact marker. Recovery uses the same marker decoder and outcome checks as live reconciliation.

Recovery reports `confirmed` for a matching receipt introduced at clean HEAD. It verifies the Issue ID, revision, number and counter change for creation, and the exact edit and active Area ancestry at the recorded base for an Issue update. It also verifies the Artifact and, for linked creation, the direct Area link and Area revision change. Recovery reports `rolled_back` for a verified clean HEAD at the recorded base with no receipt in retained history. The selected workspace stays stopped. Unavailable proof, an unsupported marker, or inconclusive history keeps the marker and returns `COMMIT_UNCERTAIN`. An interruption can leave a private `.recovery-*` directory under the workspace state directory for local inspection and cleanup.

## Private Queen conversation file contract

Preserved Queen conversations provide human recall outside Dolt and Git. `0001_initial.sql` adds no session table, and v2 maintains **no database table and no global index file** for conversations.

### Naming and layout

Store each imported conversation snapshot as one immutable UTF-8 Markdown file under `<workspace-root>/conversations/`:

```text
<workspace-root>/conversations/bots/<bot_id>/<conversation_id>.md
```

- `<bot_id>`: canonical lowercase UUIDv7 matching `bots.id` in Dolt.
- `<conversation_id>`: fresh application-generated lowercase UUIDv7 allocated per import.
- Directory mode `0700` and file mode `0600` where supported.
- Repeated imports with the same `--session-ref` allocate a new `<conversation_id>` and create a new file, preserving all earlier files and bytes unchanged.

### Minimal metadata and body format

Each file begins with a delimited YAML frontmatter block (`---` ... `---`) followed by the user-selected UTF-8 Markdown body verbatim:

```markdown
---
version: 1
workspace_id: 0195f1b2-3c4d-7e8f-9a0b-1c2d3e4f5a6b
bot_id: 0195f1b2-5000-7000-8000-000000000001
bot_slug: queen-alpha
conversation_id: 0195f1b2-6000-7000-8000-000000000001
imported_at: "2026-11-27T09:00:00Z"
title: "Release readiness discussion"
provenance:
  source: file
  session_ref: "synthetic-host-session-01"
  body_sha256: "c13dcf456b4c68087bad7c2a2cfac88ec18cbc08a551cd0d6c3523e307bfa9f2"
---

# Release readiness discussion

Synthetic Queen conversation body preserved verbatim for human recall.
```

| Field | Type | Requirement |
| --- | --- | --- |
| `version` | integer | Required; must equal `1`. |
| `workspace_id` | string | Required canonical lowercase UUID matching `hive-workspace.yml` and the committed `workspace_meta.workspace_id`. |
| `bot_id` | string | Required; lowercase UUIDv7 matching directory `<bot_id>` and `bots.id`. |
| `bot_slug` | string | Required; lowercase kebab-case slug matching `bots.slug` for `bot_id`. |
| `conversation_id` | string | Required; lowercase UUIDv7 matching `<conversation_id>.md` filename stem. |
| `imported_at` | string | Required; RFC 3339 UTC timestamp. |
| `title` | string | Optional; nonempty single-line string, max `255` chars, no control characters. |
| `provenance.source` | string | Required; `file` or `stdin`. |
| `provenance.session_ref` | string | Optional; opaque host session reference, max `255` chars, no control characters; may recur across imports. |
| `provenance.body_sha256` | string | Required; 64-character lowercase SHA-256 hex digest of the exact UTF-8 body bytes after the closing frontmatter delimiter. |

Frontmatter uses the same strict YAML parser rules (`deny_unknown_fields`, `<= 64 KiB`, no anchors/aliases/tags/duplicate keys). Candidate body size limit is `4 MiB` UTF-8 bytes; reject oversize inputs, invalid UTF-8, or NUL bytes with a typed error and no silent truncation.

### Validation, atomic publication, and reads without an index

- **Import (`conversation import`):**
  1. Acquire the workspace's OS-held exclusive lock (`$XDG_STATE_HOME/hive/v2/workspaces/<workspace_id>/lock`).
  2. Validate workspace identity (`hive-workspace.yml`, `active-root.json`, and `db/` `workspace_meta.workspace_id`) and verify the target Queen is registered and **active** (`bots.retired_at IS NULL`). Reject retired Queens on new imports.
  3. Validate the explicit `--file` (regular file, `O_NOFOLLOW`, no traversal/symlink escape) or `--stdin` input as UTF-8 within size bounds, compute `body_sha256`, and allocate a fresh UUIDv7 `conversation_id`.
  4. Ensure `conversations/bots/<bot_id>/` exists (`0700`, no symlink escape), write the complete frontmatter and verbatim body to a sibling temporary file (`.tmp-<conversation_id>-<nonce>`, mode `0600`), and `fsync` the temporary file.
  5. Publish atomically with no-clobber semantics (such as `link` + `unlink` or non-replacing atomic rename) to `<conversation_id>.md` so an existing file can never be overwritten, then `fsync` the parent directory before releasing the lock. Remove the temporary file on any failure.
  6. Do **not** create a Dolt transaction, Dolt mutation receipt, Issue revision update, or global index file.
- **Historical list, show, and export (`conversation list`, `show`, `export`):**
  - Permitted for **both active and retired** registered Queens in the selected workspace (`bots` row must exist; `retired_at` may be set).
  - `conversation list` reads bounded metadata from regular published files in the selected Queen directory. Exclude unpublished staging files and refuse unsafe paths. Validate the workspace, Queen and filename identity. Sort by conversation ID descending and page at 25 by default or up to 100. Bind cursors to the selected workspace, Queen, query, sort and a digest of the observed published metadata. A changed scope or published set returns `STALE_CURSOR`. Return `CONTEXT_LIMIT` if the scan cannot complete within the implementation's bounded metadata budget. Read conversation bodies through explicit show, export or backup.
  - `conversation show` opens `<conversation_id>.md` via descriptor-relative `O_NOFOLLOW`, validates all frontmatter fields and `body_sha256`, and returns metadata plus body without modifying state or runtime context.
  - `conversation export` validates the stored file identically and writes a standalone UTF-8 Markdown copy to a new safe destination (no-clobber, no symlink escape).
- **Context and backup/restore boundaries:**
  - `context resolve` strictly excludes `conversations/` by default.
  - Ordinary `backup create` backs up paired Dolt and `assets/` Git revisions and excludes `conversations/` by default, recording in the manifest that conversations were omitted.
  - `backup create --include-conversations` (under the exclusive workspace lock) includes a `conversations` section in the backup manifest **even when its file list is empty**, binding each entry to `workspace_id`, `bot_id`, `bot_slug`, `conversation_id`, safe relative path, and SHA-256 digest.
  - `backup restore` without `--include-conversations` validates and restores only the paired Dolt/Soul snapshot into a separate empty inactive target and omits any bundled conversation files.
  - `backup restore --include-conversations` requires the manifest's `conversations` section before writing the target (returning a typed error if absent), validates digests, paths, `workspace_id`, registered Queen (active or retired), `conversation_id`, and UTF-8 bodies, and restores the files into the inactive target's `conversations/` root outside Git worktrees.

## Synthetic inputs and verification boundaries (`#18` and `#20`)

All verification must run on isolated disposable directories using synthetic UUIDs, synthetic `SOUL.md`, and synthetic UTF-8 Markdown conversations. Never read or write live workspaces, real user data, or credentials. Native host activation and `AGENTS.md` auto-discovery verification remain owned by `#14`/`#20`.

| Boundary | Owning issue | Synthetic inputs | Required verification |
| --- | --- | --- | --- |
| XDG & contract isolation | `#18` (`A15`) | One XDG tree containing both v1 (`hive/config.yml`, `hive.yml`) and v2 (`hive/v2/config.yml`, `hive-workspace.yml`) | `--contract 2` resolves only v2 and rejects v1 with `SCHEMA_INCOMPATIBLE`; v1 refuses a v2-only root and v2 refuses a root with both markers; unset/empty/relative XDG vars fall back properly; relative/unset `XDG_RUNTIME_DIR` uses loopback, never `/tmp`; missing `HOME` fails closed |
| Precedence, bindings & provenance | `#18` (`A01`, `A11`, `A15`) | Synthetic CLI flags, machine-local `bindings.yml` with Git and non-Git directories, `config.yml`, and XDG candidate | Order `cli > binding > user > xdg-default` with exact `sources.*` provenance; invalid CLI target refuses without fallback; a valid explicit workspace wins over a different directory binding without borrowing that binding’s scope; explicit Area and Project win over same-workspace defaults. Ambiguous scope returns `AMBIGUOUS_SCOPE`. Duplicate canonical binding paths return `INVALID_CONFIG`. A multi-Area Resource returns candidates. External project files and Git config remain untouched |
| Strict YAML, paths & runtime state | `#18` (`A11`, `A14`) | Malformed YAML (unknown/duplicate keys, anchors, tags, `> 64 KiB`, non-UTF-8), symlinks escaping root, descriptor/DB `workspace_id` mismatch, and an inactive restored copy | Typed errors with redacted diagnostics; symlink traversal refused; `workspace_id` mismatch fails closed; `workspace_id`-keyed lock and `active-root.json` block writes to an inactive copy; no secrets or machine-local bindings in `hive-workspace.yml`, `db/`, `assets/`, or backup manifests |
| Committed Soul & management exclusion | `#20` (`A06`, `A20`, `D03`) | Synthetic `assets/bots/<slug>/` with committed `SOUL.md`, dirty/untracked `SOUL.md`, optional `README.md`/`AGENTS.md`, and symlink escape | Dirty/untracked Soul and symlink escapes fail; management frontmatter stripped from Soul payload; optional `README.md`/`AGENTS.md` never required and strictly excluded from `context resolve` |
| Private Queen conversations | `#20` (`A06`, `A11`, `D01`) | Synthetic active and retired Queens, two UTF-8 Markdown imports sharing one `--session-ref`, corrupt `body_sha256`, mismatched IDs, oversize/non-UTF-8/symlink input | Atomic lock-held import creates distinct `conversation_id` files preserving earlier bytes; retired Queen rejects new imports while permitting `list`/`show`/`export`; invalid inputs and corrupt files fail closed; `context resolve` excludes conversation bodies |
| Conversation backup & restore | `#20` (`A13`, `D05`) | Synthetic paired Dolt/Soul backups with and without `--include-conversations` (including empty list and retired Queen history) | Default backup omits conversations; opt-in backup writes manifest section and digests; default restore omits bundled conversations; opt-in restore fails before writing when section is absent and restores verified active/retired history into an inactive target when present |
