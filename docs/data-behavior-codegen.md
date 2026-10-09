# Versioned Behavior Generation

## What it is

The offline behaviour emitter installs the complete append-only identity and behaviour union for 26.2 and 26.3, preserving every original value and making measured shared-identity differences explicit release-selected overrides.

## How it works

- `crates/lodestone-data/tools/behavior_union.py` validates both official reports through the canonical census and identity bundle. Captured rows join by resource name and sorted properties, never by assuming a wire ID is canonical. ID/name agreement, dense coverage, duplicates, capture trailers and available manifest, report and jar hashes are checked before rendering; a missing latest row fails even when an old row has the same semantic identity.
- `behavior_rust.py` emits 27 Rust modules, 35 path shards and `behavior_manifest.json` (exact input and output digests, identity counts, the shared-change census), deterministically. Primary tables keep all 26.2 values and append complete latest tails. Immutable `behavior_versions` overrides select the measured changes: three block blast rows, two legacy-solidity states and 32 outline states. Default APIs keep base semantics; `GameDataVersion` selects release facts.
- The five latest motion columns are distinct captured facts (generic motion tag, fluid blocking, ocean-floor, motion and no-leaves heightmaps), not aliases of the old motion query, which covers only the original 32,366-state prefix; unsupported latest access stays explicit (fluid-bearing leaves can be in the no-leaves heightmap without being in the solid-leaf tag).
- The original collision prefix is an immutable production-table fixture with its original source and identity-key digests, not a fresh 26.2 capture; fixed sound ranges are retained separately so regeneration never trusts its own output. The 26.3 geometry capture covers cached or empty context only and does not prove neighbour-dependent collision. 26.3 item prototypes, tools and tags are a report/datapack join, not a native capture. Sound identities keep the old report order and append; typed property keys and values keep existing numeric discriminants.
- Consumers are the existing `lodestone_data` lookups, `GameDataVersion` identity and behaviour selection, and release-selected tool and tag access; the numeric movement table feeds selected block movement facts. Generation adds no per-query string join or mutable active release.

## How to change it

- Identity policy: `canonical_census.py` and `identity_staging.py`; readers and validation: `behavior_union.py`; Rust representation: `behavior_rust.py`. Extend `test_behavior_union.py` with independently captured expectations and executed negative controls; never refresh retained-prefix fixtures from union output.
- Install the whole group only while nothing can observe a partial replacement: the emitter renders every file and prepares replacements first, but group replacement is coordinated, not a filesystem transaction. Never raise identity counts without all total tables. The old ignored 26.2-only regeneration tests fail before writing once the union is active (their fixture tests still compare every original row); the one drift test is `canonical_union::committed_behavior_union_matches_inputs`.

## Configuration

```sh
python3 crates/lodestone-data/tools/behavior_union.py                   # print census, no writes
python3 crates/lodestone-data/tools/behavior_union.py --runtime-check   # byte-compare adopted files
python3 crates/lodestone-data/tools/behavior_union.py --runtime-install # replace the validated group
python3 crates/lodestone-data/tools/test_behavior_union.py
python3 crates/lodestone-data/tools/test_identity_staging.py --official-reports
```

`--runtime-install` replaces only that file group under `crates/lodestone-data/src/generated` and fails before replacement on missing inputs. `--base-reports`, `--latest-reports` override directories; capture paths are pinned under `.cache/mc` with no fallback to sibling columns. `--freeze-base-collision` exclusively creates the retained fixture from an unextended base table and refuses an existing one. No environment variable or feature changes output.

## Dependencies

Python 3 standard library, both report caches, pinned native behaviour captures, the report/datapack item-tool join and committed 26.2 fixtures; the identity emitter and census internally. No network, JVM or Rust build; offline controls prove joins, coverage, provenance and determinism, not compilation, gameplay or pixels.
