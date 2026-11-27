---
description: "Use for contributor setup, development practices, agent guidance and verification responsibilities."
---

# Development

Use this area for how contributors understand, change and verify Hive system software. The [Rust CLI](../../Cargo.toml) and the TypeScript development scripts have separate build and test commands. Use synthetic workspace records for development fixtures.

## Current setup

Shared tooling and setup are covered under [Tools](../tools/README.md).

Find existing automation through the [scripts index](../../scripts/README.md). When implementing or modifying project-owned scripts, use [script maintenance](scripts.md); task declarations remain in [mise.toml](../../mise.toml).

## Rust CLI

Install Rust through Rustup. The [toolchain file](../../rust-toolchain.toml) selects the compiler, rustfmt and Clippy for the checkout. Run the same locked checks used by [Rust CI](../../.github/workflows/rust.yml).

```sh
cargo test --locked --all-targets
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo build --locked
```

To check installation without changing your usual executable directory, install into a temporary root.

```sh
hive_install_root=$(mktemp -d)
cargo install --locked --path . --root "$hive_install_root"
"$hive_install_root/bin/hive" --help
"$hive_install_root/bin/hive" --version
```

Use an isolated synthetic workspace to test `config show`, `config validate`, `context` and local `doctor`. Git-backed inspection requires native Git with `--no-lazy-fetch` support and was tested on Linux with Git 2.53.0. Commands that are still pending return `NOT_IMPLEMENTED`. Follow the [CLI contract](../architecture/cli-contract.md) for configuration, output and exit codes.

## Dolt workspace descriptor inspection

On an isolated synthetic v2 workspace, use the read-only configuration command to check the descriptor and how the CLI selected it.

```sh
hive --contract 2 --json config show --scope workspace \
  --workspace /example/hive-v2-userdata
```

An explicit workspace takes precedence. Without one, the CLI uses the v2 user configuration or checks the v2 XDG data candidate. The report identifies the selected source and marks the committed Dolt identity and runtime as unverified. Continue to supply the explicit database, port, and expected workspace UUID for Dolt record commands. See the [v2 local file contract](../architecture/dolt-workspace-contract.md) for configuration and descriptor fields.

Use `hive --contract 2 --json config show` for the resolved report. It reads machine-local `bindings.yml` and selects the longest registered directory that contains the invocation directory. Use `--resource PATH` after `show` to require an exact bound directory. An explicit workspace takes precedence over the binding, and explicit `--area UUID` and `--project UUID` override the applicable binding defaults. Both IDs must be canonical lowercase UUIDv7 values. The report includes the contributing paths and marks Dolt scope membership, committed identity, and runtime as unverified.

## Dolt record validation

The v2 validator reads a single committed snapshot of Dolt `main` through a dedicated SELECT-only account on loopback. It verifies the pinned server version, required tables, migration ledger version and digest, and expected workspace UUID. It checks canonical UUIDv7 Area and Issue IDs, their graph references, lowercase sibling slugs, Issue numbers, and cycles in Area ancestry, Issue parentage, prerequisites and combined completion obligations. Validation returns up to 100 diagnostics and refuses scans above 100,000 rows per checked table. The JSON result identifies the observed Dolt commit after schema and identity verification.

Validation checks Issue closure fields and active Claim cardinality. An active Claim is invalid when the Issue's Area or any of its ancestors is archived. In-progress, review and done Issues require done prerequisites. Done Issues and epics in review require terminal children. A cancelled child meets the terminal-state check. Verification of explicit acceptance of the changed scope requires transition evidence and belongs to lifecycle provenance validation.

Validation checks canonical UUIDv7 IDs for Projects, Resources and Labels, exact Project statuses (`active`, `completed` and `archived`), canonical Label names (nonempty, trimmed of leading and trailing Unicode whitespace, at most 100 characters, and free of control characters, preserving spelling, accents, and interior spaces), and both endpoints of Project memberships, Area and Issue Resource links, and Issue Label links. Projects may span Areas, and Resources may be shared. Label-name uniqueness follows the SQL unique key and its installation collation without case-folding, Unicode rewriting, or duplicate detection beyond SQL; future label creation trims leading and trailing Unicode whitespace before storing.

Validation checks canonical UUIDv7 IDs for Artifacts and Directives. It checks Artifact kinds, including legacy `message`, `checkpoint`, and `manifest`, paired archive fields, and both endpoints of Area, Project, Issue, and Resource links. Artifacts may be standalone or link to multiple scopes. For Directives, it checks the exact status and scope fields, scope and supersession references, supersession cycles, approval reference presence for active records, and ordering of stored validity bounds. Applicability at a given time belongs to context resolution. Authorization and provenance belong to guarded operations and historical validation.

Validation checks canonical UUIDv7 IDs, owning Issue references, exact lowercase update kinds, and paired JSON-object `transition_json` payloads for committed Issue updates. For committed operation receipts, it checks canonical lowercase UUID `request_id` values, 64-character lowercase hexadecimal SHA-256 `fingerprint` values, and JSON-object `workflow_evidence_json` and `result_json` payloads. Typed payload schemas, historical transition provenance, receipt replay, and guarded authorization semantics remain for guarded write implementation.

Validation checks canonical lowercase UUIDv7 `id` values and exact portable lowercase kebab-case `slug` values for committed `bots` rows, including retired rows. It does not impose extra `display_name` or `revision` invariants, assume Claims bind registered Queens, inspect filesystem Soul assets, or validate deferred `bot_memories` semantics.

The validator compares each required table's `SHOW CREATE TABLE ... AS OF` output with the migration's Dolt 2.7.6 schema shape. Dirty and staged definitions are outside the inspected commit. The exact `issue_number` counter key must have a positive next value greater than every Issue number at that commit. Historical issuance and reuse require separate history evidence.

For an explicitly provisioned synthetic server on Dolt 2.7.6, supply the reader credentials in the process environment as `HIVE_DOLT_USER` and `HIVE_DOLT_PASSWORD`. Select the database, port and expected UUID explicitly.

```sh
hive --contract 2 --json validate --all \
  --database fixture_db --port 3307 \
  --workspace-id 01900000-0000-7000-8000-000000000001
```

Contract 1 remains the CLI default. Under contract 2, schema incompatibility and record violations exit 4. Reader authentication failures exit 6. Unverified workspace identity and unavailable runtime exit 7. Explicit workspace path and scoped validation, full lifecycle provenance checks, and bootstrap and migration commands are follow-up work.

Run the integration tests with the pinned Dolt executable. The fixtures provision disposable data and accounts, invoke the Hive binary built by Cargo, and compare the database commit and working and staged states before and after validation.

```sh
HIVE_DOLT_BIN=/absolute/path/to/dolt cargo test --locked --test dolt_validation -- --ignored
```

To reproduce the schema shape manifest, run its fixture with output enabled. It applies the canonical migration in fresh databases with inherited `utf8mb4_0900_bin` and `utf8mb4_0900_ai_ci` collations. For each database it prints the SHA-256 of the returned table definition after omitting the trailing table-default collation token, with one digest and table name per line in table name order. Both sets must match the checked-in manifest. [Schema shape manifest](../../assets/schema/dolt/0001_schema.sha256).

```sh
HIVE_DOLT_BIN=/absolute/path/to/dolt cargo test --locked --test dolt_validation \
  reproduces_initial_schema_manifest_across_inherited_collations -- --exact --ignored --nocapture
```

## Committed Issue inspection

Use the same synthetic Dolt target and SELECT-only account for `issue show`. Supply a canonical lowercase UUIDv7 or quote a positive Issue number so the shell passes the `#` prefix.

```sh
hive --contract 2 --json issue show '#42' \
  --database fixture_db --port 3307 \
  --workspace-id 01900000-0000-7000-8000-000000000001
```

The result includes the verified workspace and Dolt commit, the selected Issue, and bounded direct relation IDs. A missing Issue returns `NOT_FOUND` at the verified commit. Each relation list returns up to 25 IDs and sets `truncated` if more exist. Use explicit selection to inspect other records as their read commands become available.

`issue list` uses the same verified committed snapshot. Select exactly one of `--all`, `--area UUID`, or `--project UUID`. Area and Project selectors use canonical lowercase UUIDv7 IDs. Area selection covers only Issues owned by that Area. Project selection covers only direct members of that Project, including members across Areas.

```sh
hive --contract 2 --json issue list --area 01900000-0000-7000-8000-000000000002 \
  --status todo --limit 10 --database fixture_db --port 3307 \
  --workspace-id 01900000-0000-7000-8000-000000000001
```

The optional filters are `--status`, `--type`, `--priority` (0–3), and `--label` (exact stored name). The response contains summary fields and the observed Dolt commit. Pages are ordered by priority ascending with unspecified priority last, then oldest creation timestamp, then ascending Issue ID. The default page size is 25 and the maximum is 100. When `truncated` is true, pass `next_cursor` as `--cursor` with the same arguments. A changed workspace, commit, scope, filter, page size, or sort returns `STALE_CURSOR` (exit 5). Restart the list to read the new snapshot.

A selected Issue with malformed summary fields returns `INVALID_RECORD` at the verified commit.

`issue ready` and `issue blocked` accept the same target, scopes, filters, and pagination options. Both select executable todo Issues. Ready Issues have no hold, current Claim, unmet prerequisite, or archived Area in their ancestry. Missing prerequisites, invalid readiness metadata, and uncertain Area chains also block readiness. Blocked summaries include a `reasons` array of stable codes. The query reports prerequisite and Area blockers without exposing related records. Each cursor is specific to its view. Readiness describes the observed commit and grants no execution permission.

Global discovery and context resolution are follow-up work.

## Committed Area trees

Use the same synthetic Dolt target and SELECT-only account for `area tree`. Omit `--root` to read the forest of committed root Areas, or select one subtree by its canonical lowercase UUIDv7.

```sh
hive --contract 2 --json area tree \
  --root 01900000-0000-7000-8000-000000000002 \
  --database fixture_db --port 3307 \
  --workspace-id 01900000-0000-7000-8000-000000000001
```

The result contains an array of root summaries, including archived Areas, and a `children` array at each node. Roots and siblings sort by bytewise slug, then ID. A known leaf has empty children, while an unknown root returns `NOT_FOUND`. The limit is 100 nodes across the entire result and 32 levels, with each returned root at level 1. Larger selections return `CONTEXT_LIMIT`. Cycles encountered during traversal return `INVALID_RECORD`.

## Committed Project inspection

Use the same explicit Dolt target and SELECT-only account for `project list` and `project show` under contract 2.

```sh
hive --contract 2 --json project list --status active --limit 10 \
  --database fixture_db --port 3307 \
  --workspace-id 01900000-0000-7000-8000-000000000001

hive --contract 2 --json project show 01900000-0000-7000-8000-000000000003 \
  --database fixture_db --port 3307 \
  --workspace-id 01900000-0000-7000-8000-000000000001
```

List orders summaries by Project ID and accepts exact `active`, `completed`, or `archived` status. The default page size is 25 and the maximum is 100. Pass `next_cursor` as `--cursor` with the same arguments. A changed workspace, commit, status filter, or page size returns `STALE_CURSOR` (exit 5).

Show takes a canonical lowercase UUIDv7 and returns Project details and up to 25 direct member Issue IDs across Areas. The `issues.truncated` field indicates additional members. Use `issue list --project UUID` to page through member summaries.

## Committed Directive inspection

Use the explicit Dolt target and SELECT-only account under contract 2. Select exactly one scope with `--workspace-scope`, `--area UUID`, or `--project UUID`. Area and Project selectors match exact scopes without expanding to descendants or members. Scope and Directive IDs use canonical lowercase UUIDv7.

```sh
hive --contract 2 --json directive list --workspace-scope --status all \
  --database fixture_db --port 3307 --workspace-id 01900000-0000-7000-8000-000000000001
hive --contract 2 --json directive show 01900000-0000-7000-8000-000000000061 \
  --area 01900000-0000-7000-8000-000000000002 \
  --database fixture_db --port 3307 --workspace-id 01900000-0000-7000-8000-000000000001
```

List returns metadata ordered by ascending Directive ID. The status defaults to `active` and also accepts `proposed`, `superseded`, `withdrawn`, and `all`. The default page size is 25 and the maximum is 100. Pass `next_cursor` as `--cursor` with the same target and query. A changed commit or query returns `STALE_CURSOR`.

Show returns the selected Directive at the verified commit, including historical statuses. The combined inline body and provenance reference limit is 64 KiB. References are returned as stored values. Workflow applicability requires separate verification of scope, issuer, validity, and approval provenance.

## Committed Artifact inspection

Use an explicit Dolt target and SELECT-only account for contract 2 Artifact reads.

```sh
hive --contract 2 --json artifact list \
  --scope area:01900000-0000-7000-8000-000000000002 \
  --database fixture_db --port 3307 \
  --workspace-id 01900000-0000-7000-8000-000000000001

hive --contract 2 --json artifact show 01900000-0000-7000-8000-000000000004 \
  --scope area:01900000-0000-7000-8000-000000000002 \
  --scope project:01900000-0000-7000-8000-000000000003 \
  --database fixture_db --port 3307 \
  --workspace-id 01900000-0000-7000-8000-000000000001
```

List selects exactly one direct scope using `area:UUID`, `project:UUID`, `issue:UUID`, or `resource:UUID` and returns only Artifact ID, kind, title, and revision. It defaults to 25 items and caps pages at 100. Pass `next_cursor` as `--cursor` with the same target, scope, and page size. A changed commit or query returns `STALE_CURSOR`.

Show takes an exact canonical lowercase UUIDv7. An exact ID alone suffices for a standalone Artifact. For a linked Artifact, repeat `--scope` to explicitly cover every direct link. The CLI verifies the Artifact and each selected scope at the same committed snapshot before returning up to 64 KiB of Markdown. Incomplete coverage returns `UNAUTHORIZED` without disclosing linked scopes or body content.

## Committed Bot metadata

Use the same explicit Dolt target and SELECT-only account for contract 2 Bot reads.

```sh
hive --contract 2 --json bot list --status retired --limit 10 \
  --database fixture_db --port 3307 --workspace-id 01900000-0000-7000-8000-000000000001
hive --contract 2 --json bot show 01900000-0000-7000-8000-00000000000a \
  --database fixture_db --port 3307 --workspace-id 01900000-0000-7000-8000-000000000001
```

`bot list` defaults to `--status active` and accepts `retired` or `all`. It orders entries by ascending Bot UUID, with 25 results per page by default and a maximum of 100. Pass `next_cursor` as `--cursor` with the same target, status and page size. Changes to the committed snapshot or query return `STALE_CURSOR`.

`bot show` takes an exact lowercase UUIDv7 and can inspect retired Queens. Both commands return registry metadata and the verified Dolt commit. Registry status describes retirement.

For read-only Soul inspection of an explicitly selected workspace copy, add the descriptor root and the full Git commit object ID from its dedicated `assets/.git` repository.

```sh
hive --contract 2 --json bot show 01900000-0000-7000-8000-00000000000a \
  --database fixture_db --port 3307 --workspace-id 01900000-0000-7000-8000-000000000001 \
  --workspace ./fixture-copy --asset-commit "$asset_commit"
```

Select the copy's Dolt endpoint explicitly. Hive checks the workspace UUID against the descriptor and the endpoint's committed `main` snapshot. The committed Queen's slug selects exactly `assets/bots/<slug>/SOUL.md` at the supplied Git commit. The output includes the Soul path and UTF-8 body, Queen metadata, workspace UUID, Dolt commit, asset commit, and descriptor policy pin. Optional YAML frontmatter is omitted from the body. When present, `workspace_id`, `id` or `bot_id`, `slug` or `bot_slug`, `display_name`, and `role` must match the selected identity.

Soul inspection requires Linux with access to `/proc/self/fd` and a self-contained Git object store. Hive pins the Git and object directories for the read, rejects alternates and symlinks in the object store, and rechecks the directory identities and layout before returning a result. Git reads use temporary, isolated reader metadata outside the workspace. Hive reads the source repository’s declared SHA-1 or SHA-256 format with configuration includes disabled. It verifies the requested commit and every tree along the path against their object IDs before following child entries. The read limits are 1 MiB per commit or tree and 128 KiB for the Soul blob, which is also checked against its object ID. These checks assume the repository is under the operator’s control.

Inspection establishes portable identity agreement for the selected revisions. Physical endpoint pairing, an active-root decision, and host loading require separate evidence.

## Committed Resource reads

Use the same explicit Dolt target and SELECT-only account for contract 2 Resource reads.

```sh
hive --contract 2 --json resource list --area 01900000-0000-7000-8000-000000000002 \
  --database fixture_db --port 3307 --workspace-id 01900000-0000-7000-8000-000000000001
hive --contract 2 --json resource show 01900000-0000-7000-8000-000000000014 \
  --database fixture_db --port 3307 --workspace-id 01900000-0000-7000-8000-000000000001
```

`resource list` requires exactly one of `--all`, `--area UUID`, or `--issue UUID`. Scope and Resource selectors use canonical lowercase UUIDv7 IDs. Area and Issue scopes include only directly linked Resources. A known scope with no links returns an empty page, and an unknown scope returns `NOT_FOUND`.

List pages contain Resource IDs, kinds, names, and revisions in ascending bytewise ID order, with 25 items by default and a maximum of 100. Pass `next_cursor` as `--cursor` with the same target, scope, and page size. A changed snapshot or query returns `STALE_CURSOR`. Exact-ID `resource show` adds the stored URI to the selected record. Both commands report the verified Dolt commit and leave URIs unopened.

## Historical Queen conversations

Use contract 2 with an explicit workspace root, the same authenticated Dolt reader and target options, and exact lowercase UUIDv7 selectors for the Queen and conversation.

```sh
hive --contract 2 --json conversation show --workspace /example/hive-v2-userdata \
  --database fixture_db --port 3307 --workspace-id 01900000-0000-7000-8000-000000000001 \
  --bot 01900000-0000-7000-8000-00000000000a --id 01900000-0000-7000-8000-00000000000b
```

The read returns validated metadata and the verbatim Markdown body for an active or retired Queen. Hive matches the portable workspace UUID across the descriptor, committed Dolt target, and conversation header. The header must also match the committed Queen UUID and slug. Copies can share a workspace UUID, so select the filesystem copy and its Dolt target explicitly. Physical database-to-root binding and active-root authority are outside this inspection. See the [conversation file contract](../architecture/dolt-workspace-contract.md#private-queen-conversation-file-contract).

On Linux and macOS, list a Queen's published conversation metadata with the same explicit workspace and Dolt selection.

```sh
hive --contract 2 --json conversation list --workspace /example/hive-v2-userdata \
  --database fixture_db --port 3307 --workspace-id 01900000-0000-7000-8000-000000000001 \
  --bot 01900000-0000-7000-8000-00000000000a --limit 25
```

List returns metadata in descending conversation UUID order for one active or retired Queen. The default page size is 25 and the maximum is 100. To continue, pass `next_cursor` as `--cursor` with the same selection and page size. The observed revision is a digest of the published frontmatter. A different workspace copy, Dolt target, committed snapshot, Queen, page size, or published metadata set returns `STALE_CURSOR`. List stops at the closing frontmatter delimiter of each file and skips unpublished staging files. It returns `CONTEXT_LIMIT` if the full scan exceeds 4,096 directory entries (including staging) or 4 MiB of frontmatter, including delimiters.

## Agent-assisted development

Use [instruction ownership](agents/rule-ownership.md) to place scoped guidance, [asset lifecycle](agents/assets.md) when selecting or updating reusable Agent Assets, and [harness verification](agents/harness.md) to distinguish configuration from observed runtime behavior.

## When the project grows

Document the source layout, application setup, tests and contribution process as they become relevant. Keep design decisions in [Architecture](../architecture/README.md) and executable settings in their configuration files.
