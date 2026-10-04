# Versioned Behavior Generation

## What it is

The offline behavior emitter installs the complete append-only identity and behavior union
for 26.2 and 26.3. It preserves every original identity and behavior value while making
measured shared-identity differences explicit release-selected overrides.

## How it works

`crates/lodestone-data/tools/behavior_union.py` validates both official reports through the
canonical census and identity bundle. Captured state rows join by resource name and sorted
properties, not by assuming that a wire ID is canonical. Report ID/name agreement, dense
coverage, duplicate rejection, capture trailers, and available manifest/report/jar hashes
are checked before any output is rendered. Missing latest rows fail even when an old row
has the same semantic identity.

`behavior_rust.py` emits 27 Rust modules, 35 path shards, and `behavior_manifest.json`.
The manifest records exact input and output digests, identity counts, and the shared-change
census. Both sources and outputs are deterministic. The primary tables retain all 26.2
values and append complete latest-release tails. Immutable `behavior_versions` overrides
select the measured changes: three block blast rows,
two legacy-solidity states, and 32 outline states. Existing default APIs retain their base
semantics; `GameDataVersion` selects the release-specific facts.

The five latest motion columns are distinct captured facts: generic motion tag, fluid
blocking, ocean-floor heightmap, motion heightmap, and no-leaves heightmap. They are not
aliases for the old motion query. That old query has only its original 32,366-state prefix;
unsupported latest access must remain explicit. In particular, fluid-bearing leaves can
belong to a no-leaves heightmap despite not belonging to its solid-leaf exclusion tag.

The original collision prefix is an immutable retained production-table fixture with its
original source digest and identity-key digest. It is not a fresh 26.2 capture. Existing
fixed sound ranges are also retained separately from generated outputs, so regeneration
never trusts its own output as an input. The 26.3 geometry capture covers cached or empty
context only; its total rows do not prove neighbour-dependent collision or lifecycle rules.
Item prototypes, tools, and tags for 26.3 are an official report/datapack join, not a native
behavior capture. Sound identities preserve the old report order and append new identities.
Typed property keys and values likewise preserve existing numeric discriminants.

Production consumers are the existing `lodestone_data` lookups, `GameDataVersion` identity
and behavior selection, and release-selected tool/tag access. The numeric movement table
feeds selected block movement facts. No per-query semantic string join or mutable active
release is introduced by generation.

## How to change it

Keep identity policy in `canonical_census.py` and `identity_staging.py`, behavior readers
and validation in `behavior_union.py`, and Rust representation in `behavior_rust.py`.
Extend `test_behavior_union.py` with independently captured expected values and executed
negative controls. Never refresh the retained-prefix fixtures from union output.

Install the entire group only while no compiler or other table writer can observe a partial
replacement. The emitter renders every file first and prepares replacements before changing
runtime source; replacement of the complete group is coordinated, not a filesystem-wide
transaction. Raising identity counts without all total tables is forbidden. The old ignored
26.2-only Rust regeneration tests fail before writes once the union is active. Their ordinary
fixture tests still compare every original row. The single union drift test is
`canonical_union::committed_behavior_union_matches_inputs`.

## Configuration

From the repository root:

```sh
python3 crates/lodestone-data/tools/behavior_union.py
python3 crates/lodestone-data/tools/behavior_union.py --runtime-check
python3 crates/lodestone-data/tools/behavior_union.py --runtime-install
python3 crates/lodestone-data/tools/test_behavior_union.py
python3 crates/lodestone-data/tools/test_identity_staging.py --official-reports
```

The default command prints the semantic census without writing. `--runtime-check` compares
every adopted file byte-for-byte. `--runtime-install` replaces only that exact validated file
group under `crates/lodestone-data/src/generated`; missing inputs fail before replacement.
`--base-reports` and `--latest-reports` override report directories. Capture locations are
pinned in the reader under `.cache/mc`; there is no fallback to sibling behavior columns.
`--freeze-base-collision` exclusively creates the retained fixture from an unextended base
table and refuses an existing fixture. No environment variable or Cargo feature changes
the union output.

## Dependencies

Python 3's standard library, both official report caches, pinned native behavior captures,
the report/datapack item-tool join, and committed 26.2 fixtures are required. The identity
emitter and canonical census are internal dependencies. This workflow performs no network
request, JVM launch, or Rust build. Offline controls establish joins, coverage, provenance,
and deterministic output; they do not establish compilation, live gameplay, or rendered
pixels.
