# Climbing (scaffolding vs. ladder) and powder-snow freezing

## What it is

Two small, related additions to `lodestone-physics`, plus a correctness fix
that both lean on: scaffolding's distinct climb/descend behaviour (#210),
powder-snow freezing (#212), and a swept-segment fix to the "which block is
the player standing inside" sweep both consume (#216).

## How it works

### Scaffolding vs. a ladder (#210)

Both blocks are `BlockTags.CLIMBABLE`, so `CollisionView::is_climbable` is
identical for the two — a coarse yes/no this crate has always had. The one
place vanilla actually distinguishes them is
`LivingEntity.handleOnClimbable`'s sneak-to-hold clamp
(`LivingEntity.java:2693-2703`):

```java
double yd = Math.max(delta.y, -0.15F);
if (yd < 0.0 && !this.getInBlockState().is(Blocks.SCAFFOLDING)
    && this.isSuppressingSlidingDownLadder() && this instanceof Player) {
    yd = 0.0;
}
```

On a ladder, sneaking while moving down clamps `yd` to `0.0` — the classic
"hold in place" behaviour. On scaffolding the `!is(SCAFFOLDING)` conjunct is
`false`, so the clamp never engages: sneaking on scaffolding still descends,
capped at the ordinary `-0.15`/tick climb speed, exactly like *not* sneaking.

`CollisionView` gained a narrow new hook, `is_scaffolding`, alongside the
existing `is_climbable` (`crates/lodestone-physics/src/collision.rs`) rather
than widening `is_climbable` itself — every other `CollisionView`
implementer is unaffected by construction (default `false`). The consumer is
`travel_in_air` (`crates/lodestone-physics/src/entity.rs`), which already
queries `is_climbable` at the entity's in-block position; it now queries
`is_scaffolding` at the same cell and composes
`ctx.suppress_ladder_slide && !on_scaffolding` before calling
`handle_on_climbable`.

**What is not modelled**: vanilla's other scaffolding-specific behaviour, the
stable/unstable **collision-shape** toggle
(`ScaffoldingBlock.getCollisionShape`, `ScaffoldingBlock.java:137-147`) that
lets a sneaking player fall *through* the platform instead of standing on it.
That depends on the entity's sneak state and vertical approach direction at
query time, which `CollisionView::collision_boxes` has no parameter for —
modelling it would mean widening every implementer's shape query with a
descending/approach context almost nothing else needs. Left as a documented
gap on `CollisionView::is_scaffolding`'s own doc.

### Powder-snow freezing (#212)

Vanilla's rule is two separate pieces glued by one flag:

```java
// InsideBlockEffectType.FREEZE, applied once per tick the swept segment
// (checkInsideBlocks) finds powder snow, via PowderSnowBlock.entityInside:
setIsInPowderSnow(true);
if (canFreeze()) ticksFrozen = min(ticksRequiredToFreeze, ticksFrozen + 1);

// LivingEntity.aiStep, end of tick, unconditional:
if (!isInPowderSnow || !canFreeze()) ticksFrozen = max(0, ticksFrozen - 2);
if (tickCount % 40 == 0 && isFullyFrozen() && canFreeze())
    hurt(damageSources().freeze(), 1.0F);
```

`PlayerState` gained `frozen_ticks: u32` (0..=140,
`PlayerState::TICKS_REQUIRED_TO_FREEZE`), maintained by a new
`update_freezing` in `crates/lodestone-physics/src/player.rs`, plus reader
methods: `is_freezing`, `is_fully_frozen`, `percent_frozen` (the `0..1`
vignette ramp), and `should_apply_freeze_damage(tick_count)`.

**`update_freezing` runs unconditionally — not inside the `!flying` gate**
that guards the stuck-multiplier slowdown and the bubble-column impulse.
`Player.makeStuckInBlock` (`Player.java:1515-1518`) is what suppresses
`stuck_multiplier` while flying; nothing overrides `InsideBlockEffectType
.FREEZE`, so a creative-flying player drifting through powder snow still
accumulates `frozen_ticks` with none of the `(0.9, 1.5, 0.9)` drag. This is
the one behavioural surprise worth remembering if you go looking for where
freezing is gated and don't find a `!flying` check — there isn't one, on
purpose. `tests/freezing.rs`'s
`freezing_is_not_suppressed_by_flying_even_though_the_stuck_drag_is` pins it
with both halves measured in one test (frozen_ticks equal, stuck multipliers
different).

`is_powder_snow` is `CollisionView`'s new narrow hook, separate from
`stuck_multiplier` for exactly the reason above: the two answer different
vanilla questions over the same block, on different gates.

**What this crate does not do: apply the actual damage.** This crate has no
health/damage model anywhere (fall damage isn't computed here either — see
`PlayerState::fall_distance`'s doc); `tick_count` is likewise absent, since
every timer this crate owns is a countdown against local state, never a
count against the world's absolute clock. `should_apply_freeze_damage`
exposes the *predicate*, parameterised on a driver-supplied `tick_count`, so
a driver that does track wall-clock ticks (and does own health/damage — this
is server-authoritative in a from-scratch client, so likely never a local
concern at all) gets a correct, ready rule rather than a formula to
re-derive.

### The swept-segment fix both rely on (#216)

`update_stuck_multiplier` (which `stuck_multiplier` and, now,
`is_powder_snow` both key off through a shared `for_each_swept_cell` helper)
used to sample only the **post-move resting bounding box** — vanilla walks
the whole swept segment (`Entity.checkInsideBlocks(from, to, …)`). A mover
fast enough to pass *through* a one-block-thick cobweb or powder-snow layer
within a single tick, without ever resting inside it, took no effect at all.

`for_each_swept_cell` (`crates/lodestone-physics/src/player.rs`) now scans
the union of the pre- and post-move bounding boxes — a conservative superset
of the cells vanilla's DDA visits — and narrows to the cells the box
*actually swept through* via `segment_hits_cell`, a specialisation of
`AABB.collidedAlongVector` (`AABB.java:401-417`) to a single unit cell and a
boolean result. `tests/stuck_movement.rs`'s
`a_fast_faller_is_grabbed_by_a_one_block_layer_it_never_rests_in` constructs
exactly that geometry (with sanity assertions proving neither the pre- nor
post-move box overlaps the layer) and its sibling
`a_faller_that_never_crosses_the_layer_is_not_grabbed` is the negative
control.

`apply_bubble_column` was **not** given the same fix — nothing in #216 asked
for it, and the case it serves (a player riding a column, displacement at
most `0.7`/tick) doesn't need it. It still samples the resting box only; see
its own "Divergences" doc note, which used to say the two functions were
*deliberately* kept identical and now explains why they no longer are.

## How to change it

- A new stuck-triggering or presence-only block property follows the same
  pattern: a narrow `CollisionView` hook (default `false`/`None`), a
  name-keyed free function in `lodestone-shell/src/collision.rs`
  (`is_scaffolding_at`/`is_powder_snow_at` are the templates), wired into
  both `WorldCollision` and `LiveCollision`.
- If `apply_bubble_column` ever needs the same swept-segment treatment
  (a player launched *through* the top of a column at `1.8`/tick), reuse
  `for_each_swept_cell` rather than re-deriving the sweep a third time.
- `should_apply_freeze_damage` is unused inside this crate by design — it is
  a rule for a driver to call, not a self-contained effect. If a driver
  wires local damage prediction, this is the one function it needs; do not
  add health state to `PlayerState` to "finish" this, that decision belongs
  to whichever layer owns combat/health (out of `lodestone-physics`'s scope
  entirely in this codebase).

## Configuration

None — no flags or constants beyond the two vanilla numbers already cited
(`0.15` climb-speed cap, `140` ticks to freeze,
`PlayerState::TICKS_REQUIRED_TO_FREEZE`).

## Dependencies

- `lodestone-physics` — `CollisionView::{is_scaffolding, is_powder_snow,
  is_climbable, stuck_multiplier}`, `PlayerState::frozen_ticks`,
  `entity::travel_in_air`, `player::{update_freezing,
  update_stuck_multiplier, for_each_swept_cell, segment_hits_cell}`.
- `lodestone-shell/src/collision.rs` — the two `CollisionView` adapters that
  answer `is_scaffolding`/`is_powder_snow` by block name.
