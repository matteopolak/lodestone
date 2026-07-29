# Collision shapes

## What it is

The route from the version crate's per-block-state **collision census** to the
physics engine's `CollisionView`, so the player collides with the shapes vanilla
actually uses — a bottom slab is 8/16 tall, a fence is 1.5 blocks tall, cobweb has
no collision at all — instead of one unit cube per block that happens to occlude.

Two adapters implement `CollisionView`, both in
`crates/lodestone-shell/src/collision.rs`:

| adapter | world | id space |
|---|---|---|
| `WorldCollision` | offline demo world (`lodestone_world::World`) | the shell's 10-block demo palette |
| `LiveCollision` | snapshot of the live server world | vanilla block-state ids |

## Why it mattered

`LiveCollision` used to emit `if is_solid(cell) { push(unit cube) }` and implement
**3 of `CollisionView`'s 13 methods**. In live play that meant *no slabs, stairs,
fences, walls, ice, ladders, cobwebs, soul sand or slime blocks in collision at
all*: the player stood on top of a bottom slab at `y + 1.0` rather than `y + 0.5`,
and stopped 0.375 blocks short of a fence post.

26.2's server replays our movement delta through `move(MoverType.PLAYER, …)` and
rubber-bands as soon as horizontal disagreement passes **0.25 blocks in a single
packet, with no accumulator** (see [`baritone-port.md`](./baritone-port.md) §3.2).
`lodestone-physics` is bit-exact against two independent oracles across 26
zero-tolerance golden traces, so the integrator was never the suspect — it was
being fed a world in which slabs did not exist. Both figures above exceed the bar,
on the first tick of contact.

Measured, runnable, in
`crates/lodestone-physics/tests/partial_block_shapes.rs`.

## How it works

### The data

The census lives in `crates/protocol/v770/src/generated/collision_shapes.rs`,
generated from a dump of `getCollisionShape(...).toAabbs()` over all 32,366 states
of the real 26.2 server (`Block.BLOCK_STATE_REGISTRY`). Pure rodata: `STATE_SHAPE:
[u16; 32366]` maps a state to one of **326 distinct shapes**, and `SHAPES: [&[Aabb];
326]` points at 716 de-duplicated boxes. See
`crates/protocol/v770/tests/collision_shapes.rs` for the generator, the drift
guard, and `LODESTONE_REGEN=1`.

The census was already complete and already tested when this work started. **It
simply was not routed anywhere.** Nothing needed generating.

### The seam

Block-state ids are renumbered every version, so the shell reaches the census the
same way it reaches hardness — through `lodestone_model::VersionAdapter`:

```rust
fn block_collision(&self, state_id: u32) -> Option<&'static [BlockAabb]>;
fn block_name(&self, state_id: u32) -> Option<&'static str>;
```

`BlockAabb` (`lodestone-model/src/adapter.rs`) is the shared block-local box type,
and `lodestone_v770::collision_shapes::Aabb` is a **type alias** for it — not a
copy. That is what makes the seam zero-copy: it hands back the rodata slice itself
rather than converting box by box in the innermost loop of the physics tick.

`block_name` exists because six of the answers are *not* geometry. `friction`,
`speedFactor`, `jumpFactor`, `bounceRestitution`, `makeStuckInBlock` and
`BlockTags.CLIMBABLE` are `BlockBehaviour.Properties` fields and tag memberships,
so no shape census can carry them. They are keyed by block name — stable across
versions in a way state ids are not — in `collision.rs`, with the decompiled
`Blocks.java` line number cited for every value.

### Lookup cost

Per queried cell, `collision_boxes` is:

1. one `HashMap<(i32,i32,usize), Arc<ChunkSection>>` lookup + a paletted-container
   read (pre-existing, unchanged);
2. one virtual call through `Arc<dyn VersionAdapter>`;
3. one bounds-checked index into `STATE_SHAPE` and one into `SHAPES`;
4. a push per box (1 for most blocks, 2–3 for stairs, 1 for a free-standing fence).

**No allocation, no scan, no cache to invalidate**, and the returned slice is
`&'static`. `collision_top` is overridden for the same reason: the trait's default
allocates a `Vec` on *every* call to derive the top from the boxes, and a
pathfinder asks it for every candidate cell.

### Keeping the two adapters honest

`WorldCollision` and `LiveCollision` are two implementations of one trait, so a
disagreement between them is a bug that hides — tests pass against one while the
game misbehaves against the other. They are kept consistent **structurally**: every
one of the 13 answers is computed once, in a free function over a private
`BlockView` trait, and each `impl CollisionView` block is nothing but one-line
delegation. The only things an adapter supplies differently are the state id at a
cell, the shape of a state, its fluid, and its vanilla block name — and the demo
palette maps its 10 ids onto real vanilla names so that even the name-keyed tables
are one shared code path rather than a stub on one side.

### Status of the 13 methods

Real (10): `collision_boxes`, `collision_top`, `friction`, `speed_factor`,
`jump_factor`, `is_water`, `is_climbable`, `is_lava`, `stuck_multiplier`,
`bounce_restitution`.

Real with a stated approximation (3):

- **`fluid_at`** — kind, amount and falling from `BlockModels::fluid`, live only.
  `WorldCollision` returns `None` deliberately: the demo palette's water is a
  single property-less id with no `level`, so there is no amount to report, and
  fabricating "source, amount 8" would invent a current in flat demo lakes.
- **`blocks_motion`** — `BlockState.blocksMotion()` is
  `block != COBWEB && block != BAMBOO_SAPLING && legacySolid`, and `legacySolid` is
  a *separate* cached boolean that 26.2 overrides on **143 blocks with
  `forceSolidOn()` and 8 with `forceSolidOff()`**. No committed table carries that
  flag, so it is derived from the shape here (`calculateSolid`: mean bounding-box
  dimension ≥ 0.7291666… or Y size ≥ 1.0), which is wrong for those 151: signs,
  pressure plates, open fence gates, lanterns, chains, corals and turtle eggs read
  as *not* blocking; snow, azalea, big dripleaf, chorus plant and end rod read as
  blocking. Ladder is hard-coded off because a player meets it constantly, and
  because its mean size is *exactly* the threshold constant — that is why vanilla
  needs the override at all. Blast radius: one consumer,
  `lodestone_physics::get_flow`'s empty-neighbour branch. Nothing in player
  movement reads it.
- **`is_solid_face`** — `isFaceSturdy(FULL)` is approximated by "does any *single*
  box cover the whole face", where vanilla unions the shape first. No false
  positives, possible false negatives. Also, the seam does not say which fluid is
  flowing, so any fluid in the cell answers `false` (vanilla only excludes the
  same fluid). One consumer: a falling fluid's downward jet.

## How to change it

- **Adding a name-keyed constant** — add a row to the matching `*_for` function in
  `collision.rs` and cite the `Blocks.java` line. Do not add a second lookup path;
  both adapters read the same function.
- **A data bump (new MC version)** — regenerate the census per
  `crates/protocol/v770/tests/collision_shapes.rs`, then re-read
  `data/minecraft/tags/block/suppresses_bounce.json`. Its only member today is
  `honey_block`, which sets no restitution, so `bounce_restitution` ignores the tag;
  a future bouncy suppressor would break that **silently**.
- **Closing the `blocks_motion` gap** — dump `legacySolid` (or the `forceSolid*`
  flags) as a column beside the collision census and add a seam for it.

### Gotchas

- **Collision shape is not outline shape.** Three different vanilla shapes answer
  three different questions and they genuinely disagree: a fluid has a
  collision-less cell *and* an empty outline; kelp has an outline (so it is
  breakable) and no collision; soul sand collides to 0.875 and outlines to 1.0.
  Nothing in this module may decide what the crosshair selects — that is
  `LiveCollision::is_pickable`, and its real fix is an outline/interaction census
  beside the collision one (in flight separately, as
  `VersionAdapter::block_outline` / `block_interaction`). **The seam is exactly
  there: `is_pickable` must move to `block_outline`, and `collision_boxes` must
  never be offered as a substitute — kelp would stop being breakable.**
- **`max_y` is not capped at 1.0.** A fence is 1.5, and the 0.6 auto-step *cannot*
  mount it. Clamping makes fences look step-able and routes navigation straight
  through them.
- **Occlusion is not collision.** `LiveCollision::is_solid` (the vanilla
  classifier's `occludes`) remains, but only as the occlusion predicate and as the
  no-version-data fallback shape. Using it *as* the collision shape is the entire
  original defect.
- **A degraded view says so.** With no version data the view falls back to a unit
  cube per occluding block (a player who cannot stand up is worse than one standing
  slightly too high), but it logs once at `warn` and `LiveCollision::has_real_shapes()`
  reports it. A silent fallback is how this survived.
- **`fluid_at` being real turns fluid *push* on in live play** for the first time.
  It is bit-exact in `lodestone-physics` with its own tests, but it is a behaviour
  change beyond collision geometry — expect to be pushed by flowing water.

## Configuration

- **`--features live`** compiles a version family into the registry. Without it
  there is no census, and the view degrades to unit cubes (logged).
- `LiveCollision::new` infers the census from the **sole** compiled-in family;
  `LiveCollision::with_version_data(Some(adapter))` is the explicit form and should
  be preferred by any caller that knows the connected protocol.
- `LODESTONE_ASSETS` — the pack root the vanilla atlas (and hence the fluid
  classifier) loads from. Required by the `#[ignore]`d gates.

## Dependencies

- `lodestone-physics` — `CollisionView`, `Aabb`, `FluidCell`, `HorizontalDir`.
- `lodestone-model` — `BlockAabb`, `VersionAdapter`.
- `lodestone-registry` — resolves the adapter without naming a version crate.
- `lodestone-render` — `BlockAtlas` (occlusion fallback) and `BlockModels::fluid`
  (the one fluid classifier, shared with the mesher).
- `lodestone-world` — `ChunkSection` / `World` block reads.

## Gates

| gate | where | runs |
|---|---|---|
| shape helpers vs vanilla's own numbers | `collision.rs::tests::shape_helpers_match_vanilla_on_hand_written_shapes` | always |
| name-keyed constants vs decompiled values | `collision.rs::tests::name_keyed_constants_match_the_decompiled_values` | always |
| the census reaches `CollisionView` | `collision.rs::tests::the_real_per_state_collision_census_reaches_the_collision_view` | `#[ignore]` — needs the pack **and** `--features live` |
| name-keyed constants reach the view | `collision.rs::tests::name_keyed_constants_reach_the_view_through_the_version_seam` | `#[ignore]` — same |
| what unit cubes cost, measured | `lodestone-physics/tests/partial_block_shapes.rs` | always |

Every one carries a control that must fail the same assertion: bottom slab vs top
slab, empty-shape vs cube, and — in both the shell gate and the physics gate — the
**pre-fix view itself**, asserted to be wrong by a stated amount (0.5 blocks
vertically on a slab, 0.375 horizontally on a fence).
