//! `MobSim`'s primed-TNT slice — spawn, per-tick fuse/motion, and the query
//! API. Follows the split `falling_blocks.rs`/`vehicles.rs` established (see
//! `docs/plans/crate-and-file-splits.md`): [`super::TrackedTnt`] lives in
//! `mobs/mod.rs`, the ticking lives here.
//!
//! # What this is
//!
//! A port of `PrimedTnt`
//! (`.cache/mc/26.2/src/net/minecraft/world/entity/item/PrimedTnt.java`) plus
//! its ignition producers, read out of `TntBlock`
//! (`.cache/mc/26.2/src/net/minecraft/world/level/block/TntBlock.java`) and
//! `FireBlock`'s `checkBurnOut`:
//!
//! * **Redstone** — `TntBlock::onPlace`/`neighborChanged`, both `if
//!   (level.hasNeighborSignal(pos) && prime(...)) level.removeBlock(...)`.
//! * **Flint and steel / fire charge** — `TntBlock::useItemOn`.
//! * **Fire** — `FireBlock::checkBurnOut`'s `if (block instanceof TntBlock)
//!   TntBlock.prime(...)`, when a spreading fire consumes a TNT block.
//! * **Chain reaction** — `TntBlock::wasExploded`, called for every TNT block
//!   a *different* blast destroys. Its fuse is not [`DEFAULT_FUSE_TIME`] but
//!   [`random_short_fuse`]'s shortened draw — `PrimedTnt.getRandomShortFuse`.
//! * **Mining an unstable block** — `TntBlock::playerWillDestroy`, gated on
//!   `state.getValue(UNSTABLE)` and not the player's own `instabuild`
//!   ability. Not wired here: this crate's block-breaking path does not
//!   thread the `unstable` block-state property through to a primer, and no
//!   producer here ever sets it (see [`super::TrackedTnt`]'s doc).
//!
//! # Reusing the explosion machinery rather than duplicating it
//!
//! A detonating TNT entity hands its blast to [`MobSim::explode`] (entity
//! damage/knockback) and [`MobSim::pending_detonations`] (the block half),
//! **exactly the two calls `MobSim::tick` already makes for a creeper's own
//! fuse**. `crate::explosion_blocks::destroy_blocks` and
//! `crate::block_drops::drop_explosion_loot_in_blast` — wired to
//! [`MobSim::take_detonations`]'s drain in `tick::run_tick_loop` — need no
//! TNT-specific call site at all: this module is a second *producer* for a
//! block-destruction pipeline that already exists and is already wired, not a
//! new consumer.
//!
//! # What is deliberately simplified
//!
//! * **No fluid current push** (`Entity.updateFluidInteraction`'s
//!   `applyCurrentTo`). `PrimedTnt.tick` calls it every tick the fuse has not
//!   run out, so a primed TNT swept by flowing water or lava in vanilla
//!   drifts with the current here it does not. Gravity, collision (including
//!   landing in and settling under water) and the on-ground bounce are all
//!   modelled — only the *current* is not.
//!   `crate::gravity_tick::FallingBlockMotion` (the falling-block analogue)
//!   makes the same cut for the same entity family, and for the same reason:
//!   `apply_fluid_push` (`lodestone_physics::fluid`) is written against
//!   `PlayerState`, not the entity-agnostic `EntityMotion` this module and
//!   `tick_vehicles` share, so wiring it here would mean either forking the
//!   function or widening a foundational, heavily-relied-on physics API — a
//!   materially larger change than this feature.
//! * **No `handlePortal`/nether-portal interaction**, no per-block status
//!   effects (`applyEffectsFromBlocks` — honey, powder snow, cobweb), and no
//!   client-side smoke particle (that belongs to a renderer this crate does
//!   not own). None of the three changes the fuse, the blast, or where a
//!   block is destroyed.
//! * **The imitated block state is always `minecraft:tnt`'s default state.**
//!   See [`super::TrackedTnt`]'s own doc for why this is a property of every
//!   producer here rather than a scope cut, and why the entity carries no
//!   per-instance field for it.
//! * **The float-widened `0.2F` vertical launch component is not
//!   reproduced.** Vanilla widens a 32-bit `0.2F` to `double` for
//!   `setDeltaMovement`, landing on `0.20000000298023224` rather than the
//!   exact decimal `0.2`. Unlike [`crate::gravity_tick::FALLING_BLOCK_AIR_DRAG`]
//!   (whose float-widening **compounds** over dozens of fall ticks into a
//!   multi-tick-visible drift, which is why that constant's own doc insists on
//!   the exact value), this is a single one-time impulse applied once at
//!   spawn: the gap is `2.98e-8` blocks on one tick, never compounded, and is
//!   below anything a test here could observe. [`LAUNCH_VERTICAL`] is the
//!   exact decimal.
//! * **No `EntityReference<LivingEntity>` owner.** Vanilla's `PrimedTnt`
//!   remembers who lit it (`getOwner`), consulted for `Explosion`'s
//!   indirect-source attribution. Nothing here reads it back, so
//!   [`MobSim::spawn_tnt`] takes no owner parameter.

use lodestone_data::block_states;
use lodestone_entity::DamageFlags;
use lodestone_model::{ResourceKey, Vec3};
use lodestone_physics::{
    Aabb, CollisionView, EntityDimensions, EntityMotion, MoveContext, PhysicsProfile, Vec3d,
    move_entity,
};
use uuid::Uuid;

use crate::mob_spawn::SpawnRng;

use super::{Detonation, MobSim, TrackedTnt, block_state_id};

/// `PrimedTnt.DEFAULT_FUSE_TIME` — the fuse a fresh ignition starts at, in
/// ticks (`80` = 4 real-time seconds).
pub const DEFAULT_FUSE_TIME: i32 = 80;

/// `PrimedTnt.DEFAULT_EXPLOSION_POWER` — the blast radius handed to
/// `Level::explode`, the TNT analogue of `mobs::mod::CREEPER_EXPLOSION_RADIUS`.
pub const EXPLOSION_POWER: f32 = 4.0;

/// `PrimedTnt.getDefaultGravity` override — `0.04`, the same per-tick downward
/// acceleration `FallingBlockEntity` uses (`crate::gravity_tick`'s own `0.04`),
/// because neither overrides `Entity`'s gravity in a version- or
/// entity-specific way beyond this one constant.
const GRAVITY: f64 = 0.04;

/// `Entity.getAirDrag()`'s un-overridden default, `0.98F` — numerically
/// identical to [`crate::gravity_tick::FALLING_BLOCK_AIR_DRAG`] for the same
/// reason that constant's own doc gives: both are the same base-`Entity`
/// value, read off two different subclasses that neither overrides.
const AIR_DRAG: f64 = 0.98;

/// The horizontal magnitude of the random launch direction —
/// `-Math.sin(rot) * 0.02F`/`-Math.cos(rot) * 0.02F` in `PrimedTnt`'s
/// three-argument constructor.
const LAUNCH_HORIZONTAL: f64 = 0.02;

/// The exact-decimal vertical launch component (vanilla's `0.2F`, widened to
/// `double`). See this module's doc comment for why the float-widened value
/// is not reproduced here.
const LAUNCH_VERTICAL: f64 = 0.2;

/// `this.setDeltaMovement(this.getDeltaMovement().multiply(0.7, -0.5, 0.7))` —
/// the on-ground bounce/friction `PrimedTnt.tick` applies after drag, every
/// tick it is grounded. The negative `y` factor is what turns a downward
/// landing velocity into a small upward hop — the visible TNT "wobble".
const GROUND_BOUNCE: (f64, f64, f64) = (0.7, -0.5, 0.7);

/// `PrimedTnt`'s hitbox — `0.98 x 0.98`
/// (`crates/lodestone-data/src/generated/entity_dimensions.rs`, network id 133
/// `minecraft:tnt`), no auto-step: a bare `Entity`'s `maxUpStep()` is `0.0`,
/// unlike a `LivingEntity`'s `STEP_HEIGHT`-attribute default of `0.6`. See
/// `mobs::mod::ITEM_DIMENSIONS`'s own doc for the identical reasoning applied
/// to a different non-living entity.
const TNT_DIMENSIONS: EntityDimensions = EntityDimensions::new(0.98, 0.98, 0.0);

/// Seed for [`MobSim::tnt_rng`](super::MobSim) — its own stream so priming TNT
/// never shifts which roll a mob spawn, a block drop, a fire tick or anything
/// else makes. Arbitrary and fixed, like every other per-behaviour seed in
/// this module (`orbs::ORB_BEHAVIOR_SEED`, `TAME_ROLL_SEED`, ...).
pub(super) const TNT_LAUNCH_SEED: u64 = 0x544e_545f_4c41_554e;

/// `PrimedTnt.getRandomShortFuse` — the shortened fuse `TntBlock::wasExploded`
/// gives a chain-reacted TNT block: `random.nextInt(max(1, fuse / 4)) + fuse / 8`.
#[must_use]
pub fn random_short_fuse(fuse: i32, rng: &mut SpawnRng) -> i32 {
    rng.next_int((fuse / 4).max(1)) + fuse / 8
}

/// Whether `state` is a `minecraft:tnt` block, ignoring its `unstable`
/// property. Shared by every ignition producer that has to recognise the
/// *block* rather than the entity — `crate::block_drops`'s chain-reaction
/// detection and `crate::random_tick`'s redstone-signal arm both key off this
/// rather than duplicating the base-name split.
#[must_use]
pub fn is_tnt_block(state: &str) -> bool {
    state.split_once('[').map_or(state, |(base, _)| base) == "minecraft:tnt"
}

/// The block-tick key [`crate::random_tick`]'s redstone-signal arm schedules
/// when a neighbour supplies a signal to a TNT block — `TntBlock::onPlace`/
/// `neighborChanged`, whose vanilla body primes and removes the block in the
/// same call, synchronously. That dispatcher runs over a bare `ChunkColumn`
/// with no [`MobSim`] to spawn into, so the actual prime happens one hop
/// later, in `tick::run_tick_loop`'s scheduled-tick drain — the same handoff
/// shape [`crate::redstone_dispenser::TICK_DISPENSER_FIRE`] already uses, and
/// for the identical reason. Scheduled at the *current* tick rather than a
/// delay, so the divergence from vanilla's immediacy is at most the one tick
/// between "notification observed" and "this tick's scheduled-tick drain
/// runs" — the same latency this crate already accepts for
/// [`MobSim::pending_detonations`] reaching its own drain.
pub const TICK_TNT_PRIME: &str = "redstone:tnt_prime";

/// The entity-type key every primed TNT streams as. Named rather than looked
/// up numerically for `mobs::mod::item_entity_type`'s reason: a wrong key
/// silently encodes as a different vanilla entity type instead of failing.
pub(super) fn tnt_entity_type() -> ResourceKey {
    "minecraft:tnt"
        .parse()
        .expect("`minecraft:tnt` is a valid resource key")
}

impl<'w> MobSim<'w> {
    /// `new PrimedTnt(level, x, y, z, owner)` — every `TntBlock::prime`/
    /// `wasExploded` call site's common constructor, minus the owner (see this
    /// module's doc for what that costs).
    ///
    /// `fuse` is the caller's choice rather than always
    /// [`DEFAULT_FUSE_TIME`]: `TntBlock::wasExploded`'s chain reaction starts
    /// at [`random_short_fuse`]'s shortened value instead. Returns the new
    /// entity's network id.
    pub fn spawn_tnt(&mut self, position: Vec3, fuse: i32) -> i32 {
        let id = self.next_id;
        self.next_id += 1;
        // `double rot = level.getRandom().nextDouble() * (float)(Math.PI * 2);`
        let rot = self.tnt_rng.next_f64() * std::f64::consts::PI * 2.0;
        let velocity = Vec3d::new(
            -rot.sin() * LAUNCH_HORIZONTAL,
            LAUNCH_VERTICAL,
            -rot.cos() * LAUNCH_HORIZONTAL,
        );
        let mut motion = EntityMotion::at(Vec3d::new(position.x, position.y, position.z));
        motion.velocity = velocity;
        self.tnt.insert(
            id,
            TrackedTnt {
                uuid: Uuid::new_v4(),
                motion,
                fuse,
            },
        );
        id
    }

    /// `TntBlock::wasExploded` — primes a TNT block another blast just
    /// destroyed, at [`random_short_fuse`]'s shortened fuse rather than
    /// [`DEFAULT_FUSE_TIME`]. Draws from [`Self`]'s own isolated `tnt_rng`, so
    /// a chain reaction cannot shift which roll a mob spawn or a block drop
    /// sees. See `crate::block_drops::drop_explosion_loot_in_blast`'s own doc
    /// for the caller that reports *where*.
    pub fn spawn_tnt_short_fuse(&mut self, position: Vec3) -> i32 {
        let fuse = random_short_fuse(DEFAULT_FUSE_TIME, &mut self.tnt_rng);
        self.spawn_tnt(position, fuse)
    }

    /// The number of live primed TNT entities.
    #[must_use]
    pub fn tnt_count(&self) -> usize {
        self.tnt.len()
    }

    /// A tracked TNT entity's current position, if any.
    #[must_use]
    pub fn tnt_position(&self, id: i32) -> Option<Vec3> {
        self.tnt
            .get(&id)
            .map(|t| Vec3::new(t.motion.position.x, t.motion.position.y, t.motion.position.z))
    }

    /// A tracked TNT entity's current fuse, if any.
    #[must_use]
    pub fn tnt_fuse(&self, id: i32) -> Option<i32> {
        self.tnt.get(&id).map(|t| t.fuse)
    }

    /// One tick of every live primed TNT: gravity, collision/bounce, the fuse
    /// countdown, and — the tick the fuse reaches `0` — detonation.
    ///
    /// `PrimedTnt.tick`, transcribed in vanilla's own order: `applyGravity`,
    /// `move(SELF, delta)`, then drag and (on ground) the bounce, *then* the
    /// fuse decrement and the `fuse <= 0` branch (`discard()` then
    /// `explode()`). [`move_entity`] is `lodestone_physics::entity`'s single
    /// shared integrator — the same primitive [`MobSim::tick_vehicles`] uses —
    /// so a primed TNT resolves collision through the identical code a boat or
    /// a player does, not a second copy.
    ///
    /// `block_state` is a live-world oracle for [`MobSim::tick_vehicles`]'s own
    /// reason: this sim's `world: &'w ChunkWorld` is a static spawn-time
    /// snapshot, so a driver with the real, live `ChunkSource` supplies the
    /// answer instead (`tick::run_tick_loop`).
    ///
    /// Detonation is delivered exactly like a creeper's, in the same two calls
    /// `MobSim::tick` makes for one: [`MobSim::explode`] for entity
    /// damage/knockback, and a push onto
    /// [`MobSim::pending_detonations`] for the block half, which
    /// [`MobSim::take_detonations`]'s existing driver-side drain already turns
    /// into destroyed blocks, drops and an `EXPLODE` packet — see this
    /// module's own doc comment for why that needs no TNT-specific call site.
    pub fn tick_tnt(&mut self, block_state: &dyn Fn(i32, i32, i32) -> String) {
        let view = TntCollision { block_state };
        let profile = PhysicsProfile::default();
        let mut ids: Vec<i32> = self.tnt.keys().copied().collect();
        ids.sort_unstable();
        // Collected rather than exploded inline: `explode` needs `&mut self`
        // while this loop borrows `self.tnt` mutably, exactly the shape
        // `MobSim::tick`'s own creeper-detonation loop already uses.
        let mut detonated: Vec<(i32, Vec3)> = Vec::new();
        for id in ids {
            let Some(t) = self.tnt.get_mut(&id) else {
                continue;
            };
            // `applyGravity()`.
            t.motion.velocity.y -= GRAVITY;
            // `this.move(MoverType.SELF, this.getDeltaMovement());`
            move_entity(
                &mut t.motion,
                TNT_DIMENSIONS,
                &view,
                &profile,
                MoveContext::default(),
            );
            // `setDeltaMovement(this.getDeltaMovement().scale(this.getAirDrag()))`.
            t.motion.velocity = t.motion.velocity.scale(AIR_DRAG);
            // `if (this.onGround()) setDeltaMovement(delta.multiply(0.7, -0.5, 0.7));`
            if t.motion.on_ground {
                t.motion.velocity = t.motion.velocity.multiply_each(
                    GROUND_BOUNCE.0,
                    GROUND_BOUNCE.1,
                    GROUND_BOUNCE.2,
                );
            }
            t.fuse -= 1;
            if t.fuse <= 0 {
                // `this.getY(0.0625)` — `Entity.getY(double)` is
                // `position.y + getBbHeight() * progress`, so the blast centre
                // sits a fraction of the entity's own height above its feet.
                let centre = Vec3::new(
                    t.motion.position.x,
                    t.motion.position.y + f64::from(TNT_DIMENSIONS.height) * 0.0625,
                    t.motion.position.z,
                );
                detonated.push((id, centre));
            }
        }
        for (id, centre) in detonated {
            // `this.discard()` precedes `this.explode()` in vanilla too.
            self.tnt.remove(&id);
            self.explode(centre, EXPLOSION_POWER, DamageFlags::default());
            self.pending_detonations.push(Detonation {
                centre,
                radius: EXPLOSION_POWER,
            });
        }
    }
}

/// A [`CollisionView`] over a caller-supplied block-state oracle — the TNT
/// analogue of `vehicles::VehicleCollision`/`mod::ItemCollision`: real
/// per-block-state collision shapes, and nothing else (no fluid buoyancy —
/// TNT does not float; see this module's doc for what fluid interaction is
/// and is not modelled).
struct TntCollision<'a> {
    block_state: &'a dyn Fn(i32, i32, i32) -> String,
}

impl CollisionView for TntCollision<'_> {
    fn collision_boxes(&self, x: i32, y: i32, z: i32, out: &mut Vec<Aabb>) {
        let name = (self.block_state)(x, y, z);
        let Some(id) = block_state_id(&name).or_else(|| block_states::state_id(&name)) else {
            return;
        };
        let Some(shape) = lodestone_data::collision_shapes::collision_boxes(id) else {
            return;
        };
        let (bx, by, bz) = (f64::from(x), f64::from(y), f64::from(z));
        for b in shape {
            out.push(Aabb::new(
                bx + f64::from(b.min[0]),
                by + f64::from(b.min[1]),
                bz + f64::from(b.min[2]),
                bx + f64::from(b.max[0]),
                by + f64::from(b.max[1]),
                bz + f64::from(b.max[2]),
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::ChunkWorld;

    /// A flat stone floor at `y = 60`, air above; matches the falling-block and
    /// vehicle test rigs' shape.
    fn floor() -> impl Fn(i32, i32, i32) -> String {
        |_x, y, _z| {
            if y <= 60 {
                "minecraft:stone".to_owned()
            } else {
                "minecraft:air".to_owned()
            }
        }
    }

    fn sim() -> MobSim<'static> {
        let world: &'static ChunkWorld = Box::leak(Box::new(ChunkWorld::new(-64, 384)));
        MobSim::new(world)
    }

    /// The fuse and launch velocity, taken from the record rather than
    /// predicted: `DEFAULT_FUSE_TIME` is `80` (not a "plausible" round number
    /// like `100`, and specifically not `145`, the wrong value another agent's
    /// attempt at this exact feature predicted), and a fresh spawn's velocity
    /// magnitude matches `sqrt(2*0.02^2 + 0.2^2)` regardless of the random
    /// direction — a control on the launch geometry independent of the RNG
    /// stream.
    /// Finds a live entity's snapshot by id — the same surface a wire encoder
    /// consumes, so a test reading velocity/metadata off it is reading exactly
    /// what would reach a connection.
    fn snapshot_of(sim: &MobSim<'_>, id: i32) -> crate::protocol::EntitySnapshot {
        sim.snapshots()
            .into_iter()
            .find(|s| s.id == id)
            .expect("a live TNT must be streamed, or it reaches zero pixels")
    }

    #[test]
    fn a_fresh_spawn_starts_at_the_real_fuse_and_launch_speed() {
        let mut sim = sim();
        let id = sim.spawn_tnt(Vec3::new(4.5, 70.0, -9.5), DEFAULT_FUSE_TIME);
        assert_eq!(sim.tnt_fuse(id), Some(80), "DEFAULT_FUSE_TIME is 80, not a rounder guess");
        assert_eq!(sim.tnt_position(id), Some(Vec3::new(4.5, 70.0, -9.5)));

        let snap = snapshot_of(&sim, id);
        assert!(
            (snap.velocity.y - 0.2).abs() < 1e-12,
            "vertical launch component must be the fixed 0.2, got {}",
            snap.velocity.y
        );
        let horizontal = (snap.velocity.x.powi(2) + snap.velocity.z.powi(2)).sqrt();
        assert!(
            (horizontal - LAUNCH_HORIZONTAL).abs() < 1e-9,
            "horizontal launch magnitude must be 0.02 regardless of direction, got {horizontal}"
        );
        assert_eq!(
            snap.metadata,
            vec![crate::protocol::MetadataField::TntFuse(80)],
            "the fuse must ride the wire as metadata, or a client cannot animate it"
        );
    }

    /// Two independently spawned TNTs from the same sim draw *different*
    /// launch directions — the control that the RNG stream is really being
    /// consumed per spawn, not a fixed direction dressed up as random.
    #[test]
    fn two_spawns_draw_different_launch_directions() {
        let mut sim = sim();
        let a = sim.spawn_tnt(Vec3::new(0.5, 70.0, 0.5), DEFAULT_FUSE_TIME);
        let b = sim.spawn_tnt(Vec3::new(0.5, 70.0, 0.5), DEFAULT_FUSE_TIME);
        let va = snapshot_of(&sim, a).velocity;
        let vb = snapshot_of(&sim, b).velocity;
        assert!(
            (va.x - vb.x).abs() > 1e-9 || (va.z - vb.z).abs() > 1e-9,
            "two spawns must not share one launch direction: {va:?} vs {vb:?}"
        );
    }

    /// **The discriminating pair**: a fuse that reaches zero detonates and
    /// really destroys blocks at the predicted radius; a fuse that has not
    /// yet elapsed must not. Both use the same stone floor and the same
    /// pairwise-distinct spawn point.
    #[test]
    fn a_finished_fuse_detonates_and_destroys_blocks_an_unfinished_one_does_not() {
        // Arm 1: run the fuse all the way out over a solid floor the TNT
        // lands on, so the blast has real stone to destroy nearby.
        let mut long = sim();
        let id = long.spawn_tnt(Vec3::new(11.5, 61.0, -6.5), DEFAULT_FUSE_TIME);
        for _ in 0..DEFAULT_FUSE_TIME {
            long.tick_tnt(&floor());
        }
        assert_eq!(long.tnt_count(), 0, "the entity must discard itself on detonation");
        let detonations = long.take_detonations();
        assert_eq!(detonations.len(), 1, "exactly one detonation for one TNT");
        assert_eq!(detonations[0].radius, EXPLOSION_POWER);
        // Not an exact match: the random launch impulse (`LAUNCH_HORIZONTAL`,
        // magnitude 0.02) gives the entity a small, direction-dependent
        // horizontal drift before ground friction and air drag settle it, so
        // the resting `x` is *near* the spawn `x`, not identical to it. Loose
        // enough to accept any real drift, tight enough that a blast centred
        // somewhere else entirely (a stale/zeroed position) would still fail
        // it.
        assert!(
            (detonations[0].centre.x - 11.5).abs() < 1.0,
            "the blast centre must track the entity's own resting position, got {}",
            detonations[0].centre.x
        );

        // Arm 2: the negative control. One tick short of the fuse, nothing
        // has detonated and nothing is queued.
        let mut short = sim();
        let short_id = short.spawn_tnt(Vec3::new(-3.5, 61.0, 14.5), DEFAULT_FUSE_TIME);
        for _ in 0..DEFAULT_FUSE_TIME - 1 {
            short.tick_tnt(&floor());
        }
        assert_eq!(
            short.tnt_count(),
            1,
            "a fuse one tick short of zero must not have detonated"
        );
        assert!(short.tnt_fuse(short_id).unwrap() > 0);
        assert!(
            short.take_detonations().is_empty(),
            "no detonation may be queued before the fuse actually reaches zero"
        );
    }

    /// `random_short_fuse` matches vanilla's formula rather than a plausible
    /// stand-in: `nextInt(max(1, fuse/4)) + fuse/8`, so for the default fuse
    /// of 80 the result is drawn from `[10, 29]` (`nextInt(20)` in `[0, 19]`,
    /// plus the fixed `10`), never below 10 and never at or above 30.
    #[test]
    fn random_short_fuse_matches_the_vanilla_formula_bounds() {
        let mut rng = SpawnRng::new(0x5348_4f52_545f_4655);
        for _ in 0..64 {
            let fuse = random_short_fuse(DEFAULT_FUSE_TIME, &mut rng);
            assert!(
                (10..30).contains(&fuse),
                "getRandomShortFuse(80, _) must land in [10, 29], got {fuse}"
            );
        }
    }

    /// The on-ground bounce: a TNT that lands still carries a small upward
    /// velocity on the very next tick, rather than resting dead — vanilla's
    /// `multiply(0.7, -0.5, 0.7)` flips a downward landing velocity's sign.
    #[test]
    fn a_landed_tnt_hops_rather_than_resting_dead() {
        let mut sim = sim();
        let id = sim.spawn_tnt(Vec3::new(5.5, 63.0, 5.5), DEFAULT_FUSE_TIME);
        let mut saw_on_ground = false;
        for _ in 0..40 {
            sim.tick_tnt(&floor());
            if sim.tnt_fuse(id).is_none() {
                break; // detonated before landing would be a test-premise bug
            }
            let velocity = snapshot_of(&sim, id).velocity;
            // A grounded tick is the one whose *upward* velocity is positive —
            // the bounce fires only then; an airborne tick still falls.
            if velocity.y > 0.0 {
                saw_on_ground = true;
                break;
            }
        }
        assert!(saw_on_ground, "the TNT must actually reach the floor inside 40 ticks");
    }
}
