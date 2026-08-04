//! The movement graph: nodes that carry arrival state, and the legality
//! predicates over [`NavView`] (`docs/baritone-port.md` §4.3).
//!
//! A node is the block cell the player's **feet** occupy, plus how they got there.
//! The arrival dimension exists because whether a movement is possible, and how
//! long it takes, are both functions of entry velocity — a purely positional graph
//! must either assume the worst (and refuse moves it could make) or the best (and
//! plan moves it cannot), and the second failure is the plan-fail-replan loop.
//!
//! # M1 scope
//!
//! `Walk` only, `Arrival::{Still, Walking}`. `WalkDiagonal`, `StepUp`, `Descend`,
//! `Drop`, `Climb`, `Gap`, `Break`, `Place` and `Swim` are later milestones.
//! Adding one means a variant here, a legality rule, an input script and a
//! template key — deliberately four small edits in four named places rather than a
//! new cost formula.

use crate::view::NavView;

/// The 0.6-block auto-step height, from
/// [`lodestone_physics::EntityDimensions::PLAYER`] rather than a literal: 26.2's
/// player does not override `maxUpStep()`, so it is the `STEP_HEIGHT` attribute
/// default.
pub const STEP_HEIGHT: f64 = lodestone_physics::EntityDimensions::PLAYER.step_height as f64;

/// The player's height, for the head-clearance check.
pub const BODY_HEIGHT: f64 = lodestone_physics::EntityDimensions::PLAYER.height as f64;

/// Vanilla's `Attributes.SAFE_FALL_DISTANCE` default, in blocks
/// (`Attributes.java:87`: `new RangedAttribute(…, 3.0, …)`). Fall damage is
/// `floor(fallDistance + 1e-6 - SAFE_FALL_DISTANCE)` half-hearts
/// (`LivingEntity.java:1856`), so this is the real number below which a fall
/// deals zero damage — [`crate::policy::NavPolicy::max_fall_blocks`]'s default.
pub const SAFE_FALL_DISTANCE: f64 = 3.0;

/// Apex height of an unassisted vertical jump, in blocks above the surface it
/// began from — **derived by simulation**, not by formula
/// (`docs/baritone-port.md` §4.4's rule applied to a legality bound rather than
/// a cost): it runs `lodestone_physics::tick` with jump held against a flat
/// floor and measures the highest point reached, against the default
/// `mc_1_21` profile. `docs/baritone-port.md` §4.3 records the same figure
/// (~1.2522 blocks) computed the same way, and it is why a 1-block `StepUp`
/// (delta `1.0`) clears and a 1.5-block one (a fence, two slabs) does not.
///
/// Cached in a [`std::sync::OnceLock`] rather than threaded as a parameter:
/// legality here does not yet vary by policy or status effect (jump-boost-aware
/// legality is a later milestone, matching the cost model's own `EffectClass`
/// gap), so one computed value for the crate's lifetime is honest rather than a
/// simplification that hides a real dependency.
#[must_use]
pub fn jump_apex_height() -> f64 {
    static APEX: std::sync::OnceLock<f64> = std::sync::OnceLock::new();
    *APEX.get_or_init(|| {
        use lodestone_physics::{
            Aabb, CollisionView, FluidCell, FluidKind, HorizontalDir, MovementInput,
            PhysicsProfile, PlayerState, Vec3d, tick,
        };

        /// A single full-cube floor at `y = 0`: everything this measurement needs
        /// and nothing that could bias it.
        #[derive(Debug)]
        struct Floor;
        impl CollisionView for Floor {
            fn collision_boxes(&self, x: i32, y: i32, z: i32, out: &mut Vec<Aabb>) {
                if y == 0 {
                    out.push(Aabb::new(
                        f64::from(x),
                        0.0,
                        f64::from(z),
                        f64::from(x) + 1.0,
                        1.0,
                        f64::from(z) + 1.0,
                    ));
                }
            }
            fn collision_top(&self, _x: i32, y: i32, _z: i32) -> f64 {
                if y == 0 { 1.0 } else { 0.0 }
            }
            fn blocks_motion(&self, _x: i32, y: i32, _z: i32) -> bool {
                y == 0
            }
            fn fluid_at(&self, _x: i32, _y: i32, _z: i32) -> Option<FluidCell> {
                None
            }
            fn is_solid_face(
                &self,
                _x: i32,
                y: i32,
                _z: i32,
                _dir: HorizontalDir,
                _kind: FluidKind,
            ) -> bool {
                y == 0
            }
        }

        let profile = PhysicsProfile::mc_1_21();
        let world = Floor;
        let mut state = PlayerState::at(Vec3d::new(0.5, 1.0, 0.5), 0.0);
        state.on_ground = true;
        let mut apex = 0.0_f64;
        // One tick with jump held to leave the ground, then ride it out.
        tick(
            &mut state,
            MovementInput {
                jump: true,
                ..MovementInput::NONE
            },
            &world,
            &profile,
        );
        for _ in 0..40 {
            apex = apex.max(state.position.y - 1.0);
            if state.on_ground {
                break;
            }
            tick(&mut state, MovementInput::NONE, &world, &profile);
        }
        apex
    })
}

/// Tolerance on surface comparisons. Shapes are exact in `f32`, so this only
/// absorbs the `f32 -> f64` widening and the `y as f64` arithmetic around it.
///
/// `pub(crate)` because [`crate::search`] compares a `Step::to_surface` against its
/// cell floor with the same tolerance, to decide which cell holds the block the feet
/// rest on.
pub(crate) const SURFACE_EPS: f64 = 1e-6;

/// A horizontal direction, in `Direction.Plane.HORIZONTAL` order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Dir4 {
    /// `-Z`.
    North,
    /// `+X`.
    East,
    /// `+Z`.
    South,
    /// `-X`.
    West,
}

impl Dir4 {
    /// All four, in vanilla's horizontal order — the order neighbour expansion
    /// uses, so expansion order is fixed and the search is reproducible.
    pub const ALL: [Self; 4] = [Self::North, Self::East, Self::South, Self::West];

    /// Block delta `(dx, dz)`.
    #[must_use]
    pub const fn delta(self) -> (i32, i32) {
        match self {
            Self::North => (0, -1),
            Self::East => (1, 0),
            Self::South => (0, 1),
            Self::West => (-1, 0),
        }
    }

    /// Dense index in `0..4`.
    #[must_use]
    pub const fn index(self) -> u8 {
        match self {
            Self::North => 0,
            Self::East => 1,
            Self::South => 2,
            Self::West => 3,
        }
    }

    /// Inverse of [`Self::index`].
    #[must_use]
    pub const fn from_index(index: u8) -> Option<Self> {
        Some(match index {
            0 => Self::North,
            1 => Self::East,
            2 => Self::South,
            3 => Self::West,
            _ => return None,
        })
    }

    /// Quarter-turns clockwise from `self` to `other`, in `0..4`.
    #[must_use]
    pub const fn turns_to(self, other: Self) -> u8 {
        (other.index() + 4 - self.index()) % 4
    }

    /// The next direction clockwise — `Dir4::ALL`'s own successor, matching
    /// vanilla's `Direction.getClockWise()` for a horizontal direction
    /// (`.cache/mc/26.2/src/net/minecraft/world/level/pathfinder/WalkNodeEvaluator.java:143`:
    /// `Direction secondDirection = direction.getClockWise();`).
    ///
    /// This is the pairing [`MoveKind::WalkDiagonal`] always uses: `(North,
    /// East)`, `(East, South)`, `(South, West)`, `(West, North)` — the same
    /// four pairs the mob pathfinder iterates when deciding whether a
    /// diagonal neighbour exists at all.
    #[must_use]
    pub const fn clockwise(self) -> Self {
        match self {
            Self::North => Self::East,
            Self::East => Self::South,
            Self::South => Self::West,
            Self::West => Self::North,
        }
    }
}

/// Which way a [`MoveKind::Climb`] moves within its column.
///
/// Not a [`Dir4`] — climbing has no horizontal heading at all, which is the
/// whole reason it needs its own script (`docs/autonomous-navigation.md`'s
/// "`Climb`: stopped, and why"). Up and down are genuinely different
/// equivalence classes for the cost model (`0.2` b/t vs a capped `0.15` b/t,
/// `docs/baritone-port.md` §4.3), so — unlike [`Dir4`] for every cardinal
/// kind — direction is folded into [`MoveKind::id`] rather than being cost-
/// irrelevant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ClimbDir {
    /// +1 Y: hold jump. Universal across a ladder (which has a wall to press
    /// into) and a free-hanging vine strand (which may not) —
    /// `lodestone_physics::entity::travel_in_air`'s climb override fires on
    /// `ctx.jumping` alone, no collision required.
    Up,
    /// −1 Y: hold nothing. `handle_on_climbable`'s own velocity floor
    /// (`-0.15`) already caps the descent, unassisted.
    Down,
}

/// How the player entered a node.
///
/// Five variants for M1: standing still, or walking in one of four directions. The
/// plan's `Sprinting(Dir4)` arm is M3 and slots in here without touching the
/// packing (three bits are reserved for exactly nine states).
///
/// **`Climb` needed a sixth, [`Self::Climbing`] — but only for one reason, and
/// it is not the one every other kind's arrival dimension exists for.** For
/// *costing*, a node reached by climbing needs no distinction from
/// [`Self::Still`] at all: the script presses no forward/strafe, so there is
/// no horizontal momentum for a future edge's [`crate::cost::EntryRel`] to
/// exploit, and `crate::search::Search::expand`'s own comment records that
/// `EntryRel::of` never even reads this variant's direction (`dir()` returns
/// `None` for it, same as `Still`) — a genuinely *stronger* collapse than
/// [`MoveKind::WalkDiagonal`]'s three-way one.
///
/// The reason it exists anyway is the **executor**, not the search: a
/// dismount-onto-ground node and a mid-column, still-clinging node both come
/// out of [`climb_step`] as candidates for "where did this edge end", and
/// [`crate::drive::ClimbDrive::done`] needs to know which one it is — it must
/// require `on_ground` for a dismount (that is what "arrived" means when
/// standing) but must **never** require it mid-column, where the body is
/// never grounded at all and a `Still`-shaped "wait for on_ground" test would
/// simply hang forever. `to_surface` cannot distinguish the two cases (a
/// full-block dismount's surface is numerically identical to a continuing
/// climb's nominal cell-floor reference — both are exactly the destination
/// cell's own integer `y`), so the fact has to be carried on the node itself.
/// This is the honest answer to whether the vertical frame admits the
/// diagonal's collapse: **the cost side does, fully; the executor's own
/// arrival test does not**, and that is a genuinely different thing than
/// `EntryRel` from the one the diagonal work found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Arrival {
    /// At rest, or slow enough that entry velocity does not help.
    Still,
    /// Walking in this direction at (roughly) steady speed.
    Walking(Dir4),
    /// Mid-column on a climbable cell, never grounded. Carries no direction —
    /// a climb has no horizontal heading — which is why costing treats it
    /// identically to [`Self::Still`].
    Climbing,
}

impl Arrival {
    /// Dense index in `0..8`.
    #[must_use]
    pub const fn index(self) -> u8 {
        match self {
            Self::Still => 0,
            Self::Walking(dir) => 1 + dir.index(),
            Self::Climbing => 5,
        }
    }

    /// Inverse of [`Self::index`].
    #[must_use]
    pub const fn from_index(index: u8) -> Option<Self> {
        match index {
            0 => Some(Self::Still),
            1..=4 => match Dir4::from_index(index - 1) {
                Some(dir) => Some(Self::Walking(dir)),
                None => None,
            },
            5 => Some(Self::Climbing),
            _ => None,
        }
    }

    /// The direction of travel, or `None` at rest or mid-climb.
    #[must_use]
    pub const fn dir(self) -> Option<Dir4> {
        match self {
            Self::Still | Self::Climbing => None,
            Self::Walking(dir) => Some(dir),
        }
    }
}

/// A search node: the feet cell plus the arrival state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NavNode {
    /// Feet cell `x`.
    pub x: i32,
    /// Feet cell `y`.
    pub y: i32,
    /// Feet cell `z`.
    pub z: i32,
    /// How the player got here.
    pub arrival: Arrival,
}

/// Bits `x` and `z` each get in the packed key. `±2^25` blocks covers the world
/// border (`±29,999,984`) with room to spare.
const XZ_BITS: u32 = 26;
/// Bits `y` gets, offset by [`Y_OFFSET`]. 9 bits = 512 values, which covers every
/// vanilla dimension (`-64..=319` in the overworld).
const Y_BITS: u32 = 9;
/// What is added to world `y` before packing.
const Y_OFFSET: i32 = 64;

const XZ_MASK: u64 = (1u64 << XZ_BITS) - 1;
const XZ_HALF: i32 = 1 << (XZ_BITS - 1);
const Y_MASK: u64 = (1u64 << Y_BITS) - 1;

impl NavNode {
    /// A node at rest.
    #[must_use]
    pub const fn still(x: i32, y: i32, z: i32) -> Self {
        Self {
            x,
            y,
            z,
            arrival: Arrival::Still,
        }
    }

    /// The same cell with a different arrival.
    #[must_use]
    pub const fn with_arrival(self, arrival: Arrival) -> Self {
        Self { arrival, ..self }
    }

    /// Pack into a `u64` key, or `None` outside the representable range.
    ///
    /// `None` is not "clamp and carry on". A hash collision or a wrong equality on
    /// this key silently corrupts a plan (`docs/baritone-port.md` §2.3), so an
    /// unrepresentable node is refused at the door and the bijection is
    /// unit-tested directly.
    #[must_use]
    pub fn try_pack(self) -> Option<u64> {
        if self.x < -XZ_HALF || self.x >= XZ_HALF || self.z < -XZ_HALF || self.z >= XZ_HALF {
            return None;
        }
        let y = self.y.checked_add(Y_OFFSET)?;
        if y < 0 {
            return None;
        }
        #[allow(clippy::cast_sign_loss)]
        let y = y as u64;
        if y > Y_MASK {
            return None;
        }
        #[allow(clippy::cast_sign_loss)]
        let x = (self.x as u64) & XZ_MASK;
        #[allow(clippy::cast_sign_loss)]
        let z = (self.z as u64) & XZ_MASK;
        Some(
            (x << (XZ_BITS + Y_BITS + 3))
                | (z << (Y_BITS + 3))
                | (y << 3)
                | u64::from(self.arrival.index()),
        )
    }

    /// Inverse of [`Self::try_pack`].
    #[must_use]
    pub fn unpack(key: u64) -> Option<Self> {
        #[allow(clippy::cast_possible_truncation)]
        let arrival = Arrival::from_index((key & 0b111) as u8)?;
        #[allow(clippy::cast_possible_truncation)]
        let y = ((key >> 3) & Y_MASK) as i32 - Y_OFFSET;
        let sign_extend = |raw: u64| -> i32 {
            #[allow(clippy::cast_possible_truncation)]
            let v = (raw & XZ_MASK) as i32;
            if v >= XZ_HALF { v - (XZ_HALF << 1) } else { v }
        };
        let z = sign_extend(key >> (Y_BITS + 3));
        let x = sign_extend(key >> (XZ_BITS + Y_BITS + 3));
        Some(Self { x, y, z, arrival })
    }
}

/// One movement kind. Direction is a parameter, not a separate type.
///
/// # M2 additions
///
/// `StepUp`, `Descend` and `Drop` are M2 (`docs/baritone-port.md` §4.3, §9).
/// `Descend` and `Drop` are the same physical act — falling until the first
/// standable surface below is reached — split into two *names* because the
/// design table gives them separately, not two *searches*: [`fall_step`] finds
/// the one real landing and classifies it by how far down it was, exactly as
/// vanilla's own physics would (a body cannot fall past a surface it would
/// land on, so there is only ever one candidate per direction, never a family
/// of "try n = 2, 3, 4…").
///
/// `WalkDiagonal` is also M2, landed separately and later: it needed a real
/// generalisation of the cost model's canonical simulation frame rather than a
/// small edit (`crate::cost`'s module docs and `docs/autonomous-navigation.md`
/// record what generalised cleanly and what did not).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MoveKind {
    /// A same-cell-height step to an orthogonal neighbour, including a step up or
    /// down within [`STEP_HEIGHT`] — which is what makes bottom slabs, soul sand
    /// (top `0.875`), farmland and snow layers work.
    Walk(Dir4),
    /// +1 Y: a step up too tall for the 0.6 auto-step, which therefore needs a
    /// jump. Legal only when the destination surface is within
    /// [`jump_apex_height`] of the source (`docs/baritone-port.md` §4.3: this is
    /// the number, not a rule of thumb, that decides it — a 1.0 ascend clears,
    /// a 1.5 one does not).
    StepUp(Dir4),
    /// −1 Y onto solid ground: the nearest standable surface below is exactly
    /// one cell down. Never damaging — the resulting surface delta is always
    /// under vanilla's `SAFE_FALL_DISTANCE` (3.0 blocks,
    /// `Attributes.java:87`), so unlike [`Self::Drop`] this has no
    /// damage-vs-health legality to apply.
    Descend(Dir4),
    /// Fall more than one cell onto solid ground, landing on the block whose top
    /// is `n` cells below the source surface. Legality from expected damage vs
    /// [`crate::policy::NavPolicy::max_fall_blocks`] lives in the search's edge
    /// cost (`docs/baritone-port.md` §4.4), not here — this type only proves
    /// *where* the fall lands, which [`fall_step`] derives from the same real
    /// jar rule (`LivingEntity.java:1856`:
    /// `fallDistance + 1e-6 - SAFE_FALL_DISTANCE`) the damage check itself uses.
    Drop(Dir4, u8),
    /// A `(1, 1)` diagonal step, same-cell-height family like [`Self::Walk`]
    /// (no diagonal ascend/descend/jump — that would be a third cost-model
    /// frame on top of the one this already needed, and is left open rather
    /// than rushed; see `docs/autonomous-navigation.md`). The pair is always
    /// `(d, d.clockwise())` — [`diagonal_step`] and [`successors`] are the only
    /// constructors, and both hold that invariant.
    WalkDiagonal(Dir4, Dir4),
    /// A single-cell vertical move within a climbable column (a ladder or a
    /// vine — both are `BlockTags.CLIMBABLE` and `handle_on_climbable`/
    /// `travel_in_air`'s climb override treats them identically, so one kind
    /// covers both real block families). No horizontal displacement: `x`
    /// and `z` are unchanged, which is exactly what forces this to be a
    /// genuinely different script rather than a parameter to
    /// [`crate::drive::WalkDrive`] (`docs/autonomous-navigation.md`'s
    /// "`Climb`: stopped, and why").
    Climb(ClimbDir),
}

impl MoveKind {
    /// Dense id, for the template key. **Not** a full identity for [`Self::Drop`]
    /// — its fall distance is a separate [`crate::cost::TemplateKey`] field,
    /// because two drops of different height are not the same equivalence
    /// class and memoising them together would cost a shorter fall's edge the
    /// ticks a longer one actually takes.
    ///
    /// [`Self::WalkDiagonal`] collapses all four direction pairs to **one**
    /// id, exactly as every [`Self::Walk`] direction already does — the
    /// canonical-frame simulation in `crate::cost` treats direction as
    /// irrelevant, so the actual `(Dir4, Dir4)` never needs to reach the key.
    ///
    /// [`Self::Climb`] is the opposite case: `Up` and `Down` get **separate**
    /// ids, because — unlike a cardinal direction — the climb direction *is*
    /// cost-relevant (`0.2` b/t up vs a capped `0.15` b/t down,
    /// `docs/baritone-port.md` §4.3). Folding them into one id would memoise
    /// whichever direction was simulated first and silently cost the other
    /// its ticks — the exact `Drop`/`drop_n` failure mode this doc comment
    /// already warns about, one level up.
    #[must_use]
    pub const fn id(self) -> u8 {
        match self {
            Self::Walk(_) => 0,
            Self::StepUp(_) => 1,
            Self::Descend(_) => 2,
            Self::Drop(_, _) => 3,
            Self::WalkDiagonal(_, _) => 4,
            Self::Climb(ClimbDir::Up) => 5,
            Self::Climb(ClimbDir::Down) => 6,
        }
    }

    /// The direction this movement travels — for [`Self::WalkDiagonal`], its
    /// **first** component only, and for [`Self::Climb`], a meaningless
    /// placeholder.
    ///
    /// That is a real approximation, not a full answer: a diagonal travels
    /// along two axes at once, and no single [`Dir4`] describes it, and a
    /// climb has no horizontal heading at all. It exists only so every
    /// [`MoveKind`] has *some* answer for callers that have not been taught
    /// about either specifically — `crate::search::Search::expand` never
    /// actually reaches this arm for either kind, since it special-cases both
    /// (`crate::cost::EntryRel::of_diagonal` for the diagonal, a fixed
    /// [`crate::cost::EntryRel::Still`] for the climb — see that call site's
    /// own comment on why a climb needs no entry classification at all).
    #[must_use]
    pub const fn dir(self) -> Dir4 {
        match self {
            Self::Walk(dir) | Self::StepUp(dir) | Self::Descend(dir) | Self::Drop(dir, _) => dir,
            Self::WalkDiagonal(d1, _) => d1,
            Self::Climb(_) => Dir4::North,
        }
    }

    /// The cells this movement's legality predicate reads, relative to the source
    /// feet cell.
    ///
    /// **Static**, which is what makes it free to translate and union after the
    /// fact into a committed plan's witness set (`docs/baritone-port.md` §4.5).
    /// Recording witnesses *during* the search would add a hash insert per cell
    /// read per expanded node, which is exactly the wrong place to spend.
    ///
    /// [`Self::StepUp`]/[`Self::Descend`]/[`Self::Drop`] build theirs once per
    /// direction, lazily, via [`column_stencil`] rather than as hand-transcribed
    /// literals like [`WALK_EAST`] and its rotations: the ranges are wide enough
    /// (a `Drop`'s covers [`FALL_SCAN_CELLS`] plus head-room margin) that
    /// hand-writing four rotations of each would be the same class of
    /// transcription risk `docs/baritone-port.md`'s own evidence standards warn
    /// about, for no benefit — a stencil is read, never hot, so the one-time
    /// `Box::leak` per direction is free.
    #[must_use]
    pub fn stencil(self) -> &'static [[i32; 3]] {
        match self {
            Self::Walk(Dir4::North) => &WALK_NORTH,
            Self::Walk(Dir4::East) => &WALK_EAST,
            Self::Walk(Dir4::South) => &WALK_SOUTH,
            Self::Walk(Dir4::West) => &WALK_WEST,
            Self::StepUp(dir) => step_up_stencil(dir),
            Self::Descend(dir) => descend_stencil(dir),
            Self::Drop(dir, _) => drop_stencil(dir),
            Self::WalkDiagonal(d1, d2) => diagonal_stencil(d1, d2),
            Self::Climb(dir) => climb_stencil(dir),
        }
    }
}

/// [`MoveKind::Climb`]'s stencil: just the source and destination cells.
/// Unlike every horizontal kind, a climb reads no body-width shoulder and no
/// head-room margin — the destination is the same `(x, z)` column, one cell
/// up or down, and [`climb_step`]'s own legality check reads exactly these
/// two cells and nothing else.
const CLIMB_UP: [[i32; 3]; 2] = [[0, 0, 0], [0, 1, 0]];
const CLIMB_DOWN: [[i32; 3]; 2] = [[0, 0, 0], [0, -1, 0]];

fn climb_stencil(dir: ClimbDir) -> &'static [[i32; 3]] {
    match dir {
        ClimbDir::Up => &CLIMB_UP,
        ClimbDir::Down => &CLIMB_DOWN,
    }
}

/// Build and leak a two-column stencil spanning `y_lo..=y_hi` relative to the
/// source cell, for the source column `(0, y, 0)` and the destination column
/// `(dx, y, dz)`. Leaked rather than owned because [`MoveKind::stencil`]
/// contracts `&'static`; called at most once per direction per kind (cached by
/// the caller in a [`std::sync::OnceLock`]), so the one-time leak is a few
/// dozen bytes for the process lifetime, not a growth.
fn column_stencil(dx: i32, dz: i32, y_lo: i32, y_hi: i32) -> &'static [[i32; 3]] {
    let mut cells = Vec::with_capacity(((y_hi - y_lo + 1) * 2) as usize);
    for y in y_lo..=y_hi {
        cells.push([0, y, 0]);
        cells.push([dx, y, dz]);
    }
    Vec::leak(cells)
}

/// Lazily-built, cached-per-direction stencil for a kind whose cells are a
/// [`column_stencil`] over a fixed `y_lo..=y_hi`. One [`std::sync::OnceLock`]
/// per kind holds all four directions' `Box::leak`ed slices, computed once.
macro_rules! column_stencil_fn {
    ($name:ident, $y_lo:expr, $y_hi:expr) => {
        fn $name(dir: Dir4) -> &'static [[i32; 3]] {
            static CACHE: std::sync::OnceLock<[&'static [[i32; 3]]; 4]> = std::sync::OnceLock::new();
            let table = CACHE.get_or_init(|| {
                Dir4::ALL.map(|d| {
                    let (dx, dz) = d.delta();
                    column_stencil(dx, dz, $y_lo, $y_hi)
                })
            });
            table[dir.index() as usize]
        }
    };
}

// `StepUp`: source column from one below its support to comfortably above the
// jump apex; destination column the same, since the higher surface is the one
// that needs the taller headroom check.
column_stencil_fn!(step_up_stencil, -1, 4);
// `Descend`: destination surface can be a "below" support (needs `ty - 1`) or
// an "inside" partial block pushing the head into `ty + 2` — `-2..=1`
// covers both with margin.
column_stencil_fn!(descend_stencil, -2, 1);
// `Drop`: generous enough to cover every `n` up to `FALL_SCAN_CELLS` regardless
// of which one a specific edge landed at — over-covering a witness set is safe
// (`docs/baritone-port.md` §4.5), under-covering is not.
column_stencil_fn!(drop_stencil, -(FALL_SCAN_CELLS + 2), 2);

/// Cached per-`(d1, d2)` diagonal stencil: the source column, **both**
/// shoulder columns (`d1` and `d2` alone) and the destination column
/// (`d1 + d2`), each over the same `-1..=2` span [`WALK_EAST`] and its
/// rotations use. Four columns instead of two is the one structural
/// difference from [`column_stencil`] — a diagonal's legality genuinely reads
/// two extra columns the cardinal kinds never touch (the corner-cutting
/// check, see [`diagonal_step`]), so under-covering the witness set here
/// would mean a corner wall could change without the plan ever noticing.
fn diagonal_stencil(d1: Dir4, d2: Dir4) -> &'static [[i32; 3]] {
    static CACHE: std::sync::OnceLock<[&'static [[i32; 3]]; 4]> = std::sync::OnceLock::new();
    let table = CACHE.get_or_init(|| {
        Dir4::ALL.map(|first| {
            let second = first.clockwise();
            let (dx1, dz1) = first.delta();
            let (dx2, dz2) = second.delta();
            let mut cells = Vec::with_capacity(4 * 4);
            for y in -1..=2 {
                cells.push([0, y, 0]);
                cells.push([dx1, y, dz1]);
                cells.push([dx2, y, dz2]);
                cells.push([dx1 + dx2, y, dz1 + dz2]);
            }
            Vec::leak(cells) as &'static [[i32; 3]]
        })
    });
    debug_assert_eq!(d2, d1.clockwise(), "WalkDiagonal must pair (d, d.clockwise())");
    table[d1.index() as usize]
}

/// `Walk`'s stencil: the source column's support/body/head cells and the
/// destination column's. The four rotations are spelled out because the stencil
/// has to be `&'static`.
const WALK_EAST: [[i32; 3]; 8] = [
    [0, -1, 0],
    [0, 0, 0],
    [0, 1, 0],
    [0, 2, 0],
    [1, -1, 0],
    [1, 0, 0],
    [1, 1, 0],
    [1, 2, 0],
];
const WALK_WEST: [[i32; 3]; 8] = [
    [0, -1, 0],
    [0, 0, 0],
    [0, 1, 0],
    [0, 2, 0],
    [-1, -1, 0],
    [-1, 0, 0],
    [-1, 1, 0],
    [-1, 2, 0],
];
const WALK_SOUTH: [[i32; 3]; 8] = [
    [0, -1, 0],
    [0, 0, 0],
    [0, 1, 0],
    [0, 2, 0],
    [0, -1, 1],
    [0, 0, 1],
    [0, 1, 1],
    [0, 2, 1],
];
const WALK_NORTH: [[i32; 3]; 8] = [
    [0, -1, 0],
    [0, 0, 0],
    [0, 1, 0],
    [0, 2, 0],
    [0, -1, -1],
    [0, 0, -1],
    [0, 1, -1],
    [0, 2, -1],
];

/// Where the player's feet rest when standing in cell `(x, y, z)`, as a
/// world-space `y`, or `None` when that cell is not a place a body can stand.
///
/// This is the function `docs/baritone-port.md` §2.3 warns is **not**
/// `floor(position)`. Two cases, and both are real terrain:
///
/// * the cell holds a partial block whose top is under `1.0` — a bottom slab
///   (`0.5`), soul sand (`0.875`), farmland, a snow layer — and the feet rest on
///   *it*, inside this cell;
/// * the cell is passable and the feet rest on the block **below**, which must
///   present a full-height top.
///
/// A cell whose own shape reaches `1.0` or beyond is filled: you stand on top of
/// it, which is the cell above, not here.
///
/// # The one M1 restriction, stated so it is not mistaken for a bug
///
/// A support whose top exceeds `1.0` — a fence or a wall, which report `1.5` — is
/// **refused** rather than treated as a surface at `y + 0.5`. Walking a fence line
/// is legal in vanilla and the geometry is fiddly (the post is `0.25` wide);
/// refusing it costs nothing a walking bot wants and cannot produce a wrong answer.
/// The uncapped `top` is what makes the refusal possible at all: clamped to `1.0`,
/// a fence would read as ordinary ground.
#[must_use]
pub fn stand_surface(view: &dyn NavView, x: i32, y: i32, z: i32) -> Option<f64> {
    let inside = view.facts_at(x, y, z)?;
    // A climbable block's own collision shape must never be read as a
    // support, in **either** branch below — `LadderBlock`/`VineBlock` both
    // call `forceSolidOff()`
    // (`.cache/mc/26.2/src/net/minecraft/world/level/block/LadderBlock.java`'s
    // `Properties` — see `lodestone_model::adapter::block_blocks_motion`'s own
    // doc comment, which names the ladder's `0.7291666666666666` mean-extent
    // threshold specifically *because* landing on one produces the wrong
    // answer), so `blocks_motion` is `false` regardless of its shape.
    //
    // The shape itself is real, and — measured against the same doc
    // comment's `(1 + 1 + 3/16) / 3` figure — it is **full height**: `top ==
    // 1.0`, thin only in the horizontal axis against the wall. Two distinct,
    // previously-latent bugs followed from that, both invisible until
    // `Climb` needed to reason about climbable cells at all (no fixture in
    // this crate's tests had one before):
    //
    // - **The "inside" branch.** A `top == 1.0` cell is "filled" by every
    //   other rule this function has — a full cube, a fence's `1.5` post —
    //   so without the exemption below, standing *in* a ladder's own cell was
    //   refused outright, the same refusal a solid wall gets. A ladder could
    //   never be mounted at all.
    // - **The "below" branch.** The exemption in the "inside" branch alone is
    //   not enough: a climbable one cell **under** a candidate stand cell
    //   would still read `below_top == 1.0` and pass as a full support,
    //   letting a body appear to stand *on top of* a ladder or vine from
    //   above — which vanilla's own collision never permits, since the real
    //   shape is a thin sliver against a wall, not a horizontal cap.
    //
    // Both branches therefore treat a climbable cell exactly like `AIR` for
    // support purposes: never a floor to stand on top of, always something to
    // look past for whatever is actually beneath it.
    let inside_top = if inside.climbable {
        0.0
    } else {
        f64::from(inside.top)
    };
    if inside_top > 0.0 {
        if inside_top >= 1.0 {
            // Filled (full cube, fence, wall): stand above it, not in it.
            return None;
        }
        return Some(f64::from(y) + inside_top);
    }
    let below = view.facts_at(x, y - 1, z)?;
    let below_top = if below.climbable {
        0.0
    } else {
        f64::from(below.top)
    };
    if (below_top - 1.0).abs() <= SURFACE_EPS {
        Some(f64::from(y))
    } else {
        // Either nothing to stand on, or a fence/wall top, which M1 refuses.
        None
    }
}

/// Whether a body standing at `surface` in column `(x, z)` has room for its
/// [`BODY_HEIGHT`].
///
/// The body spans `[surface, surface + 1.8]`. The cell the feet are in contributes
/// only the surface they rest on, so the cells that must be clear are the two above
/// — and the second only when the surface is high enough in its cell to push the
/// head past `y + 2`.
#[must_use]
pub fn head_room(view: &dyn NavView, x: i32, y: i32, z: i32, surface: f64) -> bool {
    let head = surface + BODY_HEIGHT;
    let mut cell = y + 1;
    loop {
        let Some(facts) = view.facts_at(x, cell, z) else {
            return false;
        };
        // A climbable cell's nonzero collision shape (`graph::stand_surface`'s
        // own doc comment on `forceSolidOff`) does not meaningfully occupy
        // headroom either — it is a thin sliver against a wall, and a real
        // body's head passes it every time it walks past a ladder or under a
        // vine. Without this, `passable == false` (from the nonzero shape
        // alone) refused headroom for **every** cell within a body's height
        // of a climbable block — including the climbable's own stand cell
        // checking the cell directly above it, which is exactly the ladder's
        // *next* rung. A `Climb` chain could not even mount the bottom rung
        // under this bug, since `standable` at the mount cell reads the rung
        // above as the headroom sweep's first cell.
        if !facts.passable && !facts.climbable {
            return false;
        }
        if f64::from(cell) + 1.0 >= head - SURFACE_EPS {
            return true;
        }
        cell += 1;
    }
}

/// A cell the navigator will put the body in: standable, with head room, and
/// carrying no hazard anywhere the body occupies.
///
/// # The support-block check only applies to [`stand_surface`]'s "below" branch
///
/// This function used to check `facts_at(x, y - 1, z)` unconditionally, on the
/// reasoning in the comment below: the body-sweep loop starts at cell `y`, so a
/// **full-cube** support — which sits at `y - 1`, one cell the sweep never
/// reaches — needs its own explicit hazard check (the magma-block case the
/// comment names).
///
/// That reasoning does not hold for [`stand_surface`]'s other branch. When the
/// feet rest **inside** cell `y` itself — a partial block: a bottom slab
/// (`0.5`), soul sand (`0.875`), farmland, a snow layer — that block *is* cell
/// `y`, already covered by the sweep loop above (which starts at `y`, not
/// `y + 1`). Checking `y - 1` in that case inspects a cell the stand does not
/// depend on at all, and this was two bugs at once, both only reachable with a
/// **partial** support, neither exercised by any fixture in this crate's own
/// tests (every one keeps its floor mid-column, `flat()`'s `-64..320`, so
/// `y - 1` was always in range and never a hazard):
///
/// - **False refusals near a snapshot's vertical edge.** A slab sitting at the
///   very bottom of a loaded region (a session's world floor, or — found by
///   `crates/plugins/lodestone-autopilot/tests/drives_to_goal.rs`'s real-jar-data
///   gate, which is what a synthetic fixture cannot reach — a `SnapshotView`
///   whose loaded column starts exactly at the slab) has no `y - 1` to answer
///   with: `facts_at` returns `None` there, and the old unconditional `?`
///   propagated that `None` out of `standable` for a cell that is otherwise
///   completely ordinary to stand on.
/// - **False refusals from an irrelevant hazard.** A slab (or soul sand,
///   farmland, a snow layer) fully seals the player from whatever is beneath
///   it; a hazard one cell further down — lava under a soul-sand floor, say —
///   must not veto standing on the block that seals it off, but the
///   unconditional check vetoed it anyway.
#[must_use]
pub fn standable(view: &dyn NavView, x: i32, y: i32, z: i32) -> Option<f64> {
    let surface = stand_surface(view, x, y, z)?;
    if !head_room(view, x, y, z, surface) {
        return None;
    }
    // Nothing the body passes through may be a hazard, and neither may the block
    // being stood on: a magma block is a full cube you can stand on and must not.
    let head = surface + BODY_HEIGHT;
    let mut cell = y;
    while f64::from(cell) < head - SURFACE_EPS {
        if view.facts_at(x, cell, z)?.must_not_enter {
            return None;
        }
        cell += 1;
    }
    // Only the "below" branch of `stand_surface` leaves the support block
    // unswept by the loop above — `surface == y` exactly identifies it, since
    // the "inside" branch always returns a surface strictly between `y` and
    // `y + 1` (see the doc comment above and `stand_surface`'s own two
    // branches).
    let support_is_below = (surface - f64::from(y)).abs() <= SURFACE_EPS;
    if support_is_below && view.facts_at(x, y - 1, z)?.must_not_enter {
        return None;
    }
    Some(surface)
}

/// A legal edge out of a node: where it goes, and the surface it lands on.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Step {
    /// The movement performed.
    pub kind: MoveKind,
    /// The node reached.
    pub to: NavNode,
    /// World-space feet `y` at the source.
    pub from_surface: f64,
    /// World-space feet `y` at the destination.
    pub to_surface: f64,
}

/// Every legal movement out of `from`, appended to `out` in a fixed order.
///
/// Order is [`Dir4::ALL`], which is vanilla's horizontal order, so expansion is
/// deterministic — a prerequisite for the byte-identical-plan gate.
///
/// # `Climb` broke the "must be standable to have successors" precondition
///
/// Every kind before `Climb` shares one physical shape: a body that can move at
/// all is, by construction, standing on something (`docs/baritone-port.md` §4.3
/// table). A mid-column climbable rung has no floor at all — the body is
/// clinging, not standing — so the single unconditional `standable` gate this
/// function used to open with would refuse to generate **any** successor from
/// there, `Climb` included, which would make a chain of more than one climb
/// edge unreachable by construction. The gate is now two independent
/// preconditions instead of one: the horizontal families still require
/// `standable`, and `Climb` requires only that the cell itself is climbable.
///
/// # Dismounting sideways while clinging, and why only `Walk` gets it
///
/// A real ladder almost never lets you climb straight into a standable
/// landing in its own column — the cell above (or below) a rung is, in the
/// overwhelmingly common case, either another rung or the wall the ladder is
/// mounted on, neither of which is a floor (`climb_step`'s own doc comment
/// works through why). The way vanilla actually gets **off** a ladder
/// mid-climb is a horizontal step while still clinging
/// (`docs/baritone-port.md` §2.3's climbable catalogue: pressing a direction
/// key moves you, it just does not release the grip until you leave the
/// column). So a climbable-but-not-standable `from` also tries [`walk_step`]
/// — and **only** that, deliberately: `StepUp`/`Descend`/`Drop`/`WalkDiagonal`
/// each carry an assumption (jump clearance, a fall's landing scan, the
/// corner-shoulder rule) verified against a *standing* body's physics, never
/// against a clinging one, and this pass has not checked whether any of them
/// still holds. A bot dismounts by a plain sideways step or stays on the
/// ladder; the rest is an open gap, recorded rather than silently guessed at.
/// The reference surface for that step's `STEP_HEIGHT` gate is `from.y` itself
/// — the same convention every node already uses for "where the feet are".
pub fn successors(view: &dyn NavView, from: NavNode, out: &mut Vec<Step>) {
    let standable_surface = standable(view, from.x, from.y, from.z);
    let climbing_here = view
        .facts_at(from.x, from.y, from.z)
        .is_some_and(|f| f.climbable && !f.must_not_enter);

    if let Some(from_surface) = standable_surface {
        for dir in Dir4::ALL {
            if let Some(step) = walk_step(view, from, from_surface, dir) {
                out.push(step);
                continue;
            }
            if let Some(step) = step_up_step(view, from, from_surface, dir) {
                out.push(step);
            }
            if let Some(step) = fall_step(view, from, from_surface, dir) {
                out.push(step);
            }
        }
        // Diagonals last, in the same clockwise pairing order vanilla's own mob
        // evaluator iterates them (`WalkNodeEvaluator.java:142-158`) — determinism
        // for the byte-identical-plan gate, same as the cardinal loop above.
        for d1 in Dir4::ALL {
            if let Some(step) = diagonal_step(view, from, from_surface, d1, d1.clockwise()) {
                out.push(step);
            }
        }
    } else if climbing_here {
        for dir in Dir4::ALL {
            if let Some(step) = walk_step(view, from, f64::from(from.y), dir) {
                out.push(step);
            }
        }
    }

    if climbing_here {
        for dir in [ClimbDir::Up, ClimbDir::Down] {
            if let Some(step) = climb_step(view, from, dir) {
                out.push(step);
            }
        }
    }
}

/// `Walk(dir)` out of `from`, or `None` when illegal.
///
/// # The destination **cell** is not always `from.y`
///
/// A `Walk` is defined by its *surface* delta — anything inside the 0.6 auto-step —
/// and by [`stand_surface`]'s convention the feet cell is `floor(feet y)`, which for
/// a partial block is the partial block's **own** cell. Those two facts together
/// mean a walk within the auto-step can still change the cell index: soul sand's top
/// is `0.875`, so a body on soul sand stands in the soul sand's cell, while a body on
/// the full block beside it stands in the cell *above* the block it rests on. Walking
/// between them is a 0.125 step and a one-cell change of `y`.
///
/// This function used to pin `ty = from.y`, which refused that step in **both**
/// directions — every soul sand, farmland, snow-layer and path column in the world
/// unreachable, and the `0.4` speed factor soul sand exists to impose therefore
/// unreachable too. It is *not* a `Descend`: no fall is paid for, the surfaces are
/// 0.125 apart, and the auto-step check below is still the only legality rule.
///
/// The candidate order (`from.y`, then below, then above) mirrors [`seed_node`]'s and
/// exists only for determinism: at most one candidate can pass the auto-step gate,
/// because two standable cells in one column are always more than `STEP_HEIGHT`
/// apart (a cell standable on its own partial block is filled from the perspective of
/// the cell below, and a cell standable on the block below requires that block to be
/// full height).
#[must_use]
pub fn walk_step(view: &dyn NavView, from: NavNode, from_surface: f64, dir: Dir4) -> Option<Step> {
    let (dx, dz) = dir.delta();
    let (tx, tz) = (from.x + dx, from.z + dz);
    for ty in [from.y, from.y - 1, from.y + 1] {
        let Some(to_surface) = standable(view, tx, ty, tz) else {
            continue;
        };
        // A step the 0.6 auto-step cannot make is not a `Walk`. `StepUp` (M2) is the
        // kind that pays for a jump; `Descend` (M2) is the one that pays for a fall.
        if (to_surface - from_surface).abs() > STEP_HEIGHT + SURFACE_EPS {
            continue;
        }
        return Some(Step {
            kind: MoveKind::Walk(dir),
            to: NavNode {
                x: tx,
                y: ty,
                z: tz,
                arrival: Arrival::Walking(dir),
            },
            from_surface,
            to_surface,
        });
    }
    None
}

/// `StepUp(dir)` out of `from`, or `None` when illegal.
///
/// Target cell is pinned to `from.y + 1` — an ascend, unlike [`walk_step`]'s
/// three-candidate search, only ever means *one* cell up; a taller ascend is
/// not a single `StepUp` at all (nothing in real Minecraft lets a body jump
/// more than [`jump_apex_height`] above where it started).
///
/// Legality is **geometric only** here: the surface delta must exceed
/// [`STEP_HEIGHT`] (otherwise [`walk_step`] already owns it) and not exceed
/// [`jump_apex_height`], and the source column needs the same head clearance up
/// to the destination's higher surface, since the body is briefly over the
/// source column at close to the destination's height while jumping. Whether
/// the jump *actually* covers the horizontal distance in that time is the
/// cost model's question, not this one — `cost::TemplateTable`'s simulated
/// `Template.ok` is the authority on physical feasibility, exactly as it
/// already is for `Walk` (`docs/baritone-port.md` §4.4): a static predicate
/// cannot see the obstruction-free stencil world the cost model measures
/// against, but it also cannot invent a distance the simulation cannot
/// achieve, so leaving the fine physics to simulation is correct here, not a
/// shortcut.
#[must_use]
pub fn step_up_step(view: &dyn NavView, from: NavNode, from_surface: f64, dir: Dir4) -> Option<Step> {
    let (dx, dz) = dir.delta();
    let (tx, tz) = (from.x + dx, from.z + dz);
    let ty = from.y + 1;
    let to_surface = standable(view, tx, ty, tz)?;
    let delta = to_surface - from_surface;
    if delta <= STEP_HEIGHT + SURFACE_EPS || delta > jump_apex_height() + SURFACE_EPS {
        return None;
    }
    // The source column needs clearance up to the *destination's* surface: the
    // body passes over the source cell at close to that height while airborne,
    // and `standable`'s own destination-local head-room check cannot see back
    // across the boundary.
    if !head_room(view, from.x, from.y, from.z, to_surface) {
        return None;
    }
    Some(Step {
        kind: MoveKind::StepUp(dir),
        to: NavNode {
            x: tx,
            y: ty,
            z: tz,
            arrival: Arrival::Walking(dir),
        },
        from_surface,
        to_surface,
    })
}

/// How many cells below the source a [`fall_step`] scan will look for the
/// first standable surface before giving up. Real free-fall has no such limit
/// — this only bounds how far the *graph* looks; the real, policy-driven
/// damage-vs-health cap on how far a `Drop` may legally land lives in
/// `search::Search::edge_cost` (`docs/baritone-port.md` §4.4), which sees
/// [`crate::policy::NavPolicy::max_fall_blocks`] and this function does not.
pub const FALL_SCAN_CELLS: i32 = 8;

/// `Descend(dir)`/`Drop(dir, n)` out of `from`, or `None` when there is no
/// standable surface within [`FALL_SCAN_CELLS`].
///
/// # Why `Descend` and `Drop` are one search, not two
///
/// A falling body stops at the **first** surface it reaches — it cannot pass
/// through a landing to reach a deeper one — so for a given direction there is
/// exactly one candidate, never a family of "try landing 2, 3, 4 cells down".
/// This scans downward in increasing `n` and returns the first standable cell,
/// classifying it as [`MoveKind::Descend`] when `n == 1` and
/// [`MoveKind::Drop`] otherwise. `n == 0`, i.e. a surface within the auto-step,
/// is [`walk_step`]'s job: reaching one here means the *first* landing was
/// close enough that this was never a fall to begin with, so the scan aborts
/// rather than reporting a movement `walk_step` already covers.
///
/// # Hazards in the fall path, not only at the landing
///
/// Lava, fire and the rest of [`crate::facts::MUST_NOT_ENTER`] are `passable`
/// — they do not stop a fall — so a body can pass *through* one on the way to
/// a perfectly good landing further down. Every cell scanned is checked for
/// `must_not_enter` before it is considered as a landing candidate, which
/// refuses the whole direction the moment one is found: routing a plan through
/// a column that burns the bot on the way down is exactly the "must not walk
/// into" rule `docs/baritone-port.md` §2.3 makes a first-class concept.
///
/// # Slabs are excluded as `Drop` landings
///
/// `docs/baritone-port.md` §2.3/§4.4: landing on a bottom slab from a genuine
/// fall is glitchy and deals *more* damage than the height predicts, so a
/// landing whose support is the "inside a partial block" case (rather than a
/// full-height block one cell below) is refused for `n > 1`. `Descend`
/// (`n == 1`) is exempt — its surfaces are the same ones `walk_step` already
/// stands on every day at zero fall distance, and refusing them would make an
/// ordinary one-block step down onto a slab illegal for no reason.
#[must_use]
pub fn fall_step(view: &dyn NavView, from: NavNode, from_surface: f64, dir: Dir4) -> Option<Step> {
    let (dx, dz) = dir.delta();
    let (tx, tz) = (from.x + dx, from.z + dz);
    for n in 1..=FALL_SCAN_CELLS {
        let ty = from.y - n;
        if ty < view.min_y() {
            return None;
        }
        let facts = view.facts_at(tx, ty, tz)?;
        if facts.must_not_enter {
            // The whole column below this point is a wall to us: a real fall
            // cannot skip over a hazard to reach a safe landing beneath it.
            return None;
        }
        if facts.climbable {
            // A falling body's feet crossing into a climbable cell does not
            // keep falling in real vanilla: `travel_in_air`'s climb branch
            // caps descent to `-0.15` b/t the instant `is_climbable` reads
            // true at the feet position, regardless of how the body got
            // there (`lodestone_physics::entity::travel_in_air`'s doc
            // comment — the clamp is unconditional on entry, not just while
            // deliberately climbing). `Descend`/`Drop`'s own cost simulation
            // (`cost::TemplateTable::simulate`'s `StencilWorld`) models plain
            // gravity with no climbable anywhere, so it would silently
            // disagree with real physics for exactly this column — the same
            // "achievability by construction" guarantee `Climb`'s own
            // template exists to give would be violated for a kind that
            // predates `Climb` entirely. Refuse the whole direction, the same
            // way a hazard already does: a real fall through here is not the
            // fall this cost model can price.
            return None;
        }
        let Some(to_surface) = standable(view, tx, ty, tz) else {
            continue;
        };
        let delta = from_surface - to_surface;
        if delta <= STEP_HEIGHT + SURFACE_EPS {
            // The nearest landing is within the auto-step: `walk_step` already
            // owns this case, and nothing further down is reachable — a body
            // stops at the first surface it meets.
            return None;
        }
        #[allow(clippy::cast_sign_loss)]
        let is_slab_like = (to_surface - f64::from(ty)).abs() > SURFACE_EPS;
        if n > 1 && is_slab_like {
            return None;
        }
        let kind = if n == 1 {
            MoveKind::Descend(dir)
        } else {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            MoveKind::Drop(dir, n as u8)
        };
        return Some(Step {
            kind,
            to: NavNode {
                x: tx,
                y: ty,
                z: tz,
                arrival: Arrival::Walking(dir),
            },
            from_surface,
            to_surface,
        });
    }
    None
}

/// `WalkDiagonal(d1, d2)` out of `from`, or `None` when illegal. `d2` must be
/// `d1.clockwise()` — the only pairing [`successors`] ever constructs.
///
/// # The corner-cutting rule, derived from real vanilla source
///
/// A diagonal move can be physically blocked even when its destination cell is
/// wide open: the player's `0.6`-wide body clips a solid corner unless there is
/// clearance on **both** of the two orthogonal cells that corner sits between.
/// That is a fact about the moving body's shape, not about who is moving it, so
/// the mob pathfinder's own discrete rule for exactly this — real Minecraft
/// source, not Baritone — is the right citation even though this crate does not
/// (and per `docs/baritone-port.md` §3.4, should not) extend that pathfinder:
///
/// `WalkNodeEvaluator.isDiagonalValid(pos, ew, ns)`
/// (`.cache/mc/26.2/src/net/minecraft/world/level/pathfinder/WalkNodeEvaluator.java:167-182`):
///
/// ```text
/// if (ns == null || ew == null || ns.y > pos.y || ew.y > pos.y) return false;
/// ...
/// return (ns.y < pos.y || ns.costMalus >= 0.0F || canPassBetweenPosts)
///     && (ew.y < pos.y || ew.costMalus >= 0.0F || canPassBetweenPosts);
/// ```
///
/// Translated: **both** shoulders must be a legally walkable neighbour (a real
/// node, not a hazard), and **neither may sit above the current cell** — even
/// one that would otherwise be a perfectly legal [`walk_step`] (stepping off a
/// soul-sand floor onto ordinary stone beside it is a legal `Walk` one cell
/// *up*, and real vanilla refuses it as a diagonal shoulder anyway, before it
/// ever looks at cost). This function reuses [`walk_step`]'s own hazard and
/// head-room checks for the "legally walkable" half — the direct analogue of
/// the evaluator reusing its cardinal neighbour nodes for `ns`/`ew` rather than
/// re-deriving them — and adds the `y <= from.y` gate on top.
///
/// One real permissiveness is **not** replicated: vanilla accepts a shoulder
/// that is strictly *lower* than `pos.y` regardless of its malus, i.e. even a
/// hazardous one, on the reasoning that a mob's body does not reach down into
/// a cell below its feet while merely clipping past its corner. This crate
/// stays conservative instead — every shoulder goes through the same
/// `must_not_enter` refusal a stand or a walk would — because a shoulder more
/// than a step below the body is already outside [`walk_step`]'s own domain
/// (a genuine descent, not a `Walk`) and this movement does not model
/// diagonal descent at all (see the module docs on why: a third cost-model
/// frame, left open rather than rushed).
///
/// # Same-height family only
///
/// Like [`Self::Walk`] this has no ascend/descend/jump component: the
/// destination search below mirrors [`walk_step`]'s exactly, gated by
/// [`STEP_HEIGHT`]. A diagonal step-up exists in vanilla's own mob evaluator
/// (via its `jumpSize`) but is deliberately out of scope here.
#[must_use]
pub fn diagonal_step(
    view: &dyn NavView,
    from: NavNode,
    from_surface: f64,
    d1: Dir4,
    d2: Dir4,
) -> Option<Step> {
    let shoulder1 = walk_step(view, from, from_surface, d1)?;
    let shoulder2 = walk_step(view, from, from_surface, d2)?;
    if shoulder1.to.y > from.y || shoulder2.to.y > from.y {
        return None;
    }

    let (dx1, dz1) = d1.delta();
    let (dx2, dz2) = d2.delta();
    let (tx, tz) = (from.x + dx1 + dx2, from.z + dz1 + dz2);
    for ty in [from.y, from.y - 1, from.y + 1] {
        let Some(to_surface) = standable(view, tx, ty, tz) else {
            continue;
        };
        if (to_surface - from_surface).abs() > STEP_HEIGHT + SURFACE_EPS {
            continue;
        }
        return Some(Step {
            kind: MoveKind::WalkDiagonal(d1, d2),
            to: NavNode {
                x: tx,
                y: ty,
                z: tz,
                // Approximation, recorded on `MoveKind::WalkDiagonal` and in
                // `docs/autonomous-navigation.md`: the exit arrival collapses
                // onto the first component rather than gaining a genuinely
                // diagonal `Arrival` variant. `NavNode::try_pack` spends
                // exactly 3 bits (0..=7) on `Arrival::index()` and only 3 are
                // free (5, 6, 7) — one short of the 4 a full diagonal arrival
                // set needs — and the other 61 bits are already exactly
                // spent covering the real world border (`±29,999,984`), so
                // widening the field is not a free edit.
                arrival: Arrival::Walking(d1),
            },
            from_surface,
            to_surface,
        });
    }
    None
}

/// `Climb(dir)` out of `from`, or `None` when illegal.
///
/// # Why this needs no support/facing check, unlike vanilla's own `canSurvive`
///
/// `LadderBlock.canSurvive` (`.cache/mc/26.2/src/net/minecraft/world/level/block/LadderBlock.java:52-55`)
/// requires a sturdy neighbour opposite the ladder's `FACING`, and `VineBlock`
/// has its own multi-face version. This crate's per-state census
/// (`crate::facts::BlockFacts`) carries no facing or per-face attachment data —
/// only tag membership (`climbable: bool`) — so re-deriving either check is not
/// possible from what is available, and it is also unnecessary: a placed block
/// that failed its own `canSurvive` would already have reverted to air
/// (`LadderBlock::updateShape`, `VineBlock::updateShape`) before this ever runs.
/// Trusting a persisted state's own tag membership is the same trust this
/// crate already places in every other placed block (a gravity-affected block
/// is never re-derived as "about to fall" either) — legality here is entirely
/// "is the cell climbable", nothing about *why*.
///
/// # Why the destination is (almost) never a same-column dismount for `Up`
///
/// A wall-mounted ladder's own column never has a floor directly above its
/// last rung: [`standable`]'s "below" branch needs a **full** block at
/// `y - 1`, and that cell is either another climbable rung or the ladder's own
/// backing wall (which is in an *adjacent* column, not this one) — never a
/// full block in *this* column, because a full block there would have refused
/// the ladder extending into it at all. So climbing up into a same-column
/// landing is legal here (the check below is real, not decorative) but is not
/// the way a real ladder is normally exited — see [`successors`]'s own doc
/// comment for how dismounting sideways works instead. `Down` is the opposite
/// case: a ladder's *base* commonly does have solid ground directly beneath
/// its lowest rung, which is the ordinary, common way a `Climb` chain ends.
#[must_use]
pub fn climb_step(view: &dyn NavView, from: NavNode, dir: ClimbDir) -> Option<Step> {
    let source = view.facts_at(from.x, from.y, from.z)?;
    if !source.climbable || source.must_not_enter {
        return None;
    }
    let ty = match dir {
        ClimbDir::Up => from.y + 1,
        ClimbDir::Down => from.y - 1,
    };
    if ty < view.min_y() || ty > view.max_y() {
        return None;
    }
    let dest = view.facts_at(from.x, ty, from.z)?;
    if dest.must_not_enter {
        return None;
    }
    let (arrival, to_surface) = if dest.climbable {
        // Continuing to climb: no real "surface" exists mid-column, so the
        // destination cell's own floor is the nominal reference height every
        // other bookkeeping convention in this crate already uses for "where
        // the feet are". `Arrival::Climbing`, not `Still` — see its own doc
        // comment for why the executor (not the cost model) needs the two
        // told apart.
        (Arrival::Climbing, f64::from(ty))
    } else if let Some(surface) = standable(view, from.x, ty, from.z) {
        (Arrival::Still, surface)
    } else {
        return None;
    };
    Some(Step {
        kind: MoveKind::Climb(dir),
        to: NavNode {
            x: from.x,
            y: ty,
            z: from.z,
            arrival,
        },
        from_surface: f64::from(from.y),
        to_surface,
    })
}

/// Where the player's feet cell is **for planning purposes**, from a real position,
/// or `None` when nothing under them is standable.
///
/// `docs/baritone-port.md` §2.3: this is not `floor(position)`. Soul sand's top is
/// `0.875` and a bottom slab's `0.5`, so the block whose cell your feet occupy is
/// not always the block you are standing on. The resolution here is to try the cell
/// `floor(y)` names first, then the cell below (the common case: feet at `y = 64.0`
/// floor to cell 64, whose support is 63), then the cell above (mid-step onto a
/// partial block), and to require the candidate's own surface to be within a block
/// of where the body actually is — so a candidate is only accepted if the body is
/// plausibly standing on *it*.
///
/// `None` is the honest answer while airborne over terrain the snapshot does not
/// cover, and the caller must treat it as "cannot plan yet" rather than guessing.
#[must_use]
pub fn seed_node(view: &dyn NavView, position: lodestone_physics::Vec3d) -> Option<NavNode> {
    #[allow(clippy::cast_possible_truncation)]
    let (x, z) = (position.x.floor() as i32, position.z.floor() as i32);
    #[allow(clippy::cast_possible_truncation)]
    let y = position.y.floor() as i32;
    for candidate in [y, y - 1, y + 1] {
        if let Some(surface) = standable(view, x, candidate, z)
            && (surface - position.y).abs() < 1.0
        {
            return Some(NavNode::still(x, candidate, z));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facts::{FactsTable, FixtureCensus};
    use crate::view::GridView;
    use std::sync::Arc;

    fn flat() -> GridView {
        let facts = Arc::new(FactsTable::build(&FixtureCensus));
        let mut view = GridView::new(facts, FixtureCensus::AIR, -64, 320, Some((-16, -16, 16, 16)));
        view.fill(-16, 0, -16, 16, 0, 16, FixtureCensus::STONE);
        view
    }

    /// The bijection §2.3 says a collision in silently corrupts a plan.
    #[test]
    fn node_packing_is_a_bijection_over_a_realistic_range() {
        let mut seen = std::collections::HashSet::new();
        let mut count = 0usize;
        for x in [
            -33_554_432,
            -1_000_000,
            -1,
            0,
            1,
            12_345,
            1_000_000,
            33_554_431,
        ] {
            for z in [-33_554_432, -777, 0, 1, 999_999, 33_554_431] {
                for y in [-64, -1, 0, 63, 64, 319, 447] {
                    for a in 0..6u8 {
                        let node = NavNode {
                            x,
                            y,
                            z,
                            arrival: Arrival::from_index(a).unwrap(),
                        };
                        let key = node.try_pack().expect("in range");
                        assert!(seen.insert(key), "collision at {node:?}");
                        assert_eq!(NavNode::unpack(key), Some(node));
                        count += 1;
                    }
                }
            }
        }
        assert_eq!(count, seen.len());
    }

    /// Out of range is refused, not clamped — the control proving the range check
    /// does anything.
    #[test]
    fn out_of_range_nodes_are_refused() {
        assert!(NavNode::still(33_554_432, 0, 0).try_pack().is_none());
        assert!(NavNode::still(-33_554_433, 0, 0).try_pack().is_none());
        assert!(NavNode::still(0, 448, 0).try_pack().is_none());
        assert!(NavNode::still(0, -65, 0).try_pack().is_none());
    }

    #[test]
    fn flat_ground_gives_four_walks() {
        let view = flat();
        let mut out = Vec::new();
        successors(&view, NavNode::still(0, 1, 0), &mut out);
        // 4 cardinal `Walk`s plus 4 `WalkDiagonal`s — open flat ground has no
        // corner to block any of the four diagonals either.
        assert_eq!(out.len(), 8, "{out:?}");
        assert_eq!(
            out.iter()
                .filter(|s| matches!(s.kind, MoveKind::Walk(_)))
                .count(),
            4
        );
        assert_eq!(
            out.iter()
                .filter(|s| matches!(s.kind, MoveKind::WalkDiagonal(_, _)))
                .count(),
            4
        );
        assert!(out.iter().all(|s| (s.to_surface - 1.0).abs() < 1e-9));
    }

    /// A bottom slab is a `Walk`, because 0.5 is under the 0.6 auto-step. This is
    /// the case that cannot be verified against any scene currently in the tree
    /// (`docs/baritone-port.md` §3.2's world-species trap), which is why the
    /// fixture is synthetic and explicit.
    #[test]
    fn a_bottom_slab_is_walkable_and_reports_the_slab_surface() {
        let mut view = flat();
        view.set(1, 1, 0, FixtureCensus::SLAB);
        let step = walk_step(&view, NavNode::still(0, 1, 0), 1.0, Dir4::East).expect("legal");
        assert_eq!(
            step.to,
            NavNode {
                x: 1,
                y: 1,
                z: 0,
                arrival: Arrival::Walking(Dir4::East)
            }
        );
        assert!((step.to_surface - 1.5).abs() < 1e-9, "{}", step.to_surface);
    }

    /// Soul sand's top is `0.875`, so a soul-sand *floor* presents a surface 0.125
    /// **below** the stone floor beside it: walking onto it is a step down of 0.125
    /// and walking off it a step up of 0.125, both inside the 0.6 auto-step. Both
    /// directions are asserted because getting the sign wrong here refuses half the
    /// terrain in the nether.
    ///
    /// # The fixture: the soul sand is the **floor**, not a block on top of it
    ///
    /// This test was originally written as `set(1, 1, 0, SOUL_SAND)` — soul sand
    /// placed in the *feet* cell, i.e. sitting on top of the stone floor — and then
    /// asserted a `to_surface` of `1.875`. That is a step **up of 0.875**, which the
    /// auto-step genuinely cannot make (it is M2's `StepUp`), so the fixture
    /// contradicted the paragraph above it: the 0.125 the comment reasons about only
    /// exists when the soul sand *replaces* the floor block. It is the world-species
    /// trap of `CLAUDE.md`'s evidence standards, one layer down — the assertion was
    /// fine and the input was the wrong scene.
    ///
    /// Note the **cell** the walk lands in: `y = 0`, the soul sand's own cell, not
    /// `y = 1`. That is [`stand_surface`]'s convention (the feet cell is
    /// `floor(feet y)`), and it is what `walk_step` used to be unable to express.
    #[test]
    fn soul_sand_is_walkable_in_both_directions() {
        let mut view = flat();
        view.set(1, 0, 0, FixtureCensus::SOUL_SAND);
        let onto = walk_step(&view, NavNode::still(0, 1, 0), 1.0, Dir4::East).expect("onto");
        assert!((onto.to_surface - 0.875).abs() < 1e-9, "{}", onto.to_surface);
        assert_eq!(
            (onto.to.x, onto.to.y, onto.to.z),
            (1, 0, 0),
            "the feet cell is the soul sand's own cell"
        );
        let off = walk_step(&view, NavNode::still(1, 0, 0), 0.875, Dir4::East).expect("off");
        assert!((off.to_surface - 1.0).abs() < 1e-9, "{}", off.to_surface);
        assert_eq!(
            (off.to.x, off.to.y, off.to.z),
            (2, 1, 0),
            "and stepping back off it returns to the cell above the floor"
        );
    }

    /// The control that proves the `y`-candidate search above did not simply make
    /// everything legal: soul sand *on top of* the floor is a step up of 0.875 and is
    /// still refused, in both directions. Without this, "soul sand is walkable" could
    /// be satisfied by a rule that accepts any neighbour with a standable cell
    /// anywhere in the column.
    #[test]
    fn soul_sand_stacked_on_the_floor_is_still_too_tall_to_walk_onto() {
        let mut view = flat();
        view.set(1, 1, 0, FixtureCensus::SOUL_SAND);
        assert!(
            walk_step(&view, NavNode::still(0, 1, 0), 1.0, Dir4::East).is_none(),
            "0.875 is above the 0.6 auto-step: that is M2's StepUp, not a Walk"
        );
        assert!(
            walk_step(&view, NavNode::still(1, 1, 0), 1.875, Dir4::East).is_none(),
            "and the drop back off it is 0.875 too"
        );
    }

    /// The negative control for the slab: a *full* block one up is not a `Walk`,
    /// because 1.0 exceeds the auto-step. Without this, "the slab was walkable"
    /// could be satisfied by a rule that accepts everything.
    #[test]
    fn a_full_block_step_up_is_not_a_walk() {
        let mut view = flat();
        view.set(1, 1, 0, FixtureCensus::STONE);
        assert!(walk_step(&view, NavNode::still(0, 1, 0), 1.0, Dir4::East).is_none());
    }

    /// A fence is `1.5` tall and the 0.6 auto-step cannot mount it. This is trap 1
    /// of `docs/baritone-port.md` §10, and the assertion that the uncapped `top`
    /// survived all the way from the census to the legality rule.
    #[test]
    fn a_fence_is_not_walkable_through_or_over() {
        let mut view = flat();
        view.set(1, 1, 0, FixtureCensus::FENCE);
        assert_eq!(view.facts_at(1, 1, 0).unwrap().top, 1.5);
        assert!(
            walk_step(&view, NavNode::still(0, 1, 0), 1.0, Dir4::East).is_none(),
            "a clamped collision_top would make this legal, and route paths \
             through pens"
        );
    }

    #[test]
    fn a_hazard_cell_is_refused_even_though_it_is_passable() {
        let mut view = flat();
        view.set(1, 1, 0, FixtureCensus::WATER);
        assert!(view.facts_at(1, 1, 0).unwrap().passable);
        assert!(walk_step(&view, NavNode::still(0, 1, 0), 1.0, Dir4::East).is_none());
    }

    /// A magma block is a full cube you can stand on and must not, so the hazard
    /// check has to look at the *support* too, not only the cells the body spans.
    #[test]
    fn a_hazardous_support_is_refused() {
        let mut view = flat();
        view.set(1, 0, 0, FixtureCensus::LAVA);
        assert!(walk_step(&view, NavNode::still(0, 1, 0), 1.0, Dir4::East).is_none());
    }

    #[test]
    fn no_head_room_is_refused() {
        let mut view = flat();
        view.set(1, 2, 0, FixtureCensus::STONE);
        assert!(walk_step(&view, NavNode::still(0, 1, 0), 1.0, Dir4::East).is_none());
    }

    /// A slab pushes the head into the cell two above, so the second head cell has
    /// to be checked — this is the case a one-cell head-room check gets wrong.
    #[test]
    fn standing_on_a_slab_needs_two_clear_cells_above() {
        let mut view = flat();
        view.set(1, 1, 0, FixtureCensus::SLAB);
        view.set(1, 3, 0, FixtureCensus::STONE);
        assert!(
            walk_step(&view, NavNode::still(0, 1, 0), 1.0, Dir4::East).is_none(),
            "feet at 1.5 put the head at 3.3, inside cell 3"
        );
    }

    /// Outside the snapshot terminates a branch. This is the mechanism, not a
    /// penalty.
    #[test]
    fn unknown_cells_terminate_expansion() {
        let view = flat();
        let mut out = Vec::new();
        // (16, 1, 0) is the last in-bounds column, so its `+X` neighbour is
        // outside and must not be proposed. `North`, `South` and `West` are
        // the three legal cardinal walks; the two diagonals that do **not**
        // involve `East` as a shoulder (`West+North`, `South+West`) are also
        // legal, since West decreases x and never approaches the boundary —
        // the two that do (`North+East`, `East+South`) are refused by
        // `diagonal_step`'s own shoulder check, exactly as `East` alone is.
        successors(&view, NavNode::still(16, 1, 0), &mut out);
        assert_eq!(out.len(), 5, "{out:?}");
        assert!(!out.iter().any(|s| s.to.x == 17));
        assert!(
            !out.iter()
                .any(|s| matches!(s.kind, MoveKind::WalkDiagonal(d1, d2) if d1 == Dir4::North && d2 == Dir4::East)),
            "a diagonal through the East shoulder must not reach past the boundary"
        );
    }

    #[test]
    fn seeding_from_a_real_position_finds_the_supporting_cell() {
        let view = flat();
        let node = seed_node(&view, lodestone_physics::Vec3d::new(0.5, 1.0, 0.5)).expect("seed");
        assert_eq!((node.x, node.y, node.z), (0, 1, 0));
    }

    #[test]
    fn seeding_on_a_slab_finds_the_cell_the_slab_is_in() {
        let mut view = flat();
        view.set(0, 1, 0, FixtureCensus::SLAB);
        let node = seed_node(&view, lodestone_physics::Vec3d::new(0.5, 1.5, 0.5)).expect("seed");
        assert_eq!((node.x, node.y, node.z), (0, 1, 0));
    }

    /// The honest answer while there is nothing under the body.
    #[test]
    fn seeding_over_a_void_returns_none_rather_than_guessing() {
        let facts = Arc::new(FactsTable::build(&FixtureCensus));
        let view = GridView::new(facts, FixtureCensus::AIR, -64, 320, Some((-4, -4, 4, 4)));
        assert!(seed_node(&view, lodestone_physics::Vec3d::new(0.5, 100.0, 0.5)).is_none());
    }

    #[test]
    fn turns_to_is_a_quarter_turn_count() {
        assert_eq!(Dir4::East.turns_to(Dir4::East), 0);
        assert_eq!(Dir4::East.turns_to(Dir4::South), 1);
        assert_eq!(Dir4::East.turns_to(Dir4::West), 2);
        assert_eq!(Dir4::East.turns_to(Dir4::North), 3);
    }

    // --- M2: StepUp ---

    /// The number `docs/baritone-port.md` §4.3 cites (~1.2522 blocks), reproduced
    /// here by the same means (simulating a jump) rather than trusting the design
    /// doc's transcription of it.
    #[test]
    fn jump_apex_matches_the_design_docs_derived_figure() {
        let apex = jump_apex_height();
        assert!(
            (apex - 1.2522).abs() < 0.01,
            "apex {apex}, design doc says ~1.2522"
        );
        assert!(apex > STEP_HEIGHT, "or nothing could ever StepUp at all");
    }

    /// The classic one-block ascend: a block one cell taller than the floor
    /// beside it. This is the case no scene in the tree could exercise before
    /// `StepUp` existed at all.
    #[test]
    fn a_one_block_ascend_is_a_legal_step_up() {
        let mut view = flat();
        view.set(1, 1, 0, FixtureCensus::STONE);
        let step = step_up_step(&view, NavNode::still(0, 1, 0), 1.0, Dir4::East).expect("legal");
        assert_eq!(step.kind, MoveKind::StepUp(Dir4::East));
        assert_eq!((step.to.x, step.to.y, step.to.z), (1, 2, 0));
        assert!((step.to_surface - 2.0).abs() < 1e-9, "{}", step.to_surface);
    }

    /// The negative control: a floating partial block placed a full cell higher
    /// than an ordinary one-block ascend can reach — 1.875 blocks up, comfortably
    /// past [`jump_apex_height`] (~1.2522) — must be refused. Without this,
    /// "a one-block ascend is legal" could be satisfied by a rule that accepts
    /// any upward neighbour at all.
    #[test]
    fn an_ascend_taller_than_the_jump_apex_is_refused() {
        let mut view = flat();
        // Soul sand's 0.875 top, floating at y=2 rather than resting on a floor
        // at y=1 — surface 2.875 against a source surface of 1.0.
        view.set(1, 2, 0, FixtureCensus::SOUL_SAND);
        assert!(
            step_up_step(&view, NavNode::still(0, 1, 0), 1.0, Dir4::East).is_none(),
            "1.875 blocks exceeds the jump apex; a jump cannot reach it"
        );
    }

    /// `StepUp` and `walk_step` must not both claim the same one-block ascend —
    /// `walk_step` already refuses it (the auto-step is 0.6, not 1.0), and this
    /// pins that the two legality rules stay partitioned rather than overlapping.
    #[test]
    fn a_one_block_ascend_is_not_also_a_walk() {
        let mut view = flat();
        view.set(1, 1, 0, FixtureCensus::STONE);
        assert!(walk_step(&view, NavNode::still(0, 1, 0), 1.0, Dir4::East).is_none());
    }

    // --- M2: Descend/Drop (`fall_step`) ---

    /// A one-cell drop onto solid ground is `Descend`, never damaging (the
    /// resulting delta is always under `SAFE_FALL_DISTANCE`).
    #[test]
    fn a_one_cell_drop_onto_solid_ground_is_a_legal_descend() {
        let mut view = flat();
        view.set(1, 0, 0, FixtureCensus::AIR);
        view.set(1, -1, 0, FixtureCensus::STONE);
        let step = fall_step(&view, NavNode::still(0, 1, 0), 1.0, Dir4::East).expect("legal");
        assert_eq!(step.kind, MoveKind::Descend(Dir4::East));
        assert_eq!((step.to.x, step.to.y, step.to.z), (1, 0, 0));
        assert!((step.to_surface - 0.0).abs() < 1e-9, "{}", step.to_surface);
    }

    /// A two-cell drop is named `Drop`, carrying `n`, and lands where the real
    /// first-standable-surface scan says it must.
    #[test]
    fn a_two_cell_drop_onto_solid_ground_is_a_legal_drop_of_two() {
        let mut view = flat();
        view.set(1, 0, 0, FixtureCensus::AIR);
        view.set(1, -1, 0, FixtureCensus::AIR);
        view.set(1, -2, 0, FixtureCensus::STONE);
        let step = fall_step(&view, NavNode::still(0, 1, 0), 1.0, Dir4::East).expect("legal");
        assert_eq!(step.kind, MoveKind::Drop(Dir4::East, 2));
        assert_eq!((step.to.x, step.to.y, step.to.z), (1, -1, 0));
        assert!((step.to_surface - -1.0).abs() < 1e-9, "{}", step.to_surface);
    }

    /// The unreachable control this repo's evidence standards ask for: a real
    /// hazard sitting *in the fall path*, with genuine solid ground beneath it,
    /// must refuse the whole direction rather than "falling past" the lava to
    /// land safely — a passable hazard does not stop a fall physically, but
    /// routing a plan through it is exactly what
    /// `docs/baritone-port.md` §2.3's "must not walk into" rule forbids.
    #[test]
    fn a_fall_through_lava_is_refused_even_though_solid_ground_is_further_down() {
        let mut view = flat();
        view.set(1, 0, 0, FixtureCensus::AIR);
        view.set(1, -1, 0, FixtureCensus::LAVA);
        view.set(1, -2, 0, FixtureCensus::STONE);
        assert!(
            fall_step(&view, NavNode::still(0, 1, 0), 1.0, Dir4::East).is_none(),
            "solid ground two cells down must not make a lava-filled path legal"
        );
    }

    /// The control that proves the scan actually looks — an unbroken column of
    /// air all the way past [`FALL_SCAN_CELLS`] must fail closed, not silently
    /// invent a landing at the scan's edge.
    #[test]
    fn a_bottomless_column_past_the_scan_limit_has_no_legal_fall() {
        let mut view = flat();
        for n in 1..=(FALL_SCAN_CELLS + 2) {
            view.set(1, 1 - n, 0, FixtureCensus::AIR);
        }
        assert!(fall_step(&view, NavNode::still(0, 1, 0), 1.0, Dir4::East).is_none());
    }

    /// The negative control for [`fall_step`]'s slab exclusion: a `Drop` of more
    /// than one cell landing on a bottom slab — an "inside a partial block"
    /// support rather than a full block one cell below — must be refused, even
    /// though the geometry is otherwise a perfectly ordinary landing. Without
    /// this, "Drop lands correctly" could be satisfied by a rule that accepts
    /// every standable surface indiscriminately.
    #[test]
    fn dropping_two_cells_onto_a_bottom_slab_is_refused_as_a_landing() {
        let mut view = flat();
        view.set(1, 0, 0, FixtureCensus::AIR);
        view.set(1, -1, 0, FixtureCensus::SLAB);
        assert!(
            fall_step(&view, NavNode::still(0, 1, 0), 1.0, Dir4::East).is_none(),
            "a slab landing must be excluded for a genuine multi-cell drop"
        );
    }

    /// `Descend` (`n == 1`) is exempt from the slab exclusion — refusing it would
    /// make an everyday one-block step down onto a slab illegal for no physical
    /// reason. This needs an elevated source surface to produce a delta past
    /// `STEP_HEIGHT` at all (see the module docs on why `n == 1` onto an
    /// ordinary-height slab is just a `Walk`): standing on a slab one cell up
    /// (surface `1.5`) and stepping down onto a bare stone floor (surface `0.0`)
    /// is a `0.6`-plus drop that must land as `Descend`, not be refused.
    #[test]
    fn descending_from_a_slab_is_exempt_from_the_landing_exclusion() {
        let mut view = flat();
        view.set(0, 1, 0, FixtureCensus::SLAB);
        // A slab one cell down too, so the landing itself is the "inside a
        // partial block" case `fall_step`'s slab exclusion targets — but at
        // `n == 1`, which the exclusion does not apply to.
        view.set(1, 0, 0, FixtureCensus::SLAB);
        let from = NavNode::still(0, 1, 0);
        let from_surface = standable(&view, from.x, from.y, from.z).expect("standing on the slab");
        assert!((from_surface - 1.5).abs() < 1e-9, "{from_surface}");
        let step = fall_step(&view, from, from_surface, Dir4::East).expect("legal");
        assert_eq!(step.kind, MoveKind::Descend(Dir4::East));
        assert!((step.to_surface - 0.5).abs() < 1e-9, "{}", step.to_surface);
    }

    /// [`MoveKind::stencil`] for the new kinds actually builds distinct,
    /// non-empty slices per direction — the control that the lazily-built
    /// stencils are wired at all, not silently sharing one array across every
    /// direction.
    #[test]
    fn m2_stencils_are_built_per_direction_and_are_not_empty() {
        for kind_of in [
            (|d| MoveKind::StepUp(d)) as fn(Dir4) -> MoveKind,
            |d| MoveKind::Descend(d),
            |d| MoveKind::Drop(d, 2),
        ] {
            let east = kind_of(Dir4::East).stencil();
            let north = kind_of(Dir4::North).stencil();
            assert!(!east.is_empty());
            assert_ne!(east, north, "each direction must translate, not repeat +X");
        }
    }

    // --- M2: WalkDiagonal ---

    #[test]
    fn clockwise_cycles_through_all_four() {
        assert_eq!(Dir4::North.clockwise(), Dir4::East);
        assert_eq!(Dir4::East.clockwise(), Dir4::South);
        assert_eq!(Dir4::South.clockwise(), Dir4::West);
        assert_eq!(Dir4::West.clockwise(), Dir4::North);
    }

    /// The plain case: open flat ground, both shoulders and the destination
    /// clear.
    #[test]
    fn a_diagonal_over_open_flat_ground_is_legal() {
        let view = flat();
        let step = diagonal_step(&view, NavNode::still(0, 1, 0), 1.0, Dir4::North, Dir4::East)
            .expect("legal");
        assert_eq!(step.kind, MoveKind::WalkDiagonal(Dir4::North, Dir4::East));
        assert_eq!((step.to.x, step.to.y, step.to.z), (1, 1, -1));
        assert!((step.to_surface - 1.0).abs() < 1e-9);
    }

    /// The corner-cutting rule, from real vanilla source
    /// (`WalkNodeEvaluator.isDiagonalValid`,
    /// `.cache/mc/26.2/src/net/minecraft/world/level/pathfinder/WalkNodeEvaluator.java:167-182`):
    /// a diagonal requires **both** orthogonal shoulders to be legally
    /// walkable. A single-block wall in just the East shoulder — with the
    /// North shoulder and the destination both left open — must refuse the
    /// whole diagonal, matching `ew.costMalus >= 0.0F`'s requirement on `ew`
    /// alone being insufficient to save it.
    #[test]
    fn a_diagonal_across_a_blocked_corner_is_refused() {
        let mut view = flat();
        view.set(1, 1, 0, FixtureCensus::STONE);
        assert!(
            diagonal_step(&view, NavNode::still(0, 1, 0), 1.0, Dir4::North, Dir4::East).is_none(),
            "one blocked shoulder must refuse the whole diagonal"
        );
    }

    /// The other shoulder, alone, is just as disqualifying — without this the
    /// previous test could pass by coincidence if the check happened to look
    /// at only the East shoulder specifically.
    #[test]
    fn the_other_blocked_shoulder_refuses_it_too() {
        let mut view = flat();
        view.set(0, 1, -1, FixtureCensus::STONE);
        assert!(
            diagonal_step(&view, NavNode::still(0, 1, 0), 1.0, Dir4::North, Dir4::East).is_none()
        );
    }

    /// Both shoulders blocked — the classic "wedged in a corner" case
    /// `docs/baritone-port.md` §2.3 names — is refused too.
    #[test]
    fn both_shoulders_blocked_is_refused() {
        let mut view = flat();
        view.set(1, 1, 0, FixtureCensus::STONE);
        view.set(0, 1, -1, FixtureCensus::STONE);
        assert!(
            diagonal_step(&view, NavNode::still(0, 1, 0), 1.0, Dir4::North, Dir4::East).is_none()
        );
    }

    /// A blocked destination with both shoulders clear is refused too —
    /// otherwise "shoulders clear" could be satisfied by a rule that never
    /// checks the target cell at all.
    #[test]
    fn a_blocked_destination_with_clear_shoulders_is_still_refused() {
        let mut view = flat();
        view.set(1, 1, -1, FixtureCensus::STONE);
        assert!(
            diagonal_step(&view, NavNode::still(0, 1, 0), 1.0, Dir4::North, Dir4::East).is_none()
        );
    }

    /// Real vanilla refuses a shoulder that sits **above** the current cell
    /// even when it is otherwise a perfectly legal `Walk` — stepping off a
    /// soul-sand floor (surface `0.875`) onto ordinary stone beside it
    /// (surface `1.0`) is a legal one-cell-up `Walk`, and
    /// `WalkNodeEvaluator.isDiagonalValid`'s `ns.y > pos.y` check refuses it
    /// as a diagonal shoulder anyway, before it ever looks at cost. Without
    /// this the corner-cutting rule could be satisfied by a check that only
    /// ever asked "is this shoulder walkable", never "is it also not higher".
    #[test]
    fn a_shoulder_that_is_a_legal_walk_but_one_cell_higher_still_refuses_the_diagonal() {
        let mut view = flat();
        // Standing *on* soul sand (surface `0.875`, feet cell `y = 0`) and
        // stepping onto plain stone beside it is a legal `Walk` that lands
        // one cell **up** (`to.y = from.y + 1`) — the exact case
        // `soul_sand_is_walkable_in_both_directions` already proves is a
        // legal `walk_step`. Both the North and East shoulders here are that
        // same "legal walk, one cell up" case, and the destination (further
        // stone, also `0.125` above the soul sand) is independently legal —
        // so if the diagonal accepted it, the only thing that could have
        // stopped it is the `y <= from.y` gate this test exists to prove.
        view.set(1, 0, 0, FixtureCensus::SOUL_SAND);
        let from = NavNode::still(1, 0, 0);
        let from_surface = standable(&view, from.x, from.y, from.z).expect("standing on the soul sand");
        assert!((from_surface - 0.875).abs() < 1e-9, "{from_surface}");

        let shoulder = walk_step(&view, from, from_surface, Dir4::East).expect("a legal walk");
        assert_eq!(shoulder.to.y, from.y + 1, "one cell up, exactly the case this test needs");

        assert!(
            diagonal_step(&view, from, from_surface, Dir4::North, Dir4::East).is_none(),
            "a shoulder that walk_step accepts but that sits above from.y must still refuse \
             the diagonal, matching WalkNodeEvaluator's own ns.y > pos.y / ew.y > pos.y check"
        );
    }

    #[test]
    fn diagonal_stencils_are_built_per_direction_and_are_not_empty() {
        let ne = MoveKind::WalkDiagonal(Dir4::North, Dir4::East).stencil();
        let se = MoveKind::WalkDiagonal(Dir4::East, Dir4::South).stencil();
        assert!(!ne.is_empty());
        assert_ne!(ne, se, "each pairing must translate, not repeat one direction");
    }

    /// The diagonal stencil covers all four columns its own legality check
    /// reads: source, both shoulders, and the destination — the witness set
    /// (`docs/baritone-port.md` §4.5) has to see every cell a block update
    /// could invalidate this edge through.
    #[test]
    fn diagonal_stencil_covers_both_shoulders_and_the_destination() {
        let stencil = MoveKind::WalkDiagonal(Dir4::North, Dir4::East).stencil();
        for cell in [[0, 0, 0], [1, 0, 0], [0, 0, -1], [1, 0, -1]] {
            assert!(
                stencil.contains(&cell),
                "{cell:?} missing from the diagonal stencil"
            );
        }
    }

    // --- `Climb` ---

    /// A ladder starting **at** the floor: rungs at `y = 1..=3` in column
    /// `(0, *, 0)`, on the same stone floor `flat()` already gives every
    /// other column. A platform at `(1, 3, 0)` (support at `(1, 2, 0)`) gives
    /// the top rung somewhere to dismount sideways onto, at exactly its own
    /// height — this is the common, "starts at the floor" case, where the
    /// bottom rung is already both climbable and standable at once, so it
    /// never needs the "climb down onto solid ground below the lowest rung"
    /// branch at all (see [`floating_ladder`] for the fixture that does).
    fn ladder_from_floor() -> GridView {
        let mut view = flat();
        view.set(0, 1, 0, FixtureCensus::LADDER);
        view.set(0, 2, 0, FixtureCensus::LADDER);
        view.set(0, 3, 0, FixtureCensus::LADDER);
        view.set(1, 2, 0, FixtureCensus::STONE);
        view
    }

    /// A ladder that does **not** reach the floor: rungs at `y = 2..=4` in
    /// column `(0, *, 0)`, with one cell of open air (`y = 1`, standable off
    /// the real stone floor at `y = 0`) between the bottom rung and the
    /// ground. A real, legal placement — `canSurvive` only needs a sturdy
    /// block behind a rung, never a floor under it — and the fixture that
    /// makes "climb down onto solid ground below the lowest rung" a genuine
    /// case rather than a degenerate one. A platform at `(1, 4, 0)` (support
    /// at `(1, 3, 0)`) gives the fall-through-a-climbable-column control
    /// somewhere real to stand while it falls.
    fn floating_ladder() -> GridView {
        let mut view = flat();
        view.set(0, 2, 0, FixtureCensus::LADDER);
        view.set(0, 3, 0, FixtureCensus::LADDER);
        view.set(0, 4, 0, FixtureCensus::LADDER);
        view.set(1, 3, 0, FixtureCensus::STONE);
        view
    }

    /// Mounting is an ordinary `Walk`, not a `Climb` — you approach a
    /// climbable column horizontally like any other cell, and
    /// `graph::stand_surface`'s climbable fix is what makes the bottom rung
    /// read as ordinary ground (support at `y - 1`) rather than a fake
    /// partial-block surface at the ladder's own shape height.
    #[test]
    fn mounting_a_ladder_is_an_ordinary_walk_not_a_climb() {
        let view = ladder_from_floor();
        let step = walk_step(&view, NavNode::still(-1, 1, 0), 1.0, Dir4::East).expect("legal");
        assert_eq!(step.kind, MoveKind::Walk(Dir4::East));
        assert!((step.to_surface - 1.0).abs() < 1e-9, "{}", step.to_surface);
    }

    /// Continuing to climb: both the source and destination rungs are
    /// climbable, so the exit is `Arrival::Climbing` — never grounded, at the
    /// destination cell's own nominal height.
    #[test]
    fn climbing_up_between_two_climbable_rungs_continues() {
        let view = ladder_from_floor();
        let step = climb_step(&view, NavNode::still(0, 1, 0), ClimbDir::Up).expect("legal");
        assert_eq!(step.kind, MoveKind::Climb(ClimbDir::Up));
        assert_eq!(
            step.to,
            NavNode {
                x: 0,
                y: 2,
                z: 0,
                arrival: Arrival::Climbing
            }
        );
        assert!((step.to_surface - 2.0).abs() < 1e-9);
    }

    /// Climbing down off the bottom rung of a ladder that stops one cell
    /// short of the floor lands on the real floor below it — the genuine
    /// "dismount onto solid ground" case, distinct from merely continuing to
    /// another rung.
    #[test]
    fn climbing_down_off_a_floating_bottom_rung_dismounts_onto_the_floor() {
        let view = floating_ladder();
        let step = climb_step(&view, NavNode::still(0, 2, 0), ClimbDir::Down).expect("legal");
        assert_eq!(step.kind, MoveKind::Climb(ClimbDir::Down));
        assert_eq!(
            step.to,
            NavNode {
                x: 0,
                y: 1,
                z: 0,
                arrival: Arrival::Still
            }
        );
        assert!(
            (step.to_surface - 1.0).abs() < 1e-9,
            "the real floor's surface, not the ladder's own shape height"
        );
    }

    /// Dismounting sideways while clinging: from the top rung — which is
    /// climbable but **not** standable (nothing full sits below it, only
    /// another rung) — a plain `Walk` onto the adjacent platform is still
    /// offered. This is the mechanism `successors`'s own doc comment says is
    /// how a real wall-mounted ladder is actually exited, since a same-column
    /// ascend almost never lands on anything.
    #[test]
    fn dismounting_sideways_from_the_top_rung_is_a_plain_walk() {
        let view = ladder_from_floor();
        assert!(
            standable(&view, 0, 3, 0).is_none(),
            "the top rung must not be standable, or this test is not exercising the fallback"
        );
        let mut out = Vec::new();
        successors(&view, NavNode::still(0, 3, 0), &mut out);
        let walk = out
            .iter()
            .find(|s| matches!(s.kind, MoveKind::Walk(Dir4::East)))
            .expect("a sideways dismount onto the platform");
        assert_eq!(
            walk.to,
            NavNode::still(1, 3, 0).with_arrival(Arrival::Walking(Dir4::East))
        );
        assert!((walk.to_surface - 3.0).abs() < 1e-9);
    }

    /// The unreachable control this pass's brief asks for: climbing past the
    /// top rung into open air with nothing to land on is refused, not
    /// invented. Watched to fail by construction — `standable` genuinely
    /// returns `None` there (nothing full sits at `y = 3`, only the
    /// non-full top rung).
    #[test]
    fn climbing_past_the_top_of_the_ladder_into_nothing_is_refused() {
        let view = ladder_from_floor();
        assert!(climb_step(&view, NavNode::still(0, 3, 0), ClimbDir::Up).is_none());
    }

    /// A second, physically distinct unreachable control: a solid ceiling
    /// directly above the top rung refuses the climb outright — not because
    /// there is nothing to land on, but because the destination cell itself
    /// cannot hold a body at all. `standable`'s "filled" branch is what
    /// produces the refusal, the same rule that already refuses walking into
    /// a wall.
    #[test]
    fn climbing_into_a_solid_ceiling_is_refused() {
        let mut view = ladder_from_floor();
        view.set(0, 4, 0, FixtureCensus::STONE);
        assert!(climb_step(&view, NavNode::still(0, 3, 0), ClimbDir::Up).is_none());
    }

    /// The most basic unreachable control: no climbable block, no `Climb`
    /// edge at all, in either direction — climbing an ordinary column of air
    /// over stone is refused by construction, never invented.
    #[test]
    fn a_column_with_no_climbable_block_has_no_climb_step() {
        let view = flat();
        let from = NavNode::still(0, 1, 0);
        assert!(climb_step(&view, from, ClimbDir::Up).is_none());
        assert!(climb_step(&view, from, ClimbDir::Down).is_none());
        let mut out = Vec::new();
        successors(&view, from, &mut out);
        assert!(
            !out.iter().any(|s| matches!(s.kind, MoveKind::Climb(_))),
            "{out:?}"
        );
    }

    /// A full climb chain from `successors` alone, mount to dismount: the
    /// search-facing surface this whole feature exists to serve.
    #[test]
    fn successors_offers_a_climb_up_from_every_rung_but_the_top() {
        let view = ladder_from_floor();
        for y in 1..=2 {
            let mut out = Vec::new();
            successors(&view, NavNode::still(0, y, 0), &mut out);
            assert!(
                out.iter()
                    .any(|s| matches!(s.kind, MoveKind::Climb(ClimbDir::Up))),
                "rung at y={y} should offer Climb(Up): {out:?}"
            );
        }
    }

    /// `fall_step` must refuse a direction whose landing scan passes through
    /// a climbable cell — real physics arrests a fall there
    /// (`travel_in_air`'s climb branch reads the feet position unconditionally,
    /// not only while deliberately climbing), so `Descend`/`Drop`'s own
    /// gravity-only simulation would silently disagree with reality for this
    /// column. This is the one change `Climb` forced onto a kind that
    /// predates it.
    #[test]
    fn a_fall_through_a_climbable_column_is_refused() {
        let view = floating_ladder();
        // From the platform at `(1, 4, 0)`, falling `West` drops straight
        // through the ladder's own column at `(0, *, 0)`.
        assert!(fall_step(&view, NavNode::still(1, 4, 0), 4.0, Dir4::West).is_none());
    }
}
