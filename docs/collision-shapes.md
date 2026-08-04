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

The census lives in `crates/lodestone-data/src/generated/collision_shapes.rs`,
generated from a dump of `getCollisionShape(...).toAabbs()` over all 32,366 states
of the real 26.2 server (`Block.BLOCK_STATE_REGISTRY`). Pure rodata: `STATE_SHAPE:
[u16; 32366]` maps a state to one of **326 distinct shapes**, and `SHAPES: [&[Aabb];
326]` points at 716 de-duplicated boxes. See
`crates/lodestone-data/tests/collision_shapes.rs` for the generator, the drift
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
and `lodestone_data::collision_shapes::Aabb` is a **type alias** for it — not a
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
one of the 16 answers is computed once, in a free function over a private
`BlockView` trait, and each `impl CollisionView` block is nothing but one-line
delegation. The only things an adapter supplies differently are the state id at a
cell, the shape of a state, its fluid, and its vanilla block name — and the demo
palette maps its 10 ids onto real vanilla names so that even the name-keyed tables
are one shared code path rather than a stub on one side.

### Status of the 16 methods

(Was 13 as of the original fix this doc describes; `bubble_column` (#199),
`is_scaffolding` and `is_powder_snow` (#210/#212) were added since — same
structure, so they are folded into the counts and tables below rather than
kept as a separate history.)

Real (13): `collision_boxes`, `collision_top`, `friction`, `speed_factor`,
`jump_factor`, `is_water`, `is_climbable`, `is_lava`, `stuck_multiplier`,
`bounce_restitution`, `blocks_motion`, `is_scaffolding`, `is_powder_snow`.

- **`is_scaffolding`** and **`is_powder_snow`** are name-keyed identity checks
  (`is_scaffolding_at`/`is_powder_snow_at` in `collision.rs`, matching
  `v.name_of(state) == Some("minecraft:scaffolding"|"minecraft:powder_snow")`),
  not routed through `physics_at`'s `BlockPhysics` table like the other
  name-keyed answers — each is a single-block identity, not a `Properties`/tag
  fold shared with anything else `physics_at` returns. `is_scaffolding` exists
  because `is_climbable` cannot: scaffolding and a ladder share the same
  `BlockTags.CLIMBABLE` membership but differ in one downstream behaviour
  (`LivingEntity.handleOnClimbable`'s sneak-to-hold clamp — see
  `PlayerState`'s `entity.rs::travel_in_air` climbing block). `is_powder_snow`
  exists because `stuck_multiplier` answers a different vanilla question over
  the same block — see `CollisionView::is_powder_snow`'s doc for why the two
  cannot be collapsed into one (a flying player keeps freezing in powder snow
  even though the stuck-multiplier drag is suppressed).

- **`blocks_motion`** was itself a derived approximation until `24af787`. It is
  now dumped per state — `BlockState.blocksMotion()`
  (`block != COBWEB && block != BAMBOO_SAPLING && legacySolid`) — straight off the
  real server, through `VersionAdapter::block_blocks_motion` →
  `lodestone_data::block_solidity` (`crates/lodestone-data/src/generated/block_solidity.rs`,
  the same dump-and-commit shape as the collision census). `legacySolid` is a
  *separate* cached boolean 26.2 overrides on 237 blocks with `forceSolidOn()`, 8
  with `forceSolidOff()`, and leaves un-cached on 23 `dynamicShape()` blocks — none
  of the three has a getter or appears in `blocks.json`, so no shape census could
  ever have carried them; that is the whole reason this table exists.
  `blocks_motion_at` (`collision.rs`) still carries the old geometry derivation
  (`calculateSolid`: mean bounding-box dimension ≥ 0.7291666… or Y size ≥ 1.0, plus
  the three name exclusions cobweb/bamboo_sapling/ladder) — but only as a
  **fallback** for when `self.version` is `None` (no census reachable) or for
  `WorldCollision`'s 10-block demo palette, whose blocks are all full cubes or air
  and for which the fallback is exact. Used against real 26.2 data instead of the
  census, that fallback is wrong for **2,618 of 32,366 states across 202 blocks**
  (`crates/lodestone-data/tests/block_physics.rs::the_shipped_shape_derivation_gets_a_measured_set_of_blocks_wrong`):
  signs, pressure plates, chains, banners, lanterns, turtle eggs, conduits and dead
  coral read as *not* blocking; azalea, flowering azalea, big dripleaf, chorus
  plant/flower, end rod, snow and scaffolding read as blocking. **Cobweb, bamboo
  sapling and ladder are not part of that wrongness** — the fallback's three
  hard-coded name exclusions get all three right on their own, cobweb and bamboo
  sapling because they match vanilla's exclusion list verbatim and ladder because
  it is separately hard-coded off (its mean extent sits *exactly* on the threshold
  constant, which is why the constant is what it is). Blast radius of a wrong
  answer here is still small — one consumer, `lodestone_physics::get_flow`'s
  empty-neighbour branch; nothing in player movement reads it.

Real with a stated approximation (2):

- **`fluid_at`** — kind, amount and falling from `BlockModels::fluid`, live only.
  `WorldCollision` returns `None` deliberately: the demo palette's water is a
  single property-less id with no `level`, so there is no amount to report, and
  fabricating "source, amount 8" would invent a current in flat demo lakes.
- **`is_solid_face`** — `isFaceSturdy(FULL)` is approximated by "does any *single*
  box cover the whole face", where vanilla unions the shape first. No false
  positives, possible false negatives. One consumer: a falling fluid's downward
  jet. **Fixed in #216**: the seam now carries `kind`, the fluid asking the
  question (`FlowingFluid.isSolidFace` is an instance method on the flowing
  fluid, not a free function over the neighbour cell), so only a neighbour
  holding the *same* fluid answers `false` — a waterlogged solid block asked by
  a *different* fluid's falling jet now correctly falls through to the
  sturdy-face check instead of being forced to `false` by "any fluid present".

## How to change it

- **Adding a name-keyed constant** — these no longer live in `collision.rs`. Add a
  match arm to `lodestone_model::block_physics` (`crates/lodestone-model/src/adapter.rs`)
  and cite the `Blocks.java` line; `collision.rs`'s `physics_at` just calls
  `block_physics(name)` and delegates. Do not add a second lookup path — both
  adapters, and any plugin depending on `lodestone-model` directly, read the same
  function. See [`docs/plugin-api.md`](./plugin-api.md) for why this table is
  version-free (name-keyed, not state-keyed) and reachable from outside
  `lodestone-shell`.
- **A data bump (new MC version)** — regenerate the census per
  `crates/lodestone-data/tests/collision_shapes.rs`, then re-read
  `data/minecraft/tags/block/suppresses_bounce.json`. Its only member today is
  `honey_block`, which sets no restitution, so `bounce_restitution` ignores the tag;
  a future bouncy suppressor would break that **silently**.
- **The `blocks_motion` gap is closed** (`24af787`) — it is dumped, not derived; see
  "Status of the 13 methods" above. What is still open is only its fallback path
  (no version data, or the demo palette) and its one consumer
  (`lodestone_physics::get_flow`).

### Gotchas

- **Collision shape is not outline shape.** Three different vanilla shapes answer
  three different questions and they genuinely disagree: a fluid has a
  collision-less cell *and* an empty outline; kelp has an outline (so it is
  breakable) and no collision; soul sand collides to 0.875 and outlines to 1.0.
  Nothing in this module may decide what the crosshair selects — that is
  `LiveCollision::pick_boxes`, and its real fix was an outline/interaction census
  beside the collision one, `VersionAdapter::block_outline` / `block_interaction`.
  **That has since landed (`196d385`), and the ray now clips those boxes rather
  than the cell (issue #375):** `pick_boxes` emits `block_outline`'s geometry and
  `crate::raycast::raycast` intersects it, so a `1/16`-tall leaf litter is only
  targetable where it actually is — see
  [`docs/block-outline-shapes.md`](./block-outline-shapes.md). `collision_boxes`
  was never offered as a substitute for either; kelp stays breakable.
- **Nor may it decide the *distance* to what the crosshair selects.** The entity
  pick shortens its search radius to the block hit's entry distance, and that
  distance has to come from the outline box the ray struck (`RayHit::distance`),
  not from this module's boxes and not from a cube around the cell.
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
- `LiveCollision::new` takes `version: Option<Arc<dyn VersionAdapter>>` as a
  required constructor parameter (issue #42 — it used to reach for
  `collision::inferred_version_data()` internally instead of receiving it).
  `Sim::live_collision` passes `crate::collision::inferred_version_data()`
  explicitly, which infers the census from the **sole** compiled-in family; a
  caller that already knows the connected protocol's adapter should pass that
  instead. `LiveCollision::with_version_data(Some(adapter))` remains for
  overriding it after construction (the degraded-view test fixtures use it
  that way).
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
| name-keyed constants vs the shared model table | `collision.rs::tests::name_keyed_constants_come_from_the_shared_model_table` | always |
| the census reaches `CollisionView` | `collision.rs::tests::the_real_per_state_collision_census_reaches_the_collision_view` | `#[ignore]` — needs the pack **and** `--features live` |
| name-keyed constants **and `blocks_motion` routing** reach the view | `collision.rs::tests::name_keyed_constants_reach_the_view_through_the_version_seam` | `#[ignore]` — same |
| what unit cubes cost, measured | `lodestone-physics/tests/partial_block_shapes.rs` | always |
| the `blocks_motion` census itself, exhaustively, vs the JVM dump | `crates/lodestone-data/tests/block_physics.rs` (`committed_table_matches_the_committed_dump`, `blocks_motion_differs_from_legacy_solid_on_exactly_cobweb_and_bamboo_sapling`, `the_geometry_branch_alone_is_wrong_for_two_thousand_states`, `the_shipped_shape_derivation_gets_a_measured_set_of_blocks_wrong`, `hand_checked_solidity_rows`) | always |

Every one carries a control that must fail the same assertion: bottom slab vs top
slab, empty-shape vs cube, and — in both the shell gate and the physics gate — the
**pre-fix view itself**, asserted to be wrong by a stated amount (0.5 blocks
vertically on a slab, 0.375 horizontally on a fence). The seam gate additionally
carries four `forceSolidOn` states (a sign, a pressure plate, a lantern, a turtle
egg) that only pass if `VersionAdapter::block_blocks_motion` is actually consulted
— a view still deriving `blocks_motion` from geometry answers all four wrong and
passes every other assertion in that test, which is exactly the routing gate the
census needed.
