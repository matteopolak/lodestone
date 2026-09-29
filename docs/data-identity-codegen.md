# Staged Rust Identity Generation

## What it is

The Rust identity emitter converts the validated offline identity bundle into private,
deterministic Rust tables without compiling the existing registry. It prepares canonical
block, state, and item identities plus explicit per-version defaults and wire mappings;
runtime adoption remains a separate change.

## How it works

`crates/lodestone-data/tools/identity_staging.py::build_rust_files` validates the bundle
against the canonical census and both official reports before rendering any file. It can
consume a previously staged JSON bundle with `--bundle`. Canonical ordering and semantic
wire joins come directly from that bundle; the emitter never builds a second mapping policy.

Each staging directory contains six Rust files and `manifest.json`:

| File | Identity columns | Intended consumers at adoption |
| --- | --- | --- |
| `block_registry.rs` | `BLOCK_COUNT`, `BLOCK_REGISTRY_NAMES`, `STATE_BLOCK`, `BLOCK_STATE_SPANS` | `Block::name`, `StateId::block`, numeric block property lookup |
| `block_enum.rs` | `Block`, `BLOCKS_BY_REGISTRY_ID`, `REGISTRY_IDS_BY_NAME` | `Block::from_registry_id`, `Block::from_name`, exhaustive block matches |
| `block_states.rs` | `STATE_COUNT`, `PROPERTY_SETS`, `STATES` | `StateId::properties`, `block_states::block_name`, `BlockStateTable` and asset baking |
| `items.rs` | `ITEM_COUNT`, `ITEM_NAMES` | `Item::name`, item registry lookup |
| `item_enum.rs` | `Item`, `ITEMS_BY_REGISTRY_ID`, `REGISTRY_IDS_BY_NAME` | `Item::from_registry_id`, `Item::from_name`, exhaustive item matches |
| `identity_versions.rs` | Per-version defaults, wire counts, ingress and egress arrays | Version-aware default selection and adapter translation |

`STATES` retains the existing `(alphabetical block-name index, property-set index)`
representation. The first index resolves through `block_enum.rs::REGISTRY_IDS_BY_NAME`
to the canonical block-name column. Property pairs and the distinct property-set table are
sorted explicitly. `STATE_BLOCK` and spans use canonical IDs directly.

In `identity_versions.rs`, `v26_2` and, for union scope, `v26_3` each contain
`BLOCK_DEFAULT_STATES: [Option<u32>; ...]`. Ingress columns use `u32`; outgoing mappings
use `Option<u32>`. Unsupported blocks, states, items, and defaults retain `None`, including
identities added after 26.2. No default is selected by taking the lowest state ID.

The provenance manifest records the bundle and census digests, exact input report hashes,
domain counts, and SHA-256 for every Rust file. No timestamp or machine-specific path is
serialized. `--rust-check` requires the complete expected file set and exact bytes, including
the provenance manifest.

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

The files are not drop-in replacements for the active runtime. In particular, the emitter
does not supply the existing unversioned `block_enum.rs::DEFAULT_STATE`, behavior columns,
or default-state marks in the snow-support table. Adoption must choose explicit version
semantics for `Block::default_state` and `StateId::is_default`, populate total authoritative
behavior tables, and integrate state spans with the property tables. The 26.3 adapter's
existing compact-run mapping generator remains independent and active; these staged arrays
do not replace or activate it. All protocol boundaries must translate canonical IDs before
the union can be enabled.

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
python3 crates/lodestone-data/tools/test_identity_staging.py --official-reports
```

`--rust-output` exclusively creates a new directory under the repository's `.cache/` or
`/tmp` (`/private/tmp` on macOS). It rejects existing directories, runtime-source paths,
and symlink escapes. It writes nothing unless explicitly requested. JSON output/check and
Rust output/check destinations are mutually exclusive; no environment variable changes
generation. Interrupted writes may leave an incomplete private directory, which fails the
exact file-set check.

## Dependencies

Python 3's standard library, the [offline identity staging bundle](./data-generation-identities.md),
the [canonical census](./canonical-state-census.md), and cached official reports are required.
No JVM, Cargo build, compiled `Block` or `Item` enum, runtime behavior table, network request,
or version feature is involved. Python tests verify emitted column semantics and output
guards; they do not establish Rust compilation, runtime behavior, or rendered pixels.
