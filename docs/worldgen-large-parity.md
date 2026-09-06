# Large worldgen parity harness

## What it is

`scripts/worldgen-oracle/LargeParityOracle.java` is the resumable, million-chunk parity oracle for the 1001 by 1001 overworld grid centred at `(0, 0)`. Version 3 freezes one generated reference world first, then records a full SHA-256 digest of each chunk's canonical semantic record; the old v2 raw 16-bit packet fingerprints are explicitly rejected.

## How it works

The reference seed is `42` and the target coordinates are `cx, cz = -500..=500`. A v3 record has a fixed semantic schema: chunk coordinates; heightmaps sorted by numeric type with 256 decoded heights each; 24 sections of resolved global block-state ids and biome ids; block entities sorted by relative position and type with recursively key-sorted NBT; and all 26 sky then block light sections, distinguishing missing, empty, and present 2048-byte arrays. Packet palettes, packet map traversal order, and packet framing do not enter the record.

The workflow has two mandatory phases. `--mode materialize` generates the requested rectangle **plus its one-chunk halo**, completes the real chunk-status work, post-processes it, saves it, and writes a tree-digest seal. It accepts an empty directory only: an interrupted or previously frozen root is never resumed. For the complete baseline, omit the range flags so this covers `-501..=501` in both axes. `--mode export` accepts only that sealed world through a read-only mount, copies it into the container's ephemeral server-access directory, and exports semantic digests from the restarted persisted content. The manifest carries the frozen-world digest, schema digest, geometry, full digest width, and SHA-256 payload checksum; merge refuses a shard from a different frozen world.

This split prevents two independent failure modes. A generated chunk can receive a later neighbour feature write, so the old batch-immediate capture could observe transient state. Separately, semantically identical packets can have different heightmap map order or palette/container framing. Freezing before capture removes the first; canonical records remove the second.

`--packet-out` remains a one-chunk raw packet diagnostic. It is never used for the baseline, but is useful when the Rust comparison identifies a semantic mismatch and needs its existing detailed packet diff.

## How to change it

The Java exporter and Rust comparator must change together whenever a semantic field changes. Bump the schema/domain strings and manifest version rather than reinterpreting existing data. Keep decoded ids in their existing packet-cell order, sort only collections without semantic order (heightmaps, block entities, compound keys), and keep NBT lists in order.

The Rust gate in `crates/versions/26.2/tests/large_worldgen_parity.rs` decodes Lodestone's production chunk packet and applies the same canonical record before comparing the full digest. The test support reader authenticates the entire manifest before generating a local chunk. Its v2 refusal is intentional: a short raw hash cannot be converted into a semantic hash.

Run a small persisted control before any broad job. This proves both the frozen-world seal and duplicate read-only export:

```text
mkdir -p /absolute/path/parity-world
LODESTONE_ORACLE_WORLD_ROOT=/absolute/path/parity-world \
  bash scripts/worldgen-oracle/large-parity.sh --mode materialize --cx -8 7 --cz -8 7
LODESTONE_ORACLE_FROZEN_WORLD_ROOT=/absolute/path/parity-world \
  bash scripts/worldgen-oracle/large-parity.sh --mode export --out /oracle/pilot-a.lwp --cx -8 7 --cz -8 7
LODESTONE_ORACLE_FROZEN_WORLD_ROOT=/absolute/path/parity-world \
  bash scripts/worldgen-oracle/large-parity.sh --mode export --out /oracle/pilot-b.lwp --cx -8 7 --cz -8 7
cmp scripts/worldgen-oracle/pilot-a.lwp scripts/worldgen-oracle/pilot-b.lwp
python3 scripts/worldgen-oracle/large-parity-manifest.py validate scripts/worldgen-oracle/pilot-a.lwp scripts/worldgen-oracle/pilot-b.lwp
```

When changing the semantic schema, also run the cross-language control. It makes Java emit one packet body and its un-hashed canonical record, then requires Rust to decode that same packet into byte-identical record bytes and the manifest digest.

```text
LODESTONE_ORACLE_FROZEN_WORLD_ROOT=/absolute/path/parity-world \
  bash scripts/worldgen-oracle/large-parity.sh --mode export --out /oracle/cross.lwp \
  --packet-out /oracle/cross.packet --record-out /oracle/cross.record --cx 0 0 --cz 0 0
LODESTONE_LARGE_PARITY_CROSS_LANGUAGE_PACKET=/absolute/path/to/cross.packet \
LODESTONE_LARGE_PARITY_CROSS_LANGUAGE_RECORD=/absolute/path/to/cross.record \
LODESTONE_LARGE_PARITY_CROSS_LANGUAGE_MANIFEST=/absolute/path/to/cross.lwp \
  cargo test -p lodestone-v26-2 --test large_worldgen_parity \
  java_and_rust_canonical_records_agree -- --ignored
```

Only after this control passes, materialize the full grid once. Export can then use disjoint resumable shards; every export worker points at the same read-only sealed world. Run the first and second full reads into distinct shard directories, merge each, then use `accept` to make the final baseline. `accept` refuses anything except two byte-identical complete manifests from the same frozen-world identity. `full-parity-worker.sh` divides work into 16-chunk-wide shards, refuses to start without `LODESTONE_ORACLE_FROZEN_WORLD_ROOT`, and uses `LODESTONE_ORACLE_SHARD_DIR` to choose its relative output directory. Sequential workers avoid multiplying the ephemeral frozen-world copy; parallel workers are appropriate only when the host has measured enough space for those independent copies.

```text
LODESTONE_ORACLE_WORLD_ROOT=/absolute/path/parity-world \
  bash scripts/worldgen-oracle/large-parity.sh --mode materialize
for read in baseline-read-a baseline-read-b; do
  for worker in 0 1 2 3; do
    LODESTONE_ORACLE_FROZEN_WORLD_ROOT=/absolute/path/parity-world \
      LODESTONE_ORACLE_SHARD_DIR="$read" \
      bash scripts/worldgen-oracle/full-parity-worker.sh "$worker" 4
  done
done
python3 scripts/worldgen-oracle/large-parity-manifest.py merge --out /absolute/path/full-read-a.lwp /absolute/path/baseline-read-a/shard-*.lwp
python3 scripts/worldgen-oracle/large-parity-manifest.py merge --out /absolute/path/full-read-b.lwp /absolute/path/baseline-read-b/shard-*.lwp
python3 scripts/worldgen-oracle/large-parity-manifest.py accept --out /absolute/path/full-v3.lwp /absolute/path/full-read-a.lwp /absolute/path/full-read-b.lwp
LODESTONE_LARGE_PARITY_MANIFEST=/absolute/path/full-v3.lwp \
  LODESTONE_LARGE_PARITY_REQUIRE_FULL_GRID=1 \
  cargo test -p lodestone-v26-2 --test large_worldgen_parity -- --ignored
```

`LODESTONE_LARGE_PARITY_MAX_CHUNKS` makes the Rust gate compare a bounded prefix. `LODESTONE_LARGE_PARITY_REFERENCE_PACKET` and `LODESTONE_LARGE_PARITY_PACKET_OUT` retain the first raw packet pair only for mismatch diagnosis.

## Configuration

`LODESTONE_ORACLE_BATCH` controls the bounded loading batch in both phases; it does not change the frozen-world contract. `LODESTONE_ORACLE_WORLD_ROOT` is a writable host directory mounted at `/world` only for materialization. `LODESTONE_ORACLE_FROZEN_WORLD_ROOT` is mounted read-only at `/frozen` for export. The source seal is checked before it is copied into the ephemeral server-access directory.

The manifest tools accept `validate`, `merge`, `accept`, and `selftest`. `accept` is the final baseline gate: it requires two complete byte-identical read-only exports. `selftest` covers full-grid merge ordering, duplicate-read acceptance, payload tampering, different-world merge refusal, and v2 rejection.

## Dependencies

The exporter uses the locally cached compiled 26.2 server and assets under `.cache/mc/26.2`, through the container runtime wrapper. The Python validator uses only the standard library. The Rust comparator uses the production protocol decoder and a small test-only SHA-256 implementation.
