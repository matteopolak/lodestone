# Pistons

## What it is

`crates/lodestone-server/src/piston.rs` — vanilla's `PistonBaseBlock` and
`PistonStructureResolver`, ported as pure decisions over a `Fn(BlockPos) -> String` world lookup,
plus the wiring in `random_tick.rs`'s neighbour-reaction pass that makes a powered piston actually
move blocks, and the two-phase `moving_piston` transition that makes a push *animate* on a client
rather than snap. Issue #316, **partially**: read "What is not here" before assuming a contraption
works.

Nothing modelled pistons anywhere in this tree before — `lodestone-physics`'s own comment noted it
deliberately excludes `PISTON` as a type "this crate has no equivalent of".

## How it works

Four pieces.

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

**4. The two-phase move.** `begin_move` splits `apply_move`'s one-step writes into the cells that
empty *now* and the cells that hold a `moving_piston` for `PISTON_MOVE_DELAY` ticks; `finish_move`
replays the deferred writes. The second phase is **derived from** the one-step writes rather than
recomputed, which is what makes the committed world byte-identical to what the one-step path produced
— `piston.rs`'s `the_two_phase_world_matches_the_one_step_path_cell_for_cell` asserts it cell by cell
over four shapes, and its control (the *mid-animation* world, which must differ at exactly the
deferred cells) fails 4-of-4 under a `begin_move` that defers nothing.

### The two tick numbers, and where they come from

Push on tick `N`, commit on tick `N + 2`. Derived from `ServerLevel.tick`'s own ordering, not guessed:
`runBlockEvents` runs *before* `tickBlockEntities`, and `Level.addBlockEntityTicker` appends to the
live list when the loop is not already inside it, so the block entity `moveBlocks` creates on tick `N`
is ticked on tick `N` too.

| tick | `progressO` at entry | `progress` at exit | branch |
|---|---|---|---|
| `N` | 0.0 | 0.5 | ramp |
| `N + 1` | 0.5 | 1.0 | ramp |
| `N + 2` | 1.0 | 1.0 | **commit** — the entity is dropped and `movedState` is written |

A delay of 1 halves the animation; 3 holds the cells empty an extra tick. The gate asserts a per-tick
commit *count* at `N`, `N+1`, `N+2`, `N+3`, so the two candidate delays land on different ticks
(measured under a neutered delay of 1: `[(40,0),(41,4),(42,0),(43,0)]` against the correct
`[(40,0),(41,0),(42,4),(43,0)]`).

**Four cells animate on a three-block push, not three.** The arm cell is a travelling block like any
other — it carries the *head*, so the head slides out rather than appearing instantly. Predicting one
per pushed block is the plausible wrong answer and this gate failed on it first.

### The pending commit *is* the moving block entity

The reaction surface a move runs on holds a `ChunkColumn` and a `ScheduledTickQueue` and no
block-entity map, so the record travels in the scheduled tick's own **kind** string —
`piston::finish_kind` / `parse_finish_kind`, a `|`-separated `facing|extending|source|moved_state`
after the `redstone:piston_finish` prefix (`|` because a canonical state string can hold `:[],=` but
never a pipe). "Read the moving block entity at this cell" is therefore "find the pending commit at
this cell", which is what `tick.rs`'s `publish_moving_piston` and `server.rs`'s
`moving_piston_records` both do. **Match the kind by prefix, never by equality.**

### Reaching a client

Two packets per animating cell, in this order:

1. `block_update` writing `minecraft:moving_piston[facing=…,type=…]` — invisible on its own, and it
   makes the client's `sync_block_entity` create the record;
2. `block_entity_data` carrying `MovingBlockEntity::update_tag()` — `PistonMovingBlockEntity`'s
   `getUpdateTag`, i.e. `saveCustomOnly`, i.e. `saveAdditional`'s five fields with no `id`/`x`/`y`/`z`.

Either alone draws nothing. Three traps in the record, all of them silent when wrong:

* **`facing` is a `Byte`**, because `Direction.LEGACY_ID_CODEC` is `Codec.BYTE` over `get3DDataValue`
  (`DOWN..EAST` = `0..5`, vanilla's declaration order — *not* alphabetical). Written as an `Int` it
  decodes as absent and every piston animates toward `DOWN`, with a clean parse.
* **`progress` is `progressO`**, so a fresh entity reports `0.0`, not the `0.5` the server's own first
  tick already reached. The client owns the ramp (`+0.5`/tick); sending the advanced value halves the
  animation.
* The block's registry key is `moving_piston` and its **block entity's** key is `piston`. Sending the
  block's resolves to some other entity.

Note `0.0` is also the seed at which the two readings of `getExtendedProgress` (`p - 1` vs `1 - p`)
are furthest apart — a whole cell in opposite directions — so `extending` is load-bearing and nothing
here is gated at `0.5`, where they agree in magnitude.

There are **three** publishers, because block updates reach a client by three different routes:
`tick.rs`'s `publish_moving_piston` (the world tick loop's effect lane, drained *after* the
block-change lane — that ordering is why `WorldEffect::BlockEntityData` lives in the effect enum at
all), and `server.rs`'s `moving_piston_records` at both connection-side sites (`hand_use`, i.e. a
right-click, and placement). Adding a fourth route for block updates without pairing it up is how the
animation silently stops working on that route only.

## What is not here, and why #316 stays open

The intermediate state now exists, so a push animates. What is still missing:

| vanilla behaviour | why it cannot work here |
|---|---|
| **0-tick pulse** generators | depend on *interrupting* a move — vanilla's `triggerEvent` calls `finalTick()` on an entity it finds mid-animation and can start a second move in the same tick; a pending commit here runs to completion |
| `TRIGGER_DROP` (block event 2) | nothing routes a piston block *event* at all, so the case is unreachable rather than unimplemented |
| entities shoved by the push | `PistonMovingBlockEntity.moveCollidedEntities`/`moveStuckEntities`, needing a piston-aware entity AABB sweep. The intermediate state it wanted is no longer the blocker |
| riding a pushed block | `MovingPistonBlock.getCollisionShape` delegates to the entity's interpolated shape; here a `moving_piston` cell is **empty** for two ticks, so a player standing on a pushed block briefly falls through |
| interrupting a *committed* move | `finalTick`'s `isSourcePiston` arm (which writes air) is never reached — only `tick`'s commit branch is. Using `finalTick`'s arm on normal completion would delete the head of every extension |

**A push cannot cross a chunk border.** `redstone::make_lookup` reads air outside its own 16×16
footprint, so a run pushed across `x % 16 == 15` resolves against air. That is a property of the
whole `redstone*` family here, not of this module — `resolve` itself is border-agnostic; it is the
lookup that is not.

So **contraption resolution is faithful and tested; contraption timing is not.** Issue #316 asks for
BUD-switch and 0-tick traces matched tick-for-tick against a real 26.2 server, and that verification
is structurally unreachable while the intermediate state does not exist. It stays open on exactly
that.

## How to change it

* **Never recompute `finish_move`'s writes from `resolve` again.** They are a projection of
  `apply_move`'s output on purpose. Recomputing is how the animated path and the one-step path drift,
  and the drift is invisible for every shape whose resolution happens to be stable.
* **Changing the commit encoding means changing both halves** of `finish_kind`/`parse_finish_kind`
  and the round-trip gate between them. `parse_finish_kind` must keep declining every kind it did not
  write, or `tick.rs`'s commit arm would try to write a block state out of a repeater's tick.
* **To make a push cross a chunk border**, the redstone family needs a neighbourhood-wide lookup, not
  a per-column one. Everything in `piston.rs` is already written against `Fn(BlockPos) -> String`
  and needs no change.
* **Levers, buttons and pressure plates cannot power a piston**, and neither can
  `minecraft:redstone_block`. `redstone::is_signal_source` is `torch || diode || observer`, and
  neither `weak_signal` nor `direct_signal` has an arm for a `powered=true` lever. That is a gap in
  `redstone.rs`, not in this module — but it is the first thing to try interactively and it looks
  exactly like a broken piston, so **verify with a lit redstone torch**, an observer or a repeater.

## Configuration

None. No feature flags, no constants to tune — `MAX_PUSH_DEPTH` is vanilla's 12 and is not a knob.

## Dependencies

`crate::redstone` for the signal query and the state-string helpers,
`crate::neighbor_update::Direction`, and `lodestone_data::block_entity_types` for the
`hasBlockEntity` test. No block-state census for push reaction: it is per *block*, not per state, so
a name table is the right shape.
