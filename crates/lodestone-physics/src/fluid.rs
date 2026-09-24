//! Fluid **flow currents** — the horizontal push a moving fluid applies to an
//! entity standing in it (vanilla's own fluid-interaction update step → its
//! own tracker update/apply-current steps and its own flow-vector step).
//!
//! This is the part of fluid movement that the coarse "fully submerged" hooks in
//! [`crate::collision::CollisionView`] (`is_water`/`is_lava`) deliberately do not
//! model: buoyancy and drag only depend on *being* in the fluid, but the current
//! push depends on the fluid's per-block **level** and its neighbours' levels,
//! because water flows from a higher column toward a lower one.
//!
//! # Widths (load-bearing)
//!
//! Vanilla mixes `float` and `double` here on purpose and the server sees the
//! result, so the widths are reproduced exactly:
//!
//! * Vanilla's own "get own height" formula (`amount / 9.0F`) is **`float`**;
//!   the flow `distance` (this cell's height minus the neighbour's, and the
//!   `- 0.8888889F` downflow term) is computed in `float`.
//! * a direction's step multiplied by the distance is `int * float` →
//!   **`float`**, then accumulated into a **`double`** `flow_x`/`flow_z`.
//! * Vanilla's own vector-normalize step compares `length` against the
//!   `float` literal `1.0E-5F` widened to `double` —
//!   [`crate::geometry::Vec3d::normalize`] already does this.
//! * The tracker `height`, the accumulated current, the `1/count` average and
//!   the `0.014`/`0.0023…` push scales are all **`double`**.
//!
//! # Scope
//!
//! The common horizontal-flow case (a source or flowing column beside a lower or
//! empty neighbour) is modelled exactly, including the empty-neighbour *downflow*
//! branch (`0.8888889F`) and the "falling" downward jet. The push accumulates
//! and applies with vanilla's player-specific `1/count` averaging, the
//! sub-`0.4` height taper, and the `0.0045` minimum-impulse floor.

use crate::collision::CollisionView;
use crate::geometry::Vec3d;
use crate::mth;

/// Which fluid a cell holds — vanilla distinguishes water and lava tags, each
/// with its own push scale.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FluidKind {
    /// Flowing or source water.
    Water,
    /// Flowing or source lava.
    Lava,
}

/// The fluid occupying a single block cell, mirroring the parts of
/// vanilla's own fluid-state record that flow currents need.
///
/// `amount` is vanilla's own get-amount accessor: **8** for a source,
/// **1..=7** for flowing fluid (higher = deeper). `falling` is vanilla's own
/// "falling" fluid property (fluid pouring straight down).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FluidCell {
    /// Water or lava.
    pub kind: FluidKind,
    /// Vanilla's own get-amount accessor, in `1..=8` (source = 8).
    pub amount: u8,
    /// Vanilla's own "falling" fluid property.
    pub falling: bool,
}

impl FluidCell {
    /// Vanilla's own "get own height" formula: `amount / 9.0F` (a
    /// **`float`**). Source (`amount == 8`) → `0.8888889` (`8/9`).
    #[must_use]
    pub fn own_height(self) -> f32 {
        f32::from(self.amount) / 9.0f32
    }
}

/// A horizontal direction, iterated in vanilla's own horizontal-plane order
/// (`NORTH, EAST, SOUTH, WEST`) so the `float`→`double` flow accumulation matches
/// vanilla's summation order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HorizontalDir {
    /// `-Z`.
    North,
    /// `+X`.
    East,
    /// `+Z`.
    South,
    /// `-X`.
    West,
}

impl HorizontalDir {
    /// Vanilla's own horizontal-plane iteration order.
    pub const ALL: [HorizontalDir; 4] = [
        HorizontalDir::North,
        HorizontalDir::East,
        HorizontalDir::South,
        HorizontalDir::West,
    ];

    /// Vanilla's own per-direction X-step accessor.
    #[must_use]
    pub const fn step_x(self) -> i32 {
        match self {
            HorizontalDir::East => 1,
            HorizontalDir::West => -1,
            _ => 0,
        }
    }

    /// Vanilla's own per-direction Z-step accessor.
    #[must_use]
    pub const fn step_z(self) -> i32 {
        match self {
            HorizontalDir::North => -1,
            HorizontalDir::South => 1,
            _ => 0,
        }
    }
}

/// Vanilla's own "affects flow" check: the neighbour is empty, or holds the
/// *same* fluid.
///
/// A neighbouring cell affects the flow only if it is empty or holds the *same*
/// fluid; a different fluid (lava beside water) does not.
fn affects_flow(neighbour: Option<FluidCell>, kind: FluidKind) -> bool {
    match neighbour {
        None => true,
        Some(n) => n.kind == kind,
    }
}

/// Vanilla's own "get own height" formula for a neighbour cell, treating a
/// different fluid as absent. Only called after [`affects_flow`], so `Some`
/// implies same kind.
fn neighbour_own_height(neighbour: Option<FluidCell>, kind: FluidKind) -> f32 {
    match neighbour {
        Some(n) if n.kind == kind => n.own_height(),
        _ => 0.0f32,
    }
}

/// Vanilla's own "get flow" step — the unit flow direction of the fluid at
/// `(x, y, z)`.
///
/// Scans the four horizontal neighbours: a neighbour that is lower (or empty
/// over a drop) pulls the flow toward it. The result is **normalized** (a unit
/// vector or zero), so callers scale it by the per-cell height taper and the
/// water/lava push scale.
#[must_use]
pub fn get_flow(view: &dyn CollisionView, x: i32, y: i32, z: i32, cell: FluidCell) -> Vec3d {
    let this_height = cell.own_height();
    let mut flow_x = 0.0f64;
    let mut flow_z = 0.0f64;

    for dir in HorizontalDir::ALL {
        let nx = x + dir.step_x();
        let nz = z + dir.step_z();
        let neighbour = view.fluid_at(nx, y, nz);
        if !affects_flow(neighbour, cell.kind) {
            continue;
        }

        let neighbour_height = neighbour_own_height(neighbour, cell.kind);
        let mut distance = 0.0f32;
        if neighbour_height == 0.0f32 {
            // Empty neighbour: flow can spill over the edge if the neighbouring
            // block does not block motion and there is fluid one block below it.
            if !view.blocks_motion(nx, y, nz) {
                let below = view.fluid_at(nx, y - 1, nz);
                if affects_flow(below, cell.kind) {
                    let below_height = neighbour_own_height(below, cell.kind);
                    if below_height > 0.0f32 {
                        distance = this_height - (below_height - 0.8888889f32);
                    }
                }
            }
        } else if neighbour_height > 0.0f32 {
            distance = this_height - neighbour_height;
        }

        if distance != 0.0f32 {
            flow_x += f64::from((dir.step_x() as f32) * distance);
            flow_z += f64::from((dir.step_z() as f32) * distance);
        }
    }

    let mut flow = Vec3d::new(flow_x, 0.0, flow_z);
    if cell.falling {
        // A falling fluid next to a solid face gets a strong downward jet.
        for dir in HorizontalDir::ALL {
            let nx = x + dir.step_x();
            let nz = z + dir.step_z();
            if view.is_solid_face(nx, y, nz, dir, cell.kind)
                || view.is_solid_face(nx, y + 1, nz, dir, cell.kind)
            {
                flow = flow.normalize().add(Vec3d::new(0.0, -6.0, 0.0));
                break;
            }
        }
    }

    flow.normalize()
}

/// Accumulated current over the cells an entity's fluid-interaction box touches,
/// mirroring vanilla's own fluid-interaction tracker for a single fluid kind.
#[derive(Debug, Clone, Copy)]
struct Tracker {
    /// The max of `fluid_top - entity_y` across cells — the submersion
    /// depth, which tapers the current below `0.4`.
    height: f64,
    accumulated: Vec3d,
    count: i32,
}

impl Tracker {
    fn new() -> Self {
        Self {
            height: 0.0,
            accumulated: Vec3d::ZERO,
            count: 0,
        }
    }
}

/// Vanilla's own fluid-interaction update step for one fluid `kind`: scans the
/// entity's fluid-interaction box (its own bounding box deflated by `0.001`),
/// accumulates the current, and applies it to `state.velocity` (vanilla's own
/// velocity-add step).
///
/// Called at the **start** of the in-fluid tick, before the AI-step velocity
/// snap-to-zero, because vanilla runs its own fluid-interaction update step
/// in its own base per-tick step, ahead of the AI step / travel step within
/// the same tick. `push_scale` is `0.014` for water and
/// `0.0023333333333333335` (overworld) / `0.007` (nether) for lava.
///
/// The `entity_y` used for the depth is the **un-deflated** box minimum
/// (vanilla's own bounding-box accessor's minimum Y), while the loop bounds
/// and the `fluid_top >= min_y` gate use the deflated box — reproduced
/// exactly.
///
/// # Scope
///
/// [`crate::player::tick_water`]/[`crate::player::tick_lava`] invoke this from the
/// in-fluid travel branch, which the caller selects via the coarse
/// [`CollisionView::is_water`]/[`CollisionView::is_lava`] dispatch. Vanilla
/// couples the two the same way — its own fluid-interaction update step sets
/// its own in-water flag, and its own travel step picks the water branch iff
/// that flag is set — so a submerged or swimming player (the prioritised
/// case) is pushed exactly. A player only *grazing* flowing water, where the
/// coarse dispatch and vanilla's deflated box scan disagree on "in water" by
/// a tick, inherits that existing approximation; a mismatch would surface as
/// a server correction, which the live gates watch for.
pub fn apply_fluid_push(
    state: &mut crate::player::PlayerState,
    view: &dyn CollisionView,
    kind: FluidKind,
    push_scale: f64,
    profile: &crate::profile::PhysicsProfile,
) {
    let bb = state.bounding_box(profile);
    // Vanilla's own fluid-interaction-box accessor: bounding box deflated by 0.001.
    let d = 0.001;
    let box_min_x = bb.min_x + d;
    let box_min_y = bb.min_y + d;
    let box_min_z = bb.min_z + d;
    let box_max_x = bb.max_x - d;
    let box_max_y = bb.max_y - d;
    let box_max_z = bb.max_z - d;

    let x0 = mth::floor(box_min_x);
    let y0 = mth::floor(box_min_y);
    let z0 = mth::floor(box_min_z);
    let x1 = mth::ceil(box_max_x) - 1;
    let y1 = mth::ceil(box_max_y) - 1;
    let z1 = mth::ceil(box_max_z) - 1;

    // entityY is the *un-deflated* bounding-box minimum.
    let entity_y = bb.min_y;

    let mut tracker = Tracker::new();
    for x in x0..=x1 {
        for y in y0..=y1 {
            for z in z0..=z1 {
                let Some(fluid) = view.fluid_at(x, y, z) else {
                    continue;
                };
                if fluid.kind != kind {
                    continue;
                }
                // Vanilla's own get-height accessor: 1.0F if the same fluid
                // sits directly above, else its own get-own-height formula.
                let same_above = view
                    .fluid_at(x, y + 1, z)
                    .is_some_and(|above| above.kind == kind);
                let cell_height = if same_above {
                    1.0f32
                } else {
                    fluid.own_height()
                };
                let fluid_top = f64::from(y) + f64::from(cell_height);
                if fluid_top < box_min_y {
                    continue;
                }

                tracker.height = (fluid_top - entity_y).max(tracker.height);
                let mut flow = get_flow(view, x, y, z, fluid);
                if tracker.height < 0.4 {
                    flow = flow.scale(tracker.height);
                }
                tracker.accumulated = tracker.accumulated.add(flow);
                tracker.count += 1;
            }
        }
    }

    apply_current_to(state, &tracker, push_scale);
}

/// Vanilla's own tracker's apply-current-to step, for a player.
fn apply_current_to(state: &mut crate::player::PlayerState, tracker: &Tracker, scale: f64) {
    if tracker.count == 0 || tracker.accumulated.length_sqr() < f64::from(1.0e-5f32) {
        return;
    }

    // Player branch: average by the number of contributing cells (non-players
    // normalize instead).
    let mut impulse = tracker.accumulated.scale(1.0 / f64::from(tracker.count));
    let old_movement = state.velocity;
    impulse = impulse.scale(scale);
    if old_movement.x.abs() < 0.003
        && old_movement.z.abs() < 0.003
        && impulse.length() < 0.004_500_000_000_000_000_5
    {
        impulse = impulse.normalize().scale(0.004_500_000_000_000_000_5);
    }

    state.velocity = state.velocity.add(impulse);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::Aabb;
    use std::collections::HashMap;

    /// Synthetic world: a sparse map of fluid cells plus a set of
    /// motion-blocking block positions. Everything else is air.
    #[derive(Default)]
    struct FluidWorld {
        fluids: HashMap<(i32, i32, i32), FluidCell>,
        solid: std::collections::HashSet<(i32, i32, i32)>,
    }

    impl FluidWorld {
        fn water(&mut self, x: i32, y: i32, z: i32, amount: u8) {
            self.fluids.insert(
                (x, y, z),
                FluidCell {
                    kind: FluidKind::Water,
                    amount,
                    falling: false,
                },
            );
        }
    }

    impl CollisionView for FluidWorld {
        fn collision_boxes(&self, _x: i32, _y: i32, _z: i32, _out: &mut Vec<Aabb>) {}

        fn fluid_at(&self, x: i32, y: i32, z: i32) -> Option<FluidCell> {
            self.fluids.get(&(x, y, z)).copied()
        }

        fn blocks_motion(&self, x: i32, y: i32, z: i32) -> bool {
            self.solid.contains(&(x, y, z))
        }
    }

    // --- Direction tests, derived from first principles, NOT from the reference
    //     source: water flows from a higher column toward a lower/empty one. ---

    #[test]
    fn source_beside_air_over_a_drop_flows_toward_the_drop() {
        // Source at origin; empty cell to the EAST that has water one block
        // below it (a ledge water pours over). Flow must point +X (east).
        let mut world = FluidWorld::default();
        world.water(0, 0, 0, 8);
        world.water(1, -1, 0, 8); // fluid below the empty east neighbour
        let cell = world.fluid_at(0, 0, 0).unwrap();
        let flow = get_flow(&world, 0, 0, 0, cell);
        assert!(flow.x > 0.0, "flow should point +X (east), got {flow:?}");
        assert_eq!(flow.z, 0.0);
        // Normalized to a unit vector.
        assert!((flow.length() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn source_beside_lower_flowing_water_flows_toward_it() {
        // Deep column (amount 8) with a shallower neighbour (amount 3) to the
        // WEST. Water flows from deep to shallow → -X (west).
        let mut world = FluidWorld::default();
        world.water(0, 0, 0, 8);
        world.water(-1, 0, 0, 3);
        let cell = world.fluid_at(0, 0, 0).unwrap();
        let flow = get_flow(&world, 0, 0, 0, cell);
        assert!(flow.x < 0.0, "flow should point -X (west), got {flow:?}");
        assert_eq!(flow.z, 0.0);
    }

    #[test]
    fn symmetric_lower_neighbours_cancel_to_zero() {
        // Equal-and-opposite shallower neighbours east and west cancel exactly.
        let mut world = FluidWorld::default();
        world.water(0, 0, 0, 8);
        world.water(1, 0, 0, 4);
        world.water(-1, 0, 0, 4);
        let cell = world.fluid_at(0, 0, 0).unwrap();
        let flow = get_flow(&world, 0, 0, 0, cell);
        assert_eq!(flow, Vec3d::ZERO, "symmetric neighbours must cancel");
    }

    #[test]
    fn full_neighbours_all_around_produce_no_flow() {
        // Surrounded by equal-height water on all four sides: no gradient.
        let mut world = FluidWorld::default();
        world.water(0, 0, 0, 8);
        world.water(1, 0, 0, 8);
        world.water(-1, 0, 0, 8);
        world.water(0, 0, 1, 8);
        world.water(0, 0, -1, 8);
        let cell = world.fluid_at(0, 0, 0).unwrap();
        let flow = get_flow(&world, 0, 0, 0, cell);
        assert_eq!(flow, Vec3d::ZERO);
    }

    #[test]
    fn blocked_empty_neighbour_does_not_pull_flow() {
        // Empty east neighbour but the block there blocks motion (a wall):
        // no spill-over, so no flow even though there is water below it.
        let mut world = FluidWorld::default();
        world.water(0, 0, 0, 8);
        world.water(1, -1, 0, 8);
        world.solid.insert((1, 0, 0));
        let cell = world.fluid_at(0, 0, 0).unwrap();
        let flow = get_flow(&world, 0, 0, 0, cell);
        assert_eq!(flow, Vec3d::ZERO);
    }

    // --- Push application: player 1/count averaging and the 0.0045 floor. ---

    fn player_at_origin() -> crate::player::PlayerState {
        // Feet at y=0; box spans y 0..1.8 over cells (0,0,0) and (0,1,0).
        crate::player::PlayerState::at(Vec3d::new(0.5, 0.0, 0.5), 0.0)
    }

    #[test]
    fn no_current_leaves_velocity_untouched() {
        let world = FluidWorld::default();
        let profile = crate::profile::PhysicsProfile::mc_1_21();
        let mut state = player_at_origin();
        state.velocity = Vec3d::new(0.1, 0.0, 0.0);
        apply_fluid_push(&mut state, &world, FluidKind::Water, 0.014, &profile);
        assert_eq!(state.velocity, Vec3d::new(0.1, 0.0, 0.0));
    }

    #[test]
    fn deep_current_pushes_a_still_player_in_the_flow_direction() {
        // Both occupied cells are deep water; the feet cell flows east toward a
        // drop, so a still player is pushed +X above the 0.0045 floor.
        let mut world = FluidWorld::default();
        world.water(0, 0, 0, 8);
        world.water(0, 1, 0, 8);
        world.water(1, -1, 0, 8); // fluid below the empty east spill cell
        let profile = crate::profile::PhysicsProfile::mc_1_21();
        let mut state = player_at_origin();
        state.velocity = Vec3d::ZERO;
        apply_fluid_push(&mut state, &world, FluidKind::Water, 0.014, &profile);
        assert!(
            state.velocity.x > 0.0 && state.velocity.z == 0.0,
            "should be pushed +X: {:?}",
            state.velocity
        );
    }

    #[test]
    fn shallow_current_is_clamped_to_the_minimum_impulse_floor() {
        // A single shallow cell (amount 2 → height 0.222, below the 0.4 taper)
        // produces a scaled impulse under 0.0045, which the floor snaps up to
        // exactly 0.0045000000000000005 in the flow direction.
        let mut world = FluidWorld::default();
        world.water(0, 0, 0, 2); // shallow: own_height 2/9 = 0.2222…
        world.water(1, -1, 0, 8); // drop to the east
        // (0,1,0) intentionally empty so only the shallow feet cell contributes.
        let profile = crate::profile::PhysicsProfile::mc_1_21();
        let mut state = player_at_origin();
        state.velocity = Vec3d::ZERO;
        apply_fluid_push(&mut state, &world, FluidKind::Water, 0.014, &profile);
        assert!(state.velocity.x > 0.0, "pushed +X: {:?}", state.velocity);
        let mag = state.velocity.length();
        assert!(
            (mag - 0.004_500_000_000_000_000_5).abs() < 1e-12,
            "expected the 0.0045 floor, got {mag}"
        );
    }
}
