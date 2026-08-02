# Block placement prediction

## What it is

When you right-click a block on a live server the shell now **writes the placed block into the
client-owned world immediately**, instead of sending `use_item_on` and waiting for the server's
`BLOCK_UPDATE`. It writes the block state *and* the block entity, so a placed chest is a chest the
moment you click it rather than a hole for one round trip (issue
[#381](https://github.com/matteopolak/lodestone/issues/381)).

This is the prediction half of issue [#374](https://github.com/matteopolak/lodestone/issues/374).
#374 established the rule — in vanilla, *writing a block state is what creates a block entity*
(`LevelChunk.java:341`), no packet involved — and wired it into the two packet arms
(`BLOCK_UPDATE`, `SECTION_BLOCKS_UPDATE`). It could not wire it into the prediction, because the
prediction did not exist: `Sim::use_item_live` sent the action and stopped, and `lodestone-game`'s
`Placement` is a **decision** machine (`UseOnDecision`) that never touches a world. So the same bug
survived on the client's own path, one packet away from where it was fixed.

## How it works

`Sim::use_item_live` (`crates/lodestone-shell/src/sim.rs`) now does five things instead of two:

1. **Resolve the held item to a block.** Vanilla's `BlockItem` shares its block's registry name, so
   "is this item placeable?" is "is there a block called `minecraft:<item>`?" — answered by scanning
   `lodestone_data::block_states`, which also collects the block's property value domains
   (`block_states_of`).
2. **Classify the orientation** (`orientation_for_placement`) into a
   `lodestone_game::placement::OrientationKind`, or decline.
3. **Read the world facts once** into `PlacementFacts` and hand that to `Placement::use_on` as the
   `PlacementWorld`.
4. **Send** the `use_item_on` action, in every branch, exactly as before. Nothing here changes the
   wire.
5. On `UseOnDecision::Place`, **resolve the state** (`state_for_placement`) and write it through
   `sim::write_predicted_block`, then re-mesh.

`write_predicted_block` is deliberately the *same two calls in the same order* as the v770 adapter's
`BLOCK_UPDATE` arm:

```rust
world.set_block(x, y, z, state);
world.sync_block_entity(x, y, z, block_entity_type(state));
```

It is a free function over `&mut dyn WorldSink` rather than a `Sim` method so the pixel gate can
drive the production write with a bare `World`, no GPU and no server. The `Option<u32>` comes from
`lodestone-data` because **`lodestone-world` cannot depend on it** (`data → model → world` is a
cycle) — the caller resolves the block-entity type and the world only applies it. That shape is a
constraint, not a preference; do not try to make the world look it up.

### The lock discipline is why `PlacementFacts` is precomputed

`PlacementWorld` is queried re-entrantly from inside `use_on`, and every answer needs the chunk
store's read lock — while `use_on` itself needs the ECS write guard, because it mutates the
`PlacementPredictor` resource. Answering live would nest `chunks → World`, the one order
`EcsHandle`'s rule 3 forbids. `use_on` asks exactly four questions over two positions
(`is_replaceable(clicked)`, then `is_replaceable`/`is_obstructed(target)` and
`is_interactable(clicked)`), so all four are read up front. The side effect is that the entire
decision is hermetically testable with no `Sim`.

### What happens when the server refuses

**Nothing has to detect it.** Vanilla's server sends a `ClientboundBlockUpdatePacket` for *both*
candidate positions after **every** `use_item_on` — accepted, refused, or actually an interaction
(`.cache/mc/26.2/src/net/minecraft/server/network/ServerGamePacketListenerImpl.java:1397-1398`):

```java
this.send(new ClientboundBlockUpdatePacket(level, pos));
this.send(new ClientboundBlockUpdatePacket(level, pos.relative(direction)));
```

`pos` is the clicked block and `pos.relative(direction)` is the adjacent cell, and a prediction can
only ever land on one of those two. So a mispredicted cell is overwritten by the authoritative state
within one round trip, and since #374 that arm calls `sync_block_entity`, which **removes** the
block-entity record the prediction created. The removal half is not a second mechanism; it is the
same one pointing the other way.

Two caveats on that guarantee, both read off the same method: the double send is inside the
`hasClientLoaded()` / `isWithinBlockInteractionRange` / `|cursor| < 1.0000001` guards, and the
build-limit branches (`pos.getY() > maxY`, `< minY`) return *before* it. The build-limit case is
harmless because `World::set_block` is a documented no-op outside the column's height, so there is
nothing to correct.

This is why every classification in the pipeline is allowed to err toward **not** predicting but
never toward predicting something wrong: a decline costs the round trip the hole used to cost, and a
misprediction costs the same round trip plus a visible flicker.

`Sim::reconcile_predictions` runs on the same `NetUpdate::SectionBlocks` signal, but it does **not**
correct the world (the world is already correct). It clears the settled prediction from
`Placement`'s pending ledger — nothing else drains it, since `block_changed_ack` is decoded by the
adapter and has no shell consumer — and records whether the server agreed.

## Resolving a block state, and the census that does not exist

`state_for_placement` produces a **total** specification: every property of the block gets a value,
and the state id is then the unique state whose property set equals it. It is total because the
alternative needs the block's *registered default state* to fill the rest, and no census in this
tree carries one — `blocks.json`'s `"default": true` flag is not in
`lodestone_data::block_states`, whose rows are `(block index, property-set index)` only.

**Do not substitute "the lowest state id for this block" for the default.** It is wrong in a way
that looks right: `BooleanProperty`'s value set is ordered `{true, false}`, so the lowest
`minecraft:chest` id is a **waterlogged** chest (3987, not the default 3988) and the lowest
`minecraft:oak_slab` id is a **top** slab. A waterlogged chest still renders as a chest, so a pixel
gate that chose its own state would pass.

Values come from four places, in this order:

| source | rule |
|---|---|
| geometry | `facing` / `axis` / `half` (stairs) / `type` (slab) from the resolved `PlacedState` |
| explicit | `shape = straight` for stairs; `waterlogged = false`; `type = single` for a chest |
| `BLOCK_PROPERTY_OVERRIDES` | per-block, where the default is not consistent (`lit` on the three furnaces) |
| `NON_GEOMETRIC_DEFAULTS` | 60 property names whose default is the same across every block |

Anything else **declines**.

`NON_GEOMETRIC_DEFAULTS` is a measurement, not a guess. Taking each block's `"default": true` state
out of `blocks.json` and collecting per property name the set of values it holds there: 93 names
appear, and **60 take exactly one value across all 1,196 blocks**. The 17 that do not (`facing`,
`axis`, `half`, `type`, `shape`, `lit`, `waterlogged`, `level`, `mode`, `rotation`, the five
connection booleans, `bottom`) are handled above or are a reason to decline.

A further **16 unambiguous names are deliberately excluded**, because vanilla computes them at
placement time from geometry or neighbours and the registered default is therefore the wrong answer
for a *placed* block: `attachment` (`BellBlock`), `face`
(`FaceAttachedHorizontalDirectionalBlock`), `orientation` (`CrafterBlock`, `JigsawBlock`), `hinge`
(`DoorBlock`), `part` (`BedBlock`), `vertical_direction`/`thickness` (`PointedDripstoneBlock`),
`hanging` (`LanternBlock`), `distance`/`leaves`/`persistent` (`LeavesBlock` — note `persistent` is
set **true** for a player-placed leaf, so its `false` default would be actively wrong),
`instrument` (`NoteBlock`, read from the block below), `side_chain`, `tip`, `tilt`, `drag`. Omitting
a name makes every block carrying it decline.

**Measured coverage: 721 of 1,196 blocks resolve**; 453 decline; the remaining 22 (corals, coral
fans, `sea_pickle`, `conduit`) resolve to a state that differs from the registered default *only* in
`waterlogged`, and that is correct rather than divergent — vanilla sets `waterlogged` from the fluid
at the placement position, and the shell only predicts into air.

## The two facing lists, and why they are lists

Nothing in the block-state census distinguishes a 4-way `facing` that points *toward* the player
(`StairBlock`, `LadderBlock`, `BedBlock`, `DoorBlock`) from one that points *away* (`ChestBlock`,
`AbstractFurnaceBlock`, `CarvedPumpkinBlock`) — the property signatures are identical and the
difference lives only in Java. 293 blocks have a 4-value `facing` in 26.2 and 41 have a 6-value one.

So `FACING_HORIZONTAL_OPPOSITE` (41 blocks) and `FACING_ALL` (6) are hand-written, sourced by
grepping `getStateForPlacement` for `getHorizontalDirection().getOpposite()` and
`getNearestLookingDirection().getOpposite()` under
`.cache/mc/26.2/src/net/minecraft/world/level/block/`, then restricted to single-cell blocks whose
remaining properties resolve. Every entry carries its source line. A block that is not listed and is
not a stair, slab or pillar does not predict.

The principled fix for all of this is one generated table — `block index → default state id`, plus a
`state id → OrientationKind` census — dumped from `Block.BLOCK_STATE_REGISTRY` the way
`crates/protocol/v770/tests/{collision_shapes,hardness}.rs` dump theirs, living in `lodestone-data`
with the generate-or-assert + `LODESTONE_REGEN=1` pattern. That would delete both lists and the
whole property-default table. It is not done here.

## Interactability

`is_interactable_state` decides place-vs-interact and is an **over-approximation on purpose**. The
asymmetry is the design: calling an inert block interactable only *suppresses* a prediction, while
calling an interactable block inert drops a ghost block in the cell next to the chest you meant to
open. So:

- every block that **owns a block entity** is interactable, via
  `lodestone_data::block_entity_types` — that covers every container in the game through a real
  census rather than through a list;
- plus `INTERACTABLE_FRAGMENTS`, name substrings for the families that have no block entity
  (`_door`, `_button`, `_fence_gate`, `_bed`, `lever`, `_table`, `cake`, …).

Vanilla asks `BlockState.useItemOn`/`useWithoutItem`, real per-block behaviour with no census
anywhere in this tree. A block missing from the list costs one round trip.

`is_air_state` is likewise narrower than vanilla's `canBeReplaced`: only `air`, `cave_air`,
`void_air`, not water/lava/tall grass/snow layers. That set is per-state registry data we do not
have, and narrowing it is what makes `waterlogged = false` exact rather than assumed.

## How to change it

- **Adding a block to `FACING_HORIZONTAL_OPPOSITE`**: check `getStateForPlacement` in the
  decompiled source and cite the line. Then check its *other* properties resolve — a block whose
  placement reads a property this pipeline does not model will predict a wrong state, which is worse
  than declining.
- **Adding a property to `NON_GEOMETRIC_DEFAULTS`**: it must be unambiguous across every block in
  `blocks.json`'s default states *and* not computed at placement time. The second half is the one
  that bites; `persistent` satisfies the first and is wrong.
- **Adding a new `OrientationKind`** (`FacingHorizontal`, for stairs-style toward-facing blocks) is
  the natural next step. `state_for_placement` already handles it in its `facing` arm; what it needs
  is the block list.
- **The demo world's `Sim::set_block_world` deliberately does not sync block entities.** Its `value`
  is a `crate::blocks::id` constant from the shell's own ten-entry palette, so running it through
  the 26.2 census would be a category error (`id::WATER` is `5`, and real state `5` is an unrelated
  block). The demo palette contains nothing that could own a block entity.

## Configuration

None. No feature flag, no setting: the prediction is unconditional on a live session and declines
by resolving to `None`.

## Dependencies

- `lodestone-game`'s `placement` module — `Placement`, `UseOnContext`, `UseOnDecision`,
  `PlacedState`, `OrientationKind`, `PlacementWorld`. Unchanged by this work; it was already a
  complete decision machine with nothing driving its `Place` branch.
- `lodestone-data` — `block_states` (names, property sets) and `block_entity_types` (the
  `state_id → BLOCK_ENTITY_TYPE` census #374 generated).
- `lodestone-world` — `WorldSink::set_block` / `sync_block_entity`.
- `crates/lodestone-shell/src/block_entities.rs` — the consumer that turns the resulting record into
  a `ChestSpawn`.

## Gates

```bash
# hermetic, in sim.rs's test module
cargo test -p lodestone-shell --lib -- placement
# the pixel gates (GPU + client.jar)
cargo test -p lodestone-shell --test placed_chest_block_entity_pixels -- --ignored --nocapture
```

The hermetic tests pin the resolver to `blocks.json` state ids
(`placement_states_resolve_to_the_jar_oracle`), assert the six declines with the reason for each
(`unclassifiable_placements_decline_rather_than_guess`), and drive the place-vs-interact branch
through the real `Placement::use_on`.

`placed_chest_block_entity_pixels.rs` holds three gates: #374's `BLOCK_UPDATE` route, #381's local
prediction with **no packet at all** (control: no local write, which is #381 itself as a world state
rather than a deleted line), and the refused placement. The prediction gate draws the state the
*resolver* chose — not one the test picked — and asserts its properties against
`ChestBlock.getStateForPlacement` before measuring a pixel, because a waterlogged or wrong-facing
chest fills the same rect.

**These were not run.** Verification in the session that wrote this was batched; the pure resolver
was type-checked and exercised standalone (`rustc --edition 2024 --test` against a stand-in census),
and neither the workspace build nor the GPU gates were executed by its author.
