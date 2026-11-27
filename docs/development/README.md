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

Recursive Area listing, global discovery, readiness and blocker queries, and context resolution are follow-up work.

## Agent-assisted development

Use [instruction ownership](agents/rule-ownership.md) to place scoped guidance, [asset lifecycle](agents/assets.md) when selecting or updating reusable Agent Assets, and [harness verification](agents/harness.md) to distinguish configuration from observed runtime behavior.

## When the project grows

Document the source layout, application setup, tests and contribution process as they become relevant. Keep design decisions in [Architecture](../architecture/README.md) and executable settings in their configuration files.
