//! Costs derived by **simulation**, not by formula (`docs/baritone-port.md` §4.4).
//!
//! Every movement's cost is obtained by running `lodestone_physics::tick` with
//! [`crate::drive`]'s input script over a synthetic stencil world and counting
//! ticks. Four things this buys, each of which a cost table does not:
//!
//! 1. **The cost is achievable by construction.** It is the number the executor
//!    produced under the same inputs against the same physics, so the search cannot
//!    believe an edge takes 6 ticks while the executor needs 14.
//! 2. **No transcribed constants.** `PhysicsProfile` is the only source of movement
//!    numbers and it is already pinned by two independent oracles.
//! 3. **It self-heals.** When collision data or a physics rule changes, costs move
//!    with it — in directions nobody anticipated — with no cost-table edit.
//! 4. **One definition serves cost and execution.** The script *is* the executor's
//!    reference.
//!
//! # Making it fast enough
//!
//! Simulating per edge inside A\* is far too slow: a movement is 5–20 physics ticks,
//! each with dozens of collider queries. So the results are **memoised by
//! equivalence class** — [`TemplateKey`] — into a table built lazily. In the inner
//! loop an edge cost is therefore one array index for the surface's
//! [`crate::facts::BlockFacts`], one key build, one hash lookup, plus penalties.
//! Comparable to evaluating a formula, without having written one.
//!
//! # What the numbers come out as
//!
//! Derived, not asserted — but recorded here so a regression is visible. On normal
//! ground (friction 0.6, speed factor 1.0) a straight walk settles at **~4.6 ticks
//! per block** and a walk started from rest costs roughly twice that; blue ice
//! (friction 0.989) is a little under 3.6. The tests below pin the *relations* (ice
//! is faster than normal, soul sand is slower, a turn costs more than a straight)
//! rather than the absolute figures, because the absolutes are outputs of an
//! oracle-pinned integrator and the relations are the claims the search relies on.

use std::collections::HashMap;

use lodestone_physics::{
    Aabb, CollisionView, FluidCell, FluidKind, HorizontalDir, MovementInput, PhysicsProfile,
    PlayerState, Vec3d,
};

use crate::drive::WalkDrive;
use crate::graph::{Dir4, MoveKind};
use crate::ticks::Ticks;

/// Friction buckets. `docs/baritone-port.md` §4.4's `SurfaceClass`, with the values
/// coming from `lodestone_model::block_physics` rather than from here — only five
/// blocks in 26.2 differ from the default at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SurfaceClass {
    /// 0.6 — every block but five.
    Normal,
    /// 0.8 — slime.
    Slime,
    /// 0.98 — ice, packed ice, frosted ice.
    Ice,
    /// 0.989 — blue ice, the slipperiest surface in the game.
    BlueIce,
}

impl SurfaceClass {
    /// All buckets, for warming the table and for the cheapest-rate scan.
    pub const ALL: [Self; 4] = [Self::Normal, Self::Slime, Self::Ice, Self::BlueIce];

    /// Bucket a real friction value.
    #[must_use]
    pub fn of(friction: f32) -> Self {
        if friction >= 0.985 {
            Self::BlueIce
        } else if friction >= 0.9 {
            Self::Ice
        } else if friction >= 0.7 {
            Self::Slime
        } else {
            Self::Normal
        }
    }

    /// The representative friction the stencil world reports.
    #[must_use]
    pub const fn friction(self) -> f32 {
        match self {
            Self::Normal => 0.6,
            Self::Slime => 0.8,
            Self::Ice => 0.98,
            Self::BlueIce => 0.989,
        }
    }
}

/// Speed-factor buckets: `1.0`, or soul sand / honey's `0.4`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SpeedClass {
    /// 1.0.
    Normal,
    /// 0.4 — soul sand, honey block.
    Slow,
}

impl SpeedClass {
    /// Both, for warming the table.
    pub const ALL: [Self; 2] = [Self::Normal, Self::Slow];

    /// Bucket a real speed factor.
    #[must_use]
    pub fn of(speed_factor: f32) -> Self {
        if speed_factor < 0.7 {
            Self::Slow
        } else {
            Self::Normal
        }
    }

    /// The representative factor the stencil world reports.
    #[must_use]
    pub const fn factor(self) -> f32 {
        match self {
            Self::Normal => 1.0,
            Self::Slow => 0.4,
        }
    }
}

/// The entry direction *relative to the movement's own direction*.
///
/// Canonicalising the turn rather than keying on the absolute pair of directions is
/// what keeps the template table small: five entry classes instead of seventeen
/// ordered pairs, and every simulation runs in one canonical `+X` frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EntryRel {
    /// Arrived at rest.
    Still,
    /// Already moving the way this movement goes.
    Straight,
    /// Arrived from the movement's left (a quarter turn).
    Left,
    /// Arrived from the movement's right (a quarter turn).
    Right,
    /// Arrived head-on (a half turn).
    Reverse,
}

impl EntryRel {
    /// All five, for warming the table.
    pub const ALL: [Self; 5] = [
        Self::Still,
        Self::Straight,
        Self::Left,
        Self::Right,
        Self::Reverse,
    ];

    /// From the arrival direction and the movement direction.
    #[must_use]
    pub fn of(entry: Option<Dir4>, going: Dir4) -> Self {
        match entry {
            None => Self::Still,
            Some(entry) => match entry.turns_to(going) {
                0 => Self::Straight,
                1 => Self::Right,
                2 => Self::Reverse,
                _ => Self::Left,
            },
        }
    }

    /// Quarter-turns of heading change this entry implies, for `turn_penalty`.
    #[must_use]
    pub const fn quarter_turns(self) -> u32 {
        match self {
            Self::Still | Self::Straight => 0,
            Self::Left | Self::Right => 1,
            Self::Reverse => 2,
        }
    }
}

/// The equivalence class a simulated cost is memoised under.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TemplateKey {
    /// [`MoveKind::id`].
    pub kind: u8,
    /// How the body entered.
    pub entry: EntryRel,
    /// Friction bucket of the surface being walked.
    pub surface: SurfaceClass,
    /// Speed-factor bucket of the surface being walked.
    pub speed: SpeedClass,
    /// Whether the script holds sprint.
    pub sprint: bool,
    /// For [`MoveKind::Drop`] only: how many cells the fall covers. `0` for
    /// every other kind.
    ///
    /// **Not folded into [`MoveKind::id`].** A drop of 2 cells and a drop of 5
    /// are not the same equivalence class — they take genuinely different
    /// numbers of ticks — and collapsing them under one `id` would memoise the
    /// *first* drop simulated and silently cost every other fall height its
    /// ticks, which is exactly the "search believes 6, executor needs 14"
    /// failure `docs/baritone-port.md` §4.4 exists to make impossible.
    pub drop_n: u8,
}

/// A memoised simulation result.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Template {
    /// Simulated duration.
    pub ticks: Ticks,
    /// Whether the simulation actually completed the movement. `false` means the
    /// physics *could not* perform it, which is a legality answer the graph's static
    /// predicates cannot produce — and it is the answer that prevents the
    /// plan-fail-replan loop.
    pub ok: bool,
}

/// Steady-state motion on one surface, both forms.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Steady {
    /// `PlayerState::velocity` at steady state — **post-drag**, the form the
    /// integrator wants when seeding an entry state.
    velocity: Vec3d,
    /// Displacement per tick in blocks — the form a rate means.
    blocks_per_tick: f64,
}

/// Ceiling on a single template simulation. A walk over one block is ~5–20 ticks;
/// anything past this is a script that cannot finish, not a slow one.
const SIM_TICK_CAP: u32 = 80;

/// Ticks spent settling the stencil player before measuring steady-state velocity.
/// Friction convergence is geometric, so 80 is generous.
const SETTLE_TICKS: u32 = 80;

/// The lazily-built table of simulated movement costs.
///
/// Invalidate — by rebuilding — when status effects change class or the profile
/// changes. Nothing else touches it.
#[derive(Debug)]
pub struct TemplateTable {
    profile: PhysicsProfile,
    templates: HashMap<TemplateKey, Template>,
    /// Cached steady-state motion per `(surface, speed, sprint)`. Derived by
    /// simulation, like everything else here.
    steady: HashMap<(SurfaceClass, SpeedClass, bool), Steady>,
    /// How many simulations have been run, so "the table is memoised" is a
    /// measurement rather than a claim.
    simulations: usize,
}

impl TemplateTable {
    /// An empty table for `profile`.
    #[must_use]
    pub fn new(profile: PhysicsProfile) -> Self {
        Self {
            profile,
            templates: HashMap::new(),
            steady: HashMap::new(),
            simulations: 0,
        }
    }

    /// The physics profile every simulation here runs under.
    #[must_use]
    pub fn profile(&self) -> &PhysicsProfile {
        &self.profile
    }

    /// How many physics simulations the table has run in total.
    #[must_use]
    pub fn simulations(&self) -> usize {
        self.simulations
    }

    /// How many distinct keys the table holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.templates.len()
    }

    /// Whether nothing has been simulated yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.templates.is_empty()
    }

    /// The simulated cost of one movement class, computing it on first ask.
    pub fn get(&mut self, key: TemplateKey) -> Template {
        if let Some(hit) = self.templates.get(&key) {
            return *hit;
        }
        let template = self.simulate(key);
        self.templates.insert(key, template);
        template
    }

    /// Steady-state **displacement** per tick, in blocks, for a surface.
    ///
    /// # The distinction that cost a debugging session
    ///
    /// `PlayerState::velocity` at the end of a tick is **post-drag**: vanilla applies
    /// friction *after* the move, so the stored field is already
    /// `travelled × friction × 0.91`. Reading it as "blocks per tick" reports
    /// **0.1179** on normal ground where the real figure is 0.2159 — a factor of
    /// 0.546, which is exactly `0.6 × 0.91`, and which reads as a plausible walking
    /// speed rather than as an error.
    ///
    /// So the rate is measured as position delta, and the *stored velocity* is kept
    /// separately in [`Steady::velocity`] for injecting an entry state, where the
    /// field's own semantics are what the integrator wants.
    pub fn steady_speed(&mut self, surface: SurfaceClass, speed: SpeedClass, sprint: bool) -> f64 {
        self.steady_state(surface, speed, sprint).blocks_per_tick
    }

    /// Steady-state motion for a surface, computing it on first ask.
    fn steady_state(&mut self, surface: SurfaceClass, speed: SpeedClass, sprint: bool) -> Steady {
        if let Some(hit) = self.steady.get(&(surface, speed, sprint)) {
            return *hit;
        }
        // `rise: 0` — steady-state is the approach speed *before* an edge is
        // committed to, always measured on flat ground of the source surface,
        // regardless of what the edge itself eventually does.
        let world = StencilWorld::new(surface, speed, 0);
        let mut state = start_state(&world, Vec3d::new(0.5, 1.0, 0.5), sprint, &self.profile);
        // Aim far along +X so the drive never brakes and never turns.
        let drive = WalkDrive {
            cell: [1_000, 1, 0],
            surface: 1.0,
            brake: false,
            sprint,
            // `steer`: the cost model's `advance` adopts `step.yaw` before ticking, so cost is
            // measured the way the plugin executes -- which is the point of
            // deriving cost by simulation rather than by formula.
            steer: true,
            jump: false,
        };
        for _ in 0..SETTLE_TICKS {
            self.advance(&mut state, &drive, &world, sprint);
        }
        // One more tick, measured.
        let before = state.position;
        self.advance(&mut state, &drive, &world, sprint);
        self.simulations += 1;
        let dx = state.position.x - before.x;
        let dz = state.position.z - before.z;
        let value = Steady {
            velocity: state.velocity,
            blocks_per_tick: (dx * dx + dz * dz).sqrt(),
        };
        self.steady.insert((surface, speed, sprint), value);
        value
    }

    /// The cheapest ticks-per-block any ground movement in the game achieves,
    /// **deflated for strict admissibility**.
    ///
    /// The genuinely cheapest ground motion is sprinting on blue ice; normal sprint
    /// is about 1.4% slower. Deflating the cheapest rate by a further 1.5% restores
    /// strict admissibility everywhere at a heuristic cost too small to matter,
    /// which is a better answer than either using the ice rate blindly or accepting
    /// inadmissibility.
    ///
    /// Note this scans *both* sprint arms even when the policy forbids sprinting:
    /// the heuristic must bound the cheapest movement the graph could contain, and
    /// making it depend on a policy flag is how an "optimisation" quietly turns `h`
    /// inadmissible.
    pub fn cheapest_ticks_per_block(&mut self) -> f64 {
        let mut best = f64::INFINITY;
        for surface in SurfaceClass::ALL {
            for sprint in [false, true] {
                let speed = self.steady_speed(surface, SpeedClass::Normal, sprint);
                if speed > 1e-6 {
                    best = best.min(1.0 / speed);
                }
            }
        }
        if !best.is_finite() {
            // No simulation produced motion at all. Refuse to invent a rate: a zero
            // heuristic is admissible (A\* degrades to Dijkstra) and honest, where a
            // guessed one is neither.
            return 0.0;
        }
        best * 0.985
    }

    /// Run one movement's script and count ticks.
    ///
    /// # `rise`, and why the canonical frame moves vertically too
    ///
    /// `Walk` always has `rise == 0` (flat). `StepUp` rises `+1`; `Descend`
    /// falls `1`; `Drop` falls `key.drop_n` (`docs/baritone-port.md` §4.4's
    /// simulate-don't-formula rule applies just as much to a vertical
    /// movement as a horizontal one — the tick count for a jump or a fall
    /// comes from running the same integrator, never from `sqrt(2h/g)`). The
    /// destination cell and target surface both shift by `rise`, and
    /// [`StencilWorld`]'s own floor does too, so the simulated body is
    /// climbing or falling a **real** step of that height, not a flat walk
    /// with a cosmetic label.
    fn simulate(&mut self, key: TemplateKey) -> Template {
        self.simulations += 1;
        let Some(kind) = decode_kind(key.kind, key.drop_n) else {
            return Template {
                ticks: Ticks::IMPOSSIBLE,
                ok: false,
            };
        };
        let rise: i32 = match kind {
            MoveKind::Walk(_) => 0,
            MoveKind::StepUp(_) => 1,
            MoveKind::Descend(_) => -1,
            MoveKind::Drop(_, n) => -i32::from(n),
        };
        let world = StencilWorld::new(key.surface, key.speed, rise);
        // The **stored** velocity, not the displacement rate: this is going straight
        // into `PlayerState::velocity`, whose semantics are post-drag. See
        // `steady_speed`'s docs for why the two differ by `friction × 0.91`.
        let steady = self.steady_state(key.surface, key.speed, key.sprint).velocity;
        let along = (steady.x * steady.x + steady.z * steady.z).sqrt();

        // Canonical frame: source cell (0, 1, 0), destination (1, 1 + rise, 0),
        // moving +X. Entry position and velocity are the entry class made
        // concrete — the player is placed where crossing into the source cell
        // would have left them, with the velocity that crossing would have
        // carried.
        //
        // **The distances differ per entry class, and that is correct.** A body that
        // walked in along +X entered at the source cell's `-X` face and must cross a
        // whole cell; a body that turned in from `-Z` entered at the `-Z` face, so it
        // is already halfway across in `x`; a body at rest is at the cell centre. Each
        // is where the body genuinely is when the edge begins, which is what makes a
        // chain of edge costs sum to the chain's real duration. Comparing two entry
        // classes' *absolute* costs therefore compares different distances — compare
        // ticks per block instead (see the tests).
        let (position, velocity) = entry_state(key.entry, along);

        let mut state = start_state(&world, position, key.sprint, &self.profile);
        state.velocity = velocity;
        let drive = WalkDrive {
            cell: [1, 1 + rise, 0],
            surface: 1.0 + f64::from(rise),
            brake: false,
            sprint: key.sprint,
            // `steer`: `simulate` drives through `advance`, which does
            // `state.yaw = step.yaw` before `lodestone_physics::tick`.
            steer: true,
            jump: matches!(kind, MoveKind::StepUp(_)),
        };

        let mut ticks = 0u32;
        while ticks < SIM_TICK_CAP {
            let before = state.position;
            self.advance(&mut state, &drive, &world, key.sprint);
            ticks += 1;
            // A fall that drops the body below the destination surface without
            // ever registering `done` (e.g. it clipped past the landing) is a
            // script bug, not a slow edge — `SIM_TICK_CAP` below already turns
            // that into `IMPOSSIBLE`, so no extra check is needed here.
            if drive.done(&state) {
                // Sub-tick refinement: the boundary was crossed *during* this tick,
                // so charging a whole tick over-counts by up to 1.0 — which, chained
                // over a hundred edges, is a hundred ticks of pessimism and a route
                // chosen for the wrong reason.
                let travelled = state.position.x - before.x;
                let fraction = if travelled > 1e-9 {
                    ((1.0 - before.x) / travelled).clamp(0.0, 1.0)
                } else {
                    1.0
                };
                return Template {
                    ticks: Ticks::from_f64(f64::from(ticks - 1) + fraction),
                    ok: true,
                };
            }
        }
        Template {
            ticks: Ticks::IMPOSSIBLE,
            ok: false,
        }
    }

    /// One physics tick under the drive, reproducing the ECS's own attribute
    /// injection exactly.
    ///
    /// `lodestone_ecs::player::player_physics` hands physics
    /// `base·(1 + sprint_modifier)` when sprinting and the bare base otherwise, and
    /// the engine then ignores its own sprint maths so there is no double-count.
    /// **The cost model must do the identical thing**, or the number the search
    /// believes is produced under inputs the executor never uses — the exact failure
    /// §4.4 exists to eliminate. If that system's injection ever changes, this is
    /// the other half that has to change with it.
    fn advance(
        &self,
        state: &mut PlayerState,
        drive: &WalkDrive,
        world: &StencilWorld,
        sprint: bool,
    ) {
        let base = f64::from(self.profile.base_movement_speed);
        let attr = if sprint {
            base * (1.0 + f64::from(self.profile.sprint_speed_modifier))
        } else {
            base
        };
        *state = state.with_movement_speed(attr);
        let step = drive.tick(state);
        state.yaw = step.yaw;
        state.sprinting = sprint;
        lodestone_physics::tick(state, step.input, world, &self.profile);
    }
}

/// Where an entry class puts the body, in [`TemplateTable::simulate`]'s canonical
/// frame: source cell `(0, 1, 0)`, destination `(1, 1, 0)`, moving `+X` at speed
/// `along`.
///
/// A free function, and public to the crate, because **the distance an entry class
/// has left to cross is a consequence of this table** and nothing else. The tests
/// used to restate it, and got `Reverse` backwards: a reversing body entered the
/// source cell travelling `−X`, so it crossed the source cell's `+X` face — the
/// boundary it is about to cross *back* over — and is 0.001 from the destination,
/// not a whole cell. See [`boundary_distance`].
fn entry_state(entry: EntryRel, along: f64) -> (Vec3d, Vec3d) {
    match entry {
        EntryRel::Still => (Vec3d::new(0.5, 1.0, 0.5), Vec3d::ZERO),
        EntryRel::Straight => (Vec3d::new(0.001, 1.0, 0.5), Vec3d::new(along, 0.0, 0.0)),
        EntryRel::Reverse => (Vec3d::new(0.999, 1.0, 0.5), Vec3d::new(-along, 0.0, 0.0)),
        EntryRel::Right => (Vec3d::new(0.5, 1.0, 0.001), Vec3d::new(0.0, 0.0, along)),
        EntryRel::Left => (Vec3d::new(0.5, 1.0, 0.999), Vec3d::new(0.0, 0.0, -along)),
    }
}

/// How far an entry class has to travel in `x` before the movement completes —
/// derived from [`entry_state`] rather than restated, so the two cannot drift.
///
/// This is what makes two entry classes' costs comparable at all: their absolute
/// tick counts cover different distances, deliberately (see
/// [`TemplateTable::simulate`]), so only ticks *per block* is a like-for-like
/// number. `Reverse`'s distance is ~0, which is why its per-block rate is enormous
/// while its absolute cost is the smallest of the five: a reversal is pure
/// turnaround overhead and buys almost no ground.
#[cfg(test)]
fn boundary_distance(entry: EntryRel) -> f64 {
    1.0 - entry_state(entry, 0.0).0.x
}

/// A settled player standing on the stencil floor at `position`.
///
/// Burns the settle tick the physics crate documents: a player from rest reports
/// airborne for exactly one tick because `tick` runs `move()` before applying
/// gravity, so measuring from tick zero measures the settle rather than the move.
fn start_state(
    world: &StencilWorld,
    position: Vec3d,
    sprint: bool,
    profile: &PhysicsProfile,
) -> PlayerState {
    let mut state = PlayerState::at(position, 0.0);
    state.on_ground = true;
    state.sprinting = sprint;
    lodestone_physics::tick(&mut state, MovementInput::NONE, world, profile);
    state.position = position;
    state.velocity = Vec3d::ZERO;
    state.on_ground = true;
    state
}

/// `MoveKind` from its dense id (plus, for `Drop`, the fall distance the key
/// itself carried, since [`MoveKind::id`] cannot see it). Direction is
/// irrelevant to a canonical-frame simulation, so `East` stands for all four.
const fn decode_kind(id: u8, drop_n: u8) -> Option<MoveKind> {
    match id {
        0 => Some(MoveKind::Walk(Dir4::East)),
        1 => Some(MoveKind::StepUp(Dir4::East)),
        2 => Some(MoveKind::Descend(Dir4::East)),
        3 if drop_n > 0 => Some(MoveKind::Drop(Dir4::East, drop_n)),
        _ => None,
    }
}

/// The synthetic world a template simulation runs against: an unbounded floor
/// that steps once, at `x = 1`, from `y = 0` (the source cell's support) to
/// `y = rise` (the destination's) — flat when `rise == 0` (`Walk`), one cell
/// higher for `StepUp`, and one or more cells lower for `Descend`/`Drop`. One
/// friction and one speed factor, on whichever side is `y == rise`; the
/// *source* side (`x < 1`) is deliberately always plain full-height stone at
/// `friction 0.6`/`speed_factor 1.0` regardless of `surface`/`speed`, matching
/// `edge_cost`'s own rule that a movement is costed by the surface **being
/// walked onto**, not the one it started from.
///
/// Deliberately **not** a `SnapshotView` over a fabricated chunk. The template is a
/// property of a *surface class* (and, now, a *rise*), not of a place, and building
/// it from real terrain would make the cost of walking depend on which patch of
/// ground the key happened to be derived from.
#[derive(Debug)]
struct StencilWorld {
    friction: f32,
    speed_factor: f32,
    /// The destination-side floor's block-occupied `y`. `x < 1` is always the
    /// source side, floored at `y = 0`.
    rise: i32,
}

impl StencilWorld {
    const fn new(surface: SurfaceClass, speed: SpeedClass, rise: i32) -> Self {
        Self {
            friction: surface.friction(),
            speed_factor: speed.factor(),
            rise,
        }
    }

    /// The block-occupied `y` of the floor under column `x`.
    const fn floor_y(&self, x: i32) -> i32 {
        if x < 1 { 0 } else { self.rise }
    }
}

impl CollisionView for StencilWorld {
    fn collision_boxes(&self, x: i32, y: i32, z: i32, out: &mut Vec<Aabb>) {
        if y == self.floor_y(x) {
            out.push(Aabb::new(
                f64::from(x),
                f64::from(y),
                f64::from(z),
                f64::from(x) + 1.0,
                f64::from(y) + 1.0,
                f64::from(z) + 1.0,
            ));
        }
    }

    fn collision_top(&self, x: i32, y: i32, _z: i32) -> f64 {
        if y == self.floor_y(x) { 1.0 } else { 0.0 }
    }

    /// The tested surface class applies on **whichever side's floor** `y` is —
    /// source or destination, both, same as `Walk` always did when the two
    /// sides were the same height. This is what keeps `steady_state` (rise
    /// always `0`, so both sides are the same cell) measuring the surface it
    /// was asked to, and what makes `StepUp`/`Descend`/`Drop`'s destination
    /// side carry it too.
    fn friction(&self, x: i32, y: i32, _z: i32) -> f32 {
        if y == self.floor_y(x) { self.friction } else { 0.6 }
    }

    fn speed_factor(&self, x: i32, y: i32, _z: i32) -> f32 {
        if y == self.floor_y(x) { self.speed_factor } else { 1.0 }
    }

    fn blocks_motion(&self, x: i32, y: i32, _z: i32) -> bool {
        y == self.floor_y(x)
    }

    fn fluid_at(&self, _x: i32, _y: i32, _z: i32) -> Option<FluidCell> {
        None
    }

    fn is_solid_face(&self, x: i32, y: i32, _z: i32, _dir: HorizontalDir, _kind: FluidKind) -> bool {
        y == self.floor_y(x)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table() -> TemplateTable {
        TemplateTable::new(PhysicsProfile::mc_1_21())
    }

    fn walk(t: &mut TemplateTable, entry: EntryRel, surface: SurfaceClass) -> Template {
        t.get(TemplateKey {
            kind: 0,
            entry,
            surface,
            speed: SpeedClass::Normal,
            sprint: false,
            drop_n: 0,
        })
    }

    /// A canonical [`MoveKind`] template key at a given entry/surface, for the
    /// new M2 kinds — mirrors [`walk`] above.
    fn key_for(kind: MoveKind, entry: EntryRel, surface: SurfaceClass) -> TemplateKey {
        TemplateKey {
            kind: kind.id(),
            entry,
            surface,
            speed: SpeedClass::Normal,
            sprint: false,
            drop_n: if let MoveKind::Drop(_, n) = kind { n } else { 0 },
        }
    }

    /// The steady-state walk speed falls out of the integrator rather than being
    /// transcribed. `docs/baritone-port.md` §4.3 quotes 0.21586 b/t.
    #[test]
    fn steady_walk_speed_is_derived_and_lands_where_the_design_says() {
        let mut t = table();
        let v = t.steady_speed(SurfaceClass::Normal, SpeedClass::Normal, false);
        assert!(
            (v - 0.21586).abs() < 0.002,
            "derived walk speed {v} b/t, design says ~0.21586"
        );
    }

    #[test]
    fn steady_sprint_is_faster_than_steady_walk() {
        let mut t = table();
        let walk = t.steady_speed(SurfaceClass::Normal, SpeedClass::Normal, false);
        let sprint = t.steady_speed(SurfaceClass::Normal, SpeedClass::Normal, true);
        assert!(sprint > walk * 1.2, "walk {walk}, sprint {sprint}");
    }

    /// A straight walk costs about 4.6 ticks per block. This is the number the
    /// search's whole notion of time rests on.
    #[test]
    fn a_straight_walk_costs_about_four_and_a_half_ticks() {
        let mut t = table();
        let template = walk(&mut t, EntryRel::Straight, SurfaceClass::Normal);
        assert!(template.ok);
        let ticks = template.ticks.as_f64();
        assert!((4.0..5.5).contains(&ticks), "{ticks} ticks per block");
    }

    /// Ticks per block *travelled*, which is the only fair comparison between entry
    /// classes: they start at different points inside the source cell because that is
    /// where the body genuinely is, so their absolute costs cover different distances.
    /// See `TemplateTable::simulate`'s comment.
    ///
    /// The distance comes from [`boundary_distance`], i.e. from the same table
    /// `simulate` places the body with. It used to be restated here as a `match`, and
    /// the restatement had `Reverse` at a whole cell — "entered at the `-X` face" —
    /// when a reversing body is at `x = 0.999`, hard against the boundary it is about
    /// to cross back over. That made `reverse` read as 2.11 t/blk (cheaper than going
    /// straight) and failed the ordering assertion below for a reason that had nothing
    /// to do with the integrator.
    fn rate(t: &mut TemplateTable, entry: EntryRel) -> f64 {
        walk(t, entry, SurfaceClass::Normal).ticks.as_f64() / boundary_distance(entry)
    }

    /// Starting from rest is genuinely dearer **per block** than continuing, which is
    /// the entire justification for the arrival dimension costing 5× the state space.
    /// If this failed, `Arrival` would be paying for nothing.
    #[test]
    fn starting_from_rest_costs_more_per_block_than_continuing() {
        let mut t = table();
        let still = rate(&mut t, EntryRel::Still);
        let straight = rate(&mut t, EntryRel::Straight);
        assert!(
            still > straight * 1.5,
            "from rest {still:.2} t/blk vs at speed {straight:.2} t/blk"
        );
    }

    /// Every kind of turn is dearer per block than going straight, and reversing is
    /// dearer than a quarter turn. Ordering, not magnitudes — the magnitudes are the
    /// integrator's.
    ///
    /// `reverse` comes out at hundreds of ticks per block and that is not a bug: it
    /// spends ~2 ticks killing the inbound velocity and gains ~0.001 blocks of ground
    /// while doing it (see [`boundary_distance`]). Its *absolute* cost is the smallest
    /// of the five, which is why the ordering claim can only be made per block.
    #[test]
    fn turn_rates_are_ordered() {
        let mut t = table();
        let straight = rate(&mut t, EntryRel::Straight);
        let right = rate(&mut t, EntryRel::Right);
        let left = rate(&mut t, EntryRel::Left);
        let reverse = rate(&mut t, EntryRel::Reverse);
        assert!(straight < right, "straight {straight:.2} vs right {right:.2}");
        assert!(
            (right - left).abs() < 0.05,
            "the two quarter turns must be mirror images: {right:.2} vs {left:.2}"
        );
        assert!(right < reverse, "right {right:.2} vs reverse {reverse:.2}");
    }

    /// Ice really is faster and soul sand really is slower — and this is the test
    /// that would have been *vacuous* against any scene in the tree before real
    /// per-state collision landed, because both shell adapters reported 0.6 friction
    /// everywhere. It passes here because the stencil world is synthetic and states
    /// the friction explicitly.
    #[test]
    fn surface_class_changes_the_cost_in_the_right_direction() {
        let mut t = table();
        let normal = walk(&mut t, EntryRel::Straight, SurfaceClass::Normal).ticks;
        let blue_ice = walk(&mut t, EntryRel::Straight, SurfaceClass::BlueIce).ticks;
        assert!(blue_ice < normal, "blue ice {blue_ice} vs normal {normal}");

        let slow = t.get(TemplateKey {
            kind: 0,
            entry: EntryRel::Straight,
            surface: SurfaceClass::Normal,
            speed: SpeedClass::Slow,
            sprint: false,
            drop_n: 0,
        });
        assert!(slow.ok);
        assert!(
            slow.ticks > normal,
            "soul sand {} vs normal {normal}",
            slow.ticks
        );
    }

    /// The memoisation actually memoises. Without this the "sub-millisecond,
    /// amortised" claim is unmeasured.
    #[test]
    fn a_repeated_key_runs_no_further_simulation() {
        let mut t = table();
        walk(&mut t, EntryRel::Straight, SurfaceClass::Normal);
        let after_first = t.simulations();
        for _ in 0..1000 {
            walk(&mut t, EntryRel::Straight, SurfaceClass::Normal);
        }
        assert_eq!(t.simulations(), after_first);
        assert_eq!(t.len(), 1);
    }

    /// The whole reachable M1 key space is small, which is the load-bearing
    /// assumption behind "lazy table, no latency spike".
    #[test]
    fn the_m1_key_space_is_a_few_dozen_keys() {
        let mut t = table();
        for entry in EntryRel::ALL {
            for surface in SurfaceClass::ALL {
                for speed in SpeedClass::ALL {
                    for sprint in [false, true] {
                        t.get(TemplateKey {
                            kind: 0,
                            entry,
                            surface,
                            speed,
                            sprint,
                            drop_n: 0,
                        });
                    }
                }
            }
        }
        assert_eq!(t.len(), 5 * 4 * 2 * 2);
        assert!(t.len() <= 128, "{} keys is not 'a few hundred'", t.len());
    }

    /// The heuristic rate must not exceed the fastest movement the table can
    /// produce, or `h` is inadmissible and the search silently returns bad paths.
    #[test]
    fn the_heuristic_rate_is_strictly_below_every_simulated_rate() {
        let mut t = table();
        let h_rate = t.cheapest_ticks_per_block();
        assert!(h_rate > 0.0);
        for surface in SurfaceClass::ALL {
            for sprint in [false, true] {
                let real = 1.0 / t.steady_speed(surface, SpeedClass::Normal, sprint);
                assert!(
                    h_rate < real,
                    "heuristic {h_rate} t/blk is not below {surface:?} sprint={sprint} at {real}"
                );
            }
        }
    }

    #[test]
    fn entry_relation_canonicalisation_is_a_quarter_turn_map() {
        assert_eq!(EntryRel::of(None, Dir4::East), EntryRel::Still);
        assert_eq!(
            EntryRel::of(Some(Dir4::East), Dir4::East),
            EntryRel::Straight
        );
        assert_eq!(EntryRel::of(Some(Dir4::West), Dir4::East), EntryRel::Reverse);
        assert_eq!(EntryRel::of(Some(Dir4::North), Dir4::East), EntryRel::Right);
        assert_eq!(EntryRel::of(Some(Dir4::South), Dir4::East), EntryRel::Left);
    }

    #[test]
    fn surface_bucketing_matches_the_five_real_frictions() {
        assert_eq!(SurfaceClass::of(0.6), SurfaceClass::Normal);
        assert_eq!(SurfaceClass::of(0.8), SurfaceClass::Slime);
        assert_eq!(SurfaceClass::of(0.98), SurfaceClass::Ice);
        assert_eq!(SurfaceClass::of(0.989), SurfaceClass::BlueIce);
    }

    // --- M2: StepUp/Descend/Drop cost simulation ---

    /// A `StepUp` genuinely simulates a jump: it must complete (the physics can
    /// perform it — `jump_apex_height` says as much) and must cost noticeably
    /// more than a flat walk of the same one-block distance, because it spends
    /// real ticks airborne rather than only crossing horizontally.
    #[test]
    fn step_up_is_simulated_slower_than_a_flat_walk_and_still_completes() {
        let mut t = table();
        let flat = walk(&mut t, EntryRel::Straight, SurfaceClass::Normal);
        let up = t.get(key_for(
            MoveKind::StepUp(Dir4::East),
            EntryRel::Straight,
            SurfaceClass::Normal,
        ));
        assert!(up.ok, "the jump apex clears a one-block ascend; this must simulate ok");
        assert!(
            up.ticks > flat.ticks,
            "step up {} should cost more than a flat walk {}", up.ticks, flat.ticks
        );
        // A generous ceiling: jump apex is ~12 airborne ticks plus crossing, not
        // dozens more — this is the check that would catch a script that never
        // releases jump and time out against `SIM_TICK_CAP` for the wrong reason.
        assert!(up.ticks.as_f64() < 30.0, "{} ticks seems too slow for one step up", up.ticks);
    }

    /// A `Descend` is simulated too, and completes — but it costs *more* than a
    /// flat walk of the same one-block span, not less, which is the opposite of
    /// the first intuition ("falling is fast, so this should be cheap").
    ///
    /// The reason is real and worth recording, because it is exactly the sort
    /// of thing a hand-written formula would get backwards: falling covers the
    /// *vertical* distance quickly, but horizontal control while airborne is
    /// governed by `air_control` (`~0.02`), far weaker than the grounded
    /// acceleration a `Walk` uses the whole time. So the limiting factor for a
    /// `Descend` is not "how fast can I fall", it is "how fast can I cross the
    /// one block horizontally with almost no air steering" — and that is
    /// slower than walking the same block on the ground. Simulating this
    /// (rather than assuming "falling is free" from §4.3's heuristic, which is
    /// a *different*, deliberately generous claim about the search's admissible
    /// bound, not about a specific edge's real cost) is exactly what
    /// `docs/baritone-port.md` §4.4 promises: the number self-heals in a
    /// direction nobody would have written into a formula by hand.
    #[test]
    fn descend_costs_more_than_a_flat_walk_because_air_control_is_weak() {
        let mut t = table();
        let flat = walk(&mut t, EntryRel::Straight, SurfaceClass::Normal);
        let down = t.get(key_for(
            MoveKind::Descend(Dir4::East),
            EntryRel::Straight,
            SurfaceClass::Normal,
        ));
        assert!(down.ok, "stepping off a one-block ledge is always physically possible");
        assert!(
            down.ticks > flat.ticks,
            "descend {} should cost more than a flat walk {} (weak air control)",
            down.ticks,
            flat.ticks
        );
        // A generous ceiling, so a script that overshoots and has to recover
        // would still be caught rather than laundered into "plausible but slow".
        assert!(down.ticks.as_f64() < 30.0, "{} ticks seems too slow for one descend", down.ticks);
    }

    /// `Drop`'s cost genuinely depends on `n` — a longer fall takes more ticks,
    /// which is the entire reason [`TemplateKey::drop_n`] exists rather than
    /// folding every fall height into one `Drop` id. Without a distinct `drop_n`
    /// in the key, a 2-cell and a 5-cell drop would memoise to the same template
    /// and this would be flat, not increasing.
    #[test]
    fn a_longer_drop_costs_more_ticks_than_a_shorter_one() {
        let mut t = table();
        let short = t.get(key_for(
            MoveKind::Drop(Dir4::East, 2),
            EntryRel::Straight,
            SurfaceClass::Normal,
        ));
        let long = t.get(key_for(
            MoveKind::Drop(Dir4::East, 6),
            EntryRel::Straight,
            SurfaceClass::Normal,
        ));
        assert!(short.ok && long.ok, "short {short:?} long {long:?}");
        assert!(
            long.ticks > short.ticks,
            "a 6-cell drop {} must cost more than a 2-cell drop {}", long.ticks, short.ticks
        );
    }

    /// The negative control for `drop_n`: two `Drop` keys differing *only* in
    /// `drop_n` must occupy distinct memoisation slots, not collide into one.
    /// Without this, the previous test could pass by coincidence if `get`
    /// happened to memoise both under the same key for an unrelated reason.
    #[test]
    fn drop_keys_with_different_n_are_distinct_table_entries() {
        let mut t = table();
        t.get(key_for(MoveKind::Drop(Dir4::East, 2), EntryRel::Straight, SurfaceClass::Normal));
        t.get(key_for(MoveKind::Drop(Dir4::East, 3), EntryRel::Straight, SurfaceClass::Normal));
        t.get(key_for(MoveKind::Drop(Dir4::East, 3), EntryRel::Straight, SurfaceClass::Normal));
        assert_eq!(t.len(), 2, "n=2 and n=3 are different templates; the repeat of n=3 must not add a third");
    }
}
