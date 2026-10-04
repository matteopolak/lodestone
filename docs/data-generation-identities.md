# Offline Identity Staging

## What it is

The offline identity staging tool emits canonical block names, state spans, state owners,
per-version defaults, and item IDs from the official reports and the append-only canonical
census. It produces a reviewable JSON artifact without compiling Rust or changing runtime
tables, storage, adapters, or protocol support.

## How it works

`crates/lodestone-data/tools/identity_staging.py` consumes the policy in
`canonical_census.py`. An optional `--census` artifact must pass the same complete report
validation before use. The bundle records its census digest and both releases' exact report
hashes; serialization has no timestamp or machine-specific path.

Each domain's `keys` array is indexed by canonical ID. Block-state keys combine the resource
name with sorted property pairs. The domains preserve the census's per-version
`wire_to_canonical` and `canonical_to_wire` mappings. Unsupported outgoing identities remain
`null`.

`domains.blocks.state_spans[block_id]` is `[start, count]` in canonical state space.
`domains.block_states.block_ids[state_id]` identifies the owning canonical block.
`domains.blocks.versions[version].default_states[block_id]` is the canonical state ID selected
by that release's report, or `null` for an unsupported block. Defaults remain versioned even
when both reports currently agree. A default must be explicitly marked exactly once in the
report; a block's entire span must match its reported semantic states.

`--scope base` projects only the 26.2 prefix and its mappings. `--scope union` includes the
append-only union and both versions. Both scopes validate both input releases so the census
provenance and append-only policy stay identical. The 26.2-only identity columns are checked
against separately captured official-report constants, including full-column SHA-256 hashes.

The bundle contains identity facts only. The behavior union reader resolves a row's
resource name and sorted properties through `domains.block_states.keys`, or an item name
through `domains.items.keys`, before emitting numeric tables. It must also retain the source
version: a shared semantic identity does not establish shared behavior. No collision,
lighting, hardness, tool, sound, or item-component row is inferred or copied by this tool.

## How to change it

Keep canonical ordering in `canonical_census.py`; keep staging and its validation in
`identity_staging.py`. Extend `test_identity_staging.py` alongside any schema change. Spans
are checked against complete report membership, not only numeric bounds. A release adding
discontiguous states to an existing block still requires an explicit span-model upgrade.

`crates/lodestone-data/tools/fixtures/identity-staging-witnesses.json` records 26.2 expectations captured directly
with `jq`, independently of the Python generators and Rust tables. Hashes cover compact JSON
arrays with one trailing newline. Block and item keys follow registry IDs; state keys follow
state IDs; spans and defaults follow block registry IDs. Do not refresh these expected values
from the staging output. For example, independently recapture the complete default column:

```sh
jq -cs '.[0] as $blocks | [.[1]["minecraft:block"].entries | to_entries | sort_by(.value.protocol_id)[] | $blocks[.key].states[] | select(.default == true) | .id]' \
  .cache/mc/26.2/generated/reports/blocks.json \
  .cache/mc/26.2/generated/reports/registries.json | shasum -a 256
```

The official controls deliberately replace oak log's default with another valid state in
the same span, and shorten its span while retaining valid numeric bounds. Both must fail.
The hermetic JSON controls also exercise different defaults between releases and unsupported-block
defaults represented by `null`. The [Rust identity emitter](./data-identity-codegen.md) preserves
these versioned facts but requires shared semantic defaults to agree before emitting a total
canonical default column. A control gives oak log a different, valid latest-release default
and observes Rust emission fail in both scopes.

The runtime consumes the six union identity modules through the coordinated
[behavior emitter](./data-behavior-codegen.md). Canonical defaults feed `Block::default_state`
and `StateId::is_default`; numeric half-open spans feed text resolution and typed property
lookup. The union keeps every 26.2 ID unchanged, with release-specific unsupported egress
represented explicitly. JSON staging alone neither installs this union nor proves behavior,
adapter ingress/egress, persistence compatibility, palette widths, or rendered assets.

## Configuration

Python 3's standard library is sufficient. Inputs default to `blocks.json` and `registries.json`
in `.cache/mc/26.2/generated/reports` and `.cache/mc/26.3/generated/reports`; override directories
with `--base-reports` and `--latest-reports`. Missing reports fail explicitly.

```sh
python3 crates/lodestone-data/tools/identity_staging.py --scope base
python3 crates/lodestone-data/tools/identity_staging.py --scope union --output /tmp/lodestone-identities.json
python3 crates/lodestone-data/tools/identity_staging.py --scope union --check /tmp/lodestone-identities.json
python3 crates/lodestone-data/tools/test_identity_staging.py
python3 crates/lodestone-data/tools/test_identity_staging.py --official-reports
```

The default invocation validates and prints only domain counts and a digest. `--output`
creates one file exclusively, refusing an existing path. `--check` validates semantics and
requires byte-for-byte regeneration. `--census /path/to/manifest.json` consumes an existing
manifest rather than constructing one in memory. No environment variables or Rust build
features affect the output.

## Dependencies

This tool depends on the [canonical census](./canonical-state-census.md), official generated
reports, and Python's standard library. It does not depend on `BlockStateTable`, the compiled
`Block` or `Item` enums, JVM behavior dumps, or generated runtime tables. The independent hash
capture uses `jq` and SHA-256; ordinary generation and tests do not require `jq`.
