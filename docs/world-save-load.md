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
| `level.dat` is written | `crates/lodestone-server/tests/level_dat_round_trip.rs`, whose field-set expectation is a real Mojang file's own key list | **yes**, for the schema |
| a world's age accumulates across sessions | same file — an **exact** equality between session two's base and session one's final `Time` | no — a round trip |

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

## Reaching a real session (issue #468)

Everything above landed in #437 and reached **zero players**: the shell opened
every singleplayer session through `open_in_memory_with_mobs`, the
non-persistent constructor. That is this repo's dominant defect class — the
island — one layer above the code, and no server-side gate can see it, because
a server-side test constructs the persistent server itself.

Three things had to change, and only the first is the one the issue named.

### 1. The constructor swap, and the world directory

`crates/lodestone-shell/src/saves.rs` is the shell's new concept of where a
world lives. `Origin::Integrated` carries an `Option<PathBuf>`; `Some` selects
`open_persistent_with_mobs`.

**One implicit world, not a save list.** `saves::default_world_dir()` is
`<data dir>/saves/world`, and every singleplayer session opens it. This is a
product decision, argued in that module's own doc: a save list is what vanilla
does and what the `CreateWorld` screen implies, but `world_select` renders one
hardcoded row with disabled Edit/Delete buttons and pixel gates pinning that
row, so a real list is a feature rather than a wiring fix. The honest cost is
that **"Create New World" cannot create a second world** — it reopens the
existing one, and the typed seed only takes effect on the very first launch.
Nothing deletes a world.

`min_y`/`height` are read off the source via `OverworldChunkSource::min_y`/
`height` rather than written as `(-64, 384)` at the call site, because this
module's own gotcha is that they must match the world the columns came from.

### 2. The shutdown flush, which was a second island

The shell called `IntegratedServer::trigger_shutdown()` — a fire-and-forget
notify — and then dropped the handle, whose `Drop` **aborts** the tick and
serving tasks. `save_now` lives in `shutdown()`, which nothing awaited. So even
with the persistent constructor wired in, every edit since the last autosave
tick would be lost on every quit. `net.rs` now `await`s `shutdown()`, which
joins the serving and tick tasks *before* flushing, so an in-flight edit cannot
be dropped between the last tick and the write. Quitting to the title blocks
until the world is on disk; that is what vanilla's "Saving world" screen is.

### 3. The seed — and it is **not** in `level.dat`

#437's gap list said `level.dat`. That is right for 1.16.5 and **wrong for
26.2**, which is the trap: a 26.2 `level.dat` contains no seed field at all.
Verified by decompressing four real world folders with Python's stdlib `gzip`
and hand-parsing the NBT with `struct.unpack`:

| world | `level.dat` | `DataVersion` | seed inside it? |
|---|---|---|---|
| `.cache/mc/26.2/world` | 513 B | 4903 | **none** |
| `.cache/mc/creative/world` | 517 B | 4903 | **none** |
| `.cache/mc/survival/world` | 515 B | 4903 | **none** |
| `.cache/mc/1.16.5/world` | 2719 B | 2586 | `Data.WorldGenSettings.seed` |

26.2 moved it to **`<world>/data/minecraft/world_gen_settings.dat`**
(`LevelStorageSource.writeWorldGenSettings` → `writeSavedData`, which wraps the
codec output as `{ data: …, DataVersion: … }` and gzips it). Modelled by the new
`lodestone_anvil::world_gen_settings`, and resolved at world open by
`region_source::resolve_world_seed`.

**The stored seed always wins.** A requested seed is a *creation* parameter; an
existing world's own seed is authoritative, or reopening it would regenerate
every unexplored chunk from different terrain — a world self-inconsistent
exactly at the edge of where the player had been. Vanilla's own fallback when
that file is unreadable is `WorldOptions.defaultWithRandomSeed()`, i.e. exactly
this bug, which is why an unreadable-but-present file is an **error** here
rather than a silent re-roll.

### Gates

`crates/lodestone-shell/tests/singleplayer_persistence.rs` drives
`NetClient::open_singleplayer` — the shell path, not `IntegratedServer` — and
mutates by sending a dig over the wire, so nothing can pass by consulting a
world handle the product does not have. Both directions were controlled:
forcing the in-memory constructor fails the block gate ("the session ended
without writing any region file"), and bypassing `resolve_world_seed` fails the
seed gate with the observed profile equal to the *requested* seed's terrain
(all `62`) rather than the stored seed's (`69`/`70`).

## Gaps

Named rather than left to be discovered:

- **There is one world, not a list.** See "One implicit world" above.
- **`level.dat` is written and read, but only the server consumes it.**
  `LevelDatHandle` (`region_source.rs`) creates the file at world open, stamps
  `Time` and `LastPlayed` on every save and at shutdown, and reads the age back
  on reopen so a world's total tick count accumulates instead of restarting.
  The world's **name** comes from the directory, and **spawn** from the
  constructor's `mob_center` at y=64 — both are written, and **nothing outside
  the server reads either yet**: the shell still picks its own spawn and shows
  no world name, so those two fields reach disk and not pixels. That is the
  remaining half, and it lives in `lodestone-shell`.

  Do **not** add weather or a day time to `level.dat` to close this. Neither is
  in that file in 26.2 — they are `data/weather.dat` and
  `data/world_clocks.dat`, and `level.dat`'s `Time` is the world's total age,
  not the sky clock. See [`world-persistence.md`](./world-persistence.md) for
  the measured field list and the two issues that got this wrong.
- **Block entities are persisted** as of
  [#468](https://github.com/matteopolak/lodestone/issues/468) — a furnace comes
  back with its contents, its four burn/cook timers and its banked recipe uses,
  a hopper with its five slots and its transfer cooldown. Three rules make it
  work, and each is load-bearing rather than an optimisation:

  1. **Every chunk holding a block entity is written on every save**, on top of
     the dirty set. A container's contents change through the menu and the tick
     loop, neither of which touches a block, so *nothing marks its chunk dirty*
     and a dirty-only save would persist a container exactly once — at
     placement — and never again. Still mutation-proportional: bounded by the
     number of block entities in the world, not by the 512-column store.
  2. **A chunk that loads block entities is retained in `edits` from that
     moment.** This is the one exception to "only `set_block` populates the edit
     map", and it is required: `save_region` carries a chunk it has no edit
     entry for across as its *original compressed bytes*, so without this,
     smelting into a furnace that was loaded rather than placed this session
     would write the old contents straight back over the new ones with nothing
     reporting an error.
  3. **The release sweep never drops such a chunk**, for the same reason — the
     invariant below (*in `edits` but not in `dirty` implies already on disk*)
     is simply false for a container.

  Restoring is **absent-only**: a position the live registry already holds is
  left alone, because a column can be released and re-read while its furnace has
  been ticking the whole time, and overwriting would rewind the world on every
  cache miss.

  Two kinds are not written under vanilla ids, each for a stated reason.
  **Composter** is namespaced (`lodestone:composter`) because *vanilla has no
  composter block entity at all* — a vanilla composter's level is a block-state
  property and its ready delay is a scheduled block tick, and this crate models
  it as a block entity instead. A furnace's banked recipe counts are namespaced
  too, because they are keyed by this crate's own `"kind:ingredient"` string
  rather than by a vanilla recipe id. Vanilla logs a skip for an unrecognised id
  and drops it, which is the honest outcome; claiming `minecraft:composter`
  would be a claim Mojang may later define differently.
- **Containers this crate does not simulate still lose their contents.** A real
  world is full of chests, barrels, vaults, spawners and decorated pots —
  1,608 of the 1,613 block entities measured across `.cache/mc`'s worlds are
  kinds with no model here. `chunk_nbt::block_entity_from_nbt` drops them, and
  the chunk is then written without them, so **opening and re-saving a vanilla
  world empties its chests**. Closing this needs a *passthrough* that carries an
  unmodelled entry's NBT subtree verbatim, not a model for every block entity.
- **Scheduled ticks reach disk, but nothing schedules into the persisted queues
  yet.** The schema (`chunk_nbt::SavedTick`) and the save/load path
  (`region_source::ScheduledTickHandle`) are done and gated; the remaining step
  is that `tick::run_tick_loop` still keeps its `block_ticks`/`fluid_ticks` as
  **local** `let mut` bindings rather than taking the shared handle, so the
  queues persistence can see are empty in production. See
  [`tick-scheduling.md`](./tick-scheduling.md) for the wiring that closes it.

  Two schema traps live here, both measured against 22,488 real vanilla chunks
  with an independent parser, and both of which a round trip through our own
  writer **cannot** see because the writer and reader would share the mistake:

  | field | trap |
  |---|---|
  | `p` | vanilla's `-3..3` priority **value**, not the ordinal. Our `TickPriority` is declaration-ordered so `Ord` matches Java's `compareTo`, which makes `Normal`'s ordinal `3` and its value `0` — writing the ordinal silently demotes every ordinary tick in the world to `EXTREMELY_LOW` |
  | `t` | a **signed** delay relative to game time at save, negative on 1,584 of the 133,051 entries measured (to `-1046`). Loading is `trigger = game_tick + delay`, saturating at `0`; unsigned arithmetic wraps `0 + (-1000)` to ~18 quintillion |

  The game tick the delays are measured against lives on
  `ScheduledTickHandle`, stored by the tick loop once per tick, deliberately
  **not** re-derived from a second clock — that is
  [#323](https://github.com/matteopolak/lodestone/issues/323)'s bug, where
  `SET_TIME` decoded and really did darken the sky while carrying wall-clock
  elapsed-since-join.
- **Entities are still not persisted.** Mobs and dropped items are reseeded on
  every open; the chunk NBT has no `entities` list and 26.2 keeps them in a
  separate `entities/r.<x>.<z>.mca` anyway.
- **Unload-driven saving is in** (the edit map is no longer unbounded), but the
  release is **deferred to the next save**, never done at eviction. That is
  deliberate: `ChunkStore` evicts on its *miss* path, which is frequently the
  tick thread, and a region write there is the 10.86 s stall all over again.
  So `ChunkSource::unload` is a `HashSet` insert, and
  `WorldSaveHandle::save`'s sweep does the release on the blocking pool. The
  consequence worth knowing: memory is reclaimed at autosave cadence (30 s),
  not instantly, so peak retention is one autosave interval of evictions.

  The invariant that makes dropping lossless is worth repeating before
  changing any of it: *a column in `edits` but not in `dirty` has been written
  to disk*. It holds because `set_block` marks a chunk dirty **while still
  holding the `edits` lock**, and the sweep takes those two locks in the same
  order — so the not-dirty check excludes a mid-flight edit rather than merely
  making one unlikely. Change that ordering and the sweep silently drops
  blocks.

## Configuration

- `world_dir` — passed to `IntegratedServer::open_persistent_with_mobs`. For a
  real session it is `lodestone::saves::default_world_dir()`, i.e.
  `<data dir>/saves/world`, so the `LODESTONE_DATA_DIR` environment variable
  relocates saves along with `options.json` and `servers.json`.
- `autosave` — a `Duration`, same call. The shell passes
  `net::AUTOSAVE_INTERVAL`, 30 s. Far shorter than vanilla's 6000 ticks because
  a save writes only the dirty set, off-thread: a player standing still writes
  nothing. A clean quit does not depend on it, since `shutdown()` flushes.
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
