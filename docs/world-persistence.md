# World persistence: saving, loading, and the world's own state

## What it is

Everything that lets a world survive quitting: the on-disk container formats (Anvil region files, `level.dat`, and the world-generation-settings file that holds the seed), the server wiring that loads from and saves to them through the chunk pipeline, the world's persisted scalars (game rules, difficulty, clock), where a player spawns, and the point-of-interest index for beds, workstations and lit portals.

## How it works

### Container format and chunk schema

`lodestone-anvil` is a version-free, dependency-light crate that knows only the container formats: the region file envelope (an 8 KiB header of sector locations and timestamps, then compressed sector-addressed payloads, oversized chunks spilling to a sibling file), the gzip-wrapped named-NBT `level.dat`, and the file where 26.2 keeps the seed: `<world>/data/minecraft/world_gen_settings.dat` (`level.dat` has no seed field in this version, so "the seed isn't persisted" is a trap from checking the wrong file). It parses no chunk schema, so one instance serves region, entity-region and POI region files, and a browser build can depend on `lodestone-server` without the disk half (`lodestone-anvil` is a non-wasm dependency).

`lodestone-server`'s chunk-NBT module owns the `ChunkColumn` to NBT mapping. `chunk_nbt::column_from_nbt` also reads the `structures` compound back: each start's id, origin chunk, reference count and children (`id`, `BB`, `O`, `GD`, `Template`) and each `References` entry; the start's box (union of children) and terrain adjustment (the bundled structure's) are rebuilt, and an `INVALID` start is skipped. Heightmaps are never read: every loaded column derives its client maps and retained `MOTION_BLOCKING` from blocks, as a real server does. A column without either would be saved without them, and the native save refuses a column lacking `MOTION_BLOCKING`, failing the whole dimension. `tests/chunk_nbt_vanilla_oracle.rs` (ignored; needs `.cache/mc/survival/world`) checks the derived map equals the stored one in all 189,696 block columns of 741 full chunks, and the structures equal the file's 7 starts and 209 reference entries.

### The generator override

`world_gen_settings.dat` also holds an optional `dimensions.minecraft:overworld.generator` compound recording a non-default world type so reopening regenerates unexplored chunks the same way. The create-world screen writes it (`WorldGenSettings::with_overworld_flat_generator`/`with_overworld_fixed_biome_generator`) and `WorldGenSettings::overworld_generator()` reads it back as `Flat { layers, biome, features, lakes }`, `FixedBiome { biome }` or `Other` (Normal, Large Biomes, Amplified: reconstructed from the seed alone). `lodestone_server::worldgen_data::overworld_chunk_source_override` turns that into the real `ChunkSource` (`Ok(None)` when nothing overrides).

The singleplayer/LAN path (`net.rs`, `Origin::Integrated`) calls it first whenever a world directory exists and falls back to `preset_chunk_source`'s bundled default only on `Ok(None)`, so a customized world generates as chosen from first play. Native only: the browser has no world directory and always takes the default.

### The persistence layer in the source stack

It sits below the chunk cache and above the generator, so cache eviction never loses an edit and a disk-loaded column always beats a freshly generated one. Its block-set deliberately does not forward to the generator's edit tracking (that map is seeded by generating the column fresh, so forwarding would regenerate and discard a disk edit silently). Every server mutation funnels through this call, so hooking persistence changed neither the tick loop nor mob simulation.

- `RegionChunkSource` keeps the typed dimension chosen at open and returns it via `ChunkSource::dimension`, so source-aware encoders pick the right sky and block-light representation after a disk load; an unlabelled in-memory generator returns `None`. This is persistence seam forwarding, not inference, so Nether and End sources keep their wire rules after eviction.
- For End `CentreSettled` snapshots, reopen repairs an old-save gap: a section with persisted sky storage but no block-light array gets an explicit zero block layer. It is dimension- and lifecycle-gated and touches no dependency snapshots, generated columns or non-End worlds.
- A save writes only the dirty set; untouched chunks in a rewritten region file are re-emitted as original compressed bytes (the format has no incremental update and rewrites the file in one pass). Only complete resident generation snapshots are retained for saving, even in a mixed batch of complete and shaped neighbours; a shaped dependency stays in the generation cache and is never a terminal disk edit, and tick-side block changes wait for a complete column.
- An evicted column's unload is a bounded coordinate-scoped token in the `RegionChunkSource` ledger. The world owner captures it in a single-use `WorldSaveJob` before dispatching a blocking writer; the job partitions its deterministic dirty snapshot by region-file owner and runs at most two rewrites concurrently. Results are consumed in canonical owner order: failed owners requeue and no token is acknowledged until all selected owners succeed. The authoritative edit stays retained on failure, and a duplicate, superseded or cross-coordinate token cannot release it. Neither generation nor saving runs on the tick thread (both use a blocking pool).

### Scalars: game rules, difficulty, clock

One shared persistable store holds game rules, difficulty and the world clock behind a cloneable handle given to the connection loop, tick loop and persister. A value that is stored and broadcast with no real reader at its decision point is as absent as unimplemented, so every accessor needs a named production reader: game rules gate natural ticks, drops, spawns and mob griefing; difficulty gates peaceful-mob eviction and spawning, the starvation floor and fire-spread odds; the clock advances game time always and displayed day/night time only when the rule allows. Persistence reuses the reference `level.dat` field names (readable by a real client); every game rule is stored as a string regardless of type.

### World spawn

A fresh world uses the fixed outward spiral from the origin (with the climate-targeted centre described in [`worldgen.md`](./worldgen.md)), testing each column top-down for a standable surface. Standability is not "is solid": surface cover (short grass, flowers, snow layers) has no collision and would fail it, while some walkable-looking things are legitimately solid (a treetop is a real spawn outcome). A candidate is accepted only when the whole 0.6x1.8 body has no collision or fluid overlap, and the player is placed at the block's horizontal centre. If the whole search area is unsuitable (all ocean), the preferred height is a couple of blocks above sea level when clear, else the first clear height above, never underground or inside bedrock. A horizon-only water classification is a negative hint for fully submerged candidates; unknown or mixed candidates run the full predicate, so the hint cannot change the result. A per-player bed respawn point is stored separately and falls back to world spawn if the bed is gone.

### Player data

`PlayerDataStore` keeps one gzip named-NBT file per player under `players/data`. `Inventory` is one compound per occupied native slot: `Slot` byte plus the shared item form from [`item-save-format.md`](./item-save-format.md). A stack with a component lacking a saved form fails the save before the atomic file replacement, so the previous save remains.

### Points of interest

A third region-file set, `poi/` per dimension, indexes workstations, beds, bells and lit nether portals, each with a maximum claim count and occupancy; an absent claim-count field on disk means no claims remain, not never claimed. A POI is a fixed position, so saving needs only the caller's full state for a chunk, with no clear-old-copy pass. The one real consumer is the nether-portal index; without its persistence a portal lit earlier vanished on restart and a return trip built a duplicate. Restoring scans every POI file in the world, because a portal can exist anywhere the player has walked.

### Save parity against a real server

A live test hands a world to a real server in a container, lets it load and optionally save, and compares both directions (our writer to its reader and vice versa). Byte comparison cannot pass (wall-clock and tick fields advance, chunk payloads recompress differently, NBT field order is not identity). The assertion is semantic identity after structural NBT comparison, decoding packed fields to individual cells since valid packings differ. A narrowly scoped allowlist names every field a real server may change and why; nothing touching block states, positions or structures is on it, and a control asserts no allowed pattern can match such a field. This gate found defects no internal round trip could: two string spellings for one fluid state, and a save path that flattened 3-D biome data to one value per column and dropped structure references.

## How to change it

- New persisted scalar or rule: add its typed accessor, forward it through the shared handle, and find its real decision point; an accessor with no reader is an island.
- Check whether 26.2 keeps a field in `level.dat` before adding it there: weather, the day/night clock, the world border and the game-rule table are each their own save file in this version.
- A new persisted block-entity or POI kind: keep unrecognized on-disk data as an unmodified passthrough rather than dropping it, or a re-saved vanilla world comes back with emptied chests.
- Item persistence: change `item_nbt` per [`item-save-format.md`](./item-save-format.md); a component's saved shape comes from a capture of the reference server, never our own reader.
- Verify any on-disk schema change against a real file both ways, with an independently written parser for expected values.

Gotchas:
- A stored seed always wins over a requested one on reopen (regenerating from a different seed makes the world inconsistent at the edge of explored area).
- Packed index arrays are non-spanning (fixed entries per word, leftover high bits as padding); a test of only small palettes cannot tell this from dense packing until the entry width does not divide the word size.
- A missing heightmap on disk is fine (clients recompute); a wrong one is trusted and corrupts the client's view, so leave an unmodelled kind absent.
- World-metadata on-disk types are surprising (numeric-looking values as strings, a delta as a signed quantity, a priority as its numeric value rather than an ordinal); verify against a real written file, not a sibling field.
- Persistence for a subsystem that also runs in the browser stays behind its own native-only gate even when a shared caller must compile there.

## Configuration

- Each world is its own directory chosen by the world-select flow.
- The autosave interval is a small constant, far shorter than the reference default, since a save writes only the dirty set; shutdown always flushes.
- Chunks use the reference default compression.

## Dependencies

`lodestone-anvil` (container formats, native-only), `lodestone-core` (NBT tree and codec), the chunk store this sits beneath ([`chunk-lifecycle.md`](./chunk-lifecycle.md)), and a real server in a container for the parity gate only.
