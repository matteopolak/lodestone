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

use crate::drive::{ClimbDrive, WalkDrive};
use crate::graph::{ClimbDir, Dir4, MoveKind};
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

    /// Entry relation for a [`MoveKind::WalkDiagonal(d1, d2)`], from the
    /// arrival that reached the `from` node.
    ///
    /// # Why this collapses to three classes, not five
    ///
    /// `Arrival` is always cardinal — `Still` or `Walking(Dir4)` — because
    /// `WalkDiagonal`'s own exit arrival already collapses onto one cardinal
    /// component (`MoveKind::WalkDiagonal`'s doc comment), so nothing ever
    /// arrives at a node already moving diagonally. A cardinal direction is
    /// always exactly `0°`, `90°`, `180°` or `270°` from *some* reference, and
    /// a diagonal's own heading sits at an odd multiple of `45°` from every
    /// cardinal — so a cardinal entry is **never** `0°` (`Straight`) or `180°`
    /// (`Reverse`) from a diagonal `going`, and never exactly `90°`
    /// (`Left`/`Right`) either. It is always `45°` (entry is `d1` or `d2`
    /// themselves — "moving into the corner already") or `135°` (entry is
    /// `d1.opposite()` or `d2.opposite()` — "moving away from it"), and by the
    /// diagonal's own mirror symmetry (reflecting across its own axis swaps
    /// `d1` and `d2` and leaves the physics unchanged — same friction, same
    /// speed factor, same isotropic drag), those two members of each pair are
    /// cost-equivalent. Three classes — `Still`, one `45°` class, one `135°`
    /// class — are therefore both necessary and sufficient, never four or
    /// five.
    ///
    /// This reuses [`Self::Straight`]/[`Self::Reverse`] rather than minting
    /// two new variants, because [`TemplateTable::simulate`] already has a
    /// correct, tested position/velocity formula for exactly "already moving
    /// favourably, just crossed the entry face" (`Straight`) and "already
    /// moving unfavourably, must turn around" (`Reverse`) — the diagonal case
    /// needs the *same* physical state, just approached along a different
    /// pair of axes, and `Self::WalkDiagonal`'s canonical simulation frame is
    /// always the same one direction pair regardless of which real diagonal
    /// is being costed (`crate::graph::MoveKind::id`'s own doc comment makes
    /// the identical claim for the cardinal case).
    ///
    /// **A real, bounded approximation this collapse costs:** `turn_penalty`
    /// (a preference on top of the simulated cost, not a measurement — see
    /// [`crate::policy::NavPolicy::turn_penalty`]'s own doc comment) charges
    /// `Straight`'s `0` quarter-turns for a genuine `45°` realignment and
    /// `Reverse`'s `2` for a genuine `135°` one, under- and over-charging
    /// respectively by one quarter-turn's worth of preference. The simulated
    /// tick count itself is unaffected — only the additive preference is
    /// approximate, and only in the direction that still prefers walking a
    /// diagonal that continues a straight approach over one that reverses
    /// one, which is the right ordering even if the exact charge is off.
    #[must_use]
    pub fn of_diagonal(entry: Option<Dir4>, d1: Dir4, d2: Dir4) -> Self {
        match entry {
            None => Self::Still,
            Some(dir) if dir == d1 || dir == d2 => Self::Straight,
            Some(_) => Self::Reverse,
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
    ///
    /// # `WalkDiagonal` needed a second scan, not just a smaller deflation
    ///
    /// This used to scan only steady-state *cardinal* speed, on the reasoning that
    /// nothing moves faster per block than continuing in a straight line. That
    /// reasoning quietly broke the moment `WalkDiagonal` existed: its `Reverse`
    /// entry class (`EntryRel::of_diagonal`'s doc comment explains why only three
    /// classes exist at all) measured **~3.09 ticks per octile block** against a
    /// cardinal-derived `h_rate` of **~3.46** — the heuristic *overestimating* the
    /// true cost of a diagonal approached that way, a real inadmissibility this
    /// crate's own `debug_assert`-backed contract exists to forbid. The cause is
    /// not that a diagonal is genuinely faster than steady state; it is that
    /// `Reverse`'s aligned axis inherits almost no residual distance from a prior
    /// cardinal edge's own **boundary**-crossing completion (`WalkDrive::done` is a
    /// cell-boundary test, not a "reached centre" test) — see the module docs and
    /// `docs/autonomous-navigation.md` for the full account. Steady-state motion
    /// cannot see this, because it never measures a bounded edge at all. So this
    /// now also simulates every diagonal template `EntryRel::of_diagonal` can
    /// actually produce (`Still`/`Straight`/`Reverse` — never `Left`/`Right`,
    /// which it never emits) and folds their own per-octile-block rate into the
    /// same minimum.
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
        for surface in SurfaceClass::ALL {
            for sprint in [false, true] {
                for entry in [EntryRel::Still, EntryRel::Straight, EntryRel::Reverse] {
                    let template = self.get(TemplateKey {
                        kind: MoveKind::WalkDiagonal(Dir4::North, Dir4::East).id(),
                        entry,
                        surface,
                        speed: SpeedClass::Normal,
                        sprint,
                        drop_n: 0,
                    });
                    if template.ok {
                        let rate = template.ticks.as_f64() / std::f64::consts::SQRT_2;
                        if rate > 1e-6 {
                            best = best.min(rate);
                        }
                    }
                }
            }
        }
        // `Climb` folded in too, for the same reason `WalkDiagonal` had to be:
        // do not *assume* a slower-looking movement cannot be the new
        // minimum, verify it. It never is here — climbing (~5-6.67
        // ticks/block) is far slower than the cheapest horizontal rate
        // (~3.4-3.6) — but the assumption that "the horizontal rate is
        // always cheaper" is exactly the kind of unverified belief this
        // crate's own evidence standards warn about, and it costs one small
        // loop to check rather than assert. Unlike a diagonal, one real block
        // of vertical progress per edge, no `sqrt(2)` division.
        for dir in [ClimbDir::Up, ClimbDir::Down] {
            let template = self.get(TemplateKey {
                kind: MoveKind::Climb(dir).id(),
                entry: EntryRel::Still,
                surface: SurfaceClass::Normal,
                speed: SpeedClass::Normal,
                sprint: false,
                drop_n: 0,
            });
            if template.ok {
                let rate = template.ticks.as_f64();
                if rate > 1e-6 {
                    best = best.min(rate);
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
        // `Climb` does not fit this function's canonical `+x` frame at all —
        // no horizontal displacement, no floor, a body that clings instead of
        // stands. `simulate_climb` is the genuinely new vertical frame
        // `docs/autonomous-navigation.md`'s "`Climb`: stopped, and why" named
        // as one of the two hard parts; see its own doc comment for why it
        // needed a separate `CollisionView` rather than a `rise`-parameterised
        // `StencilWorld`.
        if let MoveKind::Climb(dir) = kind {
            return self.simulate_climb(dir);
        }
        let rise: i32 = match kind {
            MoveKind::Walk(_) | MoveKind::WalkDiagonal(_, _) => 0,
            MoveKind::StepUp(_) => 1,
            MoveKind::Descend(_) => -1,
            MoveKind::Drop(_, n) => -i32::from(n),
            MoveKind::Climb(_) => unreachable!("handled above"),
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
        // `WalkDiagonal` moves along **both** `+X` and `-Z` at once, in the
        // canonical `(North, East)` frame `decode_kind` always produces for
        // it — direction is irrelevant to a canonical-frame simulation, same
        // as every cardinal kind already relies on (`MoveKind::id`'s own doc
        // comment). Every other kind still moves along `+X` only.
        let is_diagonal = matches!(kind, MoveKind::WalkDiagonal(_, _));
        let dest_z: i32 = if is_diagonal { -1 } else { 0 };
        let drive = WalkDrive {
            cell: [1, 1 + rise, dest_z],
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
                //
                // Every cardinal kind moves along `x` only, so the original,
                // unconditional single-axis formula stays exactly as it was —
                // touching it at all would risk every already-tested cardinal
                // template. `WalkDiagonal` is the one kind whose `done()` can
                // become true because of a `z` crossing that happens on a
                // *different* tick than the `x` crossing (`WalkDrive::inside_cell`
                // requires both), so it alone gets the two-axis version — see
                // `completion_fraction`'s own doc comment for why a single axis
                // is unsafe there.
                let fraction = if is_diagonal {
                    completion_fraction(before, state.position, drive.cell)
                } else {
                    let travelled = state.position.x - before.x;
                    if travelled > 1e-9 {
                        ((1.0 - before.x) / travelled).clamp(0.0, 1.0)
                    } else {
                        1.0
                    }
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
        self.advance_with(state, world, sprint, |s| drive.tick(s));
    }

    /// The generalised form: any script that can produce a [`crate::drive::DriveTick`]
    /// from a [`PlayerState`], not only [`WalkDrive`], against any
    /// [`CollisionView`], not only [`StencilWorld`] — what [`Self::simulate_climb`]
    /// needs, since `Climb`'s script and world are both genuinely different
    /// (`crate::drive::ClimbDrive`, [`ClimbStencilWorld`]) rather than a
    /// parameter to the horizontal ones.
    fn advance_with(
        &self,
        state: &mut PlayerState,
        world: &dyn CollisionView,
        sprint: bool,
        tick: impl Fn(&PlayerState) -> crate::drive::DriveTick,
    ) {
        let base = f64::from(self.profile.base_movement_speed);
        let attr = if sprint {
            base * (1.0 + f64::from(self.profile.sprint_speed_modifier))
        } else {
            base
        };
        *state = state.with_movement_speed(attr);
        let step = tick(state);
        state.yaw = step.yaw;
        state.sprinting = sprint;
        lodestone_physics::tick(state, step.input, world, &self.profile);
    }

    /// `Climb`'s simulation: the genuinely new vertical frame.
    ///
    /// # Why this cannot be a `rise`-parameterised call into [`Self::simulate`]
    ///
    /// Every horizontal kind's frame is "a floor that steps once at `x = 1`",
    /// which [`StencilWorld`] already models for any `rise`. Climbing has no
    /// floor at all — the body clings to a climbable cell, never stands — and
    /// no horizontal displacement, so neither [`StencilWorld`] nor
    /// [`WalkDrive`] (which aims at a horizontal destination cell) applies.
    /// [`ClimbStencilWorld`] is climbable everywhere in one column and solid
    /// nowhere; [`ClimbDrive`] presses jump (ascend) or nothing (descend),
    /// never forward/strafe.
    ///
    /// # Why the simulation never sets `on_ground = true`
    ///
    /// Real mounting *does* sometimes begin on solid ground (walking into a
    /// ladder's own footprint at floor level), and in that case vanilla's
    /// ordinary ground-jump impulse (`0.42`) fires on the very first tick
    /// alongside the climb override, before `climbing` overwrites the stored
    /// velocity for next tick. Modelling that exactly would need a *second*
    /// climb template (mount-from-ground vs. continue-while-already-clinging)
    /// for what is a bounded, one-tick effect — real ticks-per-block instead
    /// scanned as `1.0 - Reverse` gives it exists to avoid over-fitting.
    /// This simulation instead seeds every climb template as already-clinging
    /// (`on_ground = false` throughout), which is the more common case (every
    /// edge but the first in a chain) and, for the first edge, a **safe**
    /// direction to be wrong in: the real executor may finish that one edge a
    /// hair faster than the simulated template predicts (extra free height
    /// from the ground hop), never slower — an overestimate of cost, not an
    /// underestimate, so admissibility is unaffected and "achievability by
    /// construction" has exactly this one narrow, bounded, documented
    /// exception rather than a silent one.
    ///
    /// # Why there is one template per direction, not per entry class
    ///
    /// [`EntryRel`] does not appear here at all — every call site that builds
    /// a `Climb` [`TemplateKey`] fixes `entry: EntryRel::Still`
    /// (`search::Search::expand`'s own comment on why). The script presses no
    /// forward/strafe, so there is no entry momentum for a different entry
    /// class to model in the first place; `handle_on_climbable`'s clamp
    /// applies identically from the first simulated tick regardless of how
    /// the body arrived. This is the vertical frame's answer to
    /// `docs/autonomous-navigation.md`'s question of whether it admits
    /// `WalkDiagonal`'s three-way `EntryRel` collapse: it does not need even
    /// that — it collapses to a single class, not three.
    fn simulate_climb(&mut self, dir: ClimbDir) -> Template {
        let world = ClimbStencilWorld;
        let rise: i32 = match dir {
            ClimbDir::Up => 1,
            ClimbDir::Down => -1,
        };
        let source_y = 1;
        let target_y = source_y + rise;
        let drive = ClimbDrive {
            column: [0, 0],
            target_y,
            target_surface: f64::from(target_y),
            ascending: matches!(dir, ClimbDir::Up),
            continuing: true,
        };
        // Entry position: "just crossed into the source cell in the
        // direction of travel" — the same convention `entry_state`'s
        // `Straight` uses for a cardinal walk (`0.001` past the near face),
        // applied to the `y` axis instead of `x`.
        //
        // **This is the fix for a real bug the first version of this
        // function had, found by exactly this test file's own admissibility
        // check going strongly negative.** Seeding at the source cell's
        // *exact integer floor* (`1.0`) is correct for `Up` — a full cell of
        // climbing genuinely separates `1.0` from the `2.0` boundary — but it
        // is a near-zero-distance start for `Down`: `floor(1.0) == 1`, so any
        // downward drift at all immediately satisfies `floor(y) == 0`, and
        // `Climb(Down)` measured **one tick** for a whole block, corrupting
        // `cheapest_ticks_per_block`'s minimum along the way (measured
        // `h_rate` collapsed to the bare `0.985` deflation constant — i.e.
        // "one tick per block" became the fastest movement in the entire
        // table). Seeding `Down` near the source cell's own **ceiling**
        // instead gives it the same full cell of travel `Up` already had.
        //
        // **What this does not model, and why that is the safe direction to
        // be wrong in:** a body that *freshly mounted* the top of a ladder
        // (arriving via an ordinary `Walk`, landing exactly at its cell's own
        // floor height) and immediately descends genuinely starts nearer
        // `1.0` than `2.0` — a real, smaller distance than this template
        // simulates. Using the larger, "continuing a chain" distance for that
        // edge too **overestimates** its true cost, exactly the same bounded,
        // safe-direction exception this function's own doc comment already
        // records for `Up`'s ground-jump hop — never an underestimate, so
        // admissibility is unaffected.
        const BOUNDARY_EPS: f64 = 0.001;
        let start_y = match dir {
            ClimbDir::Up => f64::from(source_y) + BOUNDARY_EPS,
            ClimbDir::Down => f64::from(source_y + 1) - BOUNDARY_EPS,
        };
        let mut state = PlayerState::at(Vec3d::new(0.5, start_y, 0.5), 0.0);
        state.on_ground = false;

        let mut ticks = 0u32;
        while ticks < SIM_TICK_CAP {
            let before = state.position;
            self.advance_with(&mut state, &world, false, |s| drive.tick(s));
            ticks += 1;
            if drive.done(&state) {
                let fraction = axis_boundary_fraction(before.y, state.position.y, target_y);
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
}

/// The synthetic world [`TemplateTable::simulate_climb`] runs against: a
/// climbable column at `(x, z) = (0, 0)`, every `y`, with **no collision
/// anywhere** — no floor, because a climbing body never stands on one, and
/// climbable blocks themselves are `blocks_motion == false`
/// (`crate::graph::stand_surface`'s own doc comment on `forceSolidOff`).
///
/// Deliberately not [`StencilWorld`]: that type's entire shape is a floor
/// stepping once in `x`, which has no vertical analogue to parameterise —
/// `Climb` needed a genuinely different world, not a new `rise` value for the
/// existing one, exactly as `docs/autonomous-navigation.md` predicted.
#[derive(Debug)]
struct ClimbStencilWorld;

impl CollisionView for ClimbStencilWorld {
    fn collision_boxes(&self, _x: i32, _y: i32, _z: i32, _out: &mut Vec<Aabb>) {}

    fn is_climbable(&self, x: i32, _y: i32, z: i32) -> bool {
        x == 0 && z == 0
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
/// irrelevant to a canonical-frame simulation, so `East` stands for all four
/// cardinal kinds and `(North, East)` stands for all four diagonal pairs.
const fn decode_kind(id: u8, drop_n: u8) -> Option<MoveKind> {
    match id {
        0 => Some(MoveKind::Walk(Dir4::East)),
        1 => Some(MoveKind::StepUp(Dir4::East)),
        2 => Some(MoveKind::Descend(Dir4::East)),
        3 if drop_n > 0 => Some(MoveKind::Drop(Dir4::East, drop_n)),
        4 => Some(MoveKind::WalkDiagonal(Dir4::North, Dir4::East)),
        5 => Some(MoveKind::Climb(ClimbDir::Up)),
        6 => Some(MoveKind::Climb(ClimbDir::Down)),
        _ => None,
    }
}

/// Fraction of the tick's own motion, on whichever axis crossed into `cell`
/// **during this tick**, needed to reach that axis's own boundary.
///
/// # Why a diagonal needs both axes, and why "just reuse the `x` formula"
/// silently drifts wrong
///
/// `Walk`/`StepUp`/`Descend`/`Drop` only ever move along `x`, so charging a
/// whole tick when the boundary was crossed partway through it only ever
/// needed to look at `x`. `WalkDiagonal` moves along **both** axes, and
/// [`WalkDrive::done`]/[`WalkDrive::inside_cell`] require **both** to already
/// be inside the destination cell — so the two axes can, and typically do,
/// cross their own boundaries on *different* ticks. On the tick where `done`
/// first becomes true, whichever axis crossed on an *earlier* tick is no
/// longer moving toward a boundary at all (it may still be drifting slightly
/// inside the cell it already reached), and measuring "how far into this
/// tick did it cross" against a boundary it crossed ticks ago produces a
/// number with no physical meaning. So only an axis that is **newly** inside
/// its target cell this tick contributes a fraction; if both are newly inside
/// on the same tick, completion is gated by whichever crosses **later**
/// within it, so the answer is the **maximum** over axes that newly crossed,
/// not either one alone or their sum.
///
/// For every cardinal kind this reduces to exactly the original single-axis
/// formula: `z`'s target always equals the source `z` (no cardinal kind ever
/// moves in `z`), so `z` is never "newly inside" its target and never
/// contributes — only `x` ever does, unconditionally, which is the same
/// answer the original code computed. `WalkDiagonal` uses this generalised
/// version alone (see the call site in [`TemplateTable::simulate`]);
/// no cardinal kind's behaviour changes.
fn completion_fraction(before: Vec3d, after: Vec3d, cell: [i32; 3]) -> f64 {
    let mut fraction: Option<f64> = None;
    for (b, a, target) in [(before.x, after.x, cell[0]), (before.z, after.z, cell[2])] {
        #[allow(clippy::cast_possible_truncation)]
        let before_cell = b.floor() as i32;
        #[allow(clippy::cast_possible_truncation)]
        let after_cell = a.floor() as i32;
        if before_cell != target && after_cell == target {
            let f = axis_boundary_fraction(b, a, target);
            fraction = Some(fraction.map_or(f, |existing: f64| existing.max(f)));
        }
    }
    fraction.unwrap_or(1.0)
}

/// How far through `before -> after`'s own displacement the near boundary of
/// `target_cell` sits, on one axis: `0.0` means the boundary was crossed at
/// the very start of the tick's motion, `1.0` means at the very end (or that
/// there was no meaningful motion at all, the same conservative default the
/// original single-axis formula used).
fn axis_boundary_fraction(before: f64, after: f64, target_cell: i32) -> f64 {
    let travelled = after - before;
    if travelled.abs() <= 1e-9 {
        return 1.0;
    }
    let boundary = if travelled > 0.0 {
        f64::from(target_cell)
    } else {
        f64::from(target_cell) + 1.0
    };
    ((boundary - before) / travelled).clamp(0.0, 1.0)
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

    // --- M2: WalkDiagonal cost simulation ---

    fn diagonal_key(entry: EntryRel) -> TemplateKey {
        key_for(
            MoveKind::WalkDiagonal(Dir4::North, Dir4::East),
            entry,
            SurfaceClass::Normal,
        )
    }

    /// A diagonal genuinely simulates and completes, and — the actual
    /// functional claim `docs/baritone-port.md` §4.1 makes ("a diagonal wins
    /// ties against two axis moves") — costs **less than two cardinal
    /// steps**, so the search prefers one diagonal edge over a two-edge
    /// cardinal detour of the same net displacement.
    ///
    /// This does *not* assert the doc's other figure, "a hair below `sqrt(2)`
    /// times a straight step", and that is a real, recorded finding rather
    /// than an oversight: that figure describes a full centre-to-centre
    /// Euclidean crossing, while [`WalkDrive::done`] is a **cell-boundary**
    /// test on both axes. `EntryRel::of_diagonal`'s entry classes reuse the
    /// cardinal `Straight`/`Reverse` position formulas verbatim (see its own
    /// doc comment), which place the *aligned* axis near its far face
    /// (`~0.999` blocks still to cross) but leave the *other* axis centred
    /// (`~0.5` blocks to cross) — a real asymmetry inherited from a
    /// prior cardinal edge's own `done()` having fired at a boundary, not at
    /// its destination's centre. Measured here: `Straight` costs **~1.17×** a
    /// cardinal step (more distance, so more than `1×`, but nowhere near
    /// `sqrt(2)`), and `Reverse` costs **~0.89×** — genuinely *less* than one
    /// cardinal step, because its aligned axis has almost no residual
    /// distance left (having already nearly reached the corresponding
    /// cardinal boundary) even though it must first kill and reverse its
    /// velocity. Both numbers are real outputs of the same integrator
    /// everything else in this crate trusts, not a formula, and the
    /// `docs/autonomous-navigation.md` update records this rather than
    /// silently asserting a number that does not hold.
    #[test]
    fn a_diagonal_step_costs_less_than_two_cardinal_steps() {
        let mut t = table();
        let straight_cardinal = walk(&mut t, EntryRel::Straight, SurfaceClass::Normal);
        for entry in [EntryRel::Still, EntryRel::Straight, EntryRel::Reverse] {
            let diagonal = t.get(diagonal_key(entry));
            assert!(diagonal.ok, "{entry:?}: a diagonal over open flat ground must simulate ok");
            let ratio = diagonal.ticks.as_f64() / straight_cardinal.ticks.as_f64();
            assert!(
                (0.5..2.0).contains(&ratio),
                "{entry:?}: diagonal/cardinal-straight ratio {ratio} — must beat two cardinal \
                 steps (<2.0), and a sanity floor against a degenerate near-zero simulation (>0.5)"
            );
        }
    }

    /// The admissibility claim `goal::octile`'s own doc comment makes ("exact
    /// once `WalkDiagonal` lands") depends on `h`'s per-block rate never
    /// exceeding the diagonal's own real rate **per octile block** (`sqrt(2)`
    /// Euclidean blocks per edge), for **every** entry class the search can
    /// actually produce — not only the intuitive one. `Reverse` is the
    /// control: measured independently in `cheapest_ticks_per_block`'s own
    /// doc comment at ~3.09 ticks/octile-block against a purely
    /// cardinal-derived rate of ~3.46, this entry class is exactly the one
    /// that would have silently broken admissibility if
    /// `cheapest_ticks_per_block` had not been taught to scan diagonal
    /// templates too.
    #[test]
    fn the_heuristic_rate_still_bounds_every_diagonal_entry_classs_own_rate() {
        let mut t = table();
        let h_rate = t.cheapest_ticks_per_block();
        for entry in [EntryRel::Still, EntryRel::Straight, EntryRel::Reverse] {
            let diagonal = t.get(diagonal_key(entry));
            assert!(diagonal.ok, "{entry:?}");
            let diagonal_rate = diagonal.ticks.as_f64() / std::f64::consts::SQRT_2;
            assert!(
                h_rate < diagonal_rate,
                "{entry:?}: heuristic {h_rate} t/blk must stay below the diagonal's \
                 {diagonal_rate} t/octile-block"
            );
        }
    }

    /// `EntryRel::of_diagonal`'s whole claim: a cardinal entry is always
    /// exactly one of the two components (`Straight`) or one of their
    /// opposites (`Reverse`), never `Left`/`Right`/anything else — checked
    /// directly against all four cardinal directions for one diagonal.
    #[test]
    fn diagonal_entry_classification_only_ever_produces_still_straight_or_reverse() {
        assert_eq!(EntryRel::of_diagonal(None, Dir4::North, Dir4::East), EntryRel::Still);
        assert_eq!(
            EntryRel::of_diagonal(Some(Dir4::North), Dir4::North, Dir4::East),
            EntryRel::Straight
        );
        assert_eq!(
            EntryRel::of_diagonal(Some(Dir4::East), Dir4::North, Dir4::East),
            EntryRel::Straight
        );
        assert_eq!(
            EntryRel::of_diagonal(Some(Dir4::South), Dir4::North, Dir4::East),
            EntryRel::Reverse
        );
        assert_eq!(
            EntryRel::of_diagonal(Some(Dir4::West), Dir4::North, Dir4::East),
            EntryRel::Reverse
        );
    }

    /// `turn_penalty` is a *preference on top of a measurement*
    /// (`crate::policy::NavPolicy::turn_penalty`'s own doc comment), not a
    /// correction to one — so `EntryRel::of_diagonal`'s `quarter_turns()`
    /// mapping (`Straight` → 0, `Reverse` → 2) only has to be a reasonable
    /// *preference ordering*, never a claim about the simulated ticks
    /// themselves. This is the control that proves that distinction is load
    /// -bearing here, not decorative: the simulated ticks for `Straight` and
    /// `Reverse` do **not** order the way cardinal `turn_rates_are_ordered`
    /// orders them (`Reverse` is cheaper, not dearer — see
    /// `a_diagonal_step_costs_less_than_two_cardinal_steps`'s doc comment for
    /// why), so asserting that ordering here would pin a false claim. What
    /// *is* still true, and asserted, is that `turn_penalty` itself continues
    /// to charge `Reverse` more than `Straight` as an additive preference,
    /// which is all `search::Search::edge_cost` actually relies on.
    #[test]
    fn diagonal_turn_penalty_still_prefers_straight_over_reverse_even_though_ticks_do_not() {
        assert!(EntryRel::Reverse.quarter_turns() > EntryRel::Straight.quarter_turns());

        let mut t = table();
        let straight = t.get(diagonal_key(EntryRel::Straight));
        let reverse = t.get(diagonal_key(EntryRel::Reverse));
        assert!(straight.ok && reverse.ok);
        assert!(
            reverse.ticks < straight.ticks,
            "recording the actual (surprising) relation, so a future change that flips it back \
             is a deliberate decision, not a silent regression: reverse {} vs straight {}",
            reverse.ticks,
            straight.ticks
        );
    }

    /// The generalised sub-tick fraction, tested directly against the
    /// single-axis formula it must reduce to for a cardinal-shaped crossing
    /// (`z` never leaves its target), and against a genuinely two-axis
    /// crossing where the two boundaries are hit on the same tick.
    #[test]
    fn completion_fraction_matches_the_original_single_axis_formula_when_z_never_moves() {
        // A cardinal-shaped crossing: only `x` moves, `z` sits at its target
        // (`0`) the whole time -- must reduce to exactly the original
        // `(1.0 - before.x) / travelled` formula.
        let before = Vec3d::new(0.9, 1.0, 0.0);
        let after = Vec3d::new(1.05, 1.0, 0.0);
        let expected = (1.0 - before.x) / (after.x - before.x);
        let got = completion_fraction(before, after, [1, 1, 0]);
        assert!((got - expected).abs() < 1e-9, "{got} vs {expected}");
    }

    /// A genuinely diagonal crossing: `z` crosses into its target (`-1`)
    /// this tick while `x` is already settled inside cell `1` from an
    /// earlier tick and merely drifts a little further. Only `z`'s own
    /// fraction may contribute -- the bug this generalisation fixes is
    /// exactly the single-axis formula reading `x`'s stale, no-longer-
    /// meaningful delta here instead.
    #[test]
    fn completion_fraction_ignores_an_axis_that_already_settled_on_an_earlier_tick() {
        // `z` crosses from cell `0` into cell `-1` (`floor(-0.1) == -1`) this
        // tick; `x` is already settled in cell `1` from an earlier tick and
        // merely drifts a little further within it.
        let before = Vec3d::new(1.05, 1.0, 0.2);
        let after = Vec3d::new(1.06, 1.0, -0.1);
        let expected_z = axis_boundary_fraction(before.z, after.z, -1);
        let got = completion_fraction(before, after, [1, 1, -1]);
        assert!((got - expected_z).abs() < 1e-9, "{got} vs {expected_z}");
    }

    /// Both axes newly cross on the same tick: the fraction is the **later**
    /// (larger) of the two, since completion needs both.
    #[test]
    fn completion_fraction_takes_the_later_of_two_simultaneous_crossings() {
        let before = Vec3d::new(0.8, 1.0, 0.3);
        let after = Vec3d::new(1.1, 1.0, -0.05);
        let fx = axis_boundary_fraction(before.x, after.x, 1);
        let fz = axis_boundary_fraction(before.z, after.z, -1);
        let got = completion_fraction(before, after, [1, 1, -1]);
        assert!((got - fx.max(fz)).abs() < 1e-9, "{got} vs max({fx}, {fz})");
        // The control that proves this is not vacuously satisfied by taking
        // the minimum instead: with these numbers the two really do differ.
        assert!((fx - fz).abs() > 1e-6, "fx {fx} and fz {fz} must differ for this test to mean anything");
    }

    // --- `Climb` ---

    fn climb_key(dir: ClimbDir) -> TemplateKey {
        key_for(MoveKind::Climb(dir), EntryRel::Still, SurfaceClass::Normal)
    }

    /// Both directions complete and cost more per block than a flat walk —
    /// climbing is genuinely slower than walking, the property
    /// `cheapest_ticks_per_block`'s own admissibility depends on staying
    /// true.
    #[test]
    fn climbing_completes_and_costs_more_than_a_flat_walk_in_both_directions() {
        let mut t = table();
        let flat = walk(&mut t, EntryRel::Straight, SurfaceClass::Normal);
        for dir in [ClimbDir::Up, ClimbDir::Down] {
            let climb = t.get(climb_key(dir));
            assert!(climb.ok, "{dir:?}");
            assert!(
                climb.ticks > flat.ticks,
                "{dir:?}: climb {} should cost more than a flat walk {}",
                climb.ticks,
                flat.ticks
            );
        }
    }

    /// **Predicted, not merely signed**: the steady climb-**up** rate is
    /// derived directly from `travel_in_air`'s own two cited constants —
    /// the override's raw `0.2` target, minus one tick of `profile.gravity`,
    /// times `profile.vertical_air_drag` — computed *outside* this crate's
    /// own simulation and checked against a hand-run tick loop of the real
    /// integrator. `docs/baritone-port.md` §4.3's own `0.2` b/t figure is
    /// the override's raw input, never simulated — this is the real,
    /// measured steady velocity, and it is a genuinely different number.
    #[test]
    fn climb_up_steady_rate_matches_the_gravity_and_drag_derived_formula() {
        let profile = PhysicsProfile::mc_1_21();
        let world = ClimbStencilWorld;
        let drive = ClimbDrive {
            column: [0, 0],
            target_y: 1_000_000,
            target_surface: 0.0,
            ascending: true,
            continuing: true,
        };
        let mut state = PlayerState::at(Vec3d::new(0.5, 1.0, 0.5), 0.0);
        state.on_ground = false;
        for _ in 0..SETTLE_TICKS {
            let step = drive.tick(&state);
            state.yaw = step.yaw;
            lodestone_physics::tick(&mut state, step.input, &world, &profile);
        }
        let before = state.position.y;
        let step = drive.tick(&state);
        state.yaw = step.yaw;
        lodestone_physics::tick(&mut state, step.input, &world, &profile);
        let measured = state.position.y - before;

        let predicted = (0.2 - f64::from(profile.gravity)) * f64::from(profile.vertical_air_drag);
        assert!(
            (measured - predicted).abs() < 1e-9,
            "measured {measured} predicted {predicted}"
        );
        // The recorded, surprising finding this test exists to pin: climbing
        // up is *slower* per block than the design doc's own worked table
        // claims, because the override sets a raw pre-gravity target that
        // real vanilla's own gravity subtraction reduces every tick before
        // it ever reaches `move_entity` — and slower than descending's
        // capped `0.15`, the opposite ordering the doc's raw `0.2`-vs-`0.15`
        // figures would suggest.
        assert!(
            predicted < 0.15,
            "climbing up ({predicted:.4} b/t) must be slower than descending's capped 0.15 \
             b/t, or the ordering test below is asserting nothing"
        );
    }

    /// The mirror image: descending's steady rate is exactly
    /// `handle_on_climbable`'s own literal velocity floor, widened through
    /// `f32` exactly as that function's own doc comment insists on (the
    /// widened value is observable at the last ULP).
    #[test]
    fn climb_down_steady_rate_matches_handle_on_climbables_own_velocity_floor() {
        let profile = PhysicsProfile::mc_1_21();
        let world = ClimbStencilWorld;
        let drive = ClimbDrive {
            column: [0, 0],
            target_y: -1_000_000,
            target_surface: 0.0,
            ascending: false,
            continuing: true,
        };
        let mut state = PlayerState::at(Vec3d::new(0.5, 1.0, 0.5), 0.0);
        state.on_ground = false;
        for _ in 0..SETTLE_TICKS {
            let step = drive.tick(&state);
            state.yaw = step.yaw;
            lodestone_physics::tick(&mut state, step.input, &world, &profile);
        }
        let before = state.position.y;
        let step = drive.tick(&state);
        state.yaw = step.yaw;
        lodestone_physics::tick(&mut state, step.input, &world, &profile);
        let measured = before - state.position.y;

        let predicted = f64::from(0.15_f32);
        assert!(
            (measured - predicted).abs() < 1e-9,
            "measured {measured} predicted {predicted}"
        );
    }

    /// The real, recorded ordering that follows from the two predicted rates
    /// above — climbing **down** is faster per block than climbing **up**,
    /// which is the opposite of what `docs/baritone-port.md` §4.3's own
    /// `0.2` (up) vs `0.15` (down) figures would predict, since neither of
    /// those two numbers is the design doc's own simulated steady velocity.
    #[test]
    fn climbing_down_costs_fewer_ticks_than_climbing_up() {
        let mut t = table();
        let up = t.get(climb_key(ClimbDir::Up));
        let down = t.get(climb_key(ClimbDir::Down));
        assert!(up.ok && down.ok);
        assert!(
            down.ticks < up.ticks,
            "down {} should cost fewer ticks than up {}",
            down.ticks,
            up.ticks
        );
    }

    /// The admissibility contract, folded to cover `Climb`: `h`'s per-block
    /// rate must stay strictly below both climb directions' own real rates,
    /// the same claim `the_heuristic_rate_is_strictly_below_every_simulated_rate`
    /// already makes for the horizontal kinds — `cheapest_ticks_per_block`'s
    /// own doc comment records *why* this needed checking rather than
    /// assuming (climbing is slower than the cheapest horizontal rate, but
    /// "slower-looking" movements have been the wrong assumption before,
    /// see `WalkDiagonal`'s `Reverse` entry class).
    #[test]
    fn the_heuristic_rate_stays_below_every_climb_directions_own_rate() {
        let mut t = table();
        let h_rate = t.cheapest_ticks_per_block();
        for dir in [ClimbDir::Up, ClimbDir::Down] {
            let climb = t.get(climb_key(dir));
            assert!(climb.ok, "{dir:?}");
            let real_rate = climb.ticks.as_f64(); // exactly one block per edge
            assert!(
                h_rate < real_rate,
                "{dir:?}: heuristic {h_rate} t/blk is not below the real climb rate {real_rate}"
            );
            // Report the measured margin, per this pass's own evidence
            // standard: a passing assertion with no stated distance is not
            // the same claim as "and here is how much slack there is".
            let margin = real_rate - h_rate;
            assert!(
                margin > 1.0,
                "{dir:?}: margin {margin} t/blk between the heuristic and the real climb rate \
                 is suspiciously thin for a kind this much slower than the cheapest movement"
            );
        }
    }

    /// The negative control proving `TemplateKey::drop_n`'s own lesson
    /// generalises: `Climb(Up)` and `Climb(Down)` must occupy distinct
    /// memoisation slots, not collide into one — `MoveKind::id`'s own doc
    /// comment states why folding them together would be exactly the
    /// "search believes 6, executor needs 14" failure this crate's whole
    /// template-table design exists to make impossible.
    #[test]
    fn climb_up_and_down_are_distinct_table_entries_with_distinct_ids() {
        assert_ne!(MoveKind::Climb(ClimbDir::Up).id(), MoveKind::Climb(ClimbDir::Down).id());
        let mut t = table();
        t.get(climb_key(ClimbDir::Up));
        t.get(climb_key(ClimbDir::Down));
        t.get(climb_key(ClimbDir::Up));
        assert_eq!(t.len(), 2, "two distinct climb templates, not memoised together");
    }
}
