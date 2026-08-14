# Point-of-interest storage

## What it is

The reader/writer for vanilla's third region-file set: `poi/`, a per-section
index of workstations, beds, bells and lit nether portals —
`PoiRecord`/`PoiSection`/`PoiManager` in vanilla. It is the second half of the
persistence work [`entity-and-player-persistence.md`](./entity-and-player-persistence.md)
covers; that doc's own "what is not done yet" section used to list this as absent.

Before this landed, `poi/` was read and written nowhere in the tree — grepping
for `PoiRecord`/`PoiSection`/`PoiManager`/`"poi"`/`/poi/` returned one hit, a doc
comment in `portal.rs` noting that vanilla indexes nether portals through
`PoiManager`.

| piece | file | what it owns |
|---|---|---|
| container | `crates/lodestone-anvil/src/region.rs` | the same generic region-file reader/writer `entity_storage.rs` and terrain use — a third *instance*, not new container code |
| POI schema | `crates/lodestone-server/src/poi_storage.rs` | `PoiRecord`, `PoiSection`, `PoiChunk`, `PoiStorage`, `Occupancy` |
| the one real consumer | `crates/lodestone-server/src/portal.rs` | `poi_records_for_index` / `restore_index_from_poi`, converting `PortalIndex` (vanilla's `PoiManager` stand-in for nether-portal lookup) to and from this module's records |
| version gate | `crates/lodestone-anvil/src/lib.rs` | `require_supported_data_version`, same as every other region set |

## How it works

### Where the files are, and the schema that agrees with neither sibling

```text
<world>/dimensions/minecraft/overworld/poi/r.<rx>.<rz>.mca
<world>/dimensions/minecraft/the_nether/poi/r.<rx>.<rz>.mca
```

Both dimensions, unlike `entity_storage.rs` and `region_source.rs`, which are
overworld-only by established scope — a lit nether portal is a POI in *both*
dimensions, and `crate::portal::PortalIndex` already tracks both. The
subdirectory name is derived from `Dimension::key`, not hand-matched a second
time.

Read off `.cache/mc/survival/world`'s own `poi/` directories (a real 26.2
server's output) rather than off a wiki page:

```text
DataVersion: Int
Sections: Compound { "<sectionY>": { Valid: Byte, Records: List<{
    pos: IntArray[3],   -- absolute block [x, y, z], NOT section-relative
    type: String,       -- e.g. "minecraft:nether_portal"
    free_tickets: Int?  -- OMITTED, not zero, means every ticket is claimed
}> } }
```

A POI chunk carries **no `Position` field of any kind** — unlike a terrain
chunk's `xPos`/`zPos` and an entity chunk's two-element `Position` IntArray.
`SectionStorage.java` never writes one; the chunk's coordinate is carried only
by its slot in the region container.

### `free_tickets`: absence means zero, not "unclaimed"

Mojang's codec (`Codec.INT.optionalFieldOf("free_tickets", 0)`) omits the field
on encode whenever it equals its declared default of `0`. Read casually, an
absent field looks like "nobody has touched this yet"; it actually means "no
tickets remain". The oracle world confirms this directly: every
`minecraft:bee_nest`/`minecraft:nether_portal` record (both registered with
`maxTickets 0`) omits the field, and three `minecraft:meeting` (bell,
`maxTickets 32`) records carry an explicit `28` or `29` — a real village's bell
partway claimed. A decoder that reads "absent" as "unclaimed" would report a
bell villagers have already claimed several times over as having all 32
tickets free.

### Occupancy — the property a POI store exists to answer

Every POI type has a maximum simultaneous claim count (`max_tickets`,
transcribed from `PoiTypes.bootstrap`). `PoiRecord::has_space` /
`PoiRecord::is_occupied` mirror vanilla's `PoiManager.Occupancy.HAS_SPACE` /
`IS_OCCUPIED`. **A query that only checks "was a POI found" cannot distinguish
a store that honours claims from one that hands out an all-tickets-taken
record anyway** — `poi_storage.rs`'s own unit test
`an_occupied_poi_is_excluded_from_a_has_space_query` exists because a
found-only assertion would pass under either implementation.

### No identity-clearing pass, unlike entities

`entity_storage.rs`'s hardest problem is a mob *moving* between chunks, solved
by tracking every live UUID so a save can clear a stale copy out of wherever
the mob used to be. A point of interest is a fixed block position — its `pos`
*is* its identity, and it does not move between chunks. `PoiStorage::save`
therefore takes the caller's **complete** state for every chunk it names, and
leaves every chunk it does not name untouched on disk. Simpler by
construction, not by omission.

### The one real consumer today, and the one wire still missing

`crate::portal::PortalIndex` already exists as an **in-memory** stand-in for
`PoiManager`'s nether-portal lookup — its own doc names "not persisted" as
"the one real gap": a portal lit in an earlier session vanishes from a fresh
index, so the first return trip after a restart falls back to a bounded local
scan and, beyond that scan's 16-block radius, builds a duplicate portal beside
the original.

`poi_records_for_index` / `restore_index_from_poi` (both in `portal.rs`)
convert a `PortalIndex`'s cells to and from this module's `PoiRecord`s, and
`tests/poi_persistence_round_trip.rs` proves the conversion survives a real
save/load through `PoiStorage` — not an in-memory shortcut. What is **not**
wired yet is calling those two functions from world open and shutdown, which
lives beside `entity_storage.rs`'s own wiring, in `IntegratedServer`'s
constructor and autosave task. That is a two-line follow-up, not a redesign.

Nothing else in this codebase produces or consumes a POI record yet: no
villager professions, no tracked bee nests, no registered bed respawn points.
Building those consumers is explicitly out of this module's scope — the same
scope note issue #303 always carried for this half.

## How to change it, and the gotchas

- **`PoiSection::add` mirrors vanilla's three-way branch exactly**: nothing at
  a position inserts; the same type already there is a no-op; a *different*
  type already there overwrites (vanilla logs a "POI data mismatch" and still
  overwrites — this port skips the log, since nothing here consumes game-log
  output).
- **`PoiSection::insert_record` is not `add`.** `add` always resets a record to
  its type's full ticket count, matching vanilla's public constructor for a
  freshly *discovered* POI. A caller that already knows a record's exact state
  — a conversion from a live index, or a reload — wants `insert_record`, which
  keeps `free_tickets` as given. Using `add` where `insert_record` belongs
  silently un-claims every ticket on every reload.
- **`max_tickets` is looked up by `ResourceKey::path()`, not the full key.**
  Every vanilla POI type is `minecraft:*`, so this is deliberate, not a
  shortcut that will bite a modded type — an unrecognised type answers `0`
  tickets, the conservative direction (never claimable) rather than handing
  out claims a real server would refuse.
- **A `Valid` field's default differs by direction.** `PoiSection::new()`
  (in-memory, freshly discovered) starts `valid: true`; a section decoded from
  a tree with no `Valid` key reads `false` (`Codec.BOOL.lenientOptionalFieldOf`'s
  own default). Nothing here currently re-derives POI from a block scan the
  way `PoiManager.checkConsistencyWithBlocks` would, so `valid` is carried
  through rather than acted on.
- **Two dimensions.** Forgetting to open a second `PoiStorage` for the Nether
  (a single overworld-only store, copy-pasted from `entity_storage.rs`'s
  pattern) silently drops every Nether-side portal POI.

## Configuration

None of its own. `PoiStorage::new` takes the world directory and a
`Dimension`, mirroring `EntityStorage::new`'s signature shape.

## Dependencies

`lodestone-anvil::region` (the shared region-file container), `lodestone-core`
(NBT), `lodestone-model` (`BlockPos`, `ResourceKey`), `std::fs`. Native only —
gated `#[cfg(not(target_arch = "wasm32"))]` in `lib.rs`, same as
`entity_storage`.

## Evidence

| claim | where |
|---|---|
| our decode of vanilla's own POI NBT is correct | `tests/poi_vanilla_oracle.rs` — 124 chunks, 150 sections, **210** records in the overworld set, exact per-type census, cross-checked against a foreign Python parser sharing no code with this repo |
| the Nether's own `poi/` set decodes and is exactly six unclaimed portal cells | same file |
| `free_tickets`'s absent-means-zero semantics match vanilla's own bytes, including a partially-claimed bell (`28`/`29` of 32) | same file |
| a POI chunk round-trips through the real save path, and an unnamed chunk survives a save that does not mention it | `poi_storage.rs`'s own unit tests |
| an occupied POI is excluded from an availability query, with a control proving the exclusion is the occupancy filter's doing | `poi_storage.rs`, `an_occupied_poi_is_excluded_from_a_has_space_query` |
| `PortalIndex` round-trips through `PoiStorage` across both dimensions with no cross-contamination | `tests/poi_persistence_round_trip.rs` |
| an older `DataVersion` is refused | `poi_storage.rs`'s own unit tests |
