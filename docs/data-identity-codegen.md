# Rust Identity Generation

## What it is

The Rust identity emitter turns the validated offline identity bundle into deterministic Rust tables without compiling the registry. The runtime consumes the append-only 26.2/26.3 union, keeping the full 26.2 identity prefix and release-specific wire maps.

## How it works

`crates/lodestone-data/tools/identity_staging.py::build_rust_files` validates the bundle against the canonical census and both official reports before rendering anything (`--bundle` accepts a previously staged JSON). Ordering and semantic wire joins come from the bundle; the emitter has no second mapping policy. Each staging directory holds six Rust files and `manifest.json`:

| File | Identity columns | Consumers |
| --- | --- | --- |
| `block_registry.rs` | `BLOCK_COUNT`, `BLOCK_REGISTRY_NAMES`, `STATE_BLOCK`, `BLOCK_STATE_SPANS` | `Block::name`, `StateId::block`, property lookup |
| `block_enum.rs` | `Block`, `BLOCKS_BY_REGISTRY_ID`, `REGISTRY_IDS_BY_NAME`, `DEFAULT_STATE` | `Block::from_registry_id`, `from_name`, `default_state` |
| `block_states.rs` | `STATE_COUNT`, `PROPERTY_SETS`, `STATES` | `StateId::properties`, `block_states::block_name`, `BlockStateTable`, asset baking |
| `items.rs` | `ITEM_COUNT`, `ITEM_NAMES` | `item::item_name` |
| `item_enum.rs` | `Item`, `ITEMS_BY_REGISTRY_ID`, `REGISTRY_IDS_BY_NAME` | typed item identities |
| `identity_versions.rs` | per-version defaults, wire counts, ingress and egress arrays | `GameDataVersion` boundaries |

- `STATES` keeps `(alphabetical block-name index, property-set index)`; the first index resolves through `REGISTRY_IDS_BY_NAME` to the canonical block column. Property pairs and the property-set table are explicitly sorted. Spans are `(start, count)`, read as `start..start + count` by `block_states::state_span`, shared by text lookup and `Properties::state_for_block`; no reverse index is resident.
- `DEFAULT_STATE` is total in canonical block space: existing blocks keep the report-marked semantic default, appended blocks take their introducing release's mark, and every report containing a shared block must agree (a conflict fails emission; currently all 1,196 shared blocks agree). `StateId::is_default` compares against it and `snow_support::is_default_state` delegates. The snow behaviour dump's default column stays an independent test witness. Never pick the lowest state ID.
- In `identity_versions.rs`, `v26_2` and (union scope) `v26_3` each hold `BLOCK_DEFAULT_STATES: [Option<u32>; ...]`. Ingress columns are `u32`, outgoing mappings `Option<u32>`; unsupported identities, including those added after 26.2, are `None`. These versioned facts stay distinct from the canonical default.
- The manifest records bundle and census digests, input report hashes, domain counts and a SHA-256 per file, with no timestamps or machine paths. `--rust-check` demands the complete file set and exact bytes. `--scope union --runtime-check` compares the six adopted modules without writing; base scope checks a historical three-file projection and intentionally disagrees with the adopted union runtime.

## How to change it

- Keep ordering, membership and mapping validation in `canonical_census.py` and the staging bundle; change Rust representation only in `build_rust_files` and its helpers, with controls in `test_identity_staging.py`. Official-report fixtures independently check every emitted 26.2 column plus selected shifted and appended wire identities.
- Names must be built-in resource locations that become distinct legal Rust enum variants. Property names and values are lowercase ASCII letters, digits and underscores; ambiguous, duplicate or unsorted spellings fail. Blocks and items must fit `u16` counts, property-set indices `u16`, state counts `u32`; review consuming types before changing a limit.
- Runtime adoption goes through the [behavior union emitter](./data-behavior-codegen.md), which installs identities with every total behaviour and typed-property table. Never install identity files alone: raising `STATE_COUNT` while a lookup stays prefix-sized is unsafe. Runtime census: 1,286 blocks, 35,723 states, 1,658 items; the original 1,196/32,366/1,537 prefixes are unchanged. `tests/block_states.rs::committed_table_matches_report` delegates its read-only check here. Adapter mapping generators remain independently checkable and must agree with the release-specific numeric maps.

## Configuration

`--scope base|union`, report-directory overrides and optional `--census`; both releases validate in either scope. `--bundle` must match the scope.

```sh
python3 crates/lodestone-data/tools/identity_staging.py --scope union --output /tmp/lodestone-identities.json
python3 crates/lodestone-data/tools/identity_staging.py --scope union --bundle /tmp/lodestone-identities.json --rust-output .cache/identity-rust-union
python3 crates/lodestone-data/tools/identity_staging.py --scope union --bundle /tmp/lodestone-identities.json --rust-check .cache/identity-rust-union
python3 crates/lodestone-data/tools/identity_staging.py --scope union --runtime-check
python3 crates/lodestone-data/tools/test_identity_staging.py --official-reports
```

`--rust-output` exclusively creates a new directory under `.cache/` or `/tmp`, rejecting existing directories, runtime-source paths and symlink escapes. JSON, Rust and runtime check destinations are mutually exclusive; no environment variable affects generation. `--runtime-check` defaults to `crates/lodestone-data/src/generated` and accepts an alternate directory. An interrupted write leaves an incomplete directory that fails the exact file-set check.

## Dependencies

Python 3 standard library, the [offline identity staging bundle](./data-generation-identities.md), the [canonical census](./canonical-state-census.md) and cached official reports. No JVM, Cargo build, runtime table, network or version feature. The Python tests prove column semantics and output guards, not Rust compilation or behaviour.
