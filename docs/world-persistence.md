# World persistence: `lodestone-anvil`

## What it is

`lodestone-anvil` (`crates/lodestone-anvil/`) reads and writes Minecraft's
on-disk world formats: the Anvil region file (`.mca`, issue
[#298](https://github.com/matteopolak/lodestone/issues/298)) and `level.dat`
world metadata (issue [#300](https://github.com/matteopolak/lodestone/issues/300)).
Before this crate, nothing in the workspace could save or load a world at
all — `grep -rln 'RegionFile\|\.mca\b|region_file|Anvil\b'` across every
`.rs` file returned nothing.

## How it works

Two independent, version-free container formats, both riding on the NBT
codec already in `lodestone-core` (`read_named_nbt`/`write_named_nbt` — the
classic *named*-root form with an empty root name, not the nameless
"network NBT" the protocol crates use):

- **Region files** (`src/region.rs`): an 8 KiB header — 1024 big-endian
  `i32` sector locations (`sectorNumber << 8 | sectorCount`) then 1024
  timestamps, indexed by `localX + localZ*32` — followed by 4 KiB-sector-
  addressed chunk payloads. Each payload is a 5-byte header (4-byte length +
  1-byte compression-scheme id) then that many compressed bytes. A chunk
  needing 256+ sectors (~1 MiB compressed) is stored externally in a
  sibling `c.<chunkX>.<chunkZ>.mcc` file instead, with no envelope of its
  own. `RegionFile::parse`/`read_chunk_raw`/`read_chunk_nbt_bytes` read it;
  `build_region`/`build_region_from_nbt` write it. Deliberately generic over
  what NBT tree a chunk holds — it parses no chunk *schema*
  (`SerializableChunkData.java`'s territory in vanilla), only the envelope,
  so the same code should work unchanged for entity-region storage later.
- **`level.dat`** (`src/level_dat.rs`): a single gzip-wrapped named-NBT
  file — unnamed root `Compound` containing a `"Data"` compound. This crate
  models exactly that envelope plus a `DataVersion` accessor into `"Data"`;
  every other `LevelData` field (seed, spawn, gamerules, weather, world
  border, ...) is unmodelled on purpose (see "How to change it").
- **Compression** (`src/compression.rs`): the scheme byte both formats use —
  gzip (id 1), zlib/"deflate" (id 2, the default and the only scheme this
  crate has real-file evidence for), uncompressed (id 3), and LZ4 (id 4).
  `level.dat` is always gzip regardless of this byte (it has no scheme byte
  of its own — `NbtIo.writeCompressed` is hardcoded to `GZIPOutputStream`).
- **LZ4 framing** (`src/lz4_block.rs`): the third-party `net.jpountz.lz4`
  block-stream format the `lz4` region scheme wraps each block in — not
  Minecraft's own format, so it's documented separately from the region
  file's own module doc, with its constants read directly out of the real
  library's class file (see that module's doc for how, since no JVM was
  available in this environment to run `javap`).

## How to change it, and the gotchas

- **No longer an island** (issue
  [#437](https://github.com/matteopolak/lodestone/issues/437), landed).
  This crate had zero production callers — a declared island on the
  standing ledger ([#436](https://github.com/matteopolak/lodestone/issues/436))
  — until `lodestone-server`'s `region_source` became its first. The chunk
  *schema* that #437 had to decide lives in `lodestone-server`'s
  `chunk_nbt`, **not here**, and the separation below still holds.
  See [`world-save-load.md`](./world-save-load.md) for the wiring, and for
  what remains unwired (the shell still opens worlds through the
  non-persistent constructor).
  Keep this crate free of a `lodestone-server` (or `lodestone-world`, or
  any protocol crate) dependency — it is depended *on*, never depends back.
- **The container format and the chunk NBT schema are two different
  problems** (issue #298's own stated trap). Don't grow `region.rs` a
  dependency on chunk internals "for convenience" — that belongs in #437's
  wiring code, operating on the `Nbt` tree this crate hands back.
- **An empty/nonexistent region file and a truncated one are different
  errors, on purpose.** `RegionFile::parse(&[])` succeeds (a legal,
  chunk-less region — matches vanilla treating "file doesn't exist yet"
  this way); `RegionFile::parse` of anything nonzero but shorter than the
  8192-byte header is `Err(Error::TruncatedRegionHeader)`. Getting this
  backwards regresses real vanilla behaviour (an all-zero sector table is
  legal, not corrupt) in one direction, or silently accepts a broken file
  in the other.
- **LZ4 has no real-file evidence.** None of this repo's oracles set
  `region-file-compression: lz4` in `server.properties` (all three leave it
  at the `deflate` default), so unlike the other three schemes, `lz4_block.rs`
  has only ever been checked `decode(encode(x)) == x` against itself. See
  that module's doc for exactly what *is* externally verified (the framing
  constants, read out of the real `lz4-java` jar's class file) versus what
  isn't (an actual `lz4`-compressed byte stream from a real server).
- **`level.dat`'s schema is deliberately thin.** Issue #300 itself says to
  sequence full `LevelData` modelling against whichever issue settles each
  field's in-memory representation first (seed, spawn, gamerules, ...),
  rather than guess a schema now that would need a second pass per
  subsystem landed afterward. Add fields to `LevelDat` as those issues land,
  not preemptively.
- **`build_region` is a single-region primitive.** It does not split a
  mixed-region chunk set across multiple `.mca` files — callers group
  chunks by `chunk_x >> 5, chunk_z >> 5` themselves (`region::region_and_local`
  computes that split for one coordinate at a time).
- **Incremental single-chunk updates to an existing file aren't
  supported yet.** `build_region`/`build_region_from_nbt` always build a
  complete region file from a full chunk set in one pass; there is no API
  for "append/replace one chunk in an existing `.mca` without rebuilding
  it". Whoever picks up #437 will likely want that.

## Configuration

None. `region-file-compression` is a **server** setting
(`server.properties`) that only ever appears here as the compression-scheme
byte on already-produced bytes — this crate doesn't choose it, a caller
does (`CompressionScheme` passed into `build_region`/`build_region_from_nbt`).

## Dependencies

`lodestone-core` (the shared NBT codec) and `flate2` (gzip/zlib, already a
workspace dependency for packet compression) via `[workspace.dependencies]`.
Two dependencies were added directly to `crates/lodestone-anvil/Cargo.toml`
instead, rather than editing the root manifest's dependency table: `lz4_flex`
(raw LZ4 block compression) and `xxhash-rust` (the LZ4 block-stream
checksum). No filesystem framework, no async runtime, no protocol crate —
`cargo tree -p lodestone-anvil` pulls in only those and their own transitive
dependencies.

## Verification

- `crates/lodestone-anvil/src/region.rs`, `src/level_dat.rs`,
  `src/compression.rs`, `src/lz4_block.rs`: unit tests, mostly self-round-trip
  plus a handful of corrupt-input controls (truncated header, declared
  length exceeding its sector, an unknown compression-scheme id, an
  externalized oversized chunk).
- `tests/region_container.rs`: a fuller self-round-trip through an actual
  file on disk, including a negative-region-boundary case and a file with
  two chunks compressed under different schemes.
- `tests/region_real_world.rs`, `tests/region_real_world_26_2.rs`,
  `tests/level_dat_real_world.rs`: the evidence that actually matters —
  reading real `.mca`/`level.dat` files this crate never wrote.
  `#[ignore]`d (they need `.cache/mc/` fixtures this repo doesn't check in);
  run with `cargo test -p lodestone-anvil -- --ignored`. See those files'
  module docs for exactly which real files, and exactly how each expected
  value was independently derived (an external Python `struct`/`zlib`/`gzip`
  parse, not this crate's own code).
  - `region_real_world.rs` reads real `.mca` files from three *older*
    protocol versions (1.8.9, 1.12.2, 1.16.5) — the container format is
    unchanged across all of them and 26.2 alike (it predates all four; only
    the chunk NBT *schema* differs, which this crate doesn't parse).
  - `region_real_world_26_2.rs` is the literal-26.2 case: this repo's own
    `creative` oracle, booted fresh, had three known blocks placed at known
    coordinates over RCON (`setblock`, then `save-all flush`), and this
    crate's `RegionFile` reader recovers exactly those blocks from the real
    files that produced — cross-checked against an independent Python NBT
    parser that walks the post-1.18 `sections`/`block_states` palette
    encoding by hand. Notably, this oracle's overworld region files live at
    `world/dimensions/minecraft/overworld/region/`, not `world/region/`
    directly (unlike the three older versions above) — observed directly
    from the real directory listing, not cited to decompiled source, and
    worth knowing before #437's wiring work goes looking for it.
  - `level_dat_real_world.rs` has real 26.2 evidence too, independent of
    the above: every one of this repo's 26.2 oracle worlds has a real
    `level.dat`, whether or not any chunk was ever saved in it.
