# Bubble columns

## What it is

The vertical impulse a `bubble_column` block applies to the player: soul sand
pushes you up (an elevator), a magma block drags you down (a drain). Issue #199.

Ported in `crates/lodestone-physics/src/player.rs` as `apply_bubble_column`, over
the `CollisionView::bubble_column` seam, and wired to the live world through
`VersionAdapter::block_bubble_column_drag`.

Before this, a bubble column moved you exactly like the plain water it already
classified as (see [`fluid-classification.md`](./fluid-classification.md)): you
swam in it, it fogged and sounded like water, and it did not lift or drain you.

## How it works

### The four constants

From `Entity.handleOnInsideBubbleColumn` / `handleOnAboveBubbleColumn`
(`Entity.java:2851-2898`). All four are `double` arithmetic against `double`
literals — there is no `f32` narrowing anywhere in this feature.

| | `drag=false` (soul sand, up) | `drag=true` (magma, down) |
|---|---|---|
| **inside** | `min(0.7, vy + 0.06)` | `max(-0.3, vy - 0.03)` |
| **above** (open air over the cell) | `min(1.8, vy + 0.1)` | `max(-0.9, vy - 0.03)` |

Note the asymmetry: the drag-down **step** is `-0.03` in both rows and only the
*clamp* widens, whereas the push-up step changes too. "The above case is three
times stronger" is wrong in one of the two columns, which is why this is a table
and not a sentence.

### Where it runs, and why not in `tick_water`

The issue expected this in `tick_water`. It does not go there.
`BubbleColumnBlock.entityInside` is reached from `applyEffectsFromBlocks`, which
`LivingEntity.aiStep` calls **after** `travel()` (`LivingEntity.java:3130` then
`:3134`, with `pushEntities()` after at `:3163`). So it sits in
`travel_and_check_inside_blocks`, immediately after the travel dispatch, next to
`update_stuck_multiplier` — the other `Block.entityInside` effect this crate
models, reached from the same single vanilla call.

The consequence is observable and is pinned by a test: **the impulse is applied
after the move, so it is integrated on the *next* tick.** Tick 0's position in a
column is bit-identical to tick 0's position in plain water; the divergence shows
up in tick 0's *velocity* and tick 1's position.

Their order relative to each other is unobservable: vanilla visits each cell once
and a cell is either a bubble column or a stuck-in-block, never both; beyond that
they touch disjoint state, sharing only `fall_distance`, which both only ever set
to `0.0`.

### One impulse per *cell*, not per tick

This is the part that surprises. `Entity.checkInsideBlocks` visits every block
position the movement intersects, dedupes by position, and calls `entityInside` on
each — and `BubbleColumnBlock` applies its impulse **immediately inside that
callback**, rather than deferring it to the `InsideBlockEffectApplier` the way fire
and freezing do. A standing player is `1.8` high, spans two cells, and therefore
takes **two** impulses every tick.

The clamp is what keeps that from running away. A port applying one impulse per
tick would converge on the same terminal velocity and climb to it at *half the
rate* — a sub-tick divergence of exactly the kind the server's movement check
accumulates. `tests/bubble_column.rs` isolates it: tick 0's velocity is the
plain-water baseline plus exactly `2 × 0.06`.

### The inside/above branch

`BubbleColumnBlock.entityInside` (`BubbleColumnBlock.java:56-64`) inspects the cell
**above** the column cell. If its collision shape is empty *and* its fluid state is
empty — real air — the stronger surface pair applies. Anything else (more column,
water, a solid lid) takes the inside pair.

The fluid half makes the common cases fall out with no special-casing: a bubble
column's own `getFluidState` is a **water source** (`BubbleColumnBlock.java:73-75`),
so a cell with more column above it reports "not open air" automatically. Only the
very top of a column sees the surface pair.

`cell_is_open_air` is the helper. **One vanilla quirk is deliberately not
reproduced**: vanilla calls `stateAbove.getCollisionShape(level, pos)` — passing
`pos`, the *lower* cell, while asking the *upper* cell's state. That is a genuine
mismatch in the game's own source, unobservable for every block whose shape does
not vary with position, and this position-keyed seam cannot express it anyway.

### The seam and the base block

`CollisionView::bubble_column(x, y, z) -> Option<bool>` returns the `DRAG_DOWN`
property, `None` for every non-column cell. It defaults to `None`, which is what
made the change provably inert for every existing implementor.

**The base block is not the seam's business.** Vanilla resolves soul sand versus
magma **once, at block-update time**, into this single boolean
(`BubbleColumnBlock.getColumnState`); the entity-side code only reads the boolean
and never looks below the column. In 26.2 the two tags have exactly one member
each — `ENABLES_BUBBLE_COLUMN_PUSH_UP` is `soul_sand`,
`ENABLES_BUBBLE_COLUMN_DRAG_DOWN` is `magma_block`.

So **there is no "doubled if a magma block is the base" term**, despite issue
#199's text saying there is. That claim was checked against `Entity.java` and
`BubbleColumnBlock.java` and is not in either.

### Reaching the screen

Physics is version-free, so the property arrives by seam:

```
player_abilities-free chain:
  ChunkSection state id
    -> LiveCollision::bubble_column_of        (shell/collision.rs)
    -> VersionAdapter::block_bubble_column_drag  (model/adapter.rs, default None)
    -> V770Adapter                            (v770/adapter.rs)
    -> lodestone_data::block_states::properties(id)  ["drag"]
```

The two states are `15294` (`drag=true`, the block's default) and `15295`
(`drag=false`) in the 26.2 global palette, per Mojang's own
`generated/reports/blocks.json`.

`WorldCollision` (the offline demo palette) answers `None` — that world has ten
full cubes, air and water, and no bubble column to report.

## How to change it

- **The impulse and its branch**: `apply_bubble_column` in
  `crates/lodestone-physics/src/player.rs`. If you change any of the four
  constants you must change `gen_golden.py`'s `apply_bubble_column` identically and
  regenerate, or the bit-exact replay fails — which is the point.
- **A new base block** (a datapack adding to either tag): nothing here changes. The
  server resolves it into `drag` before the state ever reaches us.
- **A new consumer** (mobs, boats): the impulse is on the player pipeline only.
  `Entity.handleOn*BubbleColumn` is `static` and entity-agnostic in vanilla, so
  lifting it to `lodestone-physics::entity` is the natural move if a mob needs it.

### Gotchas

- **A dry bubble column is not a world vanilla can build.** Both test-world helpers
  (`golden.rs`'s and `gen_golden.py`'s `add_bubble_column`) register the cell as
  water as well, deliberately, so no fixture can construct one. A fixture that did
  would be the *world* species of vacuous test.
- **The `resetFallDistance` asymmetry is real but unproven.** The inside branch
  resets fall distance (`Entity.java:2897`); the above branch does not (`:2865`).
  It is coded faithfully and **no test pins it**, because it cannot be observed: any
  player in a column has already been through `tick_water`, which zeroes
  `fall_distance` unconditionally. The only world that would separate them is the
  dry column above. It is left deliberately unproven rather than tested against a
  scene the game cannot produce.
- **Cell enumeration is the post-move box, not the swept path.** Vanilla walks
  `forEachBlockIntersectedBetween`, so a player moving fast enough to pass *through*
  a cell without ending inside it still takes its impulse. `apply_bubble_column`
  enumerates the post-move bounding box only — the same approximation
  `update_stuck_multiplier` has always made for the *same* vanilla call, kept
  identical so neither is quietly better than the other. Unobservable for a player
  riding a column (≤ `0.7`/tick, well under a cell); it would matter for one
  launched through the top at `1.8`/tick.
- **The deflation constant is `1.0e-5`, not vanilla's `1.0E-5F`.** Vanilla deflates
  by a *float* literal, which widens to `1.0000000116860974e-5`. Both functions use
  the `f64` `1.0e-5` for consistency with each other. It can only change an answer
  when a box edge lies within `1.2e-13` of a cell boundary.
- **`Player`'s `!abilities.flying` gate is not applied.** Both overrides
  (`Player.java:310-321`) skip the impulse entirely for a flying player. This crate
  has no abilities state — see issue #191 — so the conjunct is vacuously true. A
  driver with a flight mode must not route a flying player through `tick`.

## Configuration

None. No feature flags, no env vars, no tunables. The four constants are vanilla
literals and the property comes from generated data.

## Dependencies

- `lodestone-physics` — `player::apply_bubble_column`, `collision::CollisionView`.
- `lodestone-model` — `VersionAdapter::block_bubble_column_drag` (the seam).
- `lodestone-v770` — the 26.2 implementation, over `lodestone-data`.
- `lodestone-data` — `block_states::{block_name, properties}`.
- `lodestone-shell` — `collision.rs`'s `LiveCollision` / `WorldCollision`.

## Tests

| file | what it proves |
|---|---|
| `lodestone-physics/tests/golden.rs` | four scenarios replayed **bit-for-bit** against the independent Python oracle: `bubble_column_up`, `bubble_column_down`, `bubble_column_surface_launch`, and `bubble_column_water_control` |
| `lodestone-physics/tests/bubble_column.rs` | each constant in isolation, as a *difference* between two worlds identical but for the column, so drag and buoyancy cancel |
| `lodestone-v770/tests/bubble_column_seam.rs` | the property reaches a version-free consumer **through the trait object**, and only the two bubble-column states answer |

### The controls, and that they were watched to fail

- `bubble_column_water_control` is the same shaft with plain water. The player
  *sinks* there and *rises* in the column, from identical geometry and no input.
- `plain_water_baseline_sinks` guards every difference assertion: without it, a
  fixture whose water was mis-registered would still satisfy `0.0 + 0.06 == 0.06`.
- `above_branch_requires_open_air_over_the_cell` runs three lids (air, water, solid)
  and shows only air takes the strong step, with water and solid landing on the
  *same* answer — the two halves of `nothingAbove` are an AND.
- `no_other_state_reports_a_bubble_column` scans all 32,366 states. With the trait
  default in place the count is zero, not two, so this also catches a missing
  override — the island failure mode.
- **Negative control, run and observed:** `apply_bubble_column`'s call site was
  neutered and the suite re-run. 5 of 8 unit tests and 3 of 4 golden traces went
  red. The three unit tests that stayed green are the ones that must
  (constant-ordering, the absence-assertion, the baseline guard), and
  `bubble_column_water_control` stayed green because it genuinely has no column.
  The other 44 golden traces stayed green, confirming the change is inert for
  pre-existing scenarios. `player.rs` was restored from a scratchpad copy and md5
  verified against the pre-neuter hash.
- **Zero-deletion control:** all 43 pre-existing statics carry over with all
  **30,360** hex literals byte-identical; 4 statics added, none missing, none
  changed.

  **The first attempt at this control was wrong, and wrong in the reassuring
  direction.** It was `diff old new | grep -c '^<'`, which reported `0`. The real
  answer at that moment was ~15,000: `gen_golden.py` emits one line per tick, while
  the committed file is `cargo fmt`-reflowed to four, so the regenerated file was a
  third of the length with identical data. The pipeline had been mangled — `rtk`
  intercepts and rewrites commands, and the `grep` never saw `diff`'s real output.
  It surfaced only when `git diff --cached --numstat` reported **20,251 deletions**
  on a change that deletes nothing.

  Two lessons, both already in `CLAUDE.md` and both re-earned here: *the transform
  that makes output readable is also the transform that can invent a green*, and a
  golden-file diff must be compared **semantically** (parse out the literals) rather
  than by line, because formatting is not data. The control is now a Python parse
  that keys statics by name and compares literal lists; the file is formatted with
  `rustfmt` on **that path only** (never a bare `cargo fmt`, which in this shared
  checkout would reflow other agents' in-flight files) so the committed diff is
  purely additive.
