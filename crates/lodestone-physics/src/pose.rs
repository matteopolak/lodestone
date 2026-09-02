//! Pose-dependent dimensions, and the **fit gate** that decides which pose a
//! player is allowed to hold.
//!
//! # It is a state machine, not a lookup
//!
//! The obvious model — "swimming ⇒ use the `0.6 × 0.6` box" — is wrong, and
//! dangerously so. Vanilla's own per-tick pose-update step is, in outline:
//!
//! ```text
//! if fits(SWIMMING) {
//!     desired = compute_desired_pose()
//!     actual = if in_spectator_or_passenger || fits(desired) {
//!         desired
//!     } else if fits(CROUCHING) {
//!         CROUCHING
//!     } else {
//!         SWIMMING
//!     };
//!     set_pose(actual)
//! }
//! ```
//!
//! Three structural facts a naive port loses, in ascending order of how much they
//! cost:
//!
//! 1. **The desired pose is vetoed, not applied.** A pose whose box would not fit
//!    where the player already stands is refused and the machine falls back
//!    `desired → CROUCHING → SWIMMING`.
//! 2. **The whole body is behind an outer guard.** If even the `0.6 × 0.6`
//!    swimming box does not fit, vanilla's own pose-update step returns
//!    without ever committing a pose — the pose is **sticky**, keeping
//!    whatever it was, rather than collapsing to the smallest box. (Encased
//!    in blocks, you keep the pose you arrived in.)
//! 3. **There is no recovery if a player's box grows into a space it does not
//!    fit.** Vanilla's own dimensions-refresh step only re-centres an entity
//!    whose hitbox just grew when it is **not** running on the client and
//!    **not** a player. Both conjuncts exclude us twice over: no client ever
//!    re-centres, and no player ever does. **The fit gate is the only thing
//!    preventing a surfacing swimmer from being clipped into a low
//!    ceiling**, which is exactly what tying the box to
//!    [`PlayerState::swimming`] and skipping the gate would do.
//!
//! # What the pose actually changes
//!
//! Committing a pose sets the entity's hitbox dimensions and eye height, then
//! re-anchors the bounding box at the **feet**. So a pose change never moves
//! the player: the box's minimum Y is pinned to the feet position, and the
//! box only grows or shrinks upward. Width is `0.6` in every pose a player
//! can hold here, so in practice the pose decides exactly two numbers: **box
//! height** and **eye height**.
//!
//! Those two must move together. They are one record in vanilla, and
//! splitting them is observable: with a `0.6`-high box and a `1.62` eye,
//! `compute_fluid_state`'s cell sweep never reaches the eye's own cell, so a
//! fully submerged swimmer reads `eye_in_water == false` — no fog, no
//! overlay, and vanilla's own swimming-state update can never re-enter the
//! pose. [`update_player_pose`] therefore writes both.
//!
//! # Timing
//!
//! Vanilla's own pose-update step is the **last statement of its per-tick
//! player update**, run after the base tick, the AI step, travel and the move
//! have all happened. So the box a tick's movement collides with is the pose
//! decided at the end of the *previous* tick, and the fit gate always probes
//! the **post-move** position. [`crate::player::tick`] calls it in that
//! position; the narrower entry points ([`crate::player::tick_air`] and
//! friends) run only the travel step, not the full per-tick update, and
//! deliberately leave the pose alone.
//!
//! # Step height is not pose data
//!
//! Vanilla's own dimensions record carries width/height/eye-height and *not*
//! step height — that lives on the step-height attribute, read through
//! vanilla's own max-up-step accessor. A crouching player still steps `0.6`,
//! so every [`Pose::dimensions`] keeps [`EntityDimensions::PLAYER`]'s
//! `step_height`.

use crate::collision::{CollisionView, no_collision};
use crate::entity::EntityDimensions;
use crate::geometry::{Aabb, Vec3d};
use crate::player::{MovementInput, PlayerState};
use crate::push::{NearbyEntity, no_collision_among_entities};

/// The shrink vanilla's own fit check applies before testing collision — an
/// inflate by `-1.0E-7` on all six faces. It is what makes a box exactly as
/// tall as its gap *fit*: overlap is the strict `min < max` test, and this
/// pulls a flush face off the boundary.
pub const POSE_FIT_DEFLATION: f64 = 1.0E-7;

/// The subset of vanilla's own pose enum a player driven by this crate can
/// hold.
///
/// The five modelled poses are the five vanilla's own "desired pose" step can
/// return from state this crate has: swimming, fall-flying, spin-attack
/// (riptide), and the shift-key crouch/stand pair.
///
/// **Deliberately absent**, with vanilla's dimensions recorded so adding one
/// is mechanical rather than research:
///
/// | pose | dimensions | why it is not here |
/// |---|---|---|
/// | sleeping | a fixed `0.2 × 0.2` box, eye `0.2` | no bed/sleep state in this crate; vanilla's own "desired pose" step tests it **first**, so a driver adding sleep must add the pose too |
/// | dying | a fixed `0.2 × 0.2` box, eye `1.62` | set by the death handler, never by vanilla's own pose-update step |
///
/// Note the sleeping and dying poses are fixed-size, so the scale-attribute
/// fold does not apply to them. That fold is unmodelled here for the same
/// reason [`EntityDimensions`]'s docs give: the scale attribute is applied by
/// the caller before dimensions are constructed, and no caller in this repo
/// reports one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub enum Pose {
    /// Vanilla's own standing pose — a `0.6 × 1.8` box, eye `1.62`. The
    /// default.
    #[default]
    Standing,
    /// Vanilla's own crouching pose — a `0.6 × 1.5` box, eye `1.27`. Note the
    /// height is a distinct crouch-box constant, `1.5F` — **not** `1.65` and
    /// not `1.62`.
    Crouching,
    /// Vanilla's own swimming pose — a `0.6 × 0.6` box, eye `0.4`. The flat
    /// box that fits through a one-block gap.
    Swimming,
    /// Vanilla's own fall-flying pose — the same `0.6 × 0.6` box / eye `0.4`
    /// as [`Self::Swimming`], which is why an elytra glider also fits a
    /// one-block gap. Kept a distinct variant because vanilla's own "desired
    /// pose" step distinguishes them and because a caller reading the pose
    /// for animation must be able to.
    FallFlying,
    /// Vanilla's own spin-attack pose — the riptide-trident pose, entered by
    /// [`crate::player::apply_riptide`]. Same `0.6 × 0.6` box / eye `0.4` as
    /// [`Self::Swimming`]/[`Self::FallFlying`]; kept a distinct variant for
    /// the same reason those two are — vanilla's own "desired pose" step
    /// distinguishes it (it is checked *after* fall-flying, so gliding wins a
    /// tick where both would otherwise apply) and an animation reader needs
    /// to tell it apart from a swim.
    SpinAttack,
}

impl Pose {
    /// This pose's hitbox, as vanilla's own pose table defines it.
    ///
    /// `step_height` is **not** pose data — see the module docs — so it is
    /// [`EntityDimensions::PLAYER`]'s `0.6` for every pose.
    #[must_use]
    pub const fn dimensions(self) -> EntityDimensions {
        let height = match self {
            Self::Standing => 1.8,
            Self::Crouching => 1.5,
            Self::Swimming | Self::FallFlying | Self::SpinAttack => 0.6,
        };
        EntityDimensions::new(0.6, height, EntityDimensions::PLAYER.step_height)
    }

    /// This pose's eye height, i.e. vanilla's own eye-height-for-pose
    /// accessor.
    ///
    /// Every value is an explicit override in vanilla's own pose table; none
    /// is the `height * 0.85F` default (vanilla's own default-eye-height
    /// formula), which for standing would give `1.53` rather than `1.62`.
    #[must_use]
    pub const fn eye_height(self) -> f32 {
        match self {
            Self::Standing => crate::player::DEFAULT_EYE_HEIGHT,
            Self::Crouching => 1.27,
            Self::Swimming | Self::FallFlying | Self::SpinAttack => 0.4,
        }
    }

    /// The box vanilla's own block-and-entity fit check tests for this pose
    /// at `position`: the pose's bounding box at that position, deflated by
    /// `1.0E-7`.
    #[must_use]
    pub fn fit_box(self, position: Vec3d) -> Aabb {
        self.dimensions()
            .bounding_box(position)
            .inflate(-POSE_FIT_DEFLATION)
    }
}

/// Vanilla's own block-and-entity fit check, with the **block half only**.
///
/// This is the form [`update_player_pose`] uses when the caller has no
/// entity snapshot, and the gap it leaves is narrow rather than notional:
/// vanilla's own nearby-entity-collisions query filters on its own "can be
/// collided with" check, which the base entity type answers `false` and
/// **the living-entity type does not override**. The only three overrides in
/// 26.2 are a boat, a shulker and a happy ghast, so for a player with none of
/// those inside its pose box the entity term is *vacuously true* and this is
/// the whole predicate. See
/// [`can_player_fit_within_blocks_and_entities_when`] for the full form and
/// `docs/entity-push.md` for the measurement.
#[must_use]
pub fn can_player_fit_within_blocks_when(
    view: &dyn CollisionView,
    position: Vec3d,
    pose: Pose,
) -> bool {
    no_collision(view, pose.fit_box(position))
}

/// Vanilla's own block-and-entity fit check in full: no block collision
/// **and** no entity collision with the fit box.
///
/// The world-border term vanilla also checks remains unmodelled (no world
/// border in this engine).
#[must_use]
pub fn can_player_fit_within_blocks_and_entities_when(
    view: &dyn CollisionView,
    position: Vec3d,
    pose: Pose,
    nearby: &[NearbyEntity],
) -> bool {
    no_collision_among_entities(view, pose.fit_box(position), nearby)
}

/// Vanilla's own "desired pose" step — what the player *wants*, before the
/// fit gate has a say.
///
/// Vanilla's order is sleeping > swimming > fall-flying > spin-attack >
/// crouching/standing; the one absent branch (sleeping) is documented on
/// [`Pose`].
///
/// Three terms deserve their explanation:
///
/// * the swimming term is [`PlayerState::swimming`] — vanilla's own
///   swimming-state update's sprint-swim flag, maintained by
///   [`crate::player::tick`]. It is **not** "in water": a walking player in a
///   pond is standing.
/// * the spin-attack term is `state.auto_spin_attack_ticks > 0`
///   ([`PlayerState::is_auto_spin_attack`]) — set by
///   [`crate::player::apply_riptide`] and decremented once per tick, exactly
///   like vanilla's own auto-spin-attack countdown.
/// * the crouch term is "shift key held **and** not creative-flying", i.e.
///   the **raw shift key** (the same input vanilla's own ground-surface
///   check reads for the edge back-off) and not vanilla's own "is crouching"
///   check, which is *derived from the pose* and would be circular. This
///   crate models no creative flight, so the "not flying" conjunct is
///   vacuously true — a driver with a fly mode must not call
///   [`crate::player::tick`] while flying (which it already must not, for the
///   swimming-state update's sake).
#[must_use]
pub fn desired_pose(state: &PlayerState, input: MovementInput) -> Pose {
    if state.swimming {
        Pose::Swimming
    } else if state.fall_flying {
        Pose::FallFlying
    } else if state.is_auto_spin_attack() {
        Pose::SpinAttack
    } else if input.sneak && !state.flying {
        // Vanilla's own "desired pose" step: shift key held && not
        // creative-flying ? CROUCHING : STANDING. Holding shift while flying
        // is the *descend* control, not a crouch, so the box must stay `1.8`
        // tall — a `1.27` box would let a descending flier slip into gaps
        // vanilla keeps them out of, and `update_player_pose`'s fit gate
        // would then refuse to let them stand back up under a low ceiling.
        Pose::Crouching
    } else {
        Pose::Standing
    }
}

/// Vanilla's own pose-update step — pick the pose, veto it against the
/// world, and commit box **and** eye height together.
///
/// Called from the end of [`crate::player::tick`] /
/// [`crate::player::tick_among_entities`]; see the module docs for why that
/// position is load-bearing and why the narrower travel entry points do not
/// call it.
///
/// `nearby` supplies the entity term of the gate. Passing `&[]` is the
/// block-only predicate and is correct for every world without a boat,
/// shulker or happy ghast overlapping the player.
///
/// Vanilla's own spectator/passenger shortcut, which lets it adopt an
/// unfittable desired pose, is **not** modelled: this crate has neither
/// spectator mode nor vehicles, and a driver that adds them must apply the
/// bypass itself rather than have physics guess. Its absence is conservative
/// — it can only refuse a pose vanilla would have granted, never the
/// reverse.
pub fn update_player_pose(
    state: &mut PlayerState,
    input: MovementInput,
    view: &dyn CollisionView,
    nearby: &[NearbyEntity],
) {
    let fits = |pose: Pose| {
        can_player_fit_within_blocks_and_entities_when(view, state.position, pose, nearby)
    };

    // The outer guard: with not even the smallest box fitting, vanilla never
    // commits a pose and the pose is left exactly as it was.
    if !fits(Pose::Swimming) {
        return;
    }
    let desired = desired_pose(state, input);
    let actual = if fits(desired) {
        desired
    } else if fits(Pose::Crouching) {
        Pose::Crouching
    } else {
        Pose::Swimming
    };
    set_pose(state, actual);
}

/// The two fields of vanilla's own "set pose, then refresh dimensions" step
/// that reach this crate: the pose itself (which [`PlayerState::dimensions`]
/// reads for the box) and the derived eye height.
///
/// Vanilla's own position-reapply step has no analogue to write, because
/// this crate stores the feet position and derives the box on demand rather
/// than caching it. The size-growth recovery is *deliberately* absent — it
/// excludes players and clients twice over; see the module docs.
fn set_pose(state: &mut PlayerState, pose: Pose) {
    state.pose = pose;
    state.eye_height = pose.eye_height();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::Aabb;

    /// A floor at `y = 0` and a ceiling block at `y = 2`: a one-block gap.
    struct Gap;
    impl CollisionView for Gap {
        fn collision_boxes(&self, x: i32, y: i32, z: i32, out: &mut Vec<Aabb>) {
            if y == 0 || y == 2 {
                out.push(cube(x, y, z));
            }
        }
    }

    /// A floor at `y = 0` and a **top slab** at `y = 2` (world box `2.5 ..= 3.0`):
    /// exactly `1.5` of headroom above the floor's top face.
    struct SlabCeiling;
    impl CollisionView for SlabCeiling {
        fn collision_boxes(&self, x: i32, y: i32, z: i32, out: &mut Vec<Aabb>) {
            if y == 0 {
                out.push(cube(x, y, z));
            } else if y == 2 {
                out.push(Aabb::new(
                    f64::from(x),
                    f64::from(y) + 0.5,
                    f64::from(z),
                    f64::from(x) + 1.0,
                    f64::from(y) + 1.0,
                    f64::from(z) + 1.0,
                ));
            }
        }
    }

    struct OpenFloor;
    impl CollisionView for OpenFloor {
        fn collision_boxes(&self, x: i32, y: i32, z: i32, out: &mut Vec<Aabb>) {
            if y == 0 {
                out.push(cube(x, y, z));
            }
        }
    }

    /// Solid everywhere: not even the swimming box fits.
    struct Solid;
    impl CollisionView for Solid {
        fn collision_boxes(&self, x: i32, y: i32, z: i32, out: &mut Vec<Aabb>) {
            out.push(cube(x, y, z));
        }
    }

    fn cube(x: i32, y: i32, z: i32) -> Aabb {
        Aabb::new(
            f64::from(x),
            f64::from(y),
            f64::from(z),
            f64::from(x) + 1.0,
            f64::from(y) + 1.0,
            f64::from(z) + 1.0,
        )
    }

    fn feet() -> Vec3d {
        Vec3d::new(0.5, 1.0, 0.5)
    }

    fn player(pose: Pose) -> PlayerState {
        let mut s = PlayerState::at(feet(), 0.0);
        s.pose = pose;
        s.eye_height = pose.eye_height();
        s
    }

    fn sneaking() -> MovementInput {
        MovementInput {
            sneak: true,
            ..MovementInput::NONE
        }
    }

    #[test]
    fn standing_dimensions_are_the_shared_player_constant() {
        // If these two ever drift, every non-pose caller (navigation's
        // BODY_HEIGHT, the golden traces' `tick_air` path) silently disagrees with
        // the pose path about the same standing player.
        assert_eq!(Pose::Standing.dimensions(), EntityDimensions::PLAYER);
        assert_eq!(
            Pose::Standing.eye_height(),
            crate::player::DEFAULT_EYE_HEIGHT
        );
    }

    #[test]
    fn every_pose_keeps_the_step_height_attribute() {
        for pose in [
            Pose::Standing,
            Pose::Crouching,
            Pose::Swimming,
            Pose::FallFlying,
        ] {
            assert_eq!(
                pose.dimensions().step_height,
                EntityDimensions::PLAYER.step_height,
                "step height is the STEP_HEIGHT attribute, not pose data"
            );
            assert_eq!(pose.dimensions().width, 0.6, "width is 0.6 in every pose");
        }
    }

    #[test]
    fn the_fit_box_is_deflated_and_the_heights_are_widened_f32() {
        // Two things at once, because getting either wrong is invisible on screen.
        //
        // (1) The deflation is `1e-7` on all six faces, so the box is *inside* its
        //     nominal extent.
        // (2) The heights are `float` literals widened to `double`, and only
        //     `1.5F` is exact: `(double)0.6F == 0.6000000238418579` and
        //     `(double)1.8F == 1.7999999523162842`. A hand-written decimal `1.6`
        //     for the swimming box's top is wrong in the 8th place — the same
        //     order as the deflation it sits next to.
        let b = Pose::Swimming.fit_box(feet());
        assert_eq!(b.min_y, 1.0 + POSE_FIT_DEFLATION);
        assert_eq!(b.max_y, 1.0 + f64::from(0.6f32) - POSE_FIT_DEFLATION);
        assert_ne!(b.max_y, 1.6 - POSE_FIT_DEFLATION, "0.6F is not 0.6");
        assert_eq!(
            Pose::Standing.dimensions().bounding_box(feet()).max_y,
            1.0 + f64::from(1.8f32)
        );
        // `1.5F` *is* exact, so the crouch box's top lands precisely on a top
        // slab's bottom face — and it is the deflation that keeps a flush contact
        // out of the strict `min < max` overlap.
        let undeflated = Pose::Crouching.dimensions().bounding_box(feet());
        assert_eq!(undeflated.max_y, 2.5);
        assert_eq!(
            Pose::Crouching.fit_box(feet()).max_y,
            2.5 - POSE_FIT_DEFLATION
        );
    }

    #[test]
    fn a_swimmer_fits_a_one_block_gap_and_a_stander_does_not() {
        assert!(can_player_fit_within_blocks_when(
            &Gap,
            feet(),
            Pose::Swimming
        ));
        assert!(can_player_fit_within_blocks_when(
            &Gap,
            feet(),
            Pose::FallFlying
        ));
        // The controls: the two taller poses are refused in the same world at the
        // same position, so the assertion above is the height talking.
        assert!(!can_player_fit_within_blocks_when(
            &Gap,
            feet(),
            Pose::Crouching
        ));
        assert!(!can_player_fit_within_blocks_when(
            &Gap,
            feet(),
            Pose::Standing
        ));
    }

    #[test]
    fn releasing_shift_under_a_low_ceiling_is_vetoed_into_a_crouch() {
        // The fallback branch, and the reason the naive `pose = sneak ? CROUCHING
        // : STANDING` port is dangerous: there is no recovery for a player whose
        // box grows into a ceiling.
        let mut s = player(Pose::Crouching);
        update_player_pose(&mut s, MovementInput::NONE, &SlabCeiling, &[]);
        assert_eq!(s.pose, Pose::Crouching, "STANDING does not fit under 1.5");
        assert_eq!(s.eye_height, 1.27, "the eye height follows the box");

        // CONTROL: identical state, identical input, ceiling removed — the machine
        // *does* revert, so the line above is the fit gate and not a stuck flag.
        let mut open = player(Pose::Crouching);
        update_player_pose(&mut open, MovementInput::NONE, &OpenFloor, &[]);
        assert_eq!(open.pose, Pose::Standing);
        assert_eq!(open.eye_height, crate::player::DEFAULT_EYE_HEIGHT);
    }

    #[test]
    fn the_fallback_chain_reaches_swimming_when_even_crouching_is_refused() {
        // A one-block gap refuses both STANDING (desired) and CROUCHING, so the
        // third arm fires. A player who releases shift in a one-block gap ends up
        // *crawling*, which is vanilla and looks like a bug until you read the
        // source.
        let mut s = player(Pose::Swimming);
        update_player_pose(&mut s, MovementInput::NONE, &Gap, &[]);
        assert_eq!(s.pose, Pose::Swimming);
        assert_eq!(s.eye_height, 0.4);

        // Same world, sneaking: CROUCHING is *desired* and still refused, so the
        // second arm cannot be what produced the answer above.
        let mut c = player(Pose::Standing);
        update_player_pose(&mut c, sneaking(), &Gap, &[]);
        assert_eq!(c.pose, Pose::Swimming);
    }

    #[test]
    fn the_outer_guard_leaves_the_pose_alone_rather_than_shrinking_it() {
        // Encased in solid blocks: vanilla's own fit check for SWIMMING is
        // false, so vanilla never commits a pose and the pose is whatever it
        // already was — including a pose that manifestly does not fit.
        for held in [Pose::Standing, Pose::Crouching, Pose::Swimming] {
            let mut s = player(held);
            update_player_pose(&mut s, sneaking(), &Solid, &[]);
            assert_eq!(
                s.pose, held,
                "the outer guard must not rewrite the pose at all"
            );
            assert_eq!(s.eye_height, held.eye_height());
        }
        // CONTROL: the same held pose in a fittable world *is* rewritten, so the
        // assertion above is the guard and not an inert function.
        let mut s = player(Pose::Swimming);
        update_player_pose(&mut s, sneaking(), &OpenFloor, &[]);
        assert_eq!(s.pose, Pose::Crouching);
    }

    #[test]
    fn desired_pose_follows_vanillas_priority_order() {
        let mut s = player(Pose::Standing);
        assert_eq!(desired_pose(&s, MovementInput::NONE), Pose::Standing);
        assert_eq!(desired_pose(&s, sneaking()), Pose::Crouching);
        s.fall_flying = true;
        assert_eq!(
            desired_pose(&s, sneaking()),
            Pose::FallFlying,
            "fall flying outranks the shift key"
        );
        s.swimming = true;
        assert_eq!(
            desired_pose(&s, sneaking()),
            Pose::Swimming,
            "swimming outranks fall flying"
        );
    }

    #[test]
    fn an_entity_can_veto_a_pose_and_a_mob_cannot() {
        // The entity term of the gate, with the same asymmetry
        // `docs/entity-push.md` measured: vanilla's own "can be collided
        // with" check is false for every player and every mob, so only a
        // boat/shulker/happy ghast can change the answer.
        let dims = EntityDimensions::PLAYER;
        let mob = NearbyEntity::living(feet(), dims.bounding_box(feet()));
        assert!(can_player_fit_within_blocks_and_entities_when(
            &OpenFloor,
            feet(),
            Pose::Standing,
            &[mob]
        ));
        let mut shulker = mob;
        shulker.collidable = true;
        shulker.pushable = false;
        assert!(!can_player_fit_within_blocks_and_entities_when(
            &OpenFloor,
            feet(),
            Pose::Standing,
            &[shulker]
        ));
        // …and it reaches the machine: standing is vetoed, so the fallback runs.
        // The shulker's box is 1.8 tall, so CROUCHING is refused too and the chain
        // lands on SWIMMING — whose 0.6 box still overlaps it, which is exactly the
        // outer guard's job, so the pose is left alone.
        let mut s = player(Pose::Standing);
        update_player_pose(&mut s, MovementInput::NONE, &OpenFloor, &[shulker]);
        assert_eq!(s.pose, Pose::Standing, "outer guard: nothing fit");
    }
}
