# Block physics constants

## What it is

The block facts a **moving entity** needs that are not geometry — how slippery a
cell is, how much it slows you, whether it grabs you, whether you can climb it,
and whether it stops you at all. Seven values, split across two homes because
they are keyed differently:

| value | vanilla source | keyed by | lives in |
|---|---|---|---|
| `friction` | `Block.getFriction` | block **name** | `lodestone_model::block_physics` |
| `speed_factor` | `Block.getSpeedFactor` | block name | same |
| `jump_factor` | `Block.getJumpFactor` | block name | same |
| `bounce_restitution` | `Block.getBounceRestitution`, net of `BlockTags.SUPPRESSES_BOUNCE` | block name | same |
| `stuck_multiplier` | `Block.entityInside` → `Entity.makeStuckInBlock` | block name | same |
| `climbable` | `BlockTags.CLIMBABLE` | block name | same |
| `blocks_motion` | `BlockState.blocksMotion()` | block **state id** | `lodestone_v770::block_solidity`, reached via `VersionAdapter::block_blocks_motion` |

The six name-keyed ones used to be private functions inside
`crates/lodestone-shell/src/collision.rs`, hand-transcribed from decompiled
source. `blocks_motion` used to be *derived from the collision shape*, which was
wrong for 202 blocks. Both are now dumped from the real 26.2 server by one oracle
pass and gated in one test.

## How it works

### The split, and why it is not arbitrary

`friction` and friends are `final` fields on `BlockBehaviour`
(`BlockBehaviour.java:93-96`) copied from `Properties` at construction
(`:110-113`), and their accessors on `Block` (`Block.java:493-507`) **take no
state**. So one block has one value, and the key is the block's name — which is
stable across versions in a way state ids are not (they are renumbered every
version). That makes them the wrong shape for `VersionAdapter`: putting them
behind the version seam would mean an identical copy per protocol crate.

`legacySolid` is the opposite. `initCache()` (`BlockBehaviour.java:506-530`)
computes it **per state** from `calculateSolid()` (`:484-504`), whose geometry
branch reads that state's own `cache.collisionShape`. A closed trapdoor and an
open one genuinely differ. So it is state-keyed, and state-keyed means behind
`VersionAdapter`, exactly like `block_collision` and `block_hardness`.

The rule this follows, and the one `docs/plugin-api.md` states: **check whether
the data is state-keyed or name-keyed before choosing where it lives.**

### `lodestone_model::block_physics` — the public, plugin-reachable half

```rust
use lodestone_model::{BlockPhysics, DEFAULT_BLOCK_PHYSICS, block_physics};

let p = block_physics("minecraft:ice");   // no adapter, no instance, no allocation
assert_eq!(p.friction, 0.98);
```

`block_physics(&str) -> BlockPhysics` is a free `pub fn` in
`crates/lodestone-model/src/adapter.rs`. It returns the whole record by value
(six words) rather than one accessor per field, so a caller pays one name match
per query instead of six.

An unrecognised name yields `DEFAULT_BLOCK_PHYSICS` rather than `None`. That is
deliberate and different from `block_collision`: "no row" is not a data gap here,
it is the answer for **1,166 of 26.2's 1,196 blocks**, and forcing every caller
to handle `None` would just make each of them re-invent the same defaults.

`lodestone-model` is the home because it is version-free, it is already what
`lodestone-ecs` (and therefore every plugin) depends on, and because
`VersionAdapter::block_name` — which is where a caller gets the key — is defined
in the same file. `lodestone-shell` was the wrong home for the same reason it
would be the wrong home for `BlockAabb`: it owns the driver and the `App`, so a
plugin depending on it to reuse six `match` statements is backwards.

Only **23** blocks in 26.2 set any non-default movement constant: the 16 dyed
beds (`bounceRestitution` 0.75), ice / packed ice / frosted ice (friction 0.98),
blue ice (0.989), slime block (friction 0.8, restitution 1.0), soul sand
(`speedFactor` 0.4) and honey block (`speedFactor` 0.4, `jumpFactor` 0.5).

### `lodestone_v770::block_solidity` — the state-keyed half

Two bitsets of 4,046 bytes each in `src/generated/block_solidity.rs`, read
through `src/block_solidity.rs`:

```rust
pub fn legacy_solid(id: u32) -> Option<bool>;   // BlockState.isSolid()
pub fn blocks_motion(id: u32) -> Option<bool>;  // BlockState.blocksMotion()
```

`VersionAdapter::block_blocks_motion(state_id) -> Option<bool>` is the seam;
`LiveCollision` consumes it through the private `BlockView::blocks_motion_of`,
and `CollisionView::blocks_motion` is one line of delegation like the other
twelve answers.

### Why `blocks_motion` cannot come from the collision census

`blocksMotion()` is `block != COBWEB && block != BAMBOO_SAPLING && isSolid()`,
and `isSolid()` is a plain read of `legacySolid`. `calculateSolid()` has five
branches and only the last is geometry:

```text
properties.forceSolidOn   -> true     // 237 blocks in 26.2
properties.forceSolidOff  -> false    //   8 blocks
cache == null             -> false    //  23 dynamicShape() blocks
collisionShape.isEmpty()  -> false
else  bounds.getSize() >= 0.7291666666666666 || bounds.getYsize() >= 1.0
```

Neither `forceSolidOn` nor `forceSolidOff` has a getter, appears in
`blocks.json`, or is recoverable from the shape. `AABB.getSize()` is the *mean*
of the three extents (`AABB.java:267-272`), not the volume.

**`0.7291666666666666` is exactly `(1 + 1 + 3/16) / 3` — a ladder's mean extent.**
The threshold has that value *because* a ladder lands on it, and `Blocks.LADDER`
calls `forceSolidOff()` precisely because landing on it gives the wrong answer.
That is the whole shape of the problem in one block.

### Measured: what the shape derivation cost

Not estimated. `crates/protocol/v770/tests/block_physics.rs` reproduces the old
`shape_is_solid` body verbatim over the committed collision census and compares
it to the dumped truth for all 32,366 states:

| | states | blocks |
|---|---|---|
| **old derivation wrong** | **2,618** of 32,366 | **202** of 1,196 |
| … vanilla blocks motion, we said no | 2,497 | 194 |
| … vanilla does not, we said yes | 121 | 8 |

The under-blocked side is signs (48 blocks), banners (32), walls (31), pressure
plates (16), dead corals (15), fence gates (12), lanterns (10), chains (9),
lightning rods (8), plus cake, bell, conduit, turtle egg, amethyst clusters,
pointed dripstone, sculk vein and `moving_piston`. The over-blocked side is
exactly eight and is named in full in the test: azalea, flowering azalea, big
dripleaf, chorus flower, chorus plant, end rod, snow, scaffolding.

A second, independent figure is dumped alongside: vanilla's *own* geometry branch
with the two `forceSolid*` early-returns deleted, computed in the JVM against
`Cache.collisionShape` itself. That disagrees with the truth on **2,742 states /
222 blocks** (2,645 under, 97 over). The gap between 2,742 and 2,618 is where our
census happens to disagree with vanilla's null-shape-cache branch — the
`dynamicShape()` blocks (shulker boxes, bamboo, pointed dripstone) whose
collision shape *is* reported by `getCollisionShape` even though vanilla never
caches it, so the derivation accidentally gets some of them right.

### Blast radius, stated honestly

`blocks_motion` still has exactly **one** consumer:
`lodestone_physics::get_flow`'s empty-neighbour branch, which decides whether a
fluid spills over an edge. Nothing in player movement reads it. So this was
correctness debt, not a live bug, and nothing on screen changes today. It is
repaid so the *next* consumer — a pathfinder asking "can I stand here" — inherits
a right answer rather than a plausible one.

## How to change it

- **Adding a name-keyed constant** — add the field to `BlockPhysics` and a `match`
  arm in `block_physics` (`crates/lodestone-model/src/adapter.rs`), then extend
  `BlockPhysicsOracle.java`'s `K` line and the assertion in
  `name_keyed_constants_match_the_committed_dump_for_every_block`. Do **not** add
  it privately to `collision.rs`; that is the mistake this doc exists to undo.
- **A data bump (new MC version)** — re-dump and regenerate per the two commands
  in `crates/protocol/v770/tests/block_physics.rs`'s module docs. The dump is
  byte-reproducible across runs. The oracle reflects into
  `BlockBehaviour.Properties`' private `forceSolid*` fields and needs **no**
  `--add-opens`, because the server jar sits on the classpath and so lands in the
  unnamed module where `setAccessible` is unrestricted — that changes the day the
  server ships as a named module.
- **If a future version changes a name-keyed value**, the exhaustive gate fails.
  The fix at that point is a *per-version override* — a `VersionAdapter` method
  that shadows a row — not a silent edit to the shared table, because the shared
  table would then be wrong for one of the two versions.

### Gotchas

- **The dump agreed with the old hand-written table on every one of the six
  name-keyed constants.** Nothing changed in value; what changed is that the
  numbers are now pinned by something outside the code under test, and reachable.
  Do not read the absence of a diff as the oracle being unnecessary — read it as
  the previous transcription having been careful. The control for *this* change
  is entirely on the `blocks_motion` side.
- **`stuck_multiplier` is the one field that is not dumped**, and it cannot be:
  the vector is constructed in imperative code inside each block's `entityInside`
  override, so there is no property to read, and two of the three are conditional
  on the entity (`WebBlock` gives a `WEAVING` mob `(0.5, 0.25, 0.5)`;
  `SweetBerryBushBlock` exempts foxes and bees). What the oracle *does* dump is
  the **candidate set** — every block whose class overrides `entityInside`, 61 of
  them — so the three-row table is checked-complete rather than asserted-complete.
  If a fourth grabbing block appears, the candidate-set assertion changes and the
  gate fails.
- **`bounce_restitution` is already net of `SUPPRESSES_BOUNCE`. Do not subtract
  the tag again.** In 26.2 the tag's only member is `honey_block`, which sets no
  restitution, so the subtraction is currently a **no-op** and the contract is
  unexercised. That was flagged as a silent-breakage hazard and had no gate;
  `the_bounce_suppression_tag_is_currently_a_no_op_and_that_is_load_bearing` is
  the gate. A future bouncy suppressor fails it.
- **Beds are matched by suffix, but only inside the vanilla namespace.** All 16
  dyed beds share one `Properties` builder (`Blocks.java:684`, via the `BED`
  `ColorCollection`), so `minecraft:*_bed` is the honest key — but
  `name.starts_with("minecraft:")` is load-bearing: without it a modded
  `foo:iron_bed` would inherit 0.75 restitution.
- **`shape_is_solid` still exists in `collision.rs`, as the *degraded* fallback
  only.** It is reached when there is no version data (the same case
  `LiveCollision::has_real_shapes()` already reports) and by `WorldCollision`,
  whose ten-block demo palette is entirely full cubes and air — the one world
  where the derivation is exact. Do not delete it, and do not promote it back to
  the primary path.
- **`BlockView::blocks_motion_of` must never synthesise its answer from
  `shape_of` inside an adapter.** The whole value of `Option<bool>` here is that
  "no census" stays distinguishable from "the census says false".

## Configuration

- **`--features live`** compiles a version family in. Without it
  `VersionAdapter::block_blocks_motion` is unreachable and `blocks_motion`
  degrades to the shape derivation (wrong for the 202 blocks above), the same way
  collision degrades to unit cubes.
- `LODESTONE_REGEN=1` regenerates `src/generated/block_solidity.rs` from the
  committed dump.
- `LODESTONE_ASSETS` — needed by the `#[ignore]`d shell gates that check the seam
  reaches `CollisionView`.

## Dependencies

- `lodestone-model` — `BlockPhysics`, `DEFAULT_BLOCK_PHYSICS`, `block_physics`,
  `VersionAdapter::{block_name, block_blocks_motion}`.
- `lodestone-v770` — `block_solidity`, the generated bitsets, and the adapter impl.
- `lodestone-shell` — `collision.rs`'s two `CollisionView` adapters, the only
  consumer today.
- `lodestone-physics` — `CollisionView`, whose `friction` / `speed_factor` /
  `jump_factor` / `bounce_restitution` / `stuck_multiplier` / `is_climbable` /
  `blocks_motion` these answer.
- `oracle-java/BlockPhysicsOracle.java` + a JDK (the documented route is
  `eclipse-temurin:25-jdk` in Docker) — only to re-dump.

## Gates

| gate | where | runs |
|---|---|---|
| both bitsets vs the JVM dump, all 32,366 states | `v770/tests/block_physics.rs::committed_table_matches_the_committed_dump` | always |
| dump block boundaries vs `blocks.json` | same file, `dump_block_boundaries_match_the_block_state_table` | always |
| `blocksMotion` = `legacySolid` minus exactly cobweb + bamboo sapling | same file | always |
| vanilla's own geometry branch is wrong for 2,742 states | same file, `the_geometry_branch_alone_is_wrong_for_two_thousand_states` | always |
| **the control** — the shipped derivation is wrong for 2,618 states / 202 blocks, both directions named | same file, `the_shipped_shape_derivation_gets_a_measured_set_of_blocks_wrong` | always |
| name-keyed constants vs the dump, all 1,196 blocks | same file, `name_keyed_constants_match_the_committed_dump_for_every_block` | always |
| the 23 non-default blocks, `CLIMBABLE`'s nine, `SUPPRESSES_BOUNCE`'s one, the `entityInside` candidate set | same file | always |
| generated table is not stale vs the dump | same file, `committed_table_matches_dump` | `#[ignore]` |
| the shell's contract on the shared table, incl. `DEFAULT_BLOCK_PHYSICS` | `lodestone-shell/src/collision.rs::tests::name_keyed_constants_come_from_the_shared_model_table` | always |
| both adapters read the shared table, no stub | same, `the_demo_view_reads_the_shared_table_rather_than_a_stub` | always |
| the census reaches `CollisionView` (4 `forceSolidOn` + 3 over-blocked states, each with the derivation asserted wrong) | same, `name_keyed_constants_reach_the_view_through_the_version_seam` | `#[ignore]` — needs the pack **and** `--features live` |

## See also

- [`docs/collision-shapes.md`](./collision-shapes.md) — the state-keyed geometry
  census next door, and the `CollisionView` routing this rides on. **Its
  `blocks_motion` section is now stale**: it describes the derivation this
  replaced, and its "143 blocks with `forceSolidOn` and 8 with `forceSolidOff`"
  figure counts `Blocks.java` *call sites*, not blocks — the real per-block counts
  are 237 and 8 (plus 23 `dynamicShape()` blocks it does not mention at all).
- [`docs/plugin-api.md`](./plugin-api.md) — §3 gap 4, which asked for exactly this
  relocation; the "block physics constants … reachable without depending on
  `lodestone-shell`" row of its gap list is now closed.
- [`docs/baritone-port.md`](./baritone-port.md) — §7.5, the pathfinder that wants
  these as a cost function.
