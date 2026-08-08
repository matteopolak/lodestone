# Block placement conventions

## What it is

The server-side table of `Block.getStateForPlacement` conventions: which `facing`, `half`,
`axis`, `type`, `shape` or `hinge` a block gets when a player right-clicks it into the
world. Lives in `crates/lodestone-server/src/block_placement.rs`, called from
`server.rs`'s `placed_block_state`, and is why a placed stair, chest, furnace or anvil now
points the way vanilla points it.

## Why it is a table

There is no single convention, and picking one is wrong about half the time. Read off
26.2's own block sources:

| block | convention | source |
|---|---|---|
| stair, door, campfire, calibrated sculk sensor, decorated pot | `getHorizontalDirection()` — faces **with** the player | `StairBlock.java:102`, `DoorBlock.java:145` |
| furnace, chest, ender chest, lectern, loom, stonecutter, beehive, carved pumpkin, glazed terracotta, vault, repeater, comparator, trapdoor (top/bottom click) | `.getOpposite()` — faces **at** the player | `AbstractFurnaceBlock.java:53`, `ChestBlock.java:213` |
| anvil | `.getClockWise()` | `AnvilBlock.java:54` |
| dispenser, dropper, barrel, piston, command blocks | `getNearestLookingDirection().getOpposite()` | `DispenserBlock.java:153` |
| observer | `.getOpposite().getOpposite()`, i.e. the look direction | `ObserverBlock.java:134` |
| shulker box, amethyst cluster/buds, lightning rod | the **clicked face** | `ShulkerBoxBlock.java:101` |
| hopper | clicked face's opposite, either vertical folded onto `down` | `HopperBlock.java:85` |
| end rod | clicked face, flipped if the rod behind already points that way | `EndRodBlock.java:29` |
| ladder, tripwire hook | the clicked face | `LadderBlock.java:93` |
| pillars (log, basalt, quartz pillar, hay, …) | `axis` from the clicked face | `RotatedPillarBlock.java:44` |
| rail | `north_south`/`east_west` from the look axis | `BaseRailBlock.java:139` |
| standing sign, banner | 16-segment `rotation` from `yaw + 180°` | `StandingSignBlock.java:46` |
| skull, head | 16-segment `rotation` from `yaw` | `SkullBlock.java:48` |
| button, lever, grindstone | `face=floor/wall/ceiling` from the clicked face, `facing` from the look | `FaceAttachedHorizontalDirectionalBlock.java:38` |
| bell | `attachment` from the clicked face | `BellBlock.java:173` |

## How it works

`placement(block, ctx, block_at)` takes a `PlaceContext` (target cell, clicked face,
block-local cursor hit, yaw, pitch) and a world reader, and returns a `Placement` — a
state string plus any **extra** cells the placement owns.

**Family comes from the block-state census, not a name list.** One pass over
`lodestone_data::block_states` builds a cached block → `Shape` map recording which
properties a block's states actually carry: a `hinge` means a door, a `type` over
`top/bottom/double` means a slab, a `type` over `single/left/right` means a chest, a
three-valued `axis` means a pillar. A block added to 26.2's data reaches the right arm
with no edit here. Name lists appear only where the census cannot separate two families
carrying the same properties — a ladder and a lectern are both "one horizontal `facing`"
— and those are `FACING_IS_LOOK`, `FACING_IS_CLICKED_FACE` and `facing_is_clicked_face`.

**The cursor is what half/top/bottom needs.** `ServerBound::UseItemOn` now carries the
`BlockHitResult` cursor (three `f32`, block-local), which `upper_half` reads. Without it
every stair and slab was a bottom one.

**A wall click is a different block, not a rotated one.** `wall_variant` rewrites
`torch` → `wall_torch`, `oak_sign` → `oak_wall_sign`, `oak_hanging_sign` →
`oak_wall_hanging_sign`, `white_banner` → `white_wall_banner`, `skeleton_skull` →
`skeleton_wall_skull`, `zombie_head` → `zombie_wall_head`, `tube_coral_fan` →
`tube_coral_wall_fan` — and verifies the rewritten name against the census, so a suffix
that matches by accident yields `None` rather than an unresolvable state.

**Neighbour-dependent state.** `block_at` is consulted for a stair's `shape` (including
`canTakeShape`, which stops a run of parallel stairs cornering), a chest's `type`
pairing, a slab's `double`, an end rod's flip, and a door's hinge balance.

**Two-cell placements** travel out as `Placement::extra`: a door's upper half, a bed's
head, and a paired chest's partner re-typed from `single` to `left`/`right`.
`apply_use_item_on` writes each and sends a `block_update` for it, because the client did
not predict them.

## How to change it

Add an arm to `placement`, keyed off `Shape` where you can. Three gotchas:

* **Never compute a state id here.** Return a state *string* naming only the properties
  you mean; `v770`'s `resolve_state_id` writes them over the jar-marked default state.
  Re-deriving id arithmetic is how #476 happened.
* **`cursor` is block-local to the *clicked* block**, while vanilla's
  `getClickLocation().y - getClickedPos().getY()` is relative to the *placement* cell.
  Those agree for every horizontal click and the two vertical cases short-circuit before
  the cursor is read, which is why `upper_half` can use `cursor.y` directly. A new family
  that reads the cursor on a vertical click must account for the offset.
* **The three redstone families are intercepted in `server.rs` before this module.** Not
  a different convention — a repeater does take `.getOpposite()` like a furnace — but
  `crate::redstone` reads `delay`/`locked`/`powered` off the state *string*, so their
  placement must name the full property set. The observer is deliberately still yaw-only
  there, because `redstone_observer` models horizontal observers only.

## Known gaps

* **A chest's sneak-placement branch** (`ChestBlock.java:217-223`) is not modelled — the
  server does not carry the client's sneak state, so a sneak-placed chest pairs as a
  non-sneak one would.
* **`canSurvive` is not modelled**, so the walk over `getNearestLookingDirections()` that
  buttons, levers, ladders and bells perform is collapsed to "use the clicked face". That
  is the answer for every reachable click; it would diverge for a click whose first
  nearest direction cannot support the block.
* **A bed or door whose second cell is occupied still places**, where vanilla's
  `getStateForPlacement` returns `null` and nothing is placed.
* **Waterlogging** is never set — `is_air_or_fluid` lets a placement replace water, and
  the resulting state keeps the default `waterlogged=false`.

## Configuration

None.

## Dependencies

* `lodestone_data::block_states` — the jar-derived block-state census, both for family
  classification and for validating a rewritten wall-variant name.
* `crate::neighbor_update::Direction` for the rotation helpers, and `crate::redstone`'s
  state-string accessors (`base_name`, `get_str_property`, `direction_to_str`).
* `crate::block_placement` is consumed only by `server.rs`'s `placed_block_state`.
