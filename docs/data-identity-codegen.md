# Rust Identity Generation

## What it is

The Rust identity emitter converts the validated offline identity bundle into deterministic
Rust tables without compiling the existing registry. The runtime consumes the append-only
26.2/26.3 union, preserving the complete 26.2 identity prefix and release-specific wire maps.

## How it works

`crates/lodestone-data/tools/identity_staging.py::build_rust_files` validates the bundle
against the canonical census and both official reports before rendering any file. It can
consume a previously staged JSON bundle with `--bundle`. Canonical ordering and semantic
wire joins come directly from that bundle; the emitter never builds a second mapping policy.

Each staging directory contains six Rust files and `manifest.json`:

| File | Identity columns | Consumers |
| --- | --- | --- |
| `block_registry.rs` | `BLOCK_COUNT`, `BLOCK_REGISTRY_NAMES`, `STATE_BLOCK`, `BLOCK_STATE_SPANS` | `Block::name`, `StateId::block`, numeric block property lookup |
| `block_enum.rs` | `Block`, `BLOCKS_BY_REGISTRY_ID`, `REGISTRY_IDS_BY_NAME`, `DEFAULT_STATE` | `Block::from_registry_id`, `Block::from_name`, `Block::default_state`, exhaustive block matches |
| `block_states.rs` | `STATE_COUNT`, `PROPERTY_SETS`, `STATES` | `StateId::properties`, `block_states::block_name`, `BlockStateTable` and asset baking |
| `items.rs` | `ITEM_COUNT`, `ITEM_NAMES` | `item::item_name`, item registry lookups |
| `item_enum.rs` | `Item`, `ITEMS_BY_REGISTRY_ID`, `REGISTRY_IDS_BY_NAME` | Typed item identities and prototypes |
| `identity_versions.rs` | Per-version defaults, wire counts, ingress and egress arrays | `GameDataVersion` identity boundaries |

`STATES` retains the existing `(alphabetical block-name index, property-set index)`
representation. The first index resolves through `block_enum.rs::REGISTRY_IDS_BY_NAME`
to the canonical block-name column. Property pairs and the distinct property-set table are
sorted explicitly. `STATE_BLOCK` and spans use canonical IDs directly. Each span is
`(start, count)`, consumed as the half-open range `start..start + count` by
`block_states::state_span`. Text state lookup and `Properties::state_for_block` share that
range; neither reconstructs a resident reverse index or stores a second span table.

`DEFAULT_STATE` is total in canonical block space. Existing blocks retain the original
report-marked semantic default; an appended block takes its introducing release's mark.
Every report containing a shared block must agree on that semantic default, even when wire
IDs shift. A conflict fails Rust emission in both scopes pending a deliberate policy change.
The current reports agree for all 1,196 shared blocks. `StateId::is_default` compares against
its owning block's default, and `snow_support::is_default_state` delegates to it. The snow
behavior dump's default column remains an independent all-state test witness, not a fifth
runtime bitset. No default is selected by taking the lowest state ID.

In `identity_versions.rs`, `v26_2` and, for union scope, `v26_3` each contain
`BLOCK_DEFAULT_STATES: [Option<u32>; ...]`. Ingress columns use `u32`; outgoing mappings
use `Option<u32>`. Unsupported blocks, states, items, and defaults retain `None`, including
identities added after 26.2. These versioned facts remain distinct from the total canonical
default policy.

The provenance manifest records the bundle and census digests, exact input report hashes,
domain counts, and SHA-256 for every Rust file. No timestamp or machine-specific path is
serialized. `--rust-check` requires the complete expected file set and exact bytes, including
the provenance manifest. `--scope union --runtime-check` checks the six adopted identity
modules byte-for-byte without writing them. Base scope checks a historical three-file
projection; it intentionally disagrees with an adopted union runtime.

## How to change it

Keep canonical ordering, membership, and mapping validation in `canonical_census.py` and
the existing staging bundle. Change the Rust representation only in `build_rust_files` and
its rendering helpers, with controls in `test_identity_staging.py`. Existing official-report
fixtures independently check every emitted 26.2 identity column, and selected shifted and
appended wire identities.

Names must be built-in resource locations that convert to distinct legal Rust enum variants.
Property names and values must use lowercase ASCII letters, digits, and underscores; ambiguous,
duplicate, or unsorted property spellings fail. Blocks and items must fit the current `u16`
count interfaces, while property-set indices must fit `u16` and state counts fit `u32`.
Changing these limits requires reviewing the consuming types rather than silently narrowing
an index.

Runtime adoption goes through the [behavior union emitter](./data-behavior-codegen.md),
which invokes this emitter and installs identities together with every total behavior and
typed-property table. Never install identity files alone: raising `STATE_COUNT` while a
lookup remains prefix-sized makes valid identities unsafe. The runtime census has 1,286
blocks, 35,723 states, and 1,658 items; the original 1,196/32,366/1,537 prefixes are unchanged.
`tests/block_states.rs::committed_table_matches_report` delegates its read-only identity
check to this emitter. Adapter mapping generators remain independently checkable wire
representations; they must agree with the release-specific numeric identity maps.

## Configuration

The emitter uses the existing `--scope base` and `--scope union`, report-directory overrides,
and optional `--census` input. Both releases are validated in either scope. `--bundle` must
match the requested scope and pass the same complete validation.

```sh
python3 crates/lodestone-data/tools/identity_staging.py --scope union \
  --output /tmp/lodestone-identities.json
python3 crates/lodestone-data/tools/identity_staging.py --scope union \
  --bundle /tmp/lodestone-identities.json --rust-output .cache/identity-rust-union
python3 crates/lodestone-data/tools/identity_staging.py --scope union \
  --bundle /tmp/lodestone-identities.json --rust-check .cache/identity-rust-union
python3 crates/lodestone-data/tools/identity_staging.py --scope base \
  --rust-output .cache/identity-rust-base
python3 crates/lodestone-data/tools/identity_staging.py --scope union --runtime-check
python3 crates/lodestone-data/tools/test_identity_staging.py --official-reports
```

`--rust-output` exclusively creates a new directory under the repository's `.cache/` or
`/tmp` (`/private/tmp` on macOS). It rejects existing directories, runtime-source paths,
and symlink escapes. It writes nothing unless explicitly requested. JSON output/check,
Rust output/check, and runtime-check destinations are mutually exclusive; no environment
variable changes generation. `--runtime-check` defaults to `crates/lodestone-data/src/generated`
and accepts an alternate directory for read-only checks. Interrupted writes may leave an
incomplete private directory, which fails the
exact file-set check.

## Dependencies

Python 3's standard library, the [offline identity staging bundle](./data-generation-identities.md),
the [canonical census](./canonical-state-census.md), and cached official reports are required.
No JVM, Cargo build, compiled `Block` or `Item` enum, runtime behavior table, network request,
or version feature is involved. Python tests verify emitted column semantics and output
guards; they do not establish Rust compilation, runtime behavior, or rendered pixels.
