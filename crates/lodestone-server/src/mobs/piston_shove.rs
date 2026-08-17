//! Piston entity shoving (issue #694) — the entity-aware half of a piston
//! move that `crate::piston`'s own module doc names as still missing:
//! `PistonMovingBlockEntity`'s `moveCollidedEntities`/`moveStuckEntities`.
//!
//! # What it is
//!
//! [`MobSim::shove_from_piston`] pushes every mob whose own bounding box
//! overlaps a moving piston cell's swept path out of the way, by one full
//! block in the direction the block travels — the same shape
//! [`MobSim::explode`](super::MobSim::explode) already established for a
//! different spatial effect (a per-mob AABB built from
//! [`SimMob::shape`](super::SimMob::shape), tested against a query region).
//!
//! # How it works
//!
//! [`crate::tick`]'s own doc on why this lives here rather than in
//! `crate::piston`/`crate::random_tick`: the shared reaction surface those
//! two modules build on (`random_tick::react_to_notification`) is
//! deliberately entity-agnostic — it is the *one* reaction path dust,
//! torches, repeaters, comparators, observers, buttons **and** pistons all
//! run through, and threading a mob parameter into it would touch every one
//! of those families for a capability only pistons need. So the wiring is
//! the other option issue #694 named: `crate::tick`'s own
//! `propagate_and_react_with_entities` call sites already hold `mobs: &MobHandle`
//! (see `crate::tick::post_note_block_vibration` for the identical shape,
//! landed for note-block hearing) and already read every
//! [`RandomTickEvent`](crate::random_tick::RandomTickEvent) a reaction
//! produces; `crate::tick::shove_entities_from_piston` is the new, narrow
//! consumer that recognises a `moving_piston` write among them and calls
//! this method. No change to `crate::piston` or `crate::random_tick` at
//! all — the shared surface stays exactly as entity-agnostic as it was.
//!
//! **The swept region is derived from the *destination* cell alone, and one
//! formula covers every moving cell.** A `moving_piston` write's own
//! position is always the block's destination (`piston::apply_move` writes
//! `push_direction.relative(pos)`, and `piston::begin_move` carries that
//! position through unchanged for a pushed block; the piston's own head/base
//! cell follows the identical shape — see this module's own test for the
//! retracting-head case, where the "moving" cell is the *base*, not the
//! arm). So `source = destination - push_direction` is exactly the cell the
//! block just vacated, for a pushed block, a freshly extended head, and a
//! home-coming retracted head alike, and the query region is simply the
//! union of those two unit cells — no case split on
//! [`piston::MovingBlockEntity::source`] needed.
//!
//! # What this deliberately does not do
//!
//! - **One discrete shove, not vanilla's continuous per-tick sweep.**
//!   `PistonMovingBlockEntity.moveCollidedEntities` interpolates the moving
//!   block's shape across the whole 2-tick animation and translates an
//!   entity by exactly as much of that sweep as it overlaps, every tick.
//!   This crate's own move is already a one-step world write animated only
//!   for the client (`crate::piston`'s own module doc), so there is no
//!   per-tick progress to interpolate against; a mob overlapping the swept
//!   region is moved a full block immediately, once, when the
//!   `moving_piston` cell first appears.
//! - **No crush damage, no `PushReaction::Ignore`/`SLIME_BLOCK` bounce.**
//!   Vanilla's own crush case is not a separate mechanic here either — a
//!   mob left overlapping solid terrain after the shove takes whatever
//!   ordinary in-block damage this crate already applies to any entity
//!   stuck inside a block, ordinary suffocation, not a piston-specific hit.
//!   Every mob is treated as [`PushReaction::Normal`], real vanilla's
//!   default for the overwhelming majority of entity types.
//! - **Players are not shoved through here, but they are shoved.**
//!   [`MobSim::explode`](super::MobSim::explode)'s own doc already
//!   establishes why this method cannot reach one: a connected player's
//!   position is client-reported, not server-owned state this sim can just
//!   translate (ordinary hostile melee against a player has the identical
//!   gap — see `crate::mobs::warden::SONIC_BOOM_KNOCKBACK_HORIZONTAL`'s own
//!   doc). Issue #694's own text scoped this as needing "a server-authoritative
//!   correction sent to the client", and that now exists, just not in this
//!   file: `crate::tick::shove_entities_from_piston` (this method's own
//!   caller) also publishes a
//!   [`crate::effects::WorldEffect::PistonPlayerPush`] alongside the
//!   `sim.shove_from_piston` call below, and the *player's own connection*
//!   applies it to its own last-known position and sends a real teleport —
//!   see that variant's own doc and `crate::server`'s handling of it.
//! - **The `moving_piston` collision-shape gap is fixed for mobs, but not
//!   here.** `crate::mobs::world::ChunkWorld::collision_top` — the
//!   [`lodestone_entity::pathfinding::PathWorld`] method
//!   `NavigatingMob::ground_below` reads to find the floor beneath a mob —
//!   now treats a `moving_piston` cell as a full block rather than reading
//!   the per-state table straight through (which is genuinely empty for
//!   that state; see that method's own doc for why). A mob standing on a
//!   block mid-push no longer falls through it. **This crate has no
//!   server-side physics for a connected player** (position is
//!   client-reported, the same boundary the player-shoving note above
//!   already states), so whether a real client itself falls through a
//!   moving block is a question of client-side collision this crate cannot
//!   answer or affect either way — untouched, not merely deferred.
//!
//! # Dependencies
//!
//! [`lodestone_physics::Aabb`] for the overlap test; nothing else new.

use lodestone_model::{BlockPos, Vec3};

use super::{ChunkWorld, MobSim, SimMob};
use crate::piston::Direction;

/// `Direction`'s unit step vector as a float triple. `crate::piston::push_delta`
/// now owns this (issue #694, item 4 needed the identical conversion for a
/// player correction outside `MobSim`); kept as a local alias so this file's
/// own call sites are unchanged.
fn direction_vector(direction: Direction) -> Vec3 {
    crate::piston::push_delta(direction)
}

/// The two-cell query region a single moving piston cell sweeps: the union
/// of the unit cubes at `source` and `dest`, whichever axis they differ on.
fn swept_cell_aabb(source: BlockPos, dest: BlockPos) -> lodestone_physics::Aabb {
    let (ax, ay, az) = (f64::from(source.x), f64::from(source.y), f64::from(source.z));
    let (bx, by, bz) = (f64::from(dest.x), f64::from(dest.y), f64::from(dest.z));
    lodestone_physics::Aabb::new(
        ax.min(bx),
        ay.min(by),
        az.min(bz),
        ax.max(bx) + 1.0,
        ay.max(by) + 1.0,
        az.max(bz) + 1.0,
    )
}

/// A mob's own bounding box — [`ExplosionAabb::from_size`](lodestone_entity::explosion::Aabb::from_size)'s
/// same shape, over [`lodestone_physics::Aabb`] instead, since that is the
/// type this module's own overlap test (and every other collision
/// consumer in this crate) already uses.
fn mob_aabb(m: &SimMob<'_>) -> lodestone_physics::Aabb {
    let pos = m.position();
    let shape = m.shape();
    let half_width = f64::from(shape.width) / 2.0;
    lodestone_physics::Aabb::new(
        pos.x - half_width,
        pos.y,
        pos.z - half_width,
        pos.x + half_width,
        pos.y + f64::from(shape.height),
        pos.z + half_width,
    )
}

impl<'w> MobSim<'w> {
    /// Issue #694: shoves every live mob whose own bounding box overlaps the
    /// swept region between `source` and `dest` (see this module's own doc
    /// for why those two cells alone are the whole query, for any moving
    /// piston cell) by one full block along `push_direction`.
    ///
    /// Returns the ids of every mob actually moved, so a caller — today,
    /// only this module's own tests — can tell a real shove from a no-op
    /// query.
    pub fn shove_from_piston(
        &mut self,
        source: BlockPos,
        dest: BlockPos,
        push_direction: Direction,
    ) -> Vec<i32> {
        let region = swept_cell_aabb(source, dest);
        let delta = direction_vector(push_direction);
        let mut shoved = Vec::new();
        for m in &mut self.mobs {
            if m.health <= 0.0 {
                continue;
            }
            if !mob_aabb(m).intersects(&region) {
                continue;
            }
            let pos = m.position();
            m.mob.set_position(Vec3::new(pos.x + delta.x, pos.y + delta.y, pos.z + delta.z));
            shoved.push(m.id);
        }
        shoved
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestone_model::ResourceKey;
    use std::str::FromStr;

    fn flat_world() -> ChunkWorld {
        ChunkWorld::new(-64, 384)
    }

    fn spawn(sim: &mut MobSim<'_>, species: &str, pos: Vec3) -> i32 {
        sim.spawn_species(
            ResourceKey::from_str(&format!("minecraft:{species}")).expect("valid key"),
            pos,
        )
        .id()
    }

    /// A mob standing in the destination cell of a pushed block is shoved
    /// exactly one block further along the push direction — the direct
    /// "block about to land on you" case.
    #[test]
    fn a_mob_in_the_destination_cell_is_shoved_one_block_further() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let id = spawn(&mut sim, "pig", Vec3::new(5.5, 0.0, 0.5));

        let source = BlockPos::new(4, 0, 0);
        let dest = BlockPos::new(5, 0, 0);
        let shoved = sim.shove_from_piston(source, dest, Direction::East);

        assert_eq!(shoved, vec![id], "the pig standing in the destination cell must be shoved");
        let pos = sim.get(id).expect("alive").position();
        assert!(
            (pos.x - 6.5).abs() < 1e-9 && pos.y == 0.0 && pos.z == 0.5,
            "must move exactly one block east, no other axis: {pos:?}"
        );
    }

    /// A mob standing in the *source* cell (the block's own starting
    /// position, still touching the swept region even before the write
    /// lands there) is shoved too — the region is the union of both cells,
    /// not the destination alone.
    #[test]
    fn a_mob_in_the_source_cell_is_also_shoved() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let id = spawn(&mut sim, "pig", Vec3::new(4.5, 0.0, 0.5));

        let source = BlockPos::new(4, 0, 0);
        let dest = BlockPos::new(5, 0, 0);
        let shoved = sim.shove_from_piston(source, dest, Direction::East);

        assert_eq!(shoved, vec![id]);
        let pos = sim.get(id).expect("alive").position();
        assert!((pos.x - 5.5).abs() < 1e-9, "must move one block east from its own starting cell: {pos:?}");
    }

    /// **Control**: a mob standing well clear of the swept region — one
    /// full block short of touching either cell — must never move. Without
    /// this, the positive tests above could pass merely because
    /// `shove_from_piston` moves every mob regardless of overlap.
    #[test]
    fn a_mob_outside_the_swept_region_is_never_shoved() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let id = spawn(&mut sim, "pig", Vec3::new(10.5, 0.0, 0.5));
        let before = sim.get(id).expect("alive").position();

        let source = BlockPos::new(4, 0, 0);
        let dest = BlockPos::new(5, 0, 0);
        let shoved = sim.shove_from_piston(source, dest, Direction::East);

        assert!(shoved.is_empty(), "a mob far from the swept region must not be shoved: {shoved:?}");
        let after = sim.get(id).expect("alive").position();
        assert_eq!(before, after, "position must be untouched");
    }

    /// The retracting-head shape this module's own doc names: the "moving"
    /// cell is the piston's *base*, and the swept region derived from
    /// `source = dest - push_direction` must still be the base-to-arm span,
    /// not the arm-to-base span backwards.
    #[test]
    fn a_retracting_heads_swept_region_covers_the_base_and_the_arm_cell() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        // The piston base is at (5, 0, 0); its arm/head is one block west
        // at (4, 0, 0) — matching `Direction::West` facing. Retracting:
        // `push_direction = facing.opposite() = East`, so the "moving" cell
        // (the base itself, per `begin_move`'s retract branch) is `dest`,
        // and `source = dest - East = dest + West step` — i.e. one block
        // further *west*, landing exactly on the arm cell (4, 0, 0). A mob
        // standing at the arm cell must be caught by the same swept-region
        // formula the extending case already proved.
        let id = spawn(&mut sim, "pig", Vec3::new(4.5, 0.0, 0.5));

        let dest = BlockPos::new(5, 0, 0); // the base cell, becoming moving_piston
        let source = BlockPos::new(4, 0, 0); // derived by the caller as dest - push_direction
        let shoved = sim.shove_from_piston(source, dest, Direction::East);

        assert_eq!(shoved, vec![id], "a mob at the retracting head's own cell must be shoved");
    }

    /// Item 3 of issue #694: the `moving_piston` collision-shape gap, for a
    /// mob rather than a connected player (see this module's own doc for
    /// why a player is a different, untouched question). Driven through the
    /// real production path an idle mob's floor-finding actually runs:
    /// `MobSim::tick` → `NavigatingMob::advance`'s no-waypoint branch →
    /// `ground_below` → `PathWorld::collision_top`
    /// (`crate::mobs::world::ChunkWorld::collision_top`).
    #[test]
    fn a_mob_standing_on_a_moving_piston_cell_does_not_fall_through_it() {
        let moving = crate::piston::moving_piston_state(Direction::East, false);

        // Control, run and observed rather than assumed: the per-state
        // collision table this method would otherwise read straight through
        // really is empty for `moving_piston` — proving the fix's whole
        // premise (see `collision_top`'s own doc for why: the census was
        // dumped with no block entity present, and vanilla's own
        // `MovingPistonBlock.getCollisionShape` returns `Shapes.empty()` in
        // exactly that case).
        let state_id = lodestone_data::block_states::state_id(&moving).expect("a real 26.2 state");
        let boxes = lodestone_data::collision_shapes::collision_boxes(state_id).expect("a known state id");
        assert!(
            boxes.is_empty(),
            "control: moving_piston's own per-state collision table must be empty — \
             that is the exact gap `collision_top`'s override reads around, boxes={boxes:?}"
        );

        // One real block of solid floor, with its centre cell swapped for a
        // moving_piston — the shape a pushed block's own destination cell
        // holds for `PISTON_MOVE_DELAY` ticks.
        let mut world = ChunkWorld::new(-64, 384);
        for x in 0..3 {
            for z in 0..3 {
                world.set_solid(x, -1, z, true);
            }
        }
        world.set_block(1, -1, 1, &moving);

        let mut sim = MobSim::new(&world);
        let id = spawn(&mut sim, "pig", Vec3::new(1.5, 0.0, 1.5));

        for _ in 0..5 {
            sim.tick();
        }

        let y = sim.get(id).expect("alive").position().y;
        assert_eq!(
            y, 0.0,
            "a mob standing over a moving_piston cell must not fall through it, y={y}"
        );
    }
}
