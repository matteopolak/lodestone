# Block drops

## What it is

Breaking a block server-side now rolls that block's vanilla loot table, spawns the result as real item
entities with vanilla's `popResource` position and velocity, streams them to every connection, and lets a
player walk over them to collect them into their inventory. This is issue #337's *consumer* — the loot
roller itself already existed and had never been called.

## The five links, and which were already there

#337 was closed once and reopened by the closed-issue audit as a "confirmed island". The audit was right,
but the island was smaller than it looked: most of the chain existed, in pieces that had never been joined.

| link | state before this work |
|---|---|
| 1. a block breaks | **existed** — `apply_block_action`'s `StopDestroy` arm |
| 2. its loot table is rolled | **built, zero production callers** — `loot.rs`, 1,551 lines, 6 bundled tables |
| 3. an item entity is spawned | **existed, one caller** — `MobSim::spawn_item`, reached only by the composter's bone meal |
| 4. it is broadcast | **existed but carried the wrong value** — see below |
| 5. it falls / merges / is picked up | falls: **existed**. merges: built, zero callers. pickup: **absent** |

So the missing pieces were: the call from 1 to 2, the block→table-id mapping, `popResource`'s geometry, the
merge driver, all of pickup — and a **shipped bug** in link 4.

## The acacia-boat bug

`MobSim::snapshots` set each dropped item's `EntitySnapshot::entity_type` to **the item's own key**:

```rust
entity_type: state.item.clone(),   // e.g. `minecraft:bone_meal`
```

`entity_type` is an *entity* type. `minecraft:bone_meal` is not in the entity-type registry, so `v770`'s
`encode_add_entity_body` resolved it through `entity_type_id(name).unwrap_or(0)` — and network entity type
`0` is `minecraft:acacia_boat`. **Every dropped item this server has ever spawned arrived at the client as
a boat**, with no error logged anywhere.

Two things are worth keeping about how this survived:

* `cargo xtask connectedness` could not see it. The wire was fully connected; the value on it was wrong.
  That is the same shape as #323's `SET_TIME` (every link green, wall-clock time instead of the tick
  counter), and the tool is documented as blind to it.
* **A test asserted the bug as correct.** `projectile_and_item_registries.rs` had
  `assert_eq!(item_snap.entity_type, rk("minecraft:diamond"))`, under a test named
  "…_with_their_own_identity_…". The name and the assertion agreed with each other, so nothing about it
  read as wrong on inspection.

The item's *identity* belongs in metadata (`ItemEntity.DATA_ITEM`), not in this field.

## How it works

`apply_block_action`'s `StopDestroy` arm, in order:

1. reads the block state **before** `set_block(AIR)` — vanilla's `Level.destroyBlock` captures it first for
   the same reason, and after the write it is unrecoverable;
2. `block_drops::drop_block_loot` maps the state to `minecraft:blocks/<path>` (`Block.getLootTable`'s
   default), looks it up in the process-wide bundled set, rolls it against the empty `LootContext`, and
   turns each stack into a `PoppedItem` carrying `popResource`'s position and velocity;
3. each `PoppedItem` goes to `MobSim::spawn_item` with `ItemLifecycle::newly_dropped`, whose 10-tick delay is
   `setDefaultPickUpDelay()`.

From there the existing machinery takes over with no new task: `tick::run_tick_loop` already ticks `MobSim`
every 50 ms (advancing lifecycle and `ItemMotion` fall dynamics), and `MobSim::snapshots` already feeds the
per-connection `EntityStreamer` diff, which is the "broadcast path" — a *pull*, so a new item is picked up by
every connection's next streaming pass with nothing pushed. (#438's claim that there is no broadcast path and
no player registry is **stale**: `players.rs` landed both.)

Pickup runs in `serve_play`, right after the player's position is republished and *before* the streaming
pass, so the item's `REMOVE_ENTITIES` and the inventory's `container_set_slot` go out together.

### `popResource`'s draw order is the specification

From `Block.java:412-419` plus the `ItemEntity` constructor at `ItemEntity.java:61-66`, **five draws in the
order x, y, z, vx, vz**. `vy` is the constant `0.2` and consumes no draw. A port that draws for `vy`, or that
draws velocity before position, produces a statistically identical cloud of items and desyncs from vanilla
per-seed — which is why `pop_resource_placement` owns the RNG rather than accepting an offset, and why one
test recomputes all five values from a parallel stream instead of only checking the envelope.

`EntityTypes.ITEM` is `.sized(0.25F, 0.25F)`, so the `- halfHeight` term is `0.125`.

### Two volumes, neither of them a radius

* **Pickup** is `Player.aiStep`'s `getBoundingBox().inflate(1.0, 0.5, 1.0)` intersected against the item's
  own box. Both sets of half-extents contribute: horizontal reach is `0.3 + 1.0 + 0.125 = 1.425`, and the
  vertical band is asymmetric (`0.5 + 0.25` below the feet, `1.8 + 0.5` above). A sphere of radius 1.0 is
  wrong three separate ways.
* **Merging** is `mergeWithNeighbours`' `inflate(0.5, 0.0, 0.5)`. The **`0.0`** is load-bearing: stacks side
  by side merge, stacks a block apart vertically never do. One isotropic radius would merge a drop with one
  sitting on the block below.

### Pickup destination order

`Inventory.getSlotWithRemainingSpace` searches **selected hotbar slot → off-hand (40) → natives 0..36**, and
only merges into non-empty slots; `getFreeSlot` then places a fresh stack scanning **`items` (0..36) only**.
So a *merge* into the off-hand is normal and a *fresh stack* there is impossible. "First empty slot" still
puts the item in the inventory, which is why the gate asserts the destination rather than the arrival.

`PlayerInventory::add` returns every native index it wrote, because a stack that overflows one slot must
produce two `container_set_slot`s. `window_zero_menu_slot` converts those to menu coordinates and is tested
as the exact inverse of the existing menu→native table rather than by restating it.

## How to change it, and the gotchas

* **Bundling another block's table** is a JSON file under `assets/loot_table/blocks/`; `build.rs` re-embeds
  it and `block_loot_table_id` finds it with no code change. Keep the "zero unsupported features" invariant —
  `LootTableSet::load_bundled` debug-asserts it.
* **Only five block tables are bundled** (`stone`, `dirt`, `gravel`, `coal_ore`, `iron_ore`). Every other
  block in the game drops nothing. That is honest rather than guessed: a block with no table returns an empty
  `Vec`, exactly as one whose table rolled nothing does.
* **`doTileDrops` is not honoured.** Vanilla wraps `popResource` in
  `level.getGameRules().get(GameRules.BLOCK_DROPS)`. This crate has no live game-rule registry to consult —
  `game_rules.rs` describes rules for the wire and stores no values. A real registry is the prerequisite.
* **Tool-sensitive drops need `LootContext`, not a change here.** Under the empty context every bundled
  table's silk-touch `match_tool` branch fails and `alternatives` falls through to the un-enchanted child —
  correct for a bare hand, wrong for a silk-touch pickaxe. Fortune is likewise level 0, so `apply_bonus`
  (`ore_drops`) and `table_bonus` are at their base values.
* **The RNG stream is not vanilla's.** Draw *count* and *order* are; the stream is SplitMix64 vs vanilla's
  Xoroshiro, and vanilla additionally splits position draws (level RNG) from velocity draws (entity RNG).
  Byte-exact JVM parity is separate, larger work — `loot.rs`'s module doc records the same divergence.
* **Partial pickup shrinks the entity rather than deleting it**, matching vanilla's in-place `ItemStack`
  shrink. `MobSim::set_item_count` does this as a remove-and-respawn at the same id (preserving `age`, so a
  full player brushing past cannot reset the despawn clock), because `ItemEntityRegistry` in
  `lodestone-entity` exposes no count setter.

## How the drop becomes *visible* — `ItemEntity.DATA_ITEM` (#537)

A client draws nothing for an item entity whose stack it has not been told: vanilla's
`ItemEntityRenderer.submit` returns early on `state.item.isEmpty()`, and this project's client does the same.
For one landing this server sent `EntitySnapshot::metadata: Vec::new()` for every drop, so a broken block
spawned a real item entity that fell, merged and could be picked up — the pickup *visibly*, because the
inventory slot updates — while drawing zero pixels, with every link on the path green.

The stack travels as `ItemEntity.DATA_ITEM`, carried by `MetadataField::Item { item, count }`:

1. `protocol.rs` defines the variant. It carries a `ResourceKey`, which is not `Copy`, so **`MetadataField`
   no longer derives `Copy`** — deliberately, and permanently: a version-free vocabulary enum that derives
   `Copy` silently forbids every future field with an owned value, and the whole cost of dropping it is that
   an implementor writes `match field` rather than `match *field`.
2. `crates/protocol/v770/src/server_protocol.rs` encodes it: index **8**, serializer **7** (`ITEM_STACK`),
   then `write_optional_item_stack`'s VarInt count / VarInt registry id / empty `DataComponentPatch`. There is
   still **no `_ =>` arm** in `encode_set_entity_data`, on purpose — a new field must be encoded or fail to
   compile.
3. `MobSim::snapshots`' item loop fills `metadata` from `ItemState::item` plus the lifecycle count. The
   client's existing chain (`Value::Item` decode → `DisplayItem` → `resolve_entity_facts` → `EntityDraw::item`
   → `world_items.rs`) needed no change; it was already live and gated by
   `crates/lodestone-shell/tests/live_dropped_item.rs`.

### Where the index came from, and the collision that is *not* inheritable

Both numbers were read off the `EntityDataIndexOracle` dump already in the tree
(`crates/protocol/v770/tests/support/entity_data_index_jvm.txt:55` — `8 ItemEntity.DATA_ITEM 7 ITEM_STACK`),
never hand-counted, and the same two bytes appear in a packet captured off a real vanilla 26.2 server
(`crates/protocol/v770/tests/fixtures/item_entity_metadata_diamond.hex`).

Index 8 is the most contended index in the dump — **nineteen** claimants, including
`LivingEntity.DATA_LIVING_ENTITY_FLAGS`, `AbstractArrow.ID_FLAGS`, `ExperienceOrb.DATA_VALUE`,
`PrimedTnt.DATA_FUSE_ID`, and six other `ITEM_STACK` fields. **Neither `entity_census::is_living` (index 8's
living-vs-arrow precedent) nor `is_mob` (index 15's mob-vs-armour-stand precedent) separates the claimants
here**: both report *false* for `minecraft:item`, which does not distinguish it from `AbstractArrow` or
`PrimedTnt`. The encoder needs no census column because the guard is structural — the field list is built by
`snapshots`' **item** loop, so every `MetadataField::Item` belongs to a `minecraft:item` entity by
construction. **The invariant to keep is on the producer: never push a `MetadataField::Item` from the mob or
projectile loops.**

## Gates and their controls

`crates/lodestone-server/tests/serve_play.rs` drives the real `serve_connection` path over a solid-stone
world. Every control below was run and observed failing:

| gate | control | observed failure |
|---|---|---|
| breaking stone drops one cobblestone item, streaming as `minecraft:item` | revert `entity_type` to the item key | `left: "minecraft:cobblestone", right: "minecraft:item"` |
| an aborted dig drops nothing | drop on `AbortDestroy` too | `left: 1, right: 0` |
| a collected drop leaves the world and announces menu slot 36 | — (positive) | — |
| a freshly popped drop is not collectable | ignore `can_be_picked_up` | `left: 0, right: 1` |
| a drop 10 blocks away is not collected | make `is_within_pickup_range` return `true` | `left: 0, right: 1` |
| the drop's snapshot carries `Item { cobblestone, 1 }` | revert `metadata` to `Vec::new()` | `left: [], right: [Item { … "cobblestone" …, count: 1 }]` |

`crates/protocol/v770/tests/server_item_entity_metadata.rs` gates the wire half against the **captured
vanilla packet**, so the expected value predates the encoder: our metadata list must be byte-identical to
`08 07 01 9e 07 00 00 ff`. Control — drop the `DataComponentPatch` from the item write (the single most
plausible transcription error): `ours = [08, 07, 01, 9e, 07, ff], vanilla = [08, 07, 01, 9e, 07, 00, 00, ff]`,
and the same neuter also made the real client adapter raise no `EntityMetadataUpdated` at all.

The fixture is stone rather than "a block" deliberately: stone's table exercises `alternatives` +
`match_tool`, so the fall-through to cobblestone is proved rather than "an item dropped". `block_drops.rs`'s
unit tests cover all five bundled tables, which between them add `survives_explosion`, `table_bonus`
(gravel's 10% flint), `apply_bonus`/`ore_drops` and `explosion_decay` — a stone-only fixture would say
nothing about whether an ore's bonus functions no-op correctly at fortune 0.

Predicted-and-observed drops under the empty context: `stone`→`cobblestone`×1, `dirt`→`dirt`×1,
`coal_ore`→`coal`×1, `iron_ore`→`raw_iron`×1 (each across 64 seeds), `gravel`→`flint` or `gravel` with the
flint share bracketed to `0.07..0.13` over 4,096 samples, which excludes both `table_bonus` never passing and
always passing.

## Configuration

* `block_drops::BLOCK_DROPS_BEHAVIOR_SEED` — the per-connection roll/placement seed. Separate from
  `COMPOSTER_BEHAVIOR_SEED` so a composter click cannot shift which drop a later break rolls.
* `assets/loot_table/` — the bundled corpus (28 KB, 6 files), embedded by `build.rs`.

## Dependencies

`loot.rs` (tables), `mobs.rs` (`MobSim`/`MobHandle`, item entities and their tick), `inventory.rs`
(destination and menu-slot mapping), `mob_spawn::SpawnRng` (draws), `lodestone-entity`'s `item_entity`
(lifecycle, motion, vanilla constants), `lodestone-model` (vocabulary). Names no packet id and no protocol
version.
