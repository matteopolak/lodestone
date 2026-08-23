# Entity-versus-entity interaction

## What it is

The two things that happen when two entities occupy the same space, ported into
`lodestone-physics` as [`push.rs`](../crates/lodestone-physics/src/push.rs):

| | vanilla | here |
|---|---|---|
| **soft push** — a horizontal velocity shove, every tick, while boxes overlap | `LivingEntity.pushEntities` → `Entity.push(Entity)` | `push::entity_push_impulse` / `push::apply_entity_push` / `player::tick_among_entities` |
| **hard collision** — entity boxes clipping movement | `EntityGetter.getEntityCollisions` → `Entity.collide`, and the entity term of `noCollision` | `push::entity_collision_boxes`, `collision::collide_among_entities`, `entity::move_entity_among_entities`, `push::no_entity_collision` / `push::no_collision_among_entities` |

**The client-side half is wired.** `Sim::tick_nearby_entities` builds one mixed
snapshot of living crowd-push producers and hard colliders, and
`lodestone_ecs::player::player_physics` hands it to
`player::tick_among_entities` once per fixed tick.

**The server-side half now has a real consumer.**
`lodestone_server::mobs::MobSim::push_entities` calls `pair_push_vector` to
shove mobs apart from each other and away from an overlapping player — the
"server-authoritative mob, not client-authoritative local player" direction
this module's own doc comments distinguish below. See
[Server-side: a mob consumer](#server-side-a-mob-consumer) for what it covers
and what it still narrows.

## The rule, and the three things it is not

### `isPushable` and `canBeCollidedWith` are different predicates

| | predicate | default on `Entity` | `LivingEntity` |
|---|---|---|---|
| push | `Entity.isPushable()` | `false` | **overridden** (`LivingEntity.isPushable`): `isAlive() && !isSpectator() && !onClimbable()` |
| collide | `Entity.canBeCollidedWith(other)` | `false` | **not overridden** |

The exhaustive list of `canBeCollidedWith` overrides in 26.2 is **three classes**:
`AbstractBoat.canBeCollidedWith` (unconditional `true`), `Shulker.canBeCollidedWith` (`isAlive()`) and
`HappyGhast.canBeCollidedWith` (a baby/vehicle/still-timeout state machine with a
client-only clause admitting a player standing on its back).

**So "players and mobs pass straight through each other" is vanilla, not a
Lodestone defect.** Players shove each other apart and never clip. What was
genuinely missing — and is the visible, desync-relevant half — is the *push*.

The asymmetry runs both ways, which is why one boolean cannot serve both
questions:

* a **boat** is collidable *and* pushable, and additionally overrides
  `canCollideWith` to `AbstractBoat.canVehicleCollide`, which
  admits a merely *pushable* entity as a collider — that is the "you stand on a
  boat" case;
* a **shulker** is collidable and **not** pushable (it inherits
  `isPushable() == false`). It blocks you and never shoves.

### Who moves: symmetric, gated twice

`Entity.push(Entity)` computes **one** horizontal vector
from the two positions, hands `-v` to `this` and `+v` to `entity`, and gates each
side independently on `!isVehicle() && isPushable()`. A ridden entity absorbs the
shove and passes it on to nobody. Y is never touched.

### Ordering: vanilla has no tie-break because nothing moves

Naive pairwise separation has a real order dependency. Vanilla's answer is not a
rule for resolving it — it is that **positions are read and never written**. The
push is added to `deltaMovement` (`Entity.push(double, double, double)`, called from
`Entity.push(Entity)`) and `pushEntities` runs at the
*end* of `LivingEntity.aiStep`, after `travel`'s call in the same method has already
moved everything this tick. The impulses one entity receives are therefore a set
computed from a frozen position snapshot, and summing a set commutes. No
relaxation loop, no penetration-depth solve, no entity-id ordering.

The residue is that `f64` addition is not associative, so a crowd of two or more
pushers can land on a different last bit depending on tick order — bounded by
about one ulp of the velocity, five orders of magnitude below the 0.25-block
rubber-band threshold, and not something vanilla pins either (the server's entity
iteration order is not observable from a client). `push.rs` accumulates one push at
a time onto the velocity, exactly as vanilla does, rather than summing an impulse
and adding once, so a crowd is at least *internally* consistent.

### There is no per-entity push cap

Nothing limits how many entities push one entity per tick; `pushEntities` iterates
the whole list. `MAX_ENTITY_CRAMMING` (`LivingEntity.pushEntities`) deals **6.0
cramming damage** on `ServerLevel` only, behind a `random.nextInt(4) == 0` gate —
damage, not a movement clamp, and invisible to a client's physics. Crowd behaviour
is limited by the per-pair magnitude and by drag.

## The arithmetic, and what it actually computes

```java
double xa = entity.getX() - this.getX();
double za = entity.getZ() - this.getZ();
double dd = Mth.absMax(xa, za);
if (dd >= 0.01F) {
   dd = Math.sqrt(dd);
   xa /= dd;  za /= dd;
   double pow = 1.0 / dd;
   if (pow > 1.0) pow = 1.0;
   xa *= pow;  za *= pow;
   xa *= 0.05F;  za *= 0.05F;
   …
}
```

Writing `m = absMax(dx, dz)`, that collapses exactly to

```text
push = (dx/m, dz/m) · 0.05f · min(√m, 1.0)
```

because `pow = 1/√m` cancels one of the two `√m` divisions when the clamp does not
bind. `(dx/m, dz/m)` is the separation normalised by the **Chebyshev** norm, so its
dominant component is exactly `±1` and its length lies in `[1, √2]`.

Three consequences, each contradicting the obvious reading:

1. **The normaliser is `sqrt(absMax)`, not the vector length.** `Mth.absMax`
   is `max(|a|, |b|)`. For `(0.15, 0.08)` the two differ by
   6% — on both axes, on every tick.
2. **There is no distance falloff.** The magnitude rises with separation up to
   `m = 1` and is then **flat** at `0.05f` forever. Two entities one block apart
   shove exactly as hard as two five blocks apart. Unobservable for same-sized
   mobs (their boxes stop overlapping at `m = width < 1`), quite observable inside
   a happy ghast.
3. **`if (pow > 1.0) pow = 1.0` is a soft start near contact, not a cap on a
   blow-up.** Without it the magnitude would be a constant `0.05f`; with it, a pair
   `0.05` apart gets `0.05f·√0.05 ≈ 0.011` — a quarter of the force. Nearly
   concentric entities separate slowly and then accelerate. A "push apart by
   penetration depth" rule gets this backwards.

Two widened-`float` literals are load-bearing and neither is the decimal it looks
like: the gate is `0.01f` = `0.009999999776482582` and the scale is `0.05f` =
`0.05000000074505806`.

`push.rs` keeps the literal transcription rather than the collapsed form, because
the two are not bit-identical (three roundings versus two, in a different order).

## Client versus server: the factor of two

`EntitySelector.pushableBy` has a clause that reshapes the whole port:

```java
if (!entity.level().isClientSide() || input instanceof Player p && p.isLocalPlayer()) { … } else { return false; }
```

`entity` is the pusher, `input` the pushee. **On a client the only admissible
pushee is the local player.** Therefore:

* the local player's own `pushEntities` finds nothing (the candidate list excludes
  itself and no other candidate is the local player) — a vanilla client never
  initiates a push;
* every push the local player feels comes from some other entity's `aiStep`, which
  the client does run unconditionally for remote entities (`LivingEntity.tick` →
  `aiStep`; the `travel` inside is gated on `isEffectiveAi()`,
  `pushEntities` is not). `RemotePlayer` makes this unmistakable: its `aiStep`
  override (`client-src/.../RemotePlayer.java`) throws away the entire
  `LivingEntity` body and keeps interpolation, the swing/bob timers and
  **`this.pushEntities()`**. On the client, a remote player's whole physics
  contribution *is* shoving the local player;
* because `Entity.push(Entity)` is symmetric, iterating *our* neighbours and
  applying the receive-half to ourselves reproduces that exactly — the pair test
  and the magnitude are both symmetric.

**Measured consequence, to record rather than "fix": the server applies the impulse
twice per pair per tick and the client once.** On a server both sides' `pushEntities`
run and each calls the symmetric `Entity.push`, so the player is shoved by its own
pass *and* by the mob's. On a client only the mob's pass qualifies. A client
modelling `2×` would be the one out of step with its peers. It does not itself trip
the rubber-band check, which replays the *claimed* delta through collision rather
than re-deriving velocity (see [`edge-back-off.md`](./edge-back-off.md)).

## Where it sits in the tick

```text
tick / tick_air / tick_water / …          ← travel  (LivingEntity.java:3130)
update_stuck_multiplier
apply_entity_push                         ← pushEntities (:3163)
```

`player::tick_among_entities` is that order. The impulse lands on the velocity the
**next** tick integrates; this tick's collision sweep never sees it. A port that
pushes before `travel` is wrong on the first tick of contact and agrees from the
second — a one-tick divergence that looks correct on screen.

## Server-side: a mob consumer

`lodestone_server::mobs::MobSim::push_entities` runs after the per-mob goal/movement
loop in `MobSim::tick_with_terrain` — the same "after `travel`" placement
`player::tick_among_entities` gives the client half, and for the same reason
(`pushEntities` runs at the end of `LivingEntity::baseTick`, after that tick's own
`travel` call). It reuses `pair_push_vector` rather than re-deriving the formula,
so a fix or a golden-trace regeneration to that function covers both consumers.

Two things this consumer is, on purpose, that the client-facing API above is not:

* **A pusher of other simulated entities**, not just a receiver. A real vanilla
  client never initiates a push (`EntitySelector.pushableBy`'s clause admits only
  the local player as a pushee on the client); the server simulates every mob
  itself and is the authority on where each one ends up, so it both computes and
  *applies* both halves of a mob-mob pair, and the mob half of a player-mob pair.
* **Missing the player-recoil half entirely.** A player's velocity is
  client-authoritative here (the client sends `move_player_pos`; the server does
  not own it the way it owns a mob's), so shoving a player back needs a
  clientbound self-velocity packet the client applies to its own physics — outside
  `lodestone-server`, in `crates/protocol/**` and the client-side wiring this
  section already describes as owed. Until that lands, walking into a mob moves
  the mob but not the player — vanilla's `Entity.push` is symmetric and this port
  is deliberately only half of it.
* **A simplified overlap test.** Horizontal distance under the pair's combined
  half-widths, not vanilla's real AABB intersection (`Level::getEntities`), and
  applied once per pair per tick rather than vanilla's two (each side's own
  `pushEntities` call invokes `doPush` against the other). Both are disclosed on
  `MobSim::push_entities`'s own doc comment, along with why `isPushable`/vehicle
  exclusions and cramming damage are not modelled either.

## Wiring

### The interface the physics crate wants

Physics does **not** query for entities. `CollisionView` answers *block* geometry
and gains no method here: an entity list is a per-tick snapshot, not a repeatable
spatial query, and the two have different lifetimes and different owners. The
producer hands over a slice.

```rust
// crates/lodestone-physics/src/push.rs
pub struct NearbyEntity {
    pub position: Vec3d,        // Entity.position() — feet centre; only x/z are read
    pub bounding_box: Aabb,     // Entity.getBoundingBox(), world space
    pub pushable: bool,         // Entity.isPushable()
    pub pushes_players: bool,   // this entity runs LivingEntity.pushEntities()
    pub collidable: bool,       // Entity.canBeCollidedWith(us)
    pub is_vehicle: bool,       // Entity.isVehicle() — has passengers
    pub no_physics: bool,       // Entity.noPhysics
    pub spectator: bool,        // Entity.isSpectator()
    pub same_vehicle: bool,     // us.isPassengerOfSameVehicle(it)
    pub collision_rule: CollisionRule,  // team rule; Always when team-less
    pub allied: bool,           // ownTeam.isAlliedTo(theirTeam); false if either is team-less
}
```

`NearbyEntity::living(position, bounding_box)` is the shape almost every living
crowd-push producer takes (pushable and push-producing, not collidable, no team,
no passengers).

**What the shell must supply, and how:**

1. **Which entities.** Every entity whose bounding box could overlap the local
   player's. Vanilla's own query is `getPushableEntities(this, this.getBoundingBox())`
   — the *un-inflated* player box. A generous neighbourhood is fine: candidates that
   fail a gate contribute nothing. `lodestone-ecs`'s `EntityIndex` plus `Position`
   is the natural source; a radius filter of ~2 blocks (or ~4 to cover a happy
   ghast) around the player is ample.
2. **`position` and `bounding_box` are both required and neither derives the
   other.** The push direction reads `getX()`/`getZ()`; the pair test reads the box.
   Conflating them is a guess about an entity whose box is offset from its position.
   The box comes from the entity type's `EntityDimensions` — the shell already has
   a geometry table for rendering; scale-attribute folding happens before the box is
   built, exactly as for `EntityDimensions::PLAYER`.
3. **`pushable` is per entity type and per state.** `LivingEntity` →
   `isAlive() && !isSpectator() && !onClimbable()`; `ArmorStand.isPushable` → `false`;
   the `Entity` base (items, arrows, XP orbs) → `false`;
   boats → `true`. Getting this wrong in the permissive direction manufactures
   pushes from dropped items, which is very visible.
4. **`pushes_players` and `collidable` are independent producer capabilities.**
   The generated v770 census marks `LivingEntity.pushEntities` producers in the
   first column and the exhaustive `canBeCollidedWith` override families — boats,
   shulkers and happy ghasts — in the second. The shell includes a candidate when
   either is true. Unknown ids default-deny both.
5. **Teams**: `ClientboundSetPlayerTeamPacket` carries the collision rule. Until it
   is decoded, `CollisionRule::Always` + `allied: false` is the correct value for
   every server with no scoreboard teams — i.e. the overwhelming majority — and it
   is what `NearbyEntity::living` sets.
6. **`PushSelf`** is our own side: `alive`, `spectator`, `is_vehicle`,
   `collision_rule`. `PushSelf::LIVING_PLAYER` is the constant for an ordinary
   client. It deliberately has no `Default` — `bool::default()` would make `alive`
   false and silently produce a corpse no push can move.

The production path is
`Sim::tick_nearby_entities → NearbyEntities → player_physics →
tick_among_entities`. The shell resolves dimensions and the two type-level
capabilities through `VersionAdapter::entity_facts`; the physics crate never
queries the ECS world itself.

### What stays incomplete

* **Per-instance collider refinements** are not yet carried by the ECS snapshot.
  The type census is a maximum: a shulker must also be alive, and a happy ghast
  has its own state machine. Boats are unconditional and are the production case
  this wiring fixes.
* **`noBorderCollision`** is still unmodelled (no world border). It is now the
  *only* remaining unmodelled term of `noCollision`'s three.

**Update (2026-07-29): `no_collision_among_entities` now has a production
consumer.** `Player.updatePlayerPose`'s fit gate is exactly this predicate over a
`deflate(1.0E-7)`d pose box, and `lodestone_physics::tick_among_entities` passes
its neighbour slice to it — so a boat, shulker or happy ghast overlapping the
player can veto a pose. `tick`'s block-only form is unchanged. See
[`pose-dimensions.md`](./pose-dimensions.md). That doc also depends on the
measurement above: it is *because* `canBeCollidedWith` is false for every player
and mob that the entity term did not block the swimming-hitbox work, which an
earlier investigation had concluded it did.

## How to change it, and the gotchas

* **`MoveContext` did not gain a field, on purpose.** It is a `Copy` value type of
  plain scalars, constructed outside this crate; a borrowed slice would give it a
  lifetime and take `Copy` with it, and it already lost `Eq` when it gained an
  `f64`. `move_entity_among_entities` remains the box-slice entry point;
  `move_entity_with_nearby` is the crate-private player adapter.
* **Entity colliders are gathered once, from the *movement* box, and reused for the
  step-up pass** even though `stepUpAABB` is strictly larger (`Entity.collide`).
  An entity overlapping only the taller step-up box is invisible to the step. Do
  not re-gather.
* **Entity colliders go first in the collider list.** `collectCollidersIgnoringWorldBorder`
  puts them ahead of the block colliders, and `Shapes.collide` short-circuits to
  `0.0` once the residual falls inside `1.0E-7`, so order can decide between `0.0`
  and a surviving sub-epsilon value.
* **The two overlap tests are deliberately different.** The push pair test is
  `AABB.intersects` on the raw boxes — strict `min < max`, **no** epsilon.
  `getEntityCollisions` inflates its query by `1.0E-7` and additionally bails when
  `testArea.getSize()` (the *mean* edge length) is under `1.0E-7`. Mixing the two up
  is the easy mistake; `entity_push_flush_control` is the gate that catches it.
* **`!(dd >= 0.01F)` is not `dd < 0.01F`.** Vanilla's `if` has no else, so the
  reject branch is the negation of a `>=`, which rejects `NaN`. A `<` would fall
  through and emit a `NaN` impulse. `mth::abs_max` routes through
  `mth::java_max_f64` for the same reason — Rust's `f64::max` swallows `NaN` where
  Java's `Math.max` propagates it.
* **A ladder makes you immovable.** `onClimbable()` vetoes `isPushable()`, so a mob
  crush cannot shove a player off a ladder. `push::self_is_pushable` evaluates that
  term from the `CollisionView` rather than asking the caller, because it is the same
  feet-block climbable query `travel_in_air` already makes.

## Configuration

None. No feature flag, no env var, no tunable. `MAX_ENTITY_CRAMMING` is a
server-side game rule affecting damage only and is not read here.

## Dependencies

* `lodestone-physics::collision` — `CollisionView` (for the `onClimbable` term and
  the block half of `noCollision`), `collide_among_entities`.
* `lodestone-physics::mth` — `abs_max`, `java_max_f64`, `floor`.
* `lodestone-physics::geometry` — `Aabb::{intersects, inflate, size}`, `Vec3d`.
* Consumers: the client-facing API — see [Wiring](#wiring-what-is-still-owed).
  `no_collision_among_entities` does: `lodestone-physics::pose`'s fit gate
  ([`pose-dimensions.md`](./pose-dimensions.md)). `pair_push_vector` also has a
  server-side consumer now: `lodestone_server::mobs::MobSim::push_entities` —
  see [Server-side: a mob consumer](#server-side-a-mob-consumer).

## Gates

| gate | where | what it pins |
|---|---|---|
| the rule's arithmetic, unit level | `push.rs::tests` (12 tests) | `sqrt(absMax)` vs length, the rise-then-plateau magnitude, the widened `0.01f`/`0.05f`, symmetry, vehicle/ladder/spectator/same-vehicle vetoes, the team truth table, `NaN` rejection, crowd accumulation with no cap, and that push and collide are gated by different predicates |
| bit-exact trajectories vs the Python oracle | `tests/golden.rs`: `entity_push_shove`, `entity_push_wide_plateau`, `entity_push_flush_control` | the whole rule end-to-end through `tick_among_entities`, zero tolerance |
| the hard-collision half | `tests/entity_collision.rs` (5 tests) | a shulker clipping movement, auto-step onto a boat deck at `0.5625`, `move_entity(&[])` bit-identical to `move_entity`, the pose fit gate's two terms, same-vehicle exclusion |
| tick order and pipeline inertness | `tests/entity_push.rs` (3 tests) | the push landing on velocity *after* the move, `tick_among_entities(&[])` bit-identical to `tick` over 40 ticks × 3 starts, and the ladder veto with its control |
| the server-side mob consumer | `lodestone-server`'s `mobs::follow_range_tests`: `push_impulse_matches_a_hand_computed_off_axis_example_not_the_euclidean_alternative`, `push_impulse_is_none_just_outside_the_touch_threshold`, `overlapping_mobs_separate_over_real_ticks_and_a_distant_one_does_not_move`, `a_player_overlapping_a_mob_pushes_the_mob_away` | an off-axis magnitude that discriminates Chebyshev from Euclidean normalisation, the touch-threshold control, mob-mob separation wired into real ticks with a distant control, and a player shoving a mob without the mob shoving back |

`lodestone-physics` goes from **127 to 150 tests**, all passing, zero
non-compiling targets (`cargo test -p lodestone-physics --no-fail-fast`).

Every gate carries a control that must fail the same assertion: the block-only
`collide` walking through the shulker, the step-height-below-deck case, the flush
neighbour one ulp from live, the crowd that *does* move a standing player, and the
`1/length` normalisation that lands 6% off.

### The golden traces: which exercise the new branches

Three of the 32 do — the three named above, and they are the **only** traces in the
file with a second entity in the world. The other 29 are *provably* inert, not
merely observed to be:

* `tick_among_entities` with an empty slice is `tick` (`apply_entity_push` returns
  before reading anything) — asserted bit-for-bit in
  `tick_among_entities_with_no_neighbours_is_tick_bit_for_bit`;
* `move_entity` **is** `move_entity_among_entities(…, &[])`, and
  `collect_colliders` with an empty entity list returns `gather_colliders`
  unchanged — asserted in
  `move_entity_with_an_empty_collider_slice_is_bit_identical_to_move_entity`;
* regenerating `support/golden_traces.rs` after adding the three scenarios produced
  **1612 insertions and 0 deletions**.

### The generator was extended

`gen_golden.py` previously generated single-entity scenarios only. It now carries
`Neighbour`, `boxes_intersect`, `abs_max`, `push_entities` and `tick_with_push` —
an independent second implementation of the rule, which is what makes the three new
traces evidence rather than `decode(encode(x))`. Regenerate with:

```bash
python3 crates/lodestone-physics/tests/gen_golden.py
rustfmt --edition 2024 crates/lodestone-physics/tests/support/golden_traces.rs
```

One trap the flush control fell into first, worth knowing before writing another
two-entity scenario: the **player's** half-width is `f32(0.6)/2 =
0.300000011920929` while a `Neighbour`'s is a plain `f64` `width/2`. A neighbour
placed at the "obvious" flush `x = 1.1` therefore overlaps by `1.2e-8` and pushes.
Derive the flush coordinate from `f32(P.width)` rather than writing a decimal.

## The live gate that is owed

Nothing here has been run against a real server, and the push is not wired, so
there is nothing to run yet. When it is wired, the recipe follows
[`edge-back-off.md`](./edge-back-off.md)'s shape and its three traps apply
unchanged:

1. **It must be the survival oracle** (`./scripts/live-oracles/survival.sh`, game
   `:25565`, RCON `:25566`). The rubber-band check is skipped for `isCreative()`
   (`ServerGamePacketListenerImpl.handleMovePlayer`), so `creative.sh` gives a
   guaranteed vacuous pass.
2. **Read `Sim::teleport_count`** (`crates/lodestone-shell/src/sim.rs`).
3. **Run the unpatched build first and confirm the counter *does* increment** while
   standing in a mob, or "no corrections" is the *duration* species of vacuous test.
   This is the one case where that control is likely to actually fire, because an
   unmodelled push is a real position disagreement that grows for as long as contact
   lasts.

Two setup notes specific to this rule, from
[`CLAUDE.md`](../CLAUDE.md)'s live-server hazards:

* summon the lure with `NoAI:1b`, **not** `Invulnerable:1b` (which makes it
  un-targetable), and poll for selector visibility rather than asserting
  immediately — a freshly summoned entity is not visible until the next tick;
* a `NoAI` mob still ticks, so it still runs `pushEntities`, which is exactly what
  is being measured. A mob that is *dead* does not, and a spectator player has
  `noPhysics` set — both give a silent zero.

Expect the divergence to be **within** the 0.25-block bar per packet (the per-tick
impulse is at most `0.05f` per axis and the client/server factor-of-two difference
is at most that again), so the failure mode is drift over sustained contact rather
than an immediate correction. Measure over a long contact, not one tick.
