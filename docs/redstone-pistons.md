# Pistons

## What it is

`crates/lodestone-server/src/piston.rs` — vanilla's `PistonBaseBlock` and
`PistonStructureResolver`, ported as pure decisions over a `Fn(BlockPos) -> String` world lookup,
plus the wiring in `random_tick.rs`'s neighbour-reaction pass that makes a powered piston actually
move blocks. Issue #316, **partially**: read "What is not here" before assuming a contraption works.

Nothing modelled pistons anywhere in this tree before — `lodestone-physics`'s own comment noted it
deliberately excludes `PISTON` as a type "this crate has no equivalent of".

## How it works

Three pieces.

**1. Push reactions.** `push_reaction(state)` is `BlockBehaviour.Properties`' own
`pushReaction(...)`, extracted from `Blocks.java`: 200 `DESTROY`, 11 `BLOCK`, 16 `PUSH_ONLY` (the
glazed terracottas — the only blocks in the game that use it), everything else `NORMAL` by default.
`is_pushable` adds vanilla's four hard-coded exceptions (obsidian, crying obsidian, respawn anchor,
reinforced deepslate) and the final `!state.hasBlockEntity()`, which is read off
`lodestone_data::block_entity_types` per *state* rather than a hand-kept name list.

The exceptions are exceptions **in `isPushable`**, not `BLOCK` table rows. Making obsidian a `BLOCK`
row would give the right answer and claim vanilla says something it does not, and the table is the
thing a later reader will trust.

**2. `resolve` — the structure resolver.** The 12-block limit, the sticky (slime/honey) backwards
run, perpendicular branching, and `reorderListAtCollision`. **This is the order-sensitive part.**
`Resolution::to_push` is a *list*, and its order decides which block lands in which cell when two
sticky lines collide; `reorder_at_collision` is three sublist splices in a specific order and the
order is the behaviour. Do not simplify it.

Two details that read as bugs and are not:

* **Slime and honey do not stick to each other**, though each sticks to everything else sticky. A
  "sticky means sticky" shortcut carries a block a real contraption leaves behind.
* **A retraction resolves from `piston_pos + 2 × facing`**, not `+ 1`. Vanilla's `moveBlocks` sets
  the arm cell to air *before* constructing the resolver, so the resolver never sees `piston_head` —
  which is a `BLOCK` reaction and would refuse the whole pull. `resolve` reproduces that by masking
  the arm cell, not by special-casing `piston_head` inside `is_pushable`, because a piston head
  genuinely *is* `BLOCK` and a head in the middle of a run genuinely does stop a push.

**3. `has_extend_signal` — quasi-connectivity.** `getNeighborSignal` in vanilla's order:

1. every direction except the push direction, read at the neighbour cell — which is why a piston is
   not powered by the block it is about to push;
2. the piston's own cell from `DOWN`;
3. **`pos.above()`**, every direction except `DOWN`.

Step 3 is QC: a signal one block above the piston, touching no face of the piston itself, extends
it. It looks like a bug, it is load-bearing, and every BUD switch and observer clock in the game is
built on it. A port that "fixes" it silently loses half the contraptions people build.
`quasi_connectivity_and_the_push_direction_exclusion` pins it with a torch diagonal from the piston,
so only step 3 can see it.

### Where it runs

`random_tick.rs`'s `react_to_notification`, arm 3b-bis, beside the repeater and comparator arms.
`PistonBaseBlock.neighborChanged` → `checkIfExtend` fires a **block event**, not a scheduled tick, so
the move happens in the same neighbour pass that noticed the signal — which is why this arm mutates
in place and returns a fan-out rather than scheduling.

An extension is gated on `resolve` succeeding; a retraction is not (the head comes back with nothing
to pull). That asymmetry is `checkIfExtend`'s own. A non-sticky piston discards the resolution on
retraction and only drops its head.

## What is not here, and why #316 stays open

**The two-phase `MovingPistonBlock` transition is not modelled.** Vanilla replaces the moving cells
with `moving_piston` block entities for the duration of the animation and finishes on a later tick;
`apply_move` applies the final positions in one step. Three consequences, each named rather than left
to be discovered:

| vanilla behaviour | why it cannot work here |
|---|---|
| **0-tick pulse** generators | depend on retracting a piston *during* its own extension, which needs the intermediate state to exist and be interruptible |
| `TRIGGER_DROP` (block event 2) | has no distinct behaviour, because there is never a mid-extension to drop |
| entities shoved by the push | that is `PistonMovingBlockEntity.moveEntities`, needing the same intermediate state plus a piston-aware entity AABB sweep |

**A push cannot cross a chunk border.** `redstone::make_lookup` reads air outside its own 16×16
footprint, so a run pushed across `x % 16 == 15` resolves against air. That is a property of the
whole `redstone*` family here, not of this module — `resolve` itself is border-agnostic; it is the
lookup that is not.

So **contraption resolution is faithful and tested; contraption timing is not.** Issue #316 asks for
BUD-switch and 0-tick traces matched tick-for-tick against a real 26.2 server, and that verification
is structurally unreachable while the intermediate state does not exist. It stays open on exactly
that.

## How to change it

* **To model the animation**, the missing piece is a `moving_piston` block entity in
  `block_entities.rs` plus a two-stage scheduled tick under `piston::TICK_PISTON` — not a change to
  `resolve`, which already answers the question the second stage would ask.
* **To make a push cross a chunk border**, the redstone family needs a neighbourhood-wide lookup, not
  a per-column one. Everything in `piston.rs` is already written against `Fn(BlockPos) -> String`
  and needs no change.
* `minecraft:redstone_block` is **not** a signal source in `redstone.rs`, so it cannot currently
  power a piston. That is a gap in that module, noticed while writing these tests, and the reason
  they use a lit torch instead.

## Configuration

None. No feature flags, no constants to tune — `MAX_PUSH_DEPTH` is vanilla's 12 and is not a knob.

## Dependencies

`crate::redstone` for the signal query and the state-string helpers,
`crate::neighbor_update::Direction`, and `lodestone_data::block_entity_types` for the
`hasBlockEntity` test. No block-state census for push reaction: it is per *block*, not per state, so
a name table is the right shape.
