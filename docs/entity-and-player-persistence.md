# Entity and player persistence

## What it is

The wiring that makes mobs, dropped items and the player's own inventory survive
quitting a Lodestone world. Issues
[#302](https://github.com/matteopolak/lodestone/issues/302) (per-player `.dat`),
[#303](https://github.com/matteopolak/lodestone/issues/303) (per-chunk entity
storage) and [#305](https://github.com/matteopolak/lodestone/issues/305) (the
`DataVersion` decision). It sits alongside
[`world-save-load.md`](./world-save-load.md), which covers terrain, block entities
and scheduled ticks — everything *except* the things that move.

Before this, a restart deleted **every mob, every dropped item, and the player's
whole inventory**, with no error anywhere. The world opened, the join succeeded,
the chunks streamed, and the animals were gone.

| piece | file | what it owns |
|---|---|---|
| player container | `crates/lodestone-anvil/src/player_dat.rs` | the gzip `.dat` file, its path, the crash-safe write |
| player schema | `crates/lodestone-server/src/player_data.rs` | which NBT field means what; `PlayerDataStore` |
| entity storage | `crates/lodestone-server/src/entity_storage.rs` | the `entities/` region set, `SavedEntity`, stale clearing |
| sim bridge | `crates/lodestone-server/src/mobs.rs` | `MobSim::saved_entities` / `restore_saved` |
| version gate | `crates/lodestone-anvil/src/lib.rs` | `require_supported_data_version` |
| lifecycle | `IntegratedServer::open_persistent_with_mobs` | the restore, the autosave, the shutdown flush |

## How it works

### Where the files are, and the two paths that are not what you expect

```text
<world>/players/data/<uuid>.dat                             -- NOT playerdata/
<world>/players/data/<uuid>.dat_old                         -- previous save
<world>/dimensions/minecraft/overworld/entities/r.<rx>.<rz>.mca   -- NOT <world>/entities/
```

Both were read off `.cache/mc/survival/world`, a world a real 26.2 server wrote,
rather than off a wiki page. Two traps:

- **`players/data/`, not `playerdata/`.** Every pre-1.21 reference says
  `playerdata`. A reader pointed there finds nothing, reports "this player is
  new", and hands them an empty inventory — silent loss that looks exactly like
  correct first-join behaviour.
- **Entities live in their own region set**, a sibling of `region/`, not in a
  field of the terrain chunk. Since 1.17.

### The entity chunk schema, and the `Position` trap

```text
Position: IntArray[2]     -- [chunkX, chunkZ]
DataVersion: Int
Entities: List<Compound>
```

A *terrain* chunk carries three separate `xPos`/`yPos`/`zPos` ints. An *entity*
chunk carries one two-element `IntArray` and no `yPos`. Code that reaches for
`xPos` here finds nothing and silently files every entity in the world under chunk
`(0, 0)`.

Entity `id` is a **string** resource key, never an ordinal. This repo has already
shipped the ordinal version of that bug: every dropped item arriving as
`minecraft:acacia_boat`.

### Unmodelled fields are carried through, never dropped

Both `SavedEntity` and `PlayerData` keep every field they do not understand and
write it back verbatim (`SavedEntity::extra`, `PlayerData::preserved`). This is the
most important property in either type. A real vanilla mob carries ~30 fields this
server does not model — `Brain`, `attributes`, `memories`, `PersistenceRequired` —
and a player file carries hunger, the ender chest and the recipe book, none of which
this server simulates. A writer that emitted only what it understands
would **delete all of it** on the first save.

The reciprocal is the trap that shows up when a field graduates from preserved to
modelled: emitting our own copy while nothing *reads* it back turns a display bug into
data loss, because the first save writes this session's default over the file's real
value. `PlayerData::experience` is the worked example.

### How stale entity records are cleared, and why by UUID

A save must remove a mob's old record when the mob walks from chunk A into chunk B,
or the next load spawns it twice — and doubling the population per restart is worse
than losing it. Both obvious fixes fail:

| approach | failure |
|---|---|
| rewrite only chunks that now hold entities | A's stale copy survives; the mob duplicates every restart |
| rewrite every chunk in the file | deletes the 2093 entities of a real vanilla world the first time our sim saves |

So `EntityStorage::save` clears by **identity**. Every live entity's UUID goes into
a set; a stored record whose UUID is in that set is one of ours that has moved and
is dropped, and a record with an unknown UUID is preserved byte-for-byte. This is
exact rather than heuristic, and it is why `SavedEntity::uuid` is round-tripped
rather than regenerated on load — a fresh UUID on load would make the next save
unable to recognise its own entity.

### How the store reaches a connection

Through `ChunkSource::world_registries`, which gained a `player_data` field
alongside `block_entities` and `scheduled`. Not a new parameter: `serve_connection_inner`
and `serve_play` are at ~30 parameters between them across **eleven** wrapper call
sites, and the source is already threaded everywhere both need one. Riding the
accessor a persistent source already answers also makes it structurally impossible
for a persistent world to be served by a connection that cannot see its player
files — the island shape #468 was for block entities.

### When each thing is saved

| what | when |
|---|---|
| player `.dat` | on clean disconnect, **and** every 600 vitals ticks (~30 s) |
| entities | on the world autosave interval, and at shutdown |
| entity restore | in the **mob seeding task**, after `MobHandle::reseed` |

The periodic player save is not redundant. The disconnect save is reached on
exactly one of `serve_play`'s exit paths; every `?`, a keep-alive timeout, a task
cancelled at shutdown and a crash all skip it. A player who alt-F4s — the common
case, not the rare one — would otherwise lose the whole session.

The restore is in the seeding task **after** the reseed because
`MobHandle::reseed` replaces the whole `MobSim`. Restoring before it would delete
every saved mob with a completely green tree.

## The `DataVersion` decision (#305)

**An on-disk `DataVersion` that is not exactly 4903 is refused, loudly. There is
no upgrade path and there deliberately is not one.**

Vanilla answers a stale version with `DataFixerUpper`: several hundred
schema-to-schema fixes, one per format change since 2011. This repo writes exactly
one version. The two available behaviours were "read an older world with 26.2's
schema and silently mis-decode whatever moved" or "refuse".

Mis-decoding is not hypothetical and the failure is not cosmetic: re-saving a real
world through a schema mismatch has already, in this repo, erased every cave biome
in it, and a chunk read wrongly is a chunk written back wrongly, destroying the
original. **A world we cannot correctly upgrade must not be half-read.**

A *newer* version is refused too — a world written by a later game has a schema
this build has never seen, and guessing forward is strictly worse than guessing
backward. An absent `DataVersion` is refused: vanilla reads that as "pre-1.9, run
the whole chain", which is precisely the chain we lack.

**Where the check happens matters.** For terrain it is at **world open**
(`refuse_unreadable_world`, called from `RegionChunkSource::new`), not per chunk.
`RegionChunkSource::load` returns `Option`, and its `None` means "never saved",
which `ChunkSource::column` answers by *generating fresh terrain*; that regenerated
column then enters the edit map on the next `set_block` and is written over the
original. The per-chunk position is structurally unable to refuse — it can only
destroy. At open, refusing is total and costs nothing: the constructor returns
`Err`, no task has spawned, and not one byte has been written.

It samples the first chunk of the first region file that has one. A world is
written by one game version, so one chunk answers the question; walking all 89
region files at every open would put a multi-second scan back on the world-open
path [`world-open-latency.md`](./world-open-latency.md) spent an issue removing.

## How to change it, and the gotchas

- **A field is excluded from `extra` only if it actually *decoded*, never by
  name.** This is a bug fix, not a style choice, and the vanilla oracle caught it
  on its first run:

  | field | on `minecraft:item` | on a mob |
  |---|---|---|
  | `Age` | `Short` — ticks alive | **`Int`** — breeding age, negative for a baby |
  | `Health` | **`Short`** — a constant 5 | `Float` — real health |

  The same NBT key means two different things with two different tag types
  depending on the entity's class. A static exclusion list containing `"Age"`
  matched the sheep's `Int`, failed to decode it (the code wants a `Short`), and
  dropped it — **every baby sheep in a loaded world would silently have become an
  adult**, with a clean parse and no error. Same collision shape as `CLAUDE.md`'s
  entity-metadata-index rule, in NBT instead of metadata indices.

- **Adding a modelled player field means adding it to `MODELLED_FIELDS`**, or the
  writer emits the key twice — legal NBT, read back as whichever copy the parser
  hits last.

- **`Health` `0.0` is a dead player**, and a dead player is held on the death
  screen, which sends no chunks: restoring one looks exactly like a total chunk
  blackout with a working join and working keep-alives.
  `PlayerData::spawn_state` exists to make that decision explicit at the call site.

- **`join_pos`, not `spawn.pos`, centres the chunk view.** `serve_connection_inner`
  keeps `spawn.pos` as the *world* spawn (it is what a respawn uses) and derives the
  view centre from the restored position. Centring on world spawn instead would
  stream terrain the restored player cannot see and leave them suspended over
  nothing.

- **`MobSim::next_id` tells you whether the sim has been reseeded** (`1` fresh,
  `1000` after `reseed`). Anything that seeds the sim from outside — the entity
  restore, a future `/summon` racing world open — must check it, or the work lands
  in the sim that is discarded a moment later.

- **No new `Arc<Mutex<..>>`.** `EntityStorage` and `PlayerDataStore` are directory
  paths behind an `Arc` and nothing more; the caller owns the population and hands
  a `Vec` in. `IntegratedServer::mobs` is a third clone of the `MobHandle` that
  already existed.

- **Timing hangs off tick counters, never `Instant::now()`.**
  `PLAYER_SAVE_EVERY_VITALS_TICKS` is a count of an existing 50 ms timer's ticks:
  `lodestone-server` links into a wasm32 browser bundle where `Instant::now()`
  compiles and then panics at runtime under `panic = "abort"` with no log line.

## What is not done yet

Named here rather than left to be rediscovered as a missing mob:

- **Point-of-interest (`poi/`) storage is not implemented.** Deliberate: nothing in
  this codebase produces a POI. There are no villagers with professions, no tracked
  bee nests and no registered bed respawn points feeding one, so a POI store would
  have **zero producers** — the island this repo's first rule forbids. Critically,
  this is *not* silent data loss: we never write `poi/`, so a vanilla world's
  existing POI files are untouched by our saves. The format is recorded below for
  whoever lands the first producer.

  ```text
  DataVersion: Int
  Sections: Compound { "<sectionY>": { Valid: Byte, Records: List<{pos: IntArray[3], type: String, free_tickets: Int?}> } }
  ```

  Note an entity chunk carries `Position` and a POI chunk carries none.

- **Projectiles are not persisted.** `ProjectileMeta` holds a uuid and a type but
  the registry holds no owner, no pickup state and no damage, so writing one would
  persist an object we could not faithfully restore. Vanilla does save them.

- **Hunger and the ender chest are not *modelled*** — this server simulates neither.
  They are **preserved** verbatim in a real player file, so nothing is lost; they
  simply do not change. **Experience no longer belongs on this list**: `XpLevel` /
  `XpP` / `XpTotal` are modelled by `PlayerData::experience` and restored into the live
  session (`docs/experience.md` has the reason the save and the restore had to land
  together). `XpSeed`, the enchanting-roll seed, is still preserved.

- **Only the overworld.** `EntityStorage::new` roots at
  `dimensions/minecraft/overworld/entities`, matching `RegionChunkSource`'s own
  single-dimension scope.

## Configuration

None of its own. The world directory comes from
`IntegratedServer::open_persistent_with_mobs`; the entity save rides that
constructor's `autosave` interval.

## Dependencies

`lodestone-anvil` (region container, gzip NBT, the version gate), `lodestone-core`
(NBT codec), `lodestone-entity` (`ItemLifecycle`), `std::fs`. All target-gated to
non-wasm — a browser world has no filesystem.

## Evidence

| claim | where |
|---|---|
| our decode of vanilla's own entity NBT is correct | `tests/entity_nbt_vanilla_oracle.rs` — 880 chunks, **2093** entities, exact per-species census, all cross-checked against a foreign Python parser sharing no code with this repo |
| re-encoding a real vanilla entity loses no field and changes no tag type | same file, all 2093 |
| a mob and a dropped item survive close/reopen through the production path | `tests/entity_persistence_round_trip.rs`, with a negative control proving a fresh world holds none |
| a moved mob is not duplicated, and a foreign entity is not deleted | same file, two gates that are each other's control |
| a player's inventory, position, health and game mode survive | same file |
| an older `DataVersion` is refused | same file, plus unit gates in `lodestone-anvil` covering older, newer and absent |
