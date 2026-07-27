//! Eye-in-fluid / box-in-fluid state (`EntityFluidInteraction.update`).
//!
//! Vanilla computes, once per tick in `Entity.baseTick`, a per-fluid summary of
//! how the entity's box sits in water and lava: the fluid **height** it reaches
//! (`getFluidHeight`, `> 0` ⇒ [`FluidState::in_water`]/[`FluidState::in_lava`])
//! and whether the **eye** is submerged (`isEyeInFluid`). Those two are *distinct*
//! — the box can intersect water while the eye is in open air (wading), and the
//! eye can be under while the feet are not (at a ledge) — and vanilla tracks water
//! and lava **separately**. This module reproduces that distinction bit-exactly.
//!
//! It gates four consumers that today have no shared source of truth:
//! submerged fog colour / view distance, the underwater overlay, the
//! `ambient.underwater.*` sounds, and the swimming pose. Computing it *once* here,
//! version-free, is what stops those four inventing four disagreeing booleans.
//!
//! # Presence vs. height
//!
//! Vanilla derives presence from height (`isInFluid = getFluidHeight > 0`), and
//! height from the per-block fluid **level** (`getHeight = hasSameAbove ? 1 :
//! amount/9`). Where the world exposes that level via
//! [`CollisionView::fluid_at`], this reproduces it exactly (the surface-bobbing
//! case needs it). Where the world only exposes the coarse
//! [`CollisionView::is_water`]/[`CollisionView::is_lava`] presence booleans — as
//! the live multiplayer adapter does — a present cell is treated as a **full**
//! cell (height `1.0`), which is exact for the fully-submerged common case and is
//! the same coarseness the rest of the crate already commits to.
//!
//! # Widths (load-bearing)
//!
//! `FluidState.getHeight()` is a **`float`** (`amount / 9.0F`), added to a
//! `double` `fluidBottom`; `getEyeY()` adds a **`float`** `eyeHeight` to a
//! `double` `position.y`. Both float→double promotions are reproduced at the same
//! places, because the server re-derives eye-in-water from the position we report
//! and a `0.001`-scale disagreement at the waterline flips the boolean.

use crate::collision::CollisionView;
use crate::fluid::FluidKind;
use crate::geometry::{Aabb, Vec3d};
use crate::mth;

/// The result of `EntityFluidInteraction.update` for one entity this tick:
/// per-fluid reach height and eye-submersion, for water and lava separately.
///
/// All four fields are *outputs* — derived from the entity's pre-move box and eye
/// position against the world. Consumers read the derived predicates
/// ([`Self::in_water`], [`Self::under_water`], …) rather than the raw heights.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FluidState {
    /// `getFluidHeight(WATER)` — the tallest `fluidTop - feetY` over the box's
    /// water cells (`0.0` if the box touches no water). `> 0.0` ⇒ in water.
    pub water_height: f64,
    /// `getFluidHeight(LAVA)` — as [`Self::water_height`] for lava.
    pub lava_height: f64,
    /// `isEyeInFluid(WATER)` — the eye block-column holds water spanning the eye
    /// Y. On its own this is *not* `isUnderWater`; combine with [`Self::in_water`]
    /// via [`Self::under_water`], exactly as vanilla's `wasEyeInWater && isInWater`.
    pub eye_in_water: bool,
    /// `isEyeInFluid(LAVA)`.
    pub eye_in_lava: bool,
}

impl FluidState {
    /// The "no fluid anywhere" state.
    pub const NONE: Self = Self {
        water_height: 0.0,
        lava_height: 0.0,
        eye_in_water: false,
        eye_in_lava: false,
    };

    /// `Entity.isInWater()` — the box intersects water (`getFluidHeight > 0`).
    #[must_use]
    pub fn in_water(&self) -> bool {
        self.water_height > 0.0
    }

    /// `Entity.isInLava()` — the box intersects lava.
    #[must_use]
    pub fn in_lava(&self) -> bool {
        self.lava_height > 0.0
    }

    /// `Entity.isUnderWater()` = `wasEyeInWater && isInWater()`. This is the flag
    /// that gates submerged fog, the overlay, the ambient sounds, and the
    /// sprint-swimming pose.
    #[must_use]
    pub fn under_water(&self) -> bool {
        self.eye_in_water && self.in_water()
    }

    /// The lava analogue of [`Self::under_water`] (eye in lava and box in lava).
    #[must_use]
    pub fn under_lava(&self) -> bool {
        self.eye_in_lava && self.in_lava()
    }
}

/// `EntityFluidInteraction.update(entity, ignoreCurrent = true)` — the flow
/// current is *not* accumulated here (that lives in [`crate::fluid`]); this
/// computes only the height/eye summary the four consumers need.
///
/// * `bounding_box` is the entity's **un-deflated** box at its pre-move position;
///   the interaction box is `deflate(0.001)` of it, matching `getFluidInteractionBox`.
/// * `position` supplies `getBlockX()`/`getBlockZ()` (the eye column, `floor` of
///   the box centre) — passed explicitly rather than recomputed from the box so
///   the `float`-cancellation of `±width/2` never perturbs the floor.
/// * `eye_height` is the pose eye height (`1.62` standing, `0.4` swimming); the
///   eye Y is `position.y + (double)eye_height`, reproducing `getEyeY`.
#[must_use]
pub fn compute_fluid_state(
    bounding_box: Aabb,
    position: Vec3d,
    eye_height: f32,
    view: &dyn CollisionView,
) -> FluidState {
    // `getFluidInteractionBox()` = `boundingBox.deflate(0.001)`. The cell range is
    // `floor(min) ..= ceil(max) - 1` of that deflated box.
    let d = 0.001;
    let x0 = mth::floor(bounding_box.min_x + d);
    let y0 = mth::floor(bounding_box.min_y + d);
    let z0 = mth::floor(bounding_box.min_z + d);
    let x1 = mth::ceil(bounding_box.max_x - d) - 1;
    let y1 = mth::ceil(bounding_box.max_y - d) - 1;
    let z1 = mth::ceil(bounding_box.max_z - d) - 1;

    // The skip test compares against the *deflated* box min; the height subtracts
    // the *un-deflated* box min (`entity.getBoundingBox().minY`). Vanilla uses the
    // two different boxes here and the 0.001 gap is observable at the waterline.
    let deflated_min_y = bounding_box.min_y + d;
    let entity_y = bounding_box.min_y;

    let eye_block_x = mth::floor(position.x);
    let eye_block_z = mth::floor(position.z);
    let eye_y = position.y + f64::from(eye_height);

    let mut state = FluidState::NONE;

    for x in x0..=x1 {
        for y in y0..=y1 {
            for z in z0..=z1 {
                let Some(kind) = fluid_kind_at(view, x, y, z) else {
                    continue;
                };
                let height = cell_height(view, x, y, z, kind);
                let fluid_bottom = f64::from(y);
                let fluid_top = fluid_bottom + f64::from(height);
                if fluid_top < deflated_min_y {
                    continue;
                }

                let eyes_here = x == eye_block_x
                    && z == eye_block_z
                    && eye_y >= fluid_bottom
                    && eye_y <= fluid_top;
                let reach = fluid_top - entity_y;
                match kind {
                    FluidKind::Water => {
                        if eyes_here {
                            state.eye_in_water = true;
                        }
                        state.water_height = state.water_height.max(reach);
                    }
                    FluidKind::Lava => {
                        if eyes_here {
                            state.eye_in_lava = true;
                        }
                        state.lava_height = state.lava_height.max(reach);
                    }
                }
            }
        }
    }

    state
}

/// The fluid occupying a cell, preferring the fine [`CollisionView::fluid_at`]
/// level and falling back to the coarse `is_water`/`is_lava` presence booleans.
fn fluid_kind_at(view: &dyn CollisionView, x: i32, y: i32, z: i32) -> Option<FluidKind> {
    if let Some(cell) = view.fluid_at(x, y, z) {
        Some(cell.kind)
    } else if view.is_water(x, y, z) {
        Some(FluidKind::Water)
    } else if view.is_lava(x, y, z) {
        Some(FluidKind::Lava)
    } else {
        None
    }
}

/// `FluidState.getHeight(level, pos)` = `hasSameAbove ? 1.0F : getOwnHeight()`.
///
/// Height is a **`float`**. When [`CollisionView::fluid_at`] gives the level, the
/// own-height is `amount / 9.0F`; without it (coarse presence only) a present cell
/// is a full cell, so height is `1.0`. `hasSameAbove` checks the cell directly
/// above for the same fluid, via the same fine-then-coarse resolution.
fn cell_height(view: &dyn CollisionView, x: i32, y: i32, z: i32, kind: FluidKind) -> f32 {
    match view.fluid_at(x, y, z) {
        Some(cell) => {
            if fluid_kind_at(view, x, y + 1, z) == Some(kind) {
                1.0f32
            } else {
                cell.own_height()
            }
        }
        // Presence-only world: treat the whole cell as fluid (the crate's
        // documented coarse stance), exact for a fully-submerged entity.
        None => 1.0f32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fluid::FluidCell;
    use crate::player::DEFAULT_EYE_HEIGHT;
    use std::collections::HashMap;

    /// Synthetic world: coarse presence sets plus an optional per-cell fluid
    /// level, so a test can exercise both the fine (`fluid_at`) and coarse
    /// (`is_water`/`is_lava`) resolution paths.
    #[derive(Default)]
    struct FluidWorld {
        water: std::collections::HashSet<(i32, i32, i32)>,
        lava: std::collections::HashSet<(i32, i32, i32)>,
        cells: HashMap<(i32, i32, i32), FluidCell>,
    }

    impl FluidWorld {
        fn water(&mut self, x: i32, y: i32, z: i32) {
            self.water.insert((x, y, z));
        }
        fn water_cell(&mut self, x: i32, y: i32, z: i32, amount: u8) {
            self.water.insert((x, y, z));
            self.cells.insert(
                (x, y, z),
                FluidCell {
                    kind: FluidKind::Water,
                    amount,
                    falling: false,
                },
            );
        }
        fn lava(&mut self, x: i32, y: i32, z: i32) {
            self.lava.insert((x, y, z));
        }
    }

    impl CollisionView for FluidWorld {
        fn collision_boxes(&self, _x: i32, _y: i32, _z: i32, _out: &mut Vec<Aabb>) {}
        fn is_water(&self, x: i32, y: i32, z: i32) -> bool {
            self.water.contains(&(x, y, z))
        }
        fn is_lava(&self, x: i32, y: i32, z: i32) -> bool {
            self.lava.contains(&(x, y, z))
        }
        fn fluid_at(&self, x: i32, y: i32, z: i32) -> Option<FluidCell> {
            self.cells.get(&(x, y, z)).copied()
        }
    }

    /// The player box (0.6 × 1.8) at feet-centre `pos`.
    fn player_box(pos: Vec3d) -> Aabb {
        crate::entity::EntityDimensions::PLAYER.bounding_box(pos)
    }

    #[test]
    fn fully_submerged_eye_and_box_are_in_water() {
        // Feet at y=64 with water filling the whole column the box occupies and
        // above the eye. Coarse presence only (no `fluid_at`) — the live path.
        let mut w = FluidWorld::default();
        for y in 63..=67 {
            w.water(0, y, 0);
        }
        let pos = Vec3d::new(0.5, 64.0, 0.5);
        let s = compute_fluid_state(player_box(pos), pos, DEFAULT_EYE_HEIGHT, &w);
        assert!(s.in_water(), "box is in water");
        assert!(s.eye_in_water, "eye is in water");
        assert!(s.under_water(), "isUnderWater = eye && box");
        assert!(!s.in_lava() && !s.eye_in_lava);
    }

    #[test]
    fn wading_box_in_water_but_eye_above_is_not_underwater() {
        // One block of water at the feet; eye (feet + 1.62) is well above it.
        // This is the box-vs-eye distinction the whole module exists to make: a
        // single boolean would get this wrong.
        let mut w = FluidWorld::default();
        w.water(0, 64, 0);
        let pos = Vec3d::new(0.5, 64.0, 0.5);
        let s = compute_fluid_state(player_box(pos), pos, DEFAULT_EYE_HEIGHT, &w);
        assert!(s.in_water(), "the box intersects water");
        assert!(!s.eye_in_water, "the eye is in open air above the water");
        assert!(!s.under_water(), "not submerged despite being in water");
    }

    #[test]
    fn surface_height_keeps_the_eye_out_when_it_sits_above_the_waterline() {
        // Eye is inside the eye block-column, but the fluid there only fills part
        // of the cell (amount 1 ⇒ 1/9 high, air above) and the eye Y is above the
        // real surface. Needs the `fluid_at` height, not full-cell — the bobbing
        // case. Place the player so the eye lands low in a partly-filled cell.
        let mut w = FluidWorld::default();
        // Deep water below, a thin skim in the eye's cell with air above it.
        w.water_cell(0, 64, 0, 8);
        w.water_cell(0, 65, 0, 1); // amount 1 ⇒ top at 65 + 1/9, air above
        let pos = Vec3d::new(0.5, 63.6, 0.5); // eye ≈ 65.22, above 65.111 surface
        let s = compute_fluid_state(player_box(pos), pos, DEFAULT_EYE_HEIGHT, &w);
        assert!(s.in_water());
        assert!(
            !s.eye_in_water,
            "eye at ~65.22 is above the partial cell's surface at 65.111"
        );
    }

    #[test]
    fn eye_inside_a_partial_cell_below_the_surface_is_in_water() {
        // Same partial top cell, but the eye sits below its surface.
        let mut w = FluidWorld::default();
        w.water_cell(0, 64, 0, 8);
        w.water_cell(0, 65, 0, 8); // 8/9 ⇒ top at 65.888
        let pos = Vec3d::new(0.5, 63.6, 0.5); // eye ≈ 65.22 < 65.888
        let s = compute_fluid_state(player_box(pos), pos, DEFAULT_EYE_HEIGHT, &w);
        assert!(s.eye_in_water, "eye at ~65.22 is under the 65.888 surface");
    }

    #[test]
    fn water_and_lava_are_tracked_separately() {
        let mut w = FluidWorld::default();
        for y in 63..=66 {
            w.lava(0, y, 0);
        }
        let pos = Vec3d::new(0.5, 64.0, 0.5);
        let s = compute_fluid_state(player_box(pos), pos, DEFAULT_EYE_HEIGHT, &w);
        assert!(s.in_lava() && s.eye_in_lava && s.under_lava());
        assert!(!s.in_water() && !s.eye_in_water && !s.under_water());
    }

    #[test]
    fn box_width_drives_which_cells_are_sampled() {
        // Water only in the cell at x = -1. A full 0.6-wide player box centred at
        // x = 0.25 reaches back to min_x = -0.05 ⇒ samples cell -1 ⇒ in water. A
        // hypothetical 0.2-wide box at the same centre (min_x = 0.15) does not
        // reach cell -1 ⇒ not in water. Proves the box parameter is load-bearing:
        // a wrong (e.g. player-sized) box for a differently-shaped mob diverges
        // here rather than coinciding at a flush contact.
        let mut w = FluidWorld::default();
        for y in 63..=66 {
            w.water(-1, y, 0);
        }
        let pos = Vec3d::new(0.25, 64.0, 0.5);
        let wide = compute_fluid_state(player_box(pos), pos, DEFAULT_EYE_HEIGHT, &w);
        assert!(wide.in_water(), "0.6-wide box reaches the x=-1 water cell");

        let narrow = Aabb::new(
            pos.x - 0.1,
            pos.y,
            pos.z - 0.1,
            pos.x + 0.1,
            pos.y + 1.8,
            pos.z + 0.1,
        );
        let thin = compute_fluid_state(narrow, pos, DEFAULT_EYE_HEIGHT, &w);
        assert!(
            !thin.in_water(),
            "0.2-wide box does not reach the x=-1 water cell"
        );
    }

    #[test]
    fn dry_world_reports_nothing() {
        let w = FluidWorld::default();
        let pos = Vec3d::new(0.5, 64.0, 0.5);
        let s = compute_fluid_state(player_box(pos), pos, DEFAULT_EYE_HEIGHT, &w);
        assert_eq!(s, FluidState::NONE);
    }
}
