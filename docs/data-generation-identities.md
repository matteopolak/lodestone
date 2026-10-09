# Offline Identity Staging

## What it is

An offline tool that emits canonical block names, state spans, state owners, per-version defaults and item IDs from the official reports and the append-only canonical census, as a reviewable JSON artifact. It compiles no Rust and changes no runtime tables, storage, adapters or protocol support.

## How it works

- `crates/lodestone-data/tools/identity_staging.py` consumes the policy in `canonical_census.py`. An optional `--census` artifact must pass the same full validation. The bundle records the census digest and both releases' report hashes, with no timestamps or machine paths.
- Each domain's `keys` array is indexed by canonical ID; block-state keys combine resource name and sorted property pairs. Per-version `wire_to_canonical` and `canonical_to_wire` mappings are preserved, and unsupported outgoing identities are `null`.
- `domains.blocks.state_spans[block_id]` is `[start, count]` in canonical state space; `domains.block_states.block_ids[state_id]` names the owner; `domains.blocks.versions[version].default_states[block_id]` is the canonical state the release's report selects, or `null` if unsupported. Defaults stay versioned even when reports agree. A default must be marked exactly once in the report, and a block's span must match its reported semantic states.
- `--scope base` projects only the 26.2 prefix; `--scope union` includes the append-only union and both versions. Both validate both releases so provenance and the append-only policy match. The 26.2 columns are checked against separately captured official constants, including full-column SHA-256 hashes.
- The bundle holds identity facts only. The behaviour union reader resolves a row's name and sorted properties through `domains.block_states.keys` (or an item through `domains.items.keys`) before emitting numeric tables and keeps the source version, since shared identity does not mean shared behaviour. Nothing about collision, lighting, hardness, tools, sound or item components is inferred.

## How to change it

- Canonical ordering lives in `canonical_census.py`; staging and validation in `identity_staging.py`; extend `test_identity_staging.py` with any schema change. Spans are checked against full report membership, not only bounds, and a release adding discontiguous states to an existing block needs an explicit span-model upgrade.
- `crates/lodestone-data/tools/fixtures/identity-staging-witnesses.json` holds 26.2 expectations captured with `jq`, independent of the Python generators and Rust tables (hashes cover compact JSON arrays with one trailing newline; block and item keys follow registry IDs, state keys state IDs, spans and defaults block registry IDs). Never refresh them from staging output. Example recapture of the default column:

  ```sh
  jq -cs '.[0] as $blocks | [.[1]["minecraft:block"].entries | to_entries | sort_by(.value.protocol_id)[] | $blocks[.key].states[] | select(.default == true) | .id]' \
    .cache/mc/26.2/generated/reports/blocks.json \
    .cache/mc/26.2/generated/reports/registries.json | shasum -a 256
  ```
- Controls: replacing oak log's default with another valid state, or shortening its span while keeping valid bounds, must fail; hermetic JSON controls cover differing defaults between releases and `null` defaults. The [Rust emitter](./data-identity-codegen.md) keeps versioned facts but requires shared semantic defaults to agree (a different latest-release default for oak log fails emission in both scopes).
- The runtime consumes the six union modules through the [behavior emitter](./data-behavior-codegen.md): canonical defaults feed `Block::default_state` and `StateId::is_default`, half-open spans feed text resolution and property lookup, and every 26.2 ID is unchanged. JSON staging alone proves neither behaviour, adapter ingress or egress, persistence compatibility, palette widths nor assets.

## Configuration

Inputs default to `blocks.json` and `registries.json` in `.cache/mc/26.2/generated/reports` and `.cache/mc/26.3/generated/reports` (override with `--base-reports`, `--latest-reports`; missing reports fail).

```sh
python3 crates/lodestone-data/tools/identity_staging.py --scope base
python3 crates/lodestone-data/tools/identity_staging.py --scope union --output /tmp/lodestone-identities.json
python3 crates/lodestone-data/tools/identity_staging.py --scope union --check /tmp/lodestone-identities.json
python3 crates/lodestone-data/tools/test_identity_staging.py [--official-reports]
```

The default run prints only domain counts and a digest. `--output` creates a file exclusively; `--check` validates and requires byte-for-byte regeneration; `--census` consumes an existing manifest. No environment variables or Rust features matter.

## Dependencies

The [canonical census](./canonical-state-census.md), official reports and the Python standard library. No `BlockStateTable`, compiled enums, JVM dumps or runtime tables; `jq` only for the independent hash capture.
