# Blocks: placement, breaking, outlines, sound, entities and persistence

## What it is

Everything about a block once it exists: placement (server rule table plus client prediction), break timing, selection and interaction shapes, block sounds, simulated block-entity types, bone meal, and entity/player persistence. Drops and loot tables live in [Mining and drops](./mining-and-drops.md).

## How it works

### Server placement (`crates/lodestone-server/src/block_placement.rs`)

`placement(block, ctx, block_at)` returns a state string plus extra owned cells (door upper half, bed head, chest partner). Stairs and doors face with the player, furnaces and chests face at the player, shulker boxes and amethyst take the clicked face.

- Family comes from a cached block-to-`Shape` census (which properties a block's states carry), so new blocks reach the right arm unedited. Name lists (`FACING_IS_LOOK`, `FACING_IS_CLICKED_FACE`) exist only where the census cannot separate two same-shaped families (ladder vs lectern). A wall click is a different block (`torch` to `wall_torch`).
- Never compute a state id here: return a property-named string and let `resolve_state_id` resolve it against the registered default (a linear scan of about 32k entries; memoize for whole-column encodes).
- Sneak-right-clicking a menu block with a block item places beside it and skips chest pairing.
- Every owned cell is evaluated before mutating (replaceable, supported, inside build height), so multi-cell placement is atomic. Waterloggable states read source water per position. A rejected attempt still sends block updates for every attempted cell, clearing client prediction.
- A bare block name writes the registered default state, never the lowest id (wrong for 661 of 797 multi-state blocks: the lowest chest is waterlogged, the lowest slab a top slab). Held item resolves through `block_items::block_placed_by` (about 1,537 items); real collision boxes are tested against the placer, so empty-collision states never obstruct.

### Client prediction

On a live server the shell writes the placed block and its block entity immediately (`world.set_block` then `world.sync_block_entity`, the server confirmation's order). `PlacementFacts` are read up front because the ECS write guard must not nest a re-entrant `chunks -> World` read. The server sends `BLOCK_UPDATE` for the clicked cell, neighbour and every attempted partner after each `use_item_on`, so a misprediction self-corrects in one round trip; classification may err toward not predicting, never toward predicting wrong.

`state_for_placement` needs a value for every property, from geometry, explicit defaults, per-block overrides and `NON_GEOMETRIC_DEFAULTS` (60 of 93 property names take exactly one value across all 1,196 blocks). Measured: 721 blocks resolve, 453 decline.

### Break timing

`VersionAdapter::block_hardness` and `tool_mining` meet in `lodestone-game`'s `mining` module; `Sim::drive_mining` folds crosshair and held item into `BreakInputs`. Two traps:

- `BlockHardness::requires_correct_tool` describes the block; `BreakInputs::correct_tool` describes held item versus block and selects the 30 (correct) or 100 (wrong) divider. Bare-handed they are opposites; feeding one into the other breaks bare-hand stone in 45 ticks instead of the server-confirmed 151. `ToolMining::correct_tool` is already folded.
- `submerged` reads the raw `fluid_state.eye_in_water` flag, not `FluidState::under_water()` (whole body; drives fog).

An unknown state or version-free build refuses to dig. The v26-2 census covers all 32,366 states.

### Outline and interaction shapes

`lodestone_data::outline_shapes::{outline_boxes, interaction_boxes}` take a validated `block_states::StateId` and return static box slices (empty means none). Validate raw ids with `StateId::new` at chunk and adapter boundaries.

- Outline is a third shape beside collision and fluid presence: 50.9% of states differ from collision, only 3,328 are full cubes (cobweb: no collision, full outline; kelp: water, real outline).
- The interaction shape only refines the hit face. The drawn box and the pick ray must both read this table; `raycast` takes the hit face from the box, not the DDA cell boundary.
- `OutlineRenderer` expands each edge to six vertices of screen-space quad, minimum width `max(2.5, window_width/1920*2.5)` logical pixels (`LineList` rasterises one pixel and looks dim).
- `mobs::block_state_id` returns `Option<StateId>` (plug-in names stay `None`); call `raw()` only at numeric boundaries. `StateId::canonical_state` rebuilds the `name[key=value,...]` spelling.

### Block sound types

`lodestone_data::sound_types` is the per-state break/step/place/hit/fall census (126 distinct types over 32,366 states, a 126-entry table plus a per-state `u8`). A block-break level event carries only a state id, so the client needs it. Gotchas: 127 types are declared, 126 reachable; `IRON` differs from `METAL` (pitch 1.5: gold, diamond, emerald, rails, hoppers); air has a sound type, so guard on `!is_air`; `minecraft:decorated_pot` is keyed by state (cracked vs intact).

### Support, consumption and item use

- A block whose support cell becomes air or fluid pops off and drops (`crate::block_support`, a generated 291-row table; hand-typing lost 18 rows and invented 8). `server::collapse_unsupported` is a breadth-first cascade from the broken cell's neighbours, capped at 64; "support lost" is approximated as went-to-air-or-fluid, which fails safe; `lily_pad` and `frogspawn` are excluded; a creative self-break skips drops, a cascaded one always drops.
- Placing consumes the item. Mid-air right-click tries consumable (eat), then equippable (swap); shield-raise and kinetic weapons are not modelled. Eating ends on the server clock via a per-tick arm. An equip swap of a stack of at most 1 swaps the whole stack; larger stacks equip one and send the old piece to inventory or floor.
- `PLAYER_ACTION` and `USE_ITEM_ON` track one `PendingBreak` per connection: `StartDestroy` prices the dig and breaks at once if progress reaches 1.0; `AbortDestroy` clears it only for the same position; `StopDestroy` breaks if progress suffices, else defers. A same-tick Start/Stop pair (always, on an integrated server) never clears the 0.7 threshold, so refusing it would make non-instant blocks unbreakable.
- `Terrain263ChunkSource` regenerates unedited columns per request and retains one only once `set_block` touches it, so memory scales with edits.

### Block entities

`block_entity_types::BlockEntityType` is a separate registry domain from state ids; the census maps a validated `StateId` to `Option<BlockEntityType>`. Generated columns skip the state-write hook, so the Overworld source calls `ChunkColumn::populate_missing_block_entity_states` after structure sidecars attach, adding empty `Nbt::End` records for state-only types (e.g. `minecraft:potent_sulfur`; plain `minecraft:sulfur` is the negative control) without overwriting richer records. Callers pass column coordinates because the dense field stores local X/Z; lifecycle feature writes run the same repair.

Four pure tick-driven state machines (`composter`, `furnace`, `hopper`, `brewing` under `crates/lodestone-server/src/`) each have `tick(&mut self)` returning what changed. `BlockEntityRegistry` (`HashMap<BlockPos, BlockEntity>`) advances on the 20 Hz loop: it snapshots a chunk-owner plan, advances owners serially, and hands furnace lit flips to the world writer.

- Placement honours the held item through `block_entity_for_item`. Right-clicking a furnace or hopper opens its screen; `sync_open_container` diffs slots every 50 ms.
- A composter has no menu: `apply_composter_use` rolls a compostable item against its chance and consumes it even on failure; at fill level 8 any click extracts bone meal. It saves as `lodestone:composter`.
- `brewing::Bottle` holds a validated `lodestone_data::potion::PotionId`, saved as `potion_contents` (legacy `lodestone:potions` fallback) and revalidated on load.
- Each type's `restore` takes every field at once, so a new field without a schema update fails to compile.

Open gaps: brewing `Bottle` slots are not `ItemStack`s so the menu cannot open; non-zero windows apply the client's predicted diff verbatim; nothing sends `container_close` when the backing block breaks.

**Placement resolution is two functions** because of debug-build stack frames: each `match` arm gets its own stack slot, and `size_of::<BlockEntity>()` is 9,168 bytes (`Hopper` widest), so a forty-arm match would reserve 366,720 bytes and overflow a default thread stack without recursion. `block_entity_for_item` returns a small `PlacedBlockEntity` descriptor and only `PlacedBlockEntity::instantiate` builds a `BlockEntity` (live frames total 35,920 bytes; `Box::new(expr)` does not help, and `BlockEntity::Crafter` boxes its grid for the same reason). Read a frame from the `sub sp, sp, ...` immediate:

```bash
ar x target/debug/liblodestone_server.rlib
llvm-objdump -d --disassemble-symbols=<mangled symbol> <member>.o | head -20
```

Other frames: `BlockEntityRegistry::tick_hopper` 119,376; `chunk_nbt::block_entity_from_nbt` 131,568; `structure_loot::chests_for_chunk` 57,088. The guard is `block_entities::tests::resolving_a_placement_fits_a_modest_stack`, which re-execs the test binary on a thread with `PLACEMENT_STACK_BUDGET` so an abort becomes a named assertion.

### Bone meal

`apply_bone_meal(state, above_state, rng)` is a pure decide-then-apply function with a per-block target/success/perform triple. Wheat, carrots and potatoes always succeed, gaining 2-4 stages (beetroot: the same draw divided by 3); saplings succeed 45%; the item is consumed even on a failed roll. Grass vegetation and stage-1 sapling trees return `NotModelled` (nothing consumed): they need a feature placer, and a partial effect would draw a different number of RNG values and desync later rolls.

### Entity and player persistence

- Players: gzip `<world>/players/data/<uuid>.dat` (not `playerdata/`, which reads as "new player"). Entities: a sibling `entities/` region set with `Position: IntArray[2]` and no `yPos`; looking for terrain's `xPos` files everything under chunk (0, 0).
- A `DataVersion` gate at world open refuses any version other than exactly what this build writes, before any byte is written (a mismatched re-save once erased every cave biome).
- Unknown fields are written back verbatim (`SavedEntity::extra`, `PlayerData::preserved`). Exclude a field from the catch-all only if its decode consumed it, never by name: `Age` is a `Short` on an item and an `Int` on a mob, and a name-keyed exclusion made every baby mob an adult. New modelled player fields go in `MODELLED_FIELDS` or the writer emits the key twice.
- Stale records are cleared by UUID: live UUIDs form a set, matching stored records are dropped, unknown UUIDs are kept byte-for-byte (rewriting only chunks holding entities leaves duplicates; rewriting every chunk deletes untouched vanilla entities).
- The player `.dat` writes on clean disconnect and every ~30 s (timeouts, crashes and shutdown skip the disconnect path). Entity restore runs in the mob-seeding task after `MobHandle::replace_world`.
- Not done: projectiles are not persisted; hunger and the ender chest are preserved but not simulated; only the Overworld has entity storage.

## How to change it

- New placement family: extend `placement` off the `Shape` census; add a name list only if the census cannot separate two families.
- New support family: add the base class to `scripts/derive-block-support.py` and regenerate.
- Block-entity numbers live in each type's module, taken from the jar or generated recipe JSON. Never derive a default state, break constant or save schema from a sibling implementation.

## Configuration

`--protocol <n>` and the `live` feature select which family's hardness, outline and support censuses resolve; without `live`, digging and placement prediction are refused. Persistence takes the world directory and autosave interval from `IntegratedServer::open_persistent_with_mobs`.

## Dependencies

`lodestone_data::{block_states, collision_shapes, outline_shapes, sound_types, block_entity_types}` ([Registries](./registries.md)); `crate::redstone` state-string helpers; `crate::loot`/`block_drops` for cascade drops; `lodestone-anvil` for regions, gzip NBT and the version gate (gated off `wasm32`; [World persistence](./world-persistence.md)).
