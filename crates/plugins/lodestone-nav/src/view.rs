//! The world the search reads, and the same world as a physics
//! [`CollisionView`] (`docs/baritone-port.md` §4.2).
//!
//! # The one distinction that matters
//!
//! [`NavView::state_at`] returns `Option<u32>`, and **`None` is not air**. It
//! means "outside the snapshot", which every legality rule treats as *illegal* —
//! and that is precisely the mechanism which terminates a search at the edge of
//! the known world. Conflating the two produces a beautiful path into unloaded
//! chunks (`docs/baritone-port.md` §10 trap 3).
//!
//! On the [`CollisionView`] side the answer is the opposite: outside the snapshot
//! reads as **air**, because physics must never be handed a wall it cannot see the
//! far side of. The two answers are deliberately different and both are
//! load-bearing.
//!
//! # Why one type serves both
//!
//! Cost derivation runs `lodestone_physics::tick` (§4.4) and the executor's
//! reference trajectory runs it again. If the cost model and the executor could
//! see different terrain the search could believe an edge takes 6 ticks while the
//! executor needs 14 — §2.2(1), the coupling this whole design exists to make
//! impossible. One type, both traits, no second adapter to drift.

use std::sync::Arc;

use lodestone_physics::{Aabb, CollisionView, FluidCell, FluidKind, HorizontalDir, Vec3d};
use lodestone_world::{ChunkPos, ChunkSection, World};

use crate::facts::{BlockFacts, FactsTable};

/// A block world the search can read.
pub trait NavView {
    /// Block-state id at world coordinates, or `None` for **outside the
    /// snapshot** — a distinct answer from air. See the module docs.
    fn state_at(&self, x: i32, y: i32, z: i32) -> Option<u32>;

    /// Lowest world `y` the view can answer for.
    fn min_y(&self) -> i32;

    /// One past the highest world `y` the view can answer for.
    fn max_y(&self) -> i32;

    /// The resolved per-state facts this view is paired with.
    fn facts(&self) -> &FactsTable;

    /// Facts at a cell, or `None` outside the snapshot.
    fn facts_at(&self, x: i32, y: i32, z: i32) -> Option<&BlockFacts> {
        self.state_at(x, y, z).map(|s| self.facts().get(s))
    }
}

/// An owned, coherent, lock-free snapshot of a square of chunk columns.
///
/// Owned `Arc<ChunkSection>` handles with copy-on-write at section granularity
/// (`lodestone_world::ChunkColumn::set_block` forks only the touched section via
/// `Arc::make_mut`), so a held `Arc` **cannot** change under a reader. That is
/// what makes this `Send + Sync` and safe to search off the tick thread, and it is
/// the same discipline the section mesher already states absolutely: the world is
/// never locked while working.
///
/// Storage is a **dense grid**, not a `HashMap`. The key space is bounded and
/// known at construction, and the search asks `state_at` for every candidate cell
/// of every expanded node — `lodestone_shell::collision::LiveCollision` made
/// exactly this change for exactly this reason (a hash per queried cell).
#[derive(Debug)]
pub struct SnapshotView {
    /// `[(cz - origin_cz) * width_x + (cx - origin_cx)] * section_count + si`.
    grid: Vec<Option<Arc<ChunkSection>>>,
    /// Whether each column slot was loaded at snapshot time. An absent *section*
    /// is air; an absent *column* is outside the snapshot, and the two must not be
    /// confused (the same trap `Sim::live_collision` documents).
    loaded: Vec<bool>,
    origin_cx: i32,
    origin_cz: i32,
    width_x: i32,
    width_z: i32,
    min_y: i32,
    section_count: usize,
    air_id: u32,
    facts: Arc<FactsTable>,
}

impl SnapshotView {
    /// Snapshot a `(2 * radius + 1)`-square of columns centred on the block
    /// position `(centre_x, centre_z)`.
    ///
    /// **Must be called on the thread that owns the world lock**, exactly once per
    /// search dispatch. The caller holds the guard; this function does not take
    /// one, so it cannot be the place a lock is held across compute.
    ///
    /// Returns `None` when the centre column is not loaded: there is nothing to
    /// plan from, and inventing a start position is how a search produces a plan
    /// beginning somewhere the player is not.
    #[must_use]
    pub fn build(
        world: &World,
        centre_x: i32,
        centre_z: i32,
        radius: i32,
        facts: Arc<FactsTable>,
    ) -> Option<Self> {
        let ccx = centre_x.div_euclid(16);
        let ccz = centre_z.div_euclid(16);
        let centre = world.get(ChunkPos::new(ccx, ccz))?;
        let min_y = centre.column.min_y();
        let section_count = centre.column.section_count();
        let air_id = centre.column.air_id();

        let width = radius * 2 + 1;
        let columns = (width * width) as usize;
        let mut grid = Vec::with_capacity(columns * section_count);
        let mut loaded = Vec::with_capacity(columns);

        for dz in -radius..=radius {
            for dx in -radius..=radius {
                let pos = ChunkPos::new(ccx + dx, ccz + dz);
                let column = world.get(pos).map(|c| &c.column);
                loaded.push(column.is_some());
                for si in 0..section_count {
                    grid.push(column.and_then(|c| c.section_arc(si)));
                }
            }
        }

        Some(Self {
            grid,
            loaded,
            origin_cx: ccx - radius,
            origin_cz: ccz - radius,
            width_x: width,
            width_z: width,
            min_y,
            section_count,
            air_id,
            facts,
        })
    }

    /// Index of the column slot holding block `(x, z)`, or `None` outside.
    fn column_index(&self, x: i32, z: i32) -> Option<usize> {
        let cx = x.div_euclid(16) - self.origin_cx;
        let cz = z.div_euclid(16) - self.origin_cz;
        if cx < 0 || cz < 0 || cx >= self.width_x || cz >= self.width_z {
            return None;
        }
        Some((cz * self.width_x + cx) as usize)
    }

    /// How many columns of the requested square were actually loaded — the honest
    /// measure of how much world a search has to work with.
    #[must_use]
    pub fn loaded_columns(&self) -> usize {
        self.loaded.iter().filter(|l| **l).count()
    }

    /// Total column slots in the snapshot square.
    #[must_use]
    pub fn column_slots(&self) -> usize {
        self.loaded.len()
    }

    /// Facts at a cell for the **physics** path: outside the snapshot is air.
    fn physics_facts(&self, x: i32, y: i32, z: i32) -> &BlockFacts {
        match self.state_at(x, y, z) {
            Some(state) => self.facts.get(state),
            None => &BlockFacts::AIR,
        }
    }

    /// Append every collision box overlapping `region` — the bulk query
    /// `docs/baritone-port.md` §4.2 asks for, since the per-cell path costs ~27
    /// virtual calls per physics substep.
    pub fn colliders_in(&self, region: Aabb, out: &mut Vec<Aabb>) {
        colliders_in(self, region, out, |v, x, y, z| v.physics_facts(x, y, z));
    }
}

/// Shared body of the bulk collider query.
fn colliders_in<V, F>(view: &V, region: Aabb, out: &mut Vec<Aabb>, facts: F)
where
    F: Fn(&V, i32, i32, i32) -> &BlockFacts,
{
    #[allow(clippy::cast_possible_truncation)]
    let (x0, y0, z0) = (
        region.min_x.floor() as i32,
        region.min_y.floor() as i32,
        region.min_z.floor() as i32,
    );
    #[allow(clippy::cast_possible_truncation)]
    let (x1, y1, z1) = (
        region.max_x.floor() as i32,
        region.max_y.floor() as i32,
        region.max_z.floor() as i32,
    );
    for x in x0..=x1 {
        for y in y0..=y1 {
            for z in z0..=z1 {
                emit_boxes(facts(view, x, y, z), x, y, z, out);
            }
        }
    }
}

impl NavView for SnapshotView {
    fn state_at(&self, x: i32, y: i32, z: i32) -> Option<u32> {
        if y < self.min_y || y >= self.max_y() {
            return None;
        }
        let column = self.column_index(x, z)?;
        if !self.loaded[column] {
            return None;
        }
        let si = ((y - self.min_y) / 16) as usize;
        match &self.grid[column * self.section_count + si] {
            // An elided section is genuinely all-air; that is a real answer.
            None => Some(self.air_id),
            Some(section) => Some(section.get_block(
                x.rem_euclid(16) as usize,
                (y - self.min_y).rem_euclid(16) as usize,
                z.rem_euclid(16) as usize,
            )),
        }
    }

    fn min_y(&self) -> i32 {
        self.min_y
    }

    fn max_y(&self) -> i32 {
        self.min_y + (self.section_count * ChunkSection::EDGE) as i32
    }

    fn facts(&self) -> &FactsTable {
        &self.facts
    }
}

/// Append a state's block-local shape to `out` in **world space**, which is the
/// space [`CollisionView::collision_boxes`] is contracted in.
///
/// `f32 -> f64` widening is exact, so this is lossless against the game's `double`
/// shapes.
fn emit_boxes(facts: &BlockFacts, x: i32, y: i32, z: i32, out: &mut Vec<Aabb>) {
    let (bx, by, bz) = (f64::from(x), f64::from(y), f64::from(z));
    for b in facts.shape {
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

/// Every [`CollisionView`] answer, from a [`BlockFacts`] lookup.
///
/// A macro rather than thirteen hand-written bodies per implementor because the
/// failure mode of hand-delegation is precisely the one
/// `lodestone_shell::collision`'s module docs warn about: two adapters, one of
/// them subtly wrong, and a method added later silently falling back to the trait
/// default in one of them.
macro_rules! collision_view_from_facts {
    ($ty:ty, $facts:ident) => {
        impl CollisionView for $ty {
            fn collision_boxes(&self, x: i32, y: i32, z: i32, out: &mut Vec<Aabb>) {
                emit_boxes(self.$facts(x, y, z), x, y, z, out);
            }

            fn collision_top(&self, x: i32, y: i32, z: i32) -> f64 {
                f64::from(self.$facts(x, y, z).top)
            }

            fn friction(&self, x: i32, y: i32, z: i32) -> f32 {
                self.$facts(x, y, z).friction
            }

            fn speed_factor(&self, x: i32, y: i32, z: i32) -> f32 {
                self.$facts(x, y, z).speed_factor
            }

            fn jump_factor(&self, x: i32, y: i32, z: i32) -> f32 {
                self.$facts(x, y, z).jump_factor
            }

            fn is_water(&self, x: i32, y: i32, z: i32) -> bool {
                self.$facts(x, y, z).water
            }

            fn is_lava(&self, x: i32, y: i32, z: i32) -> bool {
                self.$facts(x, y, z).lava
            }

            fn is_climbable(&self, x: i32, y: i32, z: i32) -> bool {
                self.$facts(x, y, z).climbable
            }

            fn stuck_multiplier(&self, x: i32, y: i32, z: i32) -> Option<Vec3d> {
                self.$facts(x, y, z)
                    .stuck_multiplier
                    .map(|[x, y, z]| Vec3d::new(x, y, z))
            }

            /// **An approximation, and the one place this view is knowingly
            /// coarser than the shell's.**
            ///
            /// Vanilla derives a fluid cell's height from its `level` property,
            /// which needs a per-state property lookup the version seam does not
            /// expose (`VersionAdapter` has no `properties`). The shell reaches it
            /// through `lodestone_render::BlockModels::fluid`, which this crate
            /// must not depend on. So a fluid cell here reports a **source block**
            /// (amount 8, not falling).
            ///
            /// The consequence is confined: a flowing-water cell pushes as if it
            /// were full. M1 refuses to enter water at all
            /// ([`crate::facts::MUST_NOT_ENTER`]), and the executor's *reference*
            /// trajectory is computed against the live shell view rather than this
            /// one, so the two disagree only inside water. Fix by routing fluid
            /// level through the seam (`docs/baritone-port.md` §7.5).
            fn fluid_at(&self, x: i32, y: i32, z: i32) -> Option<FluidCell> {
                let facts = self.$facts(x, y, z);
                let kind = if facts.water {
                    FluidKind::Water
                } else if facts.lava {
                    FluidKind::Lava
                } else {
                    return None;
                };
                Some(FluidCell {
                    kind,
                    amount: 8,
                    falling: false,
                })
            }

            fn blocks_motion(&self, x: i32, y: i32, z: i32) -> bool {
                self.$facts(x, y, z).blocks_motion
            }

            /// Under-approximating, exactly as the shell's is: a face counts as
            /// full when a *single* box covers it, where vanilla unions the whole
            /// shape first. No false positives, possible false negatives, and the
            /// only consumer is a falling fluid's downward jet.
            fn is_solid_face(&self, x: i32, y: i32, z: i32, dir: HorizontalDir) -> bool {
                let facts = self.$facts(x, y, z);
                if facts.water || facts.lava {
                    return false;
                }
                let (axis, at_max) = match dir {
                    HorizontalDir::North => (2, false),
                    HorizontalDir::South => (2, true),
                    HorizontalDir::West => (0, false),
                    HorizontalDir::East => (0, true),
                };
                let span = if axis == 0 { [1, 2] } else { [0, 1] };
                facts.shape.iter().any(|b| {
                    let touches = if at_max {
                        b.max[axis] >= 1.0
                    } else {
                        b.min[axis] <= 0.0
                    };
                    touches && span.iter().all(|&a| b.min[a] <= 0.0 && b.max[a] >= 1.0)
                })
            }

            fn bounce_restitution(&self, x: i32, y: i32, z: i32) -> f32 {
                self.$facts(x, y, z).bounce_restitution
            }
        }
    };
}

collision_view_from_facts!(SnapshotView, physics_facts);

/// A [`CollisionView`] + [`NavView`] over an explicit set of block states.
///
/// Deliberately **not** `#[cfg(test)]`: `docs/baritone-port.md` §6 requires a
/// fixture world that structurally contains slabs, soul sand, fences and ice,
/// because both shell collision adapters are coarse in the same way and a rule
/// about partial blocks can be "verified" against every existing scene and mean
/// nothing. That fixture has to be constructible from an integration test and from
/// the plugin's own gates, so the builder is public.
#[derive(Debug)]
pub struct GridView {
    cells: std::collections::HashMap<(i32, i32, i32), u32>,
    bounds: Option<(i32, i32, i32, i32)>,
    min_y: i32,
    max_y: i32,
    air: u32,
    facts: Arc<FactsTable>,
}

impl GridView {
    /// An all-air view spanning `y` in `min_y..max_y` and, when `bounds` is
    /// `Some((x0, z0, x1, z1))`, only that inclusive horizontal rectangle —
    /// everything outside it answers `None`, which is what lets a test exercise
    /// the edge-of-world termination path.
    #[must_use]
    pub fn new(
        facts: Arc<FactsTable>,
        air: u32,
        min_y: i32,
        max_y: i32,
        bounds: Option<(i32, i32, i32, i32)>,
    ) -> Self {
        Self {
            cells: std::collections::HashMap::new(),
            bounds,
            min_y,
            max_y,
            air,
            facts,
        }
    }

    /// Set one cell.
    pub fn set(&mut self, x: i32, y: i32, z: i32, state: u32) -> &mut Self {
        self.cells.insert((x, y, z), state);
        self
    }

    /// Fill an inclusive box.
    pub fn fill(
        &mut self,
        x0: i32,
        y0: i32,
        z0: i32,
        x1: i32,
        y1: i32,
        z1: i32,
        state: u32,
    ) -> &mut Self {
        for x in x0..=x1 {
            for y in y0..=y1 {
                for z in z0..=z1 {
                    self.cells.insert((x, y, z), state);
                }
            }
        }
        self
    }

    /// Append every collision box overlapping `region`.
    pub fn colliders_in(&self, region: Aabb, out: &mut Vec<Aabb>) {
        colliders_in(self, region, out, |v, x, y, z| v.physics_facts(x, y, z));
    }

    fn physics_facts(&self, x: i32, y: i32, z: i32) -> &BlockFacts {
        match self.state_at(x, y, z) {
            Some(state) => self.facts.get(state),
            None => &BlockFacts::AIR,
        }
    }
}

impl NavView for GridView {
    fn state_at(&self, x: i32, y: i32, z: i32) -> Option<u32> {
        if y < self.min_y || y >= self.max_y {
            return None;
        }
        if let Some((x0, z0, x1, z1)) = self.bounds
            && (x < x0 || x > x1 || z < z0 || z > z1)
        {
            return None;
        }
        Some(*self.cells.get(&(x, y, z)).unwrap_or(&self.air))
    }

    fn min_y(&self) -> i32 {
        self.min_y
    }

    fn max_y(&self) -> i32 {
        self.max_y
    }

    fn facts(&self) -> &FactsTable {
        &self.facts
    }
}

collision_view_from_facts!(GridView, physics_facts);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facts::FixtureCensus;

    fn grid() -> GridView {
        let facts = Arc::new(FactsTable::build(&FixtureCensus));
        let mut view = GridView::new(facts, FixtureCensus::AIR, -64, 320, Some((-8, -8, 8, 8)));
        view.fill(-8, 0, -8, 8, 0, 8, FixtureCensus::STONE);
        view
    }

    /// The distinction the whole design rests on: outside the snapshot is `None`
    /// on the nav path and **air** on the physics path.
    #[test]
    fn outside_is_none_for_nav_and_air_for_physics() {
        let view = grid();
        assert_eq!(view.state_at(100, 1, 0), None, "outside the bounds");
        assert_eq!(view.facts_at(100, 1, 0), None);

        let mut boxes = Vec::new();
        view.collision_boxes(100, 1, 0, &mut boxes);
        assert!(
            boxes.is_empty(),
            "physics must see air outside, never an invented wall"
        );
        assert_eq!(view.collision_top(100, 1, 0), 0.0);
    }

    #[test]
    fn a_slab_reports_its_real_top_through_both_traits() {
        let mut view = grid();
        view.set(2, 1, 0, FixtureCensus::SLAB);
        assert_eq!(view.facts_at(2, 1, 0).unwrap().top, 0.5);
        assert_eq!(view.collision_top(2, 1, 0), 0.5);
        let mut boxes = Vec::new();
        view.collision_boxes(2, 1, 0, &mut boxes);
        assert_eq!(boxes.len(), 1);
        assert_eq!(boxes[0].max_y, 1.5, "world-space, not block-local");
    }

    /// Ice is slippery *through the view*, which is the property no scene in the
    /// tree could exercise before per-state data landed.
    #[test]
    fn friction_and_speed_factor_reach_the_physics_trait() {
        let mut view = grid();
        view.set(3, 0, 0, FixtureCensus::BLUE_ICE);
        view.set(4, 0, 0, FixtureCensus::SOUL_SAND);
        assert_eq!(view.friction(3, 0, 0), 0.989);
        assert_eq!(view.speed_factor(4, 0, 0), 0.4);
        assert_eq!(view.friction(0, 0, 0), 0.6, "the control");
    }

    /// The physics view really does collide: a player dropped onto the fixture's
    /// floor settles on its top face. This is the check that the `CollisionView`
    /// impl is wired, not merely present.
    #[test]
    fn the_view_actually_stops_a_falling_player() {
        use lodestone_physics::{MovementInput, PhysicsProfile, PlayerState, tick};
        let view = grid();
        let profile = PhysicsProfile::mc_1_21();
        let mut state = PlayerState::at(Vec3d::new(0.5, 4.0, 0.5), 0.0);
        for _ in 0..60 {
            tick(&mut state, MovementInput::NONE, &view, &profile);
        }
        assert!((state.position.y - 1.0).abs() < 1e-6, "{}", state.position.y);
        assert!(state.on_ground);
    }

    /// The bulk query and the per-cell path must agree, or the "27 virtual calls"
    /// optimisation is a second, differently-wrong adapter.
    #[test]
    fn the_bulk_query_agrees_with_the_per_cell_path() {
        let mut view = grid();
        view.set(1, 1, 0, FixtureCensus::SLAB);
        view.set(2, 1, 0, FixtureCensus::FENCE);
        let region = Aabb::new(-0.5, 0.0, -0.5, 3.5, 3.0, 1.5);

        let mut bulk = Vec::new();
        view.colliders_in(region, &mut bulk);

        let mut per_cell = Vec::new();
        for x in -1..=3 {
            for y in 0..=3 {
                for z in -1..=1 {
                    view.collision_boxes(x, y, z, &mut per_cell);
                }
            }
        }
        assert_eq!(bulk.len(), per_cell.len());
        for b in &bulk {
            assert!(per_cell.contains(b), "{b:?} missing from the per-cell set");
        }
    }
}
