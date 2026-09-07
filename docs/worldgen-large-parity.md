# Large worldgen parity harness

## What it is

`scripts/worldgen-oracle/LargeParityOracle.java` is the resumable, 251,001-chunk parity oracle for the 501 by 501 grid centred at `(0, 0)`. It freezes one generated reference world first, then records a full SHA-256 digest of each chunk's canonical semantic record; the old v2 raw 16-bit packet fingerprints are explicitly rejected.

## How it works

The reference seed is `42` and the target coordinates are `cx, cz = -250..=250`, for `501 × 501 = 251,001` chunks. A v3 record has a fixed semantic schema: chunk coordinates; heightmaps sorted by numeric type with 256 decoded heights each; 24 sections of resolved global block-state ids and biome ids; block entities sorted by relative position and type with recursively key-sorted NBT; and all 26 sky then block light sections, distinguishing missing, empty, and present 2048-byte arrays. Packet palettes, packet map traversal order, and packet framing do not enter the record.

For every compared coordinate, the Rust gate obtains the centre and its eight adjacent columns through the normal generation dispatcher, then calls the production neighbour-aware initial-chunk encoder. This is required for light too: an emissive block in a neighbouring column can illuminate the centre across its border, while the one-column encoder deliberately has no such input. The gate keeps that light in the canonical record; it never substitutes a synthetic or all-missing light payload. The frozen-world materialization halo supplies the corresponding settled neighbours on the reference side.

The dimension-aware v4 format is additive. It keeps the same 501 by 501 bounds and 32-byte per-chunk digests, but authenticates one of the three dimension identities (`minecraft:overworld`, `minecraft:the_nether`, or `minecraft:the_end`) in header bytes `168..200` as SHA-256 of its resource-location string and uses that dimension's decoded window: overworld `min_y=-64` with 24 sections, Nether/End `min_y=0` with 16 sections. The v5 End format keeps that header shape but has new manifest and record domains. Its export recomputes light from the frozen blocks in an ephemeral copy using the centre batch plus its one-chunk light neighbourhood, then waits for all those columns before encoding. It also elides only a trailing all-15 sky-light layer, because an initial chunk enables its column after queueing supplied layers and a missing top sky layer resolves to the same full value. Mixed sky arrays and all block-light tags remain exact. This avoids treating allocation history as terrain while preserving a real light-value mismatch. V3 and v4 records remain readable and byte-for-byte unchanged; a merge or duplicate-read acceptance rejects different dimensions or semantic versions.

The workflow has two mandatory phases. `--mode materialize` generates the requested rectangle **plus its one-chunk halo** (the complete baseline therefore materializes `-251..=251`, or `253,009` chunks), completes the real chunk-status work, post-processes it, saves it, and writes a tree-digest seal. Before removing each batch's existing exact loading tickets, materialization advances only the scheduler (never resident chunk ticks), fences every batch column's deferred light work, and checks that every resident full chunk reports correct light. Normal eviction and clean epoch shutdown persist that settled state; materialization does not force a whole-cache flush for every batch. It does not add an export-specific loading halo or change tile/order geometry. Materialization is ordered 16 by 16 chunk tiles, with each bounded epoch run in a fresh container/JVM against the same persistent root; the shell drives the epochs automatically. This matters because the server retains point-of-interest section data beyond ticket removal, and an in-process server restart closes shared executors instead of yielding a reusable clean heap. A progress journal records the exact geometry, epoch size, next tile, and an in-flight tile range. It advances only after the prior JVM exited cleanly; an interrupted epoch, a changed range or epoch size, a missing journal, or an already sealed root fails closed rather than being resumed ambiguously. For the complete baseline, omit the range flags so the requested grid is `-250..=250` and its halo is `-251..=251` in both axes. `--mode export` accepts only that sealed world through a read-only mount, copies it into the container's ephemeral server-access directory, and exports semantic digests from the restarted persisted content. The manifest carries the frozen-world digest, schema digest, geometry, full digest width, and SHA-256 payload checksum; merge refuses a shard from a different frozen world. A root sealed before this per-batch light settlement is not an acceptable End baseline and must be regenerated.

This split prevents two independent failure modes. A generated chunk can receive a later neighbour feature write, so the old batch-immediate capture could observe transient state. Separately, semantically identical packets can have different heightmap map order or palette/container framing. Freezing before capture removes the first; canonical records remove the second.

`--packet-out` remains a one-chunk raw packet diagnostic. It is never used for the baseline, but is useful when the Rust comparison identifies a semantic mismatch and needs its existing detailed packet diff.

## How to change it

The Java exporter and Rust comparator must change together whenever a semantic field changes. Bump the schema/domain strings and manifest version rather than reinterpreting existing data. Keep decoded ids in their existing packet-cell order, sort only collections without semantic order (heightmaps, block entities, compound keys), and keep NBT lists in order. For v4/v5, pass the dimension identity through packet decoding and source selection; do not infer it from a column's height because Nether and End intentionally share a window. V5's light normalization is only for an initial chunk's trailing full-sky tail; never apply it to block light, a mixed sky array, or a later light-only update.

The Rust gate in `crates/versions/26.2/tests/large_worldgen_parity.rs` generates a complete 3 by 3 local input, sends it through the neighbour-aware production initial-chunk encoder, then decodes the resulting packet and applies the schema-selected canonical record before comparing the full digest. Its north-neighbour glowstone control proves that the old one-column path has zero block light at the centre border while the neighbour-aware path carries the expected value. The test support reader authenticates the entire manifest before generation. Its v2 refusal is intentional: a short raw hash cannot be converted into a semantic hash.

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

Only after this control passes, materialize the full grid once. Export can then use disjoint resumable shards; every export worker points at the same read-only sealed world. Run the first and second full reads into distinct shard directories, merge each, then use `accept` to make the final baseline. `accept` refuses anything except two byte-identical complete manifests from the same frozen-world identity. `full-parity-worker.sh` divides work into 16-chunk-wide shards, refuses to start without `LODESTONE_ORACLE_FROZEN_WORLD_ROOT`, and uses `LODESTONE_ORACLE_SHARD_DIR` to choose its relative output directory. Legacy overworld shards remain directly below that directory; v4 Nether/End shards add a dimension subdirectory so existing v3 merge globs continue to work. Sequential workers avoid multiplying the ephemeral frozen-world copy; parallel workers are appropriate only when the host has measured enough space for those independent copies.

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

`LODESTONE_ORACLE_BATCH` controls the bounded loading batch in both phases; it does not change the frozen-world contract. `LODESTONE_ORACLE_EPOCH_TILES` controls the clean-JVM materialization epoch in 16 by 16 tiles and defaults to `32` (at most 8,192 nominal chunks before edge clipping). It is durable provenance: keep it unchanged while resuming a materialization. `LODESTONE_ORACLE_WORLD_ROOT` is a writable host directory mounted at `/world` only for materialization. `LODESTONE_ORACLE_FROZEN_WORLD_ROOT` is mounted read-only at `/frozen` for export. The source seal is checked before it is copied into the ephemeral server-access directory.

The manifest tools accept `validate`, `merge`, `accept`, and `selftest`. `accept` is the final baseline gate: it requires two complete byte-identical read-only exports from the same dimension, semantic version, and frozen-world identity. `selftest` covers full-grid merge ordering, duplicate-read acceptance, payload tampering, different-world and different-dimension merge/accept refusal, schema rejection, and v2 rejection. Existing v3 overworld and v4 dimension exports continue to validate and merge byte-for-byte. End exports now select v5 automatically and must be kept separate from v4 shards; accepted v4 artifacts are not regenerated by this migration.

Dimension roots are independent worlds and must never share a materialization directory. Start a Nether or End materialization with an empty writable root (the command resumes clean epochs until its dimension-specific seal appears):

```text
mkdir -p /private/tmp/lodestone-nether-501-seed42
LODESTONE_ORACLE_WORLD_ROOT=/private/tmp/lodestone-nether-501-seed42 \
LODESTONE_ORACLE_DIMENSION=nether \
  bash scripts/worldgen-oracle/large-parity.sh --mode materialize --dimension nether

mkdir -p /private/tmp/lodestone-end-501-seed42
LODESTONE_ORACLE_WORLD_ROOT=/private/tmp/lodestone-end-501-seed42 \
LODESTONE_ORACLE_DIMENSION=end \
  bash scripts/worldgen-oracle/large-parity.sh --mode materialize --dimension end
```

Use `LODESTONE_ORACLE_OUTPUT_ROOT` for shard files so exports do not write under the repository's `/oracle` mount. For example, a Nether worker writes to `/private/tmp/lodestone-nether-shards/baseline-tiles/nether/` while reading the sealed root read-only:

```text
mkdir -p /private/tmp/lodestone-nether-shards
LODESTONE_ORACLE_FROZEN_WORLD_ROOT=/private/tmp/lodestone-nether-501-seed42 \
LODESTONE_ORACLE_OUTPUT_ROOT=/private/tmp/lodestone-nether-shards \
LODESTONE_ORACLE_DIMENSION=nether \
  bash scripts/worldgen-oracle/full-parity-worker.sh 0 4
```

## Dependencies

The exporter uses the locally cached compiled 26.2 server and assets under `.cache/mc/26.2`, through the container runtime wrapper. The Python validator uses only the standard library. The Rust comparator uses the production protocol decoder and a small test-only SHA-256 implementation.
