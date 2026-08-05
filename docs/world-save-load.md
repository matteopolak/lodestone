# World save and load

## What it is

The wiring that makes a Lodestone world survive quitting: `lodestone-server`
loads chunk columns from Anvil region files on disk and writes back everything
the world mutated, through `lodestone-anvil`'s container codec. Issue
[#437](https://github.com/matteopolak/lodestone/issues/437).

Before it, `lodestone-server` had **no save or load path at all** and
`lodestone-anvil` had **zero production callers** — no `Cargo.toml` in the
workspace named it, a declared island per
[#436](https://github.com/matteopolak/lodestone/issues/436). Three pieces close
that gap:

| piece | file | what it owns |
|---|---|---|
| chunk schema | `crates/lodestone-server/src/chunk_nbt.rs` | `ChunkColumn` ↔ the chunk NBT tree (`SerializableChunkData.java`'s territory) |
| persistence layer | `crates/lodestone-server/src/region_source.rs` | disk load, edit retention, the dirty set, region writes |
| lifecycle | `IntegratedServer::open_persistent_with_mobs` | composition, the autosave task, the shutdown flush |

## How it works

### Where it intercepts

The chunk path is three layers deep and persistence goes in the **middle**:

```text
ChunkStore              bounded LRU cache, 512 columns, lossless eviction
  └─ RegionChunkSource     disk load, edit retention, dirty set
       └─ OverworldChunkSource -> the generator
```

Both boundaries are forced, not stylistic:

- **Below `ChunkStore`.** The store's 512-column bound is only safe because
  eviction is *lossless*, and eviction is lossless only because the layer under
  it retains every edit permanently (see [`chunk-store.md`](./chunk-store.md)).
  Put persistence above the cache and dropping a cache entry drops a block.
  Persistence becomes that retaining layer.
- **Above `OverworldChunkSource`.** A column loaded from disk has to beat a
  generated one.

### The trap: `set_block` is deliberately not forwarded

`RegionChunkSource::set_block` does **not** call `self.inner.set_block`.

This looks wrong — forwarding is exactly what `ChunkStore` does — and it is the
single most important line in the module. `OverworldChunkSource::set_block`
seeds its edit map by *generating* the column first. Forwarding would therefore
take a chunk that exists on disk, regenerate fresh worldgen terrain underneath
the player's edit, and discard everything they built, with no error anywhere.
The authoritative edit map lives in `RegionChunkSource` instead, seeded from its
own `column()`, which consults disk first.

### One choke point carries every mutation

Everything that changes the world does so through `ChunkSource::set_block` —
player edits (`server::apply_use_item_on`), random ticks, and since
`ChunkWorld::block_cues`/`pending_grazes` the mob sim's grazing too. Hooking
that one call means `tick.rs`, `mobs.rs` and `server.rs` are untouched, which
also leaves `MobSim`'s immutable `world: &'w ChunkWorld` borrow alone.

### What a save costs

The dirty set, and only the dirty set. This is a correctness-shaped concern
rather than a micro-optimisation: `ChunkStore` holds up to 512 columns at
~192 KiB each, so a save proportional to *residency* would write ~100 MiB every
autosave for a player standing still. Three mutated chunks write three columns.
That is asserted as a **count**, with both wrong hypotheses named in the failure
message, in `tests/world_persistence_round_trip.rs`.

Region files are rewritten whole, because `lodestone_anvil::region` builds a
complete `.mca` in one pass and has no incremental single-chunk update. To keep
that cheap, untouched chunks are re-emitted as **their original compressed
bytes**, never decoded — so saving one chunk in a full region is a sector copy,
not 1,024 NBT round trips.

### Not on the tick thread

The world-open stall (10.86 s → 75.6 ms,
[`world-open-latency.md`](./world-open-latency.md)) was the last large
performance defect in this crate, and a synchronous region write on the tick
thread would be the same class of bug. So `WorldSaveHandle` holds no generator
and no cache — just the edit map, the dirty set and a path — which is what lets
both the autosave timer and the shutdown flush run inside `spawn_blocking`. The
only work on the mutation path itself is a `HashSet` insert.

## Evidence

Stated precisely, because a save/load round trip through our own codec is a
textbook vacuous gate and this repo has the scar (hermetic chunk fixtures built
with our own encoder passed throughout, then a live gate produced 49 ×
"unexpected end of input").

| claim | evidenced by | external? |
|---|---|---|
| we read what vanilla wrote | `tests/chunk_nbt_vanilla_oracle.rs` — 222,208 block columns across 868 chunks of a real 26.2 world agree with vanilla's own `WORLD_SURFACE` heightmap | **yes** |
| vanilla can read what we wrote | `tests/write_path_jvm_oracle.rs` + `scripts/anvil-oracle/` — 24 probes read back by Mojang's own `RegionFile`, `BlockState.CODEC` and `SimpleBitStorage` | **yes** |
| a mutation survives close/reopen | `tests/world_persistence_round_trip.rs` | no — a round trip, and it says so |
| `level.dat` is written | **not implemented** — see Gaps | — |

Every control below was **run and observed**, not described:

| control | observed |
|---|---|
| decode vanilla with dense packing | `PaletteIndexOutOfRange { index: 24, len: 17 }` |
| transpose x/z inside a section | heightmap comparison fails at named coordinates |
| disable save | all three round-trip tests fail; reopen reads `air` where `diamond_block` was placed |
| disable load | **only two** fail — the write-count test still passes, so a red run says which half broke |
| write with dense packing | 16 of 24 JVM probes disagree, by vanilla's adjudication; the other 8 agreed, which is why the fixture forces a 5-bit palette |

## How to change it, and the gotchas

- **Packing is non-spanning.** `64 / bits` entries per long with the high bits
  left as padding, *not* a dense bit stream. This is the one that silently
  corrupts a world: every palette of 16 or fewer entries reads identically under
  both rules, so a fixture built from small palettes proves nothing. Read off a
  real file — a 20-entry palette measures **342** longs, not the 320 a dense
  stream gives. Any new gate here must include a palette wide enough that
  `64 % bits != 0`.
- **Heightmaps are deliberately not written.** Vanilla re-primes any heightmap
  missing from the file (`SerializableChunkData.java` lines 291–302), so
  omitting them is supported, whereas a *wrong* one is trusted and corrupts
  terrain silently. Computing `MOTION_BLOCKING` correctly needs a per-state
  "blocks motion" census this crate does not have. Do not add a partial
  `Heightmaps` compound — `status.heightmapsAfter()` decides per type, so half
  of one is worse than none.
- **`Status` is the only genuinely mandatory field.** `parse` returns `null` on
  an empty `Status` and defaults everything else. We write `minecraft:full`;
  anything less makes a real server re-run worldgen over our terrain.
- **Properties are sorted by name** when a palette entry becomes a canonical
  state string. Not cosmetic — `lodestone_data::block_states::properties` is
  documented sorted and worldgen's strings are sorted, so an unsorted
  reconstruction is `!=` the identical state and every downstream `match` misses.
- **26.2's layout is `<world>/dimensions/minecraft/overworld/region/`**, not the
  pre-1.21 `<world>/region/`. Verified against `.cache/mc/survival/world`.
- **`min_y`/`height` come from the caller**, never from `yPos`: vanilla writes
  light-only sections one past each end of the world, so inferring the extent
  from the section list yields a column 32 rows too tall.
- **`RegionFile` opens its file read-write**, so the JVM oracle cannot be handed
  a read-only mount; `scripts/anvil-oracle/run.sh` copies into the container.
- **`Bootstrap.bootStrap()` wraps `System.out`** in a logger, so oracle output
  arrives as `[09:12:40] [main/INFO]: [STDOUT]: RESULT …`. A `strip_prefix` on
  the marker matches nothing and reads as "the oracle returned no results" —
  indistinguishable from a broken write path.

## Gaps

Named rather than left to be discovered:

- **The shell does not use this yet.** `lodestone-shell/src/net.rs` still calls
  `IntegratedServer::open_in_memory_with_mobs`, the non-persistent constructor,
  so a real singleplayer session still does not save. Switching it to
  `open_persistent_with_mobs` with a world directory is the remaining wire, and
  it lives in a crate this work did not own.
- **`level.dat` is not written or read.** `lodestone_anvil::level_dat` exists and
  models `DataVersion`; nothing here calls it. The consequence is real: the
  world seed is not persisted, so reopening a world with a different seed
  regenerates different terrain outside the saved region files.
- **Block entities, entities and scheduled ticks are not persisted.** The chunk
  NBT is written with empty `block_entities`, `block_ticks` and `fluid_ticks`
  lists. A furnace keeps its position but not its contents across a reopen.
- **No unload-driven save.** Chunks are written on the autosave timer and at
  shutdown, not when evicted from `ChunkStore`. Eviction remains lossless
  because the edit map is unbounded — which means the edit map is now the
  process's real memory bound for a heavily-built world.

## Configuration

- `world_dir` — passed to `IntegratedServer::open_persistent_with_mobs`.
- `autosave` — a `Duration`, same call.
- Chunks are written with `CompressionScheme::Zlib`, vanilla's
  `RegionFileVersion.DEFAULT`.
- `chunk_nbt::DATA_VERSION` is `4903`, read off a real 26.2 world.

## Dependencies

- [`lodestone-anvil`](./world-persistence.md) — the `.mca` container. Added as a
  **non-wasm target dependency**: it is `std::fs`-based and `lodestone-server`
  really does build for `wasm32`, where a browser world has no filesystem. The
  whole `region_source` module is `cfg`-gated off there.
- `lodestone-core` — the NBT tree and codec.
- [`chunk-store.md`](./chunk-store.md) — the cache this sits beneath.
- `.cache/mc/26.2` and the `container` runtime, for the JVM oracle only.
