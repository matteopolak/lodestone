//! Fluid spread — water and lava flowing, on the scheduled-tick queue.
//!
//! # What this is
//!
//! A port of the real flowing-fluid engine and its water/lava specializations,
//! plus the two pieces of the real liquid block that
//! drive them (the should-spread-liquid check, and the place/neighbor-changed
//! scheduling), read out of the pinned 26.2 decompile as record definitions.
//!
//! Before this landed, nothing in this crate ticked a fluid at all. The
//! *classification* side was well covered — [`crate::chunk::is_water`],
//! [`crate::random_tick::has_full_fluid`] — so a placed water source was
//! correctly recognised as water and then sat there as a single cube forever.
//! `crate::tick::run_tick_loop`'s `fluid_ticks.drain_due` loop had an empty body
//! with a comment saying so.
//!
//! # The algorithm, in the order the real engine evaluates it
//!
//! [`run_scheduled_tick`] is the real flowing-fluid tick:
//!
//! 1. if the cell is **not** a source, recompute what it *should* be with
//!    [`new_liquid`] (the real "new liquid" derivation) and rewrite it — to air if the
//!    answer is empty, otherwise to the new level plus a fresh scheduled tick;
//! 2. then [`spread`] (the real spread step) — try **down** first, and only fall back to
//!    the four horizontal directions when down is blocked (or when this is a
//!    source, or when the cell below is not a hole);
//! 3. the real spread-to-sides step consults [`spread_targets`] (the real "get spread"
//!    derivation), which is the slope-finding: every horizontal direction is
//!    scored by [`slope_distance`] (the real slope-distance query) — how many
//!    steps until a hole — and only the **joint minimum** directions receive
//!    fluid. That is why water on flat ground spreads outward evenly but water
//!    beside a one-block pit flows *only* toward the pit.
//!
//! Nothing here draws from an RNG, and that is a property of the algorithm
//! rather than of this port: fluid spread in the real engine is fully deterministic.
//! There is exactly one RNG consumer in the whole family and it is a *delay*,
//! not a decision — see "the named gaps" below.
//!
//! # The numbers, which are per-fluid and per-dimension
//!
//! | | drop-off | slope-find distance | tick delay |
//! |---|---|---|---|
//! | water | 1 | 4 | 5 |
//! | lava, overworld | 2 | 2 | 30 |
//! | lava, nether (`fast_lava`) | 1 | 4 | 10 |
//!
//! The drop-off is what fixes the reach: a source has `amount = 8`, each step
//! horizontally costs the drop-off, and an amount of `0` is empty. So **water
//! reaches 7 cells** from a source on flat ground (amounts 7,6,5,4,3,2,1) and
//! **overworld lava reaches 3** (amounts 6,4,2). Those two counts are the whole
//! visible signature of this module and `flat_ground_water_spread_matches_vanilla_drop_off`
//! predicts them from the table above rather than from our own output.
//!
//! `fast_lava` is the real fast-lava dimension attribute — not
//! a gamerule and not a difficulty. This crate hosts the overworld only
//! ([`FluidEnv::OVERWORLD`]); [`FluidEnv::NETHER`] exists so the arithmetic is
//! written down once rather than rediscovered when a second dimension lands.
//!
//! # The level encoding, which is the easiest thing here to get backwards
//!
//! The *block* carries `level` in `0..=15`; the *fluid* carries `amount` in
//! `1..=8` plus a `falling` flag. The real legacy-level derivation and the real
//! liquid block's own state cache (read back by its fluid-state query) are the
//! two halves of the mapping:
//!
//! | block `level` | fluid |
//! |---|---|
//! | `0` (also a bare `minecraft:water`) | source, `amount = 8` |
//! | `1..=7` | flowing, `amount = 8 - level` |
//! | `8..=15` | **falling**, `amount = 8` |
//!
//! So `level` counts *down* from a full source, and `level=1` is the wettest
//! flowing state rather than the driest. The real fluid-state query clamps
//! `level` to `8`, which is why `9..=15` are all the same falling state.
//!
//! # The named gaps
//!
//! Each of these is a deliberate reduction, not an oversight, and each is chosen
//! so the error direction is inert rather than plausible-looking:
//!
//! * **The real lava spread-delay's RNG quadrupling**
//!   is not modelled. It multiplies the delay by 4 with probability 3/4 when a
//!   *non-falling* lava cell's height **rises**, and this crate's fluid tick has
//!   no RNG in scope (the tick loop's lives inside `RandomTickScheduler`). It
//!   affects lava's timing while deepening and never the final pattern, so the
//!   consequence is lava that settles slightly faster than the real engine, not lava
//!   that settles somewhere else.
//! * **The real pre-destroy hook** is a plain overwrite here. The real engine
//!   drops the destroyed block's loot for water and plays a
//!   fizz level-event for lava. We destroy the
//!   block correctly and emit neither the drop nor the sound.
//! * **The real should-spread-liquid check runs at tick time, not at edit time.**
//!   The real engine calls
//!   it from the liquid block's place/neighbor-changed hooks; [`run_scheduled_tick`]
//!   evaluates it as its own first step instead. Same outcome, one scheduled-tick
//!   delay later, and it keeps the whole family reachable through one entry
//!   point.
//! * **Bubble columns** are not
//!   modelled at all — this crate has no bubble-column block.
//! * **A waterlogged block does not originate a spread here.**
//!   [`run_scheduled_tick`] returns early unless the block is
//!   `minecraft:water`/`minecraft:lava`, so a waterlogged slab is a source for
//!   *reading* ([`fluid_state_of`], and so for every neighbour's
//!   [`new_liquid`]) but never runs [`spread`] itself. The real engine does: the
//!   real fluid tick
//!   takes a position rather than a liquid block, and every waterloggable block
//!   schedules a water tick at the container's own position from its own
//!   shape-update hook, across roughly 50 block classes. The consequence is
//!   water reaching *fewer* cells than the real engine past a waterlogged block, never
//!   more — chosen so the error is inert rather than a flood.
//!
//! # How to change it
//!
//! The geometry predicates are the part most likely to need work, and they are
//! the part that is a *reduction* rather than a transliteration:
//! [`can_pass_through_wall`] is the real merged-face-occlusion check evaluated over
//! `lodestone_data::collision_shapes`' axis-aligned box lists with an exact
//! coordinate-sweep coverage test, because those boxes are all the real
//! per-shape box decomposition's own output. It is exact for a static shape and **wrong for a
//! neighbour-dependent one** (stairs, fences, walls, panes): our census is keyed
//! by block state, and the real collision-shape query for those consults the
//! neighbours. Water flowing against a fence corner may therefore disagree.
//!
//! Do not "fix" that by loosening the predicate — it fails toward *not*
//! spreading, and a loosened version fails toward water leaking through walls,
//! which is unrecoverable in a saved world.

use std::collections::HashMap;

use lodestone_model::BlockPos;

use crate::chunk::ChunkSource;
use crate::neighbor_update::Direction;
use crate::scheduled_tick::{ScheduledTick, ScheduledTickQueue, TickPriority};

/// The scheduled-tick `kind` string every fluid tick carries, the way
/// `crate::redstone::TICK_TORCH` and friends key the block queue.
///
/// One kind for both fluids deliberately. The real engine keys its per-chunk
/// pending-tick table by the
/// fluid *instance*, so water and lava at one position could in principle hold
/// two independent pending ticks — but they cannot both occupy one block, so the
/// distinction is unobservable, and collapsing it means
/// [`ScheduledTickQueue::has_scheduled`]'s `(pos, kind)` dedup does exactly what
/// the real per-position tick tracking does.
pub const TICK_FLUID: &str = "lodestone:fluid";

/// The real horizontal iteration order: **north, east, south, west**.
///
/// Not load-bearing for any result here and written down anyway. Every
/// horizontal loop in the real flowing-fluid engine accumulates a `max` ([`new_liquid`]), a
/// `min` ([`slope_distance`]) or a `<=`-keeps-ties set ([`spread_targets`]), all
/// three of which are order-independent — so a wrong order would be invisible,
/// which is exactly why it is worth pinning rather than guessing.
const HORIZONTAL: [Direction; 4] =
    [Direction::North, Direction::East, Direction::South, Direction::West];

/// The real liquid block's possible-flow-directions set, the set
/// the should-spread-liquid check walks: **down, south, north, east, west** — no `up`.
///
/// It reads each direction's *opposite*, so the cells actually probed are up,
/// north, south, west, east: a lava cell is quenched by water above or beside it
/// and **not** by water below it.
const POSSIBLE_FLOW_DIRECTIONS: [Direction; 5] = [
    Direction::Down,
    Direction::South,
    Direction::North,
    Direction::East,
    Direction::West,
];

/// Which fluid a cell holds. The real empty-fluid sentinel is `Option::None`
/// at every call site rather
/// than a third variant, matching how the real fluid-state "is empty" check reads in practice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FluidKind {
    /// `minecraft:water` / `minecraft:flowing_water`, and the fluid state of any
    /// `waterlogged=true` block.
    Water,
    /// `minecraft:lava` / `minecraft:flowing_lava`.
    Lava,
}

impl FluidKind {
    /// The block name this fluid writes. One name for both the source and the
    /// flowing form, because 26.2's *block* registry has only
    /// `minecraft:water`/`minecraft:lava` — the `flowing_` split is a *fluid*
    /// registry distinction, carried on the block by `level`.
    #[must_use]
    pub fn block_name(self) -> &'static str {
        match self {
            FluidKind::Water => "minecraft:water",
            FluidKind::Lava => "minecraft:lava",
        }
    }
}

/// One entry of the real **fluid registry** — the distinction [`FluidKind`]
/// deliberately collapses. `minecraft:water` and `minecraft:flowing_water` are
/// two different fluid *instances* in the real registry, and
/// the real "is same fluid" check is what treats them as one family.
///
/// It exists for exactly one predicate, and that predicate is load-bearing:
/// the real can-place-liquid check for a waterloggable block is an exact
/// instance comparison against the water fluid, so **no flowing state can ever waterlog a container**.
/// Passing a `FluidKind` there instead loses the distinction and waterlogs on
/// every flow — and because [`fluid_state_of`] correctly reports a
/// `waterlogged=true` block as a *source*, each newly waterlogged block then
/// feeds its neighbours at `amount = 8`. The level never decrements, so the
/// spread has no bound at all: the reach stops being `8 / drop_off` cells and
/// becomes the size of the waterloggable terrain, at one block write and one
/// scheduled tick per cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FluidType {
    /// Which family — the real "is same fluid" equivalence class.
    pub kind: FluidKind,
    /// `true` for `minecraft:water`/`minecraft:lava`, `false` for
    /// `minecraft:flowing_water`/`minecraft:flowing_lava`.
    pub source: bool,
}

impl FluidType {
    /// The real per-fluid "get source" query.
    #[must_use]
    pub const fn source(kind: FluidKind) -> FluidType {
        FluidType { kind, source: true }
    }

    /// The real per-fluid "get flowing" query.
    #[must_use]
    pub const fn flowing(kind: FluidKind) -> FluidType {
        FluidType { kind, source: false }
    }
}

/// One cell's fluid state — the real fluid-state record
/// reduced to the two properties that decide spreading.
///
/// `amount` is `1..=8` (the real amount query); `8` with `falling == false` is a
/// source. See this module's own doc comment for the `level` ⇄ `(amount,
/// falling)` mapping, which is the easiest thing here to invert by accident.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FluidState {
    /// Which fluid.
    pub kind: FluidKind,
    /// `1..=8`. The real amount query.
    pub amount: u8,
    /// The real "falling" flag — set on a cell fed from directly above, which
    /// makes it spread sideways at the full `7` regardless of its own amount
    /// (the real spread-to-sides step).
    pub falling: bool,
}

impl FluidState {
    /// A source: `amount == 8` and not falling.
    ///
    /// The `!falling` half is not redundant. The falling-flowing state a
    /// cell directly under a fluid takes also has `amount == 8`, and it is
    /// **not** a source: the real flowing water's falling variant reports
    /// "is source" as `false`
    /// unconditionally. Treating it as one would
    /// make a waterfall's column self-sustaining and it would never drain.
    #[must_use]
    pub fn is_source(self) -> bool {
        self.amount == 8 && !self.falling
    }

    /// The real "get type" query — which fluid-registry instance this state belongs
    /// to, source or flowing.
    ///
    /// Derived from [`is_source`](Self::is_source), so `getFlowing(8, true)`
    /// answers *flowing* despite `amount == 8`. That is the whole point:
    /// [`can_hold_specific_fluid`] compares this against
    /// `FluidType::source(Water)`, so a falling column must not read as a source
    /// there either.
    #[must_use]
    pub fn fluid_type(self) -> FluidType {
        FluidType {
            kind: self.kind,
            source: self.is_source(),
        }
    }

    /// The real "is full" query — `amount == 8`, falling included.
    #[must_use]
    pub fn is_full(self) -> bool {
        self.amount == 8
    }

    /// The real "own height" query — `amount / 9.0`.
    /// Note the divisor is **9**, not 8, so even a full non-stacked fluid is
    /// `0.888…` tall rather than `1.0`.
    #[must_use]
    pub fn own_height(self) -> f32 {
        f32::from(self.amount) / 9.0
    }

    /// The real legacy-level derivation — the
    /// `level` property value this state is stored as.
    #[must_use]
    pub fn legacy_level(self) -> u32 {
        if self.is_source() {
            0
        } else {
            u32::from(8 - self.amount.min(8)) + u32::from(self.falling) * 8
        }
    }

    /// The canonical block-state string [`crate::chunk::ChunkColumn`] stores for
    /// this fluid — the real "create legacy block" derivation.
    #[must_use]
    pub fn block_state(self) -> String {
        format!("{}[level={}]", self.kind.block_name(), self.legacy_level())
    }
}

/// The two dimension-dependent constants the real flowing-fluid engine reads off the level, and
/// the two gamerules the real can-convert-to-source check reads.
///
/// `fast_lava` is the real fast-lava dimension attribute,
/// a **dimension attribute** — nether lava is four times cheaper to cross and
/// three times faster. The two conversion flags are the real
/// `water_source_conversion` (default `true`) and `lava_source_conversion`
/// (default `false`) gamerules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FluidEnv {
    /// The real fast-lava dimension attribute.
    pub fast_lava: bool,
    /// The real `water_source_conversion` gamerule, default `true`.
    pub water_source_conversion: bool,
    /// The real `lava_source_conversion` gamerule, default `false`.
    pub lava_source_conversion: bool,
    /// Lowest world `y` that exists — the dimension's build-height floor.
    ///
    /// **Load-bearing, not decoration.** Every step of the spread reads the cell
    /// *below* the one it is looking at, so a fluid resting on the bottom of the
    /// world asks for `min_y - 1`. `ChunkColumn::block_state` indexes unguarded
    /// and **panics** there, and the panic would land on the world tick thread.
    /// [`block_at`] answers air outside these bounds instead, which is also
    /// the real behaviour: the real block-state query returns void air when
    /// the position is outside build height.
    pub min_y: i32,
    /// Number of block rows above [`min_y`](Self::min_y).
    pub height: i32,
}

impl FluidEnv {
    /// The only dimension this crate hosts today: slow lava, vanilla-default
    /// conversion rules, 26.2's overworld `-64..320`.
    pub const OVERWORLD: FluidEnv = FluidEnv {
        fast_lava: false,
        water_source_conversion: true,
        lava_source_conversion: false,
        min_y: -64,
        height: 384,
    };

    /// Written down so the `fast_lava` arithmetic exists in one place rather
    /// than being rediscovered. Nothing constructs this in production yet — the
    /// server joins into the overworld only.
    pub const NETHER: FluidEnv = FluidEnv {
        fast_lava: true,
        water_source_conversion: true,
        lava_source_conversion: false,
        min_y: 0,
        height: 256,
    };

    /// [`OVERWORLD`](Self::OVERWORLD) with the vertical extent a real
    /// [`ChunkSource`]'s columns actually have.
    ///
    /// Callers should prefer this over the constant: the constant's `-64..320` is
    /// 26.2's overworld, and a source whose columns are shorter (every test
    /// double in this crate, and `WorldgenChunkSource`) would otherwise be read
    /// out of range. See [`min_y`](Self::min_y) for what happens then.
    #[must_use]
    pub const fn overworld_in(min_y: i32, height: i32) -> FluidEnv {
        FluidEnv {
            fast_lava: FluidEnv::OVERWORLD.fast_lava,
            water_source_conversion: FluidEnv::OVERWORLD.water_source_conversion,
            lava_source_conversion: FluidEnv::OVERWORLD.lava_source_conversion,
            min_y,
            height,
        }
    }

    /// `true` iff `y` is inside this dimension's build height — the real
    /// inside-build-height query.
    #[must_use]
    const fn contains_y(self, y: i32) -> bool {
        y >= self.min_y && y < self.min_y + self.height
    }

    /// The real drop-off query — how much `amount` one horizontal step costs.
    #[must_use]
    pub fn drop_off(self, kind: FluidKind) -> u8 {
        match kind {
            FluidKind::Water => 1,
            FluidKind::Lava => {
                if self.fast_lava {
                    1
                } else {
                    2
                }
            }
        }
    }

    /// The real slope-find-distance query — how far [`slope_distance`] looks for a hole.
    #[must_use]
    pub fn slope_find_distance(self, kind: FluidKind) -> u32 {
        match kind {
            FluidKind::Water => 4,
            FluidKind::Lava => {
                if self.fast_lava {
                    4
                } else {
                    2
                }
            }
        }
    }

    /// The real tick-delay query, in game ticks.
    #[must_use]
    pub fn tick_delay(self, kind: FluidKind) -> u64 {
        match kind {
            FluidKind::Water => 5,
            FluidKind::Lava => {
                if self.fast_lava {
                    10
                } else {
                    30
                }
            }
        }
    }

    /// The real can-convert-to-source query — whether two adjacent sources make a third.
    #[must_use]
    pub fn can_convert_to_source(self, kind: FluidKind) -> bool {
        match kind {
            FluidKind::Water => self.water_source_conversion,
            FluidKind::Lava => self.lava_source_conversion,
        }
    }
}

// ---------------------------------------------------------------------------
// State-string <-> fluid-state
// ---------------------------------------------------------------------------

/// The block state at `pos`, or air when `pos` is outside the dimension's build
/// height — the real block-state query, whose own first line checks whether the
/// position is outside build height and returns void air's default state if so.
///
/// **Every world read in this module goes through here**, and that is a hard
/// invariant rather than a style preference: the spread reads the cell below
/// whatever it is looking at, `ChunkColumn::block_state` indexes unguarded, and a
/// fluid resting on the floor of the world would therefore panic the world tick
/// thread. See [`FluidEnv::min_y`].
fn block_at<S: ChunkSource + ?Sized>(world: &S, env: FluidEnv, pos: BlockPos) -> String {
    if env.contains_y(pos.y) {
        world.block_state(pos.x, pos.y, pos.z)
    } else {
        crate::chunk::AIR.to_owned()
    }
}

/// Strips a `[...]` property suffix, the same way every other canonical-name
/// comparison in this crate does.
fn base_name(state: &str) -> &str {
    state.split('[').next().unwrap_or(state)
}

/// The value of `state`'s `key=` property. A whole-key match, so `level` cannot
/// be found inside another property's name.
fn property_of<'s>(state: &'s str, key: &str) -> Option<&'s str> {
    let props = state.split_once('[')?.1.strip_suffix(']')?;
    props.split(',').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k.trim() == key).then_some(v.trim())
    })
}

/// The real "get fluid state" query — the fluid a block state holds, or `None` for a
/// block with no fluid.
///
/// Three producers, matching the real liquid block's own fluid-state query
/// and the real waterloggable-block interface:
///
/// * `minecraft:water`/`minecraft:lava`, whose `level` decodes per this module's
///   own table (an absent `level` is the default state, `0`, a source);
/// * any state carrying `waterlogged=true`, whose fluid state is a **water
///   source** even though its block is a slab or a fence;
/// * everything else, `None`.
#[must_use]
pub fn fluid_state_of(state: &str) -> Option<FluidState> {
    let kind = match base_name(state) {
        "minecraft:water" => FluidKind::Water,
        "minecraft:lava" => FluidKind::Lava,
        _ => {
            return (property_of(state, "waterlogged") == Some("true")).then_some(FluidState {
                kind: FluidKind::Water,
                amount: 8,
                falling: false,
            });
        }
    };
    // The real fluid-state query's clamp to 8 is why 9..=15 all read
    // as the one falling state rather than as ever-thinner flows.
    let level = property_of(state, "level")
        .and_then(|value| value.parse::<u8>().ok())
        .unwrap_or(0)
        .min(8);
    Some(match level {
        0 => FluidState {
            kind,
            amount: 8,
            falling: false,
        },
        8 => FluidState {
            kind,
            amount: 8,
            falling: true,
        },
        other => FluidState {
            kind,
            amount: 8 - other,
            falling: false,
        },
    })
}

/// `true` iff `state`'s fluid is `kind` — the real "is same fluid" check,
/// which treats a fluid and its flowing twin as one.
fn is_same_fluid(state: &str, kind: FluidKind) -> bool {
    fluid_state_of(state).is_some_and(|fluid| fluid.kind == kind)
}

/// The real "is source block of this type" check.
fn is_source_of_type(state: &str, kind: FluidKind) -> bool {
    fluid_state_of(state).is_some_and(|fluid| fluid.kind == kind && fluid.is_source())
}

// ---------------------------------------------------------------------------
// Bucket place/pickup. This crate ticks fluid
// already in the world; these two functions are the missing entry point a
// dispenser (or, eventually, a player's direct use) needs to *start* one.
// Water and lava only — see `crate::redstone_dispenser`'s own behaviour
// table for why the powder-snow/fish/axolotl/tadpole buckets are not here
// (each needs an entity or block mechanic this module has nothing to do
// with fluid placement).
// ---------------------------------------------------------------------------

/// The real "empty contents" target check, reduced to the
/// case this crate can answer without a full replaceability model: the
/// three air variants. The real engine additionally empties onto any block
/// that is replaceable by the fluid (a torch, tall grass, a flower,
/// …); refusing those here **under**-empties rather than over-empties —
/// naming a target as fillable that the real engine would refuse is the direction
/// that would be a real bug (griefing a block that should have survived),
/// and this cannot do that.
#[must_use]
pub fn is_bucket_emptiable_target(state: &str) -> bool {
    matches!(
        base_name(state),
        "minecraft:air" | "minecraft:cave_air" | "minecraft:void_air"
    )
}

/// The source [`FluidKind`] a filled bucket item empties, or `None` for a
/// bucket variant this module does not place fluid for (empty bucket, or any
/// of the non-water/lava filled buckets — see this section's own doc comment).
#[must_use]
pub fn bucket_empty_item_kind(item: &str) -> Option<FluidKind> {
    match item {
        "minecraft:water_bucket" => Some(FluidKind::Water),
        "minecraft:lava_bucket" => Some(FluidKind::Lava),
        _ => None,
    }
}

/// The block-state string to place at a target that
/// [`is_bucket_emptiable_target`] accepts — always the source state,
/// `FluidState::is_source`'s own shape (`amount == 8`, not falling).
#[must_use]
pub fn bucket_empty_state(kind: FluidKind) -> &'static str {
    match kind {
        FluidKind::Water => "minecraft:water[level=0]",
        FluidKind::Lava => "minecraft:lava[level=0]",
    }
}

/// The real bucket-pickup water/lava half: the fluid kind at
/// `target_state`, if it is a **source** (the real pickup refuses
/// a flowing, non-source cell — an empty bucket dipped in a stream's middle
/// comes back empty, matching the real engine exactly).
#[must_use]
pub fn bucket_pickup_kind(target_state: &str) -> Option<FluidKind> {
    let fluid = fluid_state_of(target_state)?;
    fluid.is_source().then_some(fluid.kind)
}

/// The filled-bucket item a pickup of `kind` yields.
#[must_use]
pub fn filled_bucket_item(kind: FluidKind) -> &'static str {
    match kind {
        FluidKind::Water => "minecraft:water_bucket",
        FluidKind::Lava => "minecraft:lava_bucket",
    }
}

/// The real "get height" query — `hasSameAbove ? 1.0 : ownHeight`.
/// Only lava's can-be-replaced-with check needs it.
fn fluid_height(fluid: FluidState, above_state: &str) -> f32 {
    if is_same_fluid(above_state, fluid.kind) {
        1.0
    } else {
        fluid.own_height()
    }
}

// ---------------------------------------------------------------------------
// Geometry: what a fluid may occupy, and what it may pass through
// ---------------------------------------------------------------------------

/// The real can-hold-any-fluid check's explicit block list. Every one of
/// these has no collision box, so
/// the blocks-motion test would let fluid in; the real engine names them individually.
///
/// Doors and the real sign tag are matched by suffix rather than listed,
/// because both are per-wood families of a dozen-plus blocks.
const NEVER_HOLDS_FLUID: [&str; 6] = [
    "minecraft:ladder",
    "minecraft:sugar_cane",
    "minecraft:bubble_column",
    "minecraft:nether_portal",
    "minecraft:end_portal",
    "minecraft:end_gateway",
];

/// `state`'s global 26.2 block-state id, air for anything the census does not
/// carry — [`crate::chunk::resolve_palette_state_id`], deliberately not a
/// re-derivation of it (CLAUDE.md: two test helpers hand-duplicated an older
/// version of that fallback and became silent callers when it changed).
fn state_id(state: &str) -> u32 {
    crate::chunk::resolve_palette_state_id(state)
}

/// The real blocks-motion query, out of `lodestone_data`'s jar-derived census.
///
/// `None` from the census means the state is not in the table, and the safe
/// answer is **yes it blocks** — a gap must stop fluid rather than let it
/// through a block we failed to classify.
fn blocks_motion(state: &str) -> bool {
    lodestone_data::block_solidity::blocks_motion(state_id(state)).unwrap_or(true)
}

/// `true` iff this block is a real liquid-block-container — in 26.2 that is
/// anything implementing the waterloggable-block interface, i.e. anything
/// with a `waterlogged` property.
fn is_waterloggable(state: &str) -> bool {
    property_of(state, "waterlogged").is_some()
}

/// The real can-hold-any-fluid check.
///
/// Order matters and is the real engine's: the liquid-block-container test comes
/// **first**, so a waterloggable slab qualifies even though it very much blocks
/// motion.
fn can_hold_any_fluid(state: &str) -> bool {
    if is_waterloggable(state) {
        return true;
    }
    if blocks_motion(state) {
        return false;
    }
    let name = base_name(state);
    if NEVER_HOLDS_FLUID.contains(&name) || name == "minecraft:structure_void" {
        return false;
    }
    // Doors and the real sign tag, both per-wood families.
    !(name.ends_with("_door")
        || name.ends_with("_sign")
        || name.ends_with("_hanging_sign")
        || name.ends_with("_wall_sign"))
}

/// The real can-hold-specific-fluid check, which delegates to the real
/// can-place-liquid check on the waterloggable-block interface — whose one
/// 26.2 implementation's entire body is an instance comparison against the
/// water fluid.
///
/// **That is an instance comparison against the *source*, not a family test**,
/// and it is the only thing in the whole family stopping flowing water from
/// waterlogging every slab, fence and stair it reaches — see [`FluidType`].
/// The real "is same fluid" check would answer `true` for the flowing twin; `==` does not.
///
/// The real can-place-liquid check deliberately says nothing about the block being
/// waterlogged *already*; that clause lives in the real place-liquid step, and so lives in
/// [`spread_to`] here. Putting it in this predicate instead is inert (an
/// already-waterlogged block reads as a source, so the real
/// can-maybe-pass-through check
/// excludes it first) but it misplaces the rule, and the rule that matters is
/// the one above.
fn can_hold_specific_fluid(state: &str, fluid: FluidType) -> bool {
    if !is_waterloggable(state) {
        return true;
    }
    fluid == FluidType::source(FluidKind::Water)
}

/// The real can-hold-fluid check.
fn can_hold_fluid(state: &str, fluid: FluidType) -> bool {
    can_hold_any_fluid(state) && can_hold_specific_fluid(state, fluid)
}

/// The real can-be-replaced-with check — whether the fluid **already** at a cell
/// yields to `incoming` arriving from `direction`.
///
/// The two implementations disagree in an important way and neither is
/// symmetric:
///
/// * empty — always yields;
/// * water — `direction == DOWN && !incoming.is(WATER)`,
///   so standing water yields **only** to lava falling into it, never to more
///   water. That single clause is what stops a fluid tick rewriting a cell with
///   the state it already holds, forever;
/// * lava — `height >= 0.4444 && incoming.is(WATER)`,
///   i.e. lava at least four ninths deep yields to water from any direction.
fn can_be_replaced_with(
    existing: Option<FluidState>,
    above_state: &str,
    incoming: FluidKind,
    direction: Direction,
) -> bool {
    let Some(existing) = existing else {
        return true;
    };
    match existing.kind {
        FluidKind::Water => direction == Direction::Down && incoming != FluidKind::Water,
        FluidKind::Lava => {
            fluid_height(existing, above_state) >= 0.444_444_45 && incoming == FluidKind::Water
        }
    }
}

/// A 2D rectangle in the face plane, in the two axes other than the interface's.
type FaceRect = (f32, f32, f32, f32);

/// The real merged-face-occlusion check, evaluated over
/// `lodestone_data::collision_shapes`.
///
/// # What the real check actually computes, and how this reproduces it
///
/// Take the interface plane between the two cells. The shape on the *negative*
/// side contributes its cross-section at its own `max` on the axis, but **only
/// if that max is exactly 1.0**; the shape on the positive side contributes its
/// cross-section at `min == 0.0`. Union the two, and the face occludes iff the
/// union covers the whole unit square (`joinIsNotEmpty(block(), union,
/// ONLY_FIRST)` is "is there any part of the full face the union misses").
///
/// Because every box in the census is axis-aligned, "the cross-section at max ==
/// 1.0" is exactly "the 2D projection of every box whose `max[axis]` is 1.0", and
/// the coverage test is an exact coordinate sweep rather than a rasterisation —
/// see [`covers_unit_square`].
fn merged_face_occludes(source_state: &str, target_state: &str, direction: Direction) -> bool {
    let (axis, positive) = match direction {
        Direction::West => (0, false),
        Direction::East => (0, true),
        Direction::Down => (1, false),
        Direction::Up => (1, true),
        Direction::North => (2, false),
        Direction::South => (2, true),
    };
    // `first` is the shape on the negative side of the interface, `second` the
    // one on the positive side. For a positive `direction` the source is the
    // negative side; for a negative one the target is.
    let (first, second) = if positive {
        (source_state, target_state)
    } else {
        (target_state, source_state)
    };

    let mut rects: Vec<FaceRect> = Vec::new();
    let (u, v) = match axis {
        0 => (1, 2),
        1 => (0, 2),
        _ => (0, 1),
    };
    for aabb in lodestone_data::collision_shapes::collision_boxes(state_id(first)).unwrap_or(&[]) {
        if (aabb.max[axis] - 1.0).abs() <= 1.0e-7 {
            rects.push((aabb.min[u], aabb.min[v], aabb.max[u], aabb.max[v]));
        }
    }
    for aabb in lodestone_data::collision_shapes::collision_boxes(state_id(second)).unwrap_or(&[]) {
        if aabb.min[axis].abs() <= 1.0e-7 {
            rects.push((aabb.min[u], aabb.min[v], aabb.max[u], aabb.max[v]));
        }
    }
    covers_unit_square(&rects)
}

/// `true` iff `rects` together cover all of `[0,1]²`.
///
/// An exact coordinate sweep, not a rasterisation: collect every distinct edge
/// coordinate on each axis, and check that every elementary cell of the
/// resulting grid has some rect containing its centre. Exact for any set of
/// axis-aligned rectangles at any resolution, which matters because the shape
/// census carries a handful of non-sixteenth coordinates (a cauldron's
/// `0.1875`, a lily pad's `0.09375`) that a 16×16 grid would round.
fn covers_unit_square(rects: &[FaceRect]) -> bool {
    if rects.is_empty() {
        return false;
    }
    let mut us: Vec<f32> = vec![0.0, 1.0];
    let mut vs: Vec<f32> = vec![0.0, 1.0];
    let mut push_interior = |axis: &mut Vec<f32>, value: f32| {
        if value > 0.0 && value < 1.0 {
            axis.push(value);
        }
    };
    for &(u0, v0, u1, v1) in rects {
        push_interior(&mut us, u0);
        push_interior(&mut us, u1);
        push_interior(&mut vs, v0);
        push_interior(&mut vs, v1);
    }
    us.sort_by(f32::total_cmp);
    vs.sort_by(f32::total_cmp);
    us.dedup();
    vs.dedup();
    for pair in us.windows(2) {
        let cu = f32::midpoint(pair[0], pair[1]);
        for vpair in vs.windows(2) {
            let cv = f32::midpoint(vpair[0], vpair[1]);
            let covered = rects
                .iter()
                .any(|&(u0, v0, u1, v1)| cu > u0 && cu < u1 && cv > v0 && cv < v1);
            if !covered {
                return false;
            }
        }
    }
    true
}

/// `true` iff the state's collision shape is exactly the full cube —
/// the real full-block-shape identity test.
fn is_full_cube(state: &str) -> bool {
    let boxes =
        lodestone_data::collision_shapes::collision_boxes(state_id(state)).unwrap_or(&[]);
    boxes.len() == 1
        && boxes[0].min.iter().all(|&c| c.abs() <= 1.0e-7)
        && boxes[0].max.iter().all(|&c| (c - 1.0).abs() <= 1.0e-7)
}

/// The real can-pass-through-wall check, minus the
/// two debug-only guards and the thread-local occlusion cache.
///
/// The three early exits are the real check's own and they are the whole hot path: a
/// full-cube target or source blocks unconditionally, and two empty shapes pass
/// unconditionally. Only the mixed case reaches the face merge.
fn can_pass_through_wall(direction: Direction, source_state: &str, target_state: &str) -> bool {
    if is_full_cube(target_state) || is_full_cube(source_state) {
        return false;
    }
    let source_empty = lodestone_data::collision_shapes::collision_boxes(state_id(source_state))
        .is_none_or(<[_]>::is_empty);
    let target_empty = lodestone_data::collision_shapes::collision_boxes(state_id(target_state))
        .is_none_or(<[_]>::is_empty);
    if source_empty && target_empty {
        return true;
    }
    !merged_face_occludes(source_state, target_state, direction)
}

/// The real can-maybe-pass-through check.
fn can_maybe_pass_through(
    kind: FluidKind,
    direction: Direction,
    source_state: &str,
    target_state: &str,
) -> bool {
    !is_source_of_type(target_state, kind)
        && can_hold_any_fluid(target_state)
        && can_pass_through_wall(direction, source_state, target_state)
}

/// The real can-pass-through check — the
/// [`can_maybe_pass_through`] test plus the fluid-specific holdability the
/// slope search needs.
///
/// The fluid it asks about is **always the flowing instance**: the real
/// slope-distance query
/// is the only caller and it always passes the flowing form. The slope search is
/// asking whether a *flow* could continue through the cell, so it must not route
/// through a container that a flow cannot enter.
fn can_pass_through(
    kind: FluidKind,
    direction: Direction,
    source_state: &str,
    target_state: &str,
) -> bool {
    can_maybe_pass_through(kind, direction, source_state, target_state)
        && can_hold_specific_fluid(target_state, FluidType::flowing(kind))
}

// ---------------------------------------------------------------------------
// The spread algorithm
// ---------------------------------------------------------------------------

/// The real spread-context record — the per-call
/// state and hole caches the slope search shares.
///
/// Keyed by horizontal offset from the origin only, exactly like the real
/// cache-key derivation, because [`slope_distance`] never changes `y`. The
/// is-hole cache reads the cell *below* uncached, which the real
/// implementation does too.
struct SpreadContext {
    origin: BlockPos,
    kind: FluidKind,
    env: FluidEnv,
    states: HashMap<(i32, i32), String>,
    holes: HashMap<(i32, i32), bool>,
}

impl SpreadContext {
    fn new(origin: BlockPos, kind: FluidKind, env: FluidEnv) -> Self {
        Self {
            origin,
            kind,
            env,
            states: HashMap::new(),
            holes: HashMap::new(),
        }
    }

    fn key(&self, pos: BlockPos) -> (i32, i32) {
        (pos.x - self.origin.x, pos.z - self.origin.z)
    }

    fn block_state<S: ChunkSource + ?Sized>(&mut self, world: &S, pos: BlockPos) -> String {
        let key = self.key(pos);
        let env = self.env;
        self.states
            .entry(key)
            .or_insert_with(|| block_at(world, env, pos))
            .clone()
    }

    fn is_hole<S: ChunkSource + ?Sized>(&mut self, world: &S, pos: BlockPos) -> bool {
        let key = self.key(pos);
        if let Some(&cached) = self.holes.get(&key) {
            return cached;
        }
        let state = self.block_state(world, pos);
        let below = Direction::Down.relative(pos);
        let below_state = block_at(world, self.env, below);
        let answer = is_water_hole(&state, &below_state, self.kind);
        self.holes.insert(key, answer);
        answer
    }
}

/// The real "is water hole" check — "would fluid at
/// `top` fall out the bottom?".
///
/// Despite the name it is fluid-agnostic; lava uses it too.
fn is_water_hole(top_state: &str, bottom_state: &str, kind: FluidKind) -> bool {
    if !can_pass_through_wall(Direction::Down, top_state, bottom_state) {
        return false;
    }
    // The real can-hold-fluid check against the flowing instance — so a dry
    // slab underneath is **not** a hole even though
    // it is waterloggable.
    is_same_fluid(bottom_state, kind) || can_hold_fluid(bottom_state, FluidType::flowing(kind))
}

/// The real "get new liquid" derivation — what the fluid at
/// `pos` *should* be, derived only from its neighbours.
///
/// This is the function that makes the whole system converge. Three rules, in
/// the real engine's order:
///
/// 1. **two or more adjacent sources** (plus a solid or same-fluid floor, plus
///    the dimension's conversion gamerule) make this cell a source — the
///    infinite-water-pool rule;
/// 2. **fluid directly above** makes this cell the flowing form at amount 8,
///    falling, a full
///    falling cell, regardless of what is beside it;
/// 3. otherwise `highest_neighbour - drop_off`, and `<= 0` is empty.
///
/// Note rule 3 reads the highest neighbour that this cell can be reached
/// *from* — [`can_pass_through_wall`] is consulted per direction, so a wall between
/// two cells stops the level being inherited through it.
fn new_liquid<S: ChunkSource + ?Sized>(
    world: &S,
    env: FluidEnv,
    pos: BlockPos,
    state: &str,
    kind: FluidKind,
) -> Option<FluidState> {
    let mut highest_neighbour = 0u8;
    let mut neighbour_sources = 0u32;
    for direction in HORIZONTAL {
        let relative = direction.relative(pos);
        let neighbour_state = block_at(world, env, relative);
        let Some(neighbour) = fluid_state_of(&neighbour_state) else {
            continue;
        };
        if neighbour.kind != kind {
            continue;
        }
        if !can_pass_through_wall(direction, state, &neighbour_state) {
            continue;
        }
        if neighbour.is_source() {
            neighbour_sources += 1;
        }
        highest_neighbour = highest_neighbour.max(neighbour.amount);
    }

    if neighbour_sources >= 2 && env.can_convert_to_source(kind) {
        let below = Direction::Down.relative(pos);
        let below_state = block_at(world, env, below);
        // The real "is solid" check is "the collision shape
        // is a full cube", which is what stops a source forming over a hole.
        if is_full_cube(&below_state) || is_source_of_type(&below_state, kind) {
            return Some(FluidState {
                kind,
                amount: 8,
                falling: false,
            });
        }
    }

    let above = Direction::Up.relative(pos);
    let above_state = block_at(world, env, above);
    if is_same_fluid(&above_state, kind)
        && can_pass_through_wall(Direction::Up, state, &above_state)
    {
        return Some(FluidState {
            kind,
            amount: 8,
            falling: true,
        });
    }

    let amount = highest_neighbour.saturating_sub(env.drop_off(kind));
    (amount > 0).then_some(FluidState {
        kind,
        amount,
        falling: false,
    })
}

/// The real "get slope distance" query — how many
/// horizontal steps from `pos` until a hole, capped at the real
/// slope-find-distance query.
///
/// Returns `1000` (the real implementation's own sentinel) when no hole is within reach. The
/// recursion never revisits the direction it came from, so it explores a tree
/// rather than a graph and terminates on depth alone.
fn slope_distance<S: ChunkSource + ?Sized>(
    world: &S,
    env: FluidEnv,
    kind: FluidKind,
    pos: BlockPos,
    pass: u32,
    from: Direction,
    state: &str,
    context: &mut SpreadContext,
) -> u32 {
    let mut lowest = 1000;
    for direction in HORIZONTAL {
        if direction == from {
            continue;
        }
        let test_pos = direction.relative(pos);
        let test_state = context.block_state(world, test_pos);
        if !can_pass_through(kind, direction, state, &test_state) {
            continue;
        }
        if context.is_hole(world, test_pos) {
            return pass;
        }
        if pass < env.slope_find_distance(kind) {
            let value = slope_distance(
                world,
                env,
                kind,
                test_pos,
                pass + 1,
                direction.opposite(),
                &test_state,
                context,
            );
            lowest = lowest.min(value);
        }
    }
    lowest
}

/// The real "get spread" query — which horizontal
/// directions receive fluid, and at what level.
///
/// The tie rule is what shapes every visible flow: score each candidate by
/// [`slope_distance`], keep only the joint minimum (`distance < lowest` clears
/// the set, `distance <= lowest` adds to it). On flat ground every direction
/// scores the sentinel `1000` and all four are kept; one cell away from a pit,
/// exactly one direction scores `0` and the other three are discarded.
fn spread_targets<S: ChunkSource + ?Sized>(
    world: &S,
    env: FluidEnv,
    pos: BlockPos,
    state: &str,
    kind: FluidKind,
) -> Vec<(Direction, FluidState)> {
    let mut lowest = 1000;
    let mut result: Vec<(Direction, FluidState)> = Vec::new();
    let mut context: Option<SpreadContext> = None;

    for direction in HORIZONTAL {
        let test_pos = direction.relative(pos);
        let test_state = block_at(world, env, test_pos);
        if !can_maybe_pass_through(kind, direction, state, &test_state) {
            continue;
        }
        let Some(new_fluid) = new_liquid(world, env, test_pos, &test_state, kind) else {
            continue;
        };
        // The real can-hold-specific-fluid check against the *new liquid's
        // own* instance, which is what makes a waterloggable
        // neighbour a candidate only when this cell's new-liquid derivation answered
        // with a source.
        if !can_hold_specific_fluid(&test_state, new_fluid.fluid_type()) {
            continue;
        }
        let context = context.get_or_insert_with(|| SpreadContext::new(pos, kind, env));
        let distance = if context.is_hole(world, test_pos) {
            0
        } else {
            slope_distance(
                world,
                env,
                kind,
                test_pos,
                1,
                direction.opposite(),
                &test_state,
                context,
            )
        };
        if distance < lowest {
            result.clear();
        }
        if distance <= lowest {
            let above = Direction::Up.relative(test_pos);
            let above_state = block_at(world, env, above);
            if can_be_replaced_with(
                fluid_state_of(&test_state),
                &above_state,
                new_fluid.kind,
                direction,
            ) {
                result.push((direction, new_fluid));
            }
            lowest = distance;
        }
    }
    result
}

/// The real "spread to" step plus
/// lava's own override of it.
///
/// Two behaviours worth naming:
///
/// * a liquid-block-container target is **waterlogged rather than replaced**, and
///   only ever for a water **source** — the real place-liquid step's
///   own guard is an instance identity check against the water source, so a
///   flowing state fails it. Note
///   both branches `return`: the real "spread to" step never falls through to
///   an ordinary block write for a container, so a slab is never overwritten
///   by water and a
///   refused place-liquid writes **nothing at all**;
/// * lava spreading **down** into water becomes `minecraft:stone`. That is the
///   only stone-generation path in the family; the obsidian/cobblestone one is
///   [`quench_lava`], from a different callback.
fn spread_to<S: ChunkSource + ?Sized>(
    world: &S,
    env: FluidEnv,
    pos: BlockPos,
    target_state: &str,
    direction: Direction,
    fluid: FluidState,
    changes: &mut Vec<(BlockPos, String)>,
) {
    if fluid.kind == FluidKind::Lava
        && direction == Direction::Down
        && fluid_state_of(target_state).is_some_and(|existing| existing.kind == FluidKind::Water)
        && matches!(base_name(target_state), "minecraft:water")
    {
        write_block(world, env, pos, "minecraft:stone", changes);
        return;
    }
    if is_waterloggable(target_state) {
        if fluid.fluid_type() == FluidType::source(FluidKind::Water)
            && property_of(target_state, "waterlogged") != Some("true")
        {
            let waterlogged = crate::redstone::with_property(target_state, "waterlogged", "true");
            write_block(world, env, pos, &waterlogged, changes);
        }
        return;
    }
    write_block(world, env, pos, &fluid.block_state(), changes);
}

/// The real "source neighbor count" query.
fn source_neighbour_count<S: ChunkSource + ?Sized>(
    world: &S,
    env: FluidEnv,
    pos: BlockPos,
    kind: FluidKind,
) -> u32 {
    HORIZONTAL
        .into_iter()
        .filter(|direction| {
            let relative = direction.relative(pos);
            is_source_of_type(&block_at(world, env, relative), kind)
        })
        .count() as u32
}

/// The real "spread to sides" step.
///
/// The `falling` override is the load-bearing line: a falling cell spreads at
/// `7` no matter what its own amount is, which is why a waterfall spreads a full
/// seven blocks at its base rather than the six an ordinary `8 - 1` would give.
fn spread_to_sides<S: ChunkSource + ?Sized>(
    world: &S,
    env: FluidEnv,
    pos: BlockPos,
    fluid: FluidState,
    state: &str,
    changes: &mut Vec<(BlockPos, String)>,
) {
    let neighbour_amount = if fluid.falling {
        7
    } else {
        i32::from(fluid.amount) - i32::from(env.drop_off(fluid.kind))
    };
    if neighbour_amount <= 0 {
        return;
    }
    for (direction, new_fluid) in spread_targets(world, env, pos, state, fluid.kind) {
        let target = direction.relative(pos);
        let target_state = block_at(world, env, target);
        spread_to(world, env, target, &target_state, direction, new_fluid, changes);
    }
}

/// The real "spread" step — down first, sides
/// second.
///
/// The `sourceNeighborCount >= 3` clause is easy to miss and very visible: a
/// cell that *did* manage to drain downward normally spreads no further, **but**
/// one with three or more adjacent sources spreads sideways as well. That is
/// what makes a large pool draining into a single hole still fill the rest of
/// the pool instead of racing to the hole.
fn spread<S: ChunkSource + ?Sized>(
    world: &S,
    env: FluidEnv,
    pos: BlockPos,
    state: &str,
    fluid: FluidState,
    changes: &mut Vec<(BlockPos, String)>,
) {
    let below = Direction::Down.relative(pos);
    let below_state = block_at(world, env, below);
    if can_maybe_pass_through(fluid.kind, Direction::Down, state, &below_state) {
        if let Some(new_below) = new_liquid(world, env, below, &below_state, fluid.kind) {
            let below_above_state = block_at(world, env, pos);
            if can_be_replaced_with(
                fluid_state_of(&below_state),
                &below_above_state,
                new_below.kind,
                Direction::Down,
            ) && can_hold_specific_fluid(&below_state, new_below.fluid_type())
            {
                spread_to(
                    world,
                    env,
                    below,
                    &below_state,
                    Direction::Down,
                    new_below,
                    changes,
                );
                if source_neighbour_count(world, env, pos, fluid.kind) >= 3 {
                    spread_to_sides(world, env, pos, fluid, state, changes);
                }
                return;
            }
        }
    }

    if fluid.is_source() || !is_water_hole(state, &below_state, fluid.kind) {
        spread_to_sides(world, env, pos, fluid, state, changes);
    }
}

/// The real should-spread-liquid check, inverted to
/// return the block a quenched lava cell becomes.
///
/// `Some(block)` means the lava was quenched and **must not tick**; `None` means
/// it spreads normally. Three outcomes:
///
/// * water above or beside a lava **source** → `minecraft:obsidian`;
/// * water above or beside **flowing** lava → `minecraft:cobblestone`;
/// * blue ice beside lava that sits on soul soil → `minecraft:basalt`.
///
/// Note what is *not* here: a lava cell with water **below** it is not quenched
/// (see [`POSSIBLE_FLOW_DIRECTIONS`]) — that case is lava spreading down into
/// water, which [`spread_to`] turns into stone instead.
fn quench_lava<S: ChunkSource + ?Sized>(
    world: &S,
    env: FluidEnv,
    pos: BlockPos,
    fluid: FluidState,
) -> Option<&'static str> {
    if fluid.kind != FluidKind::Lava {
        return None;
    }
    let below = Direction::Down.relative(pos);
    let over_soul_soil =
        base_name(&block_at(world, env, below)) == "minecraft:soul_soil";
    for direction in POSSIBLE_FLOW_DIRECTIONS {
        let neighbour = direction.opposite().relative(pos);
        let neighbour_state = block_at(world, env, neighbour);
        if is_same_fluid(&neighbour_state, FluidKind::Water) {
            return Some(if fluid.is_source() {
                "minecraft:obsidian"
            } else {
                "minecraft:cobblestone"
            });
        }
        if over_soul_soil && base_name(&neighbour_state) == "minecraft:blue_ice" {
            return Some("minecraft:basalt");
        }
    }
    None
}

/// Writes one cell through the world and records it for the wire.
///
/// Both halves, always: `spread` reads the world back as it mutates (the real
/// block write is immediate and the real new-liquid derivation re-reads), so a version that only
/// collected changes and applied them afterwards would compute the *second*
/// cell of a flow against pre-flow terrain.
fn write_block<S: ChunkSource + ?Sized>(
    world: &S,
    env: FluidEnv,
    pos: BlockPos,
    state: &str,
    changes: &mut Vec<(BlockPos, String)>,
) {
    // The real block-write function opens with the same guard and returns
    // `false`
    // without doing anything, for a position outside build height. Silently dropping the
    // write is therefore faithful, and it is also the only safe answer:
    // `ChunkColumn::set_block` indexes unguarded.
    if !env.contains_y(pos.y) {
        return;
    }
    world.set_block(pos.x, pos.y, pos.z, state);
    changes.push((pos, state.to_owned()));
}

/// One due fluid tick — the real flowing-fluid tick with
/// the real should-spread-liquid check folded in front of it.
///
/// A position holding no fluid is a **silent no-op**, and that is deliberate:
/// world state can change between a tick being scheduled and it coming due —
/// [`ticks_after_edit`] only seeds a position that holds a fluid at edit time,
/// but a later edit (or an earlier due tick in the same drain) can remove it
/// before this one fires.
///
/// Every block this writes is appended to `changes` *and* already written
/// through `world`; the caller forwards `changes` to connected clients.
pub fn run_scheduled_tick<S: ChunkSource + ?Sized>(
    world: &S,
    env: FluidEnv,
    pos: BlockPos,
    fluid_ticks: &mut ScheduledTickQueue<String>,
    current_tick: u64,
    changes: &mut Vec<(BlockPos, String)>,
) {
    let state = block_at(world, env, pos);
    let Some(fluid) = fluid_state_of(&state) else {
        return;
    };
    // A waterlogged block reads as a water source and is skipped here, so it
    // never *originates* a spread. **That is a deliberate reduction and not
    // real behaviour** — see this module's "named gaps". The real fluid tick
    // takes a
    // position, not a liquid block, and 26.2 really does schedule ticks at a
    // container's own position from two places: the real place-liquid step
    // ends with a scheduled tick for its own fluid type, and 50
    // waterloggable block classes schedule a water tick at their own position
    // while waterlogged, from their own shape-update hook. So the real engine
    // spreads *out of* a waterlogged slab as a source
    // and we do not.
    //
    // Left as-is because the error direction is inert: water reaches fewer cells
    // than the real engine, never more, and removing this line without a gate on the
    // spread it unlocks would trade a measured behaviour for an unmeasured one.
    if !matches!(base_name(&state), "minecraft:water" | "minecraft:lava") {
        return;
    }

    // Whether this cell still holds fluid after the recompute below, and so
    // whether `spread` runs. **Not an early `return`, and that distinction was
    // measured**: the neighbour-notification loop at the end of this function is
    // what makes a receding flow drain, so a `return` from either the quench
    // branch or the drained branch strands every neighbour of a cell that just
    // became air or stone. Symptom, exactly: a flow whose source was removed
    // froze mid-ramp at `level=7` and never ticked again.
    let mut still_fluid = true;
    let mut fluid = fluid;
    let mut state = state;

    if let Some(quenched) = quench_lava(world, env, pos, fluid) {
        write_block(world, env, pos, quenched, changes);
        still_fluid = false;
    } else if !fluid.is_source() {
        match new_liquid(world, env, pos, &state, fluid.kind) {
            None => {
                write_block(world, env, pos, crate::chunk::AIR, changes);
                // The real engine falls through to `spread` with an *empty*
                // fluid state
                // and `spread`'s own first line rejects it; this flag is that
                // rejection, hoisted so the notify loop below is still reached.
                still_fluid = false;
            }
            Some(new_fluid) if new_fluid != fluid => {
                fluid = new_fluid;
                state = fluid.block_state();
                write_block(world, env, pos, &state, changes);
                fluid_ticks.schedule(
                    (pos.x, pos.y, pos.z),
                    TICK_FLUID.to_owned(),
                    current_tick + env.tick_delay(fluid.kind),
                    TickPriority::Normal,
                );
            }
            Some(_) => {}
        }
    }

    if still_fluid {
        spread(world, env, pos, &state, fluid, changes);
    }

    // Every cell this wrote, **and every neighbour of one**, owes itself a tick.
    //
    // This is the real "on block set" neighbor-update half — a chain that ends
    // in the real liquid block's own neighbor-changed hook scheduling a fluid
    // tick. This crate has no
    // block-lifecycle callback, so the write list is the equivalent hook.
    //
    // **The neighbour half is not an optimisation, it is the only thing that
    // makes a flow drain.** Water never replaces water horizontally
    // (the real water can-be-replaced-with check only yields to lava falling
    // in from directly above),
    // so a receding flow cannot be pushed back by the cell behind it — each cell
    // has to re-evaluate its *own* new-liquid derivation and shrink. Measured while
    // building this: with only the written cells rescheduled, removing a source
    // left the ramp frozen at `level=3` forever, because nothing ever ticked the
    // cells the shrinking one no longer wrote to.
    //
    // The delay is the *ticking* fluid's, not each neighbour's own. The real
    // engine reads
    // the neighbour's own block, and we would have to read the
    // neighbour to know it. The only case that differs is water and lava
    // adjacent, where a lava cell gets water's 5 instead of its own 30, so it
    // reacts sooner; `run_scheduled_tick` reschedules with the correct delay from
    // then on. `ScheduledTickQueue::schedule`'s `(pos, kind)` dedup absorbs the
    // overlap between neighbourhoods.
    // **A neighbour is only scheduled when it already holds a fluid**, and that
    // condition is the whole difference between the real flow rate and twice it.
    //
    // The real neighbor-changed hook is a method **on the liquid block** — the
    // block at the
    // notified position — so the real engine only schedules a fluid tick when
    // that position is itself a
    // liquid. An **air** cell's neighbor-changed hook is the generic block
    // default and
    // schedules no fluid tick at all.
    //
    // Scheduling air too made a falling column advance two cells per tick delay
    // instead of one. When a source spread down, the notify loop
    // handed the cell *below the newly written one* — the air the flow was about
    // to enter — a tick at the same `delay`. Both fell due in the same drain, and
    // the drain order puts the written cell first, so it spread into the air cell
    // and then the air cell, now liquid, spread one further inside that same
    // pass. Every constant was right, which is why reading `tick_delay` could not
    // find this: `FluidEnv::tick_delay` really is 5/30/10 and the queue really is
    // drained once per game tick.
    //
    // The receding case the unconditional form existed for is untouched: water
    // never replaces water horizontally, so a shrinking ramp drains only because
    // each cell re-evaluates its own new-liquid derivation, and every one of
    // those cells
    // *is* a fluid. It is exactly the air neighbours that were never the real
    // engine's to
    // schedule. `changed` itself stays unconditional — that is
    // the real liquid block's own on-place scheduled tick, and on a receding
    // write it is
    // air, where a drain is a documented no-op rather than a runaway.
    let delay = current_tick + env.tick_delay(fluid.kind);
    let touched: Vec<BlockPos> = changes.iter().map(|(pos, _)| *pos).collect();
    for changed in touched {
        for notified in [
            changed,
            Direction::Down.relative(changed),
            Direction::Up.relative(changed),
            Direction::North.relative(changed),
            Direction::South.relative(changed),
            Direction::West.relative(changed),
            Direction::East.relative(changed),
        ] {
            if !env.contains_y(notified.y) {
                continue;
            }
            let holds_fluid = notified == changed
                || fluid_state_of(&world.block_state(notified.x, notified.y, notified.z)).is_some();
            if holds_fluid {
                fluid_ticks.schedule(
                    (notified.x, notified.y, notified.z),
                    TICK_FLUID.to_owned(),
                    delay,
                    TickPriority::Normal,
                );
            }
        }
    }
}

/// The fluid ticks one block edit owes — the edited cell and each of its six
/// neighbours that already holds a fluid, each at that fluid's own
/// [`FluidEnv::tick_delay`], as a **relative** delay [`crate::tick::run_tick_loop`]
/// rebases onto its own counter.
///
/// This is the seeding hook, and it stands in for the real liquid block's
/// on-place, neighbor-changed and shape-update hooks, none of which this
/// crate has a block-lifecycle equivalent for. Breaking the block under an ocean
/// floor, or beside a spring, is exactly the neighbor-changed case: the water
/// itself did not change, so only a notification can start it moving.
///
/// It reads `world` at the edited cell and each neighbour to decide which of
/// the seven hold a fluid at all — a dry cell is never scheduled, mirroring
/// [`run_scheduled_tick`]'s own end-of-tick notify loop, whose "a neighbour is
/// only scheduled when it already holds a fluid" rule this applies at edit
/// time instead of at drain time.
///
/// A fixed delay of `1` used to stand in here for the fluid's own tick delay.
/// That made a newly placed source's first spread land one tick after the
/// edit instead of five, and — because every neighbour was seeded at that
/// same short delay regardless of what it held — a neighbour that received
/// fluid from the edited cell's own spread could come due in the very same
/// drain and spread again, advancing the front two cells on the first tick
/// instead of one.
#[must_use]
pub fn ticks_after_edit<S: ChunkSource + ?Sized>(
    world: &S,
    env: FluidEnv,
    pos: BlockPos,
) -> Vec<ScheduledTick<String>> {
    // Built through a real queue rather than struct literals because
    // `ScheduledTick::sub_tick_order` is private — the same idiom
    // `server::propagate_placement` uses, and for the same reason.
    let mut pending: ScheduledTickQueue<String> = ScheduledTickQueue::new();
    for candidate in [
        pos,
        Direction::Down.relative(pos),
        Direction::Up.relative(pos),
        Direction::North.relative(pos),
        Direction::South.relative(pos),
        Direction::West.relative(pos),
        Direction::East.relative(pos),
    ] {
        if let Some(fluid) = fluid_state_of(&block_at(world, env, candidate)) {
            pending.schedule(
                (candidate.x, candidate.y, candidate.z),
                TICK_FLUID.to_owned(),
                env.tick_delay(fluid.kind),
                TickPriority::Normal,
            );
        }
    }
    pending.drain_due(u64::MAX, usize::MAX)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use super::*;
    use crate::chunk::ChunkColumn;

    const MIN_Y: i32 = -64;
    const HEIGHT: i32 = 384;
    const FLOOR_Y: i32 = 0;

    /// A [`ChunkSource`] that really retains its edits, across as many columns
    /// as the test touches.
    ///
    /// Retention is the whole point: `run_scheduled_tick` reads the world back
    /// as it writes, so a source whose `column()` returned fresh terrain would
    /// make every gate below measure the first step of a flow and nothing after
    /// it. [`the_rig_retains_its_own_edits`] is the premise check.
    struct Rig {
        columns: Mutex<HashMap<(i32, i32), ChunkColumn>>,
    }

    impl Rig {
        /// Stone floor at [`FLOOR_Y`] across chunks `(-1..=1, -1..=1)`, air
        /// above. Three chunks wide on each axis so a flow can cross a border.
        fn flat() -> Self {
            let mut columns = HashMap::new();
            for cx in -1..=1 {
                for cz in -1..=1 {
                    let mut column = ChunkColumn::new(MIN_Y, HEIGHT);
                    for x in 0..16 {
                        for z in 0..16 {
                            column.set_block(x, FLOOR_Y, z, "minecraft:stone");
                        }
                    }
                    columns.insert((cx, cz), column);
                }
            }
            Self {
                columns: Mutex::new(columns),
            }
        }
    }

    impl ChunkSource for Rig {
        fn column(&self, cx: i32, cz: i32) -> ChunkColumn {
            self.columns
                .lock()
                .expect("rig poisoned")
                .entry((cx, cz))
                .or_insert_with(|| ChunkColumn::new(MIN_Y, HEIGHT))
                .clone()
        }

        fn block_state(&self, x: i32, y: i32, z: i32) -> String {
            let (cx, cz) = (x.div_euclid(16), z.div_euclid(16));
            self.columns
                .lock()
                .expect("rig poisoned")
                .get(&(cx, cz))
                .map(|c| {
                    c.block_state(x.rem_euclid(16), y, z.rem_euclid(16))
                        .to_string()
                })
                .unwrap_or_else(|| crate::chunk::AIR.to_owned())
        }

        fn biome_state_at(&self, x: i32, y: i32, z: i32) -> String {
            let (cx, cz) = (x.div_euclid(16), z.div_euclid(16));
            self.columns
                .lock()
                .expect("rig poisoned")
                .get(&(cx, cz))
                .map(|c| {
                    c.biome_state_at(x.rem_euclid(16), y, z.rem_euclid(16))
                        .to_string()
                })
                .unwrap_or_else(|| crate::chunk::AIR.to_owned())
        }

        fn set_block(&self, x: i32, y: i32, z: i32, name: &str) {
            let (cx, cz) = (x.div_euclid(16), z.div_euclid(16));
            self.columns
                .lock()
                .expect("rig poisoned")
                .entry((cx, cz))
                .or_insert_with(|| ChunkColumn::new(MIN_Y, HEIGHT))
                .set_block(x.rem_euclid(16), y, z.rem_euclid(16), name);
        }
    }

    /// Drains the fluid queue until it is quiet or `max_ticks` pass, returning
    /// the tick count consumed. The tick loop's own drain, reduced to one queue.
    fn settle(rig: &Rig, seed: BlockPos, max_ticks: u64) -> u64 {
        let mut queue: ScheduledTickQueue<String> = ScheduledTickQueue::new();
        for pending in ticks_after_edit(rig, FluidEnv::OVERWORLD, seed) {
            queue.schedule(pending.pos, pending.kind, pending.trigger_tick, pending.priority);
        }
        let mut changes = Vec::new();
        for tick in 1..=max_ticks {
            let due = queue.drain_due(tick, usize::MAX);
            if due.is_empty() && queue.is_empty() {
                return tick;
            }
            for entry in due {
                let pos = BlockPos::new(entry.pos.0, entry.pos.1, entry.pos.2);
                changes.clear();
                run_scheduled_tick(rig, FluidEnv::OVERWORLD, pos, &mut queue, tick, &mut changes);
            }
        }
        max_ticks
    }

    /// The depth, in cells below `source`, that a falling column has reached
    /// after exactly `ticks` game ticks — the tick loop's own fluid drain,
    /// reduced to one queue and stepped one tick at a time.
    ///
    /// Counts **cells**, never elapsed time. A duration here would be attributed
    /// to the wrong cause (CLAUDE.md), and this quantity is exactly integral:
    /// vanilla's spread is deterministic, so there is one right answer per tick
    /// number rather than a range.
    fn fall_depth_after(rig: &Rig, source: BlockPos, ticks: u64) -> i32 {
        let mut queue: ScheduledTickQueue<String> = ScheduledTickQueue::new();
        for pending in ticks_after_edit(rig, FluidEnv::OVERWORLD, source) {
            queue.schedule(pending.pos, pending.kind, pending.trigger_tick, pending.priority);
        }
        let mut changes = Vec::new();
        for tick in 1..=ticks {
            for entry in queue.drain_due(tick, usize::MAX) {
                let pos = BlockPos::new(entry.pos.0, entry.pos.1, entry.pos.2);
                changes.clear();
                run_scheduled_tick(rig, FluidEnv::OVERWORLD, pos, &mut queue, tick, &mut changes);
            }
        }
        let mut depth = 0;
        while fluid_state_of(&rig.block_state(source.x, source.y - depth - 1, source.z)).is_some() {
            depth += 1;
        }
        depth
    }

    /// The tick number at which `pos` first reads as holding a fluid, or
    /// `None` if it never does within `max_ticks` — the horizontal
    /// counterpart of [`fall_depth_after`], for a position one cell over
    /// rather than one cell down.
    fn first_wet_tick(rig: &Rig, seed: BlockPos, pos: BlockPos, max_ticks: u64) -> Option<u64> {
        if fluid_state_of(&rig.block_state(pos.x, pos.y, pos.z)).is_some() {
            return Some(0);
        }
        let mut queue: ScheduledTickQueue<String> = ScheduledTickQueue::new();
        for pending in ticks_after_edit(rig, FluidEnv::OVERWORLD, seed) {
            queue.schedule(pending.pos, pending.kind, pending.trigger_tick, pending.priority);
        }
        let mut changes = Vec::new();
        for tick in 1..=max_ticks {
            let due = queue.drain_due(tick, usize::MAX);
            if due.is_empty() && queue.is_empty() {
                return None;
            }
            for entry in due {
                let entry_pos = BlockPos::new(entry.pos.0, entry.pos.1, entry.pos.2);
                changes.clear();
                run_scheduled_tick(rig, FluidEnv::OVERWORLD, entry_pos, &mut queue, tick, &mut changes);
            }
            if fluid_state_of(&rig.block_state(pos.x, pos.y, pos.z)).is_some() {
                return Some(tick);
            }
        }
        None
    }

    /// **The cell immediately beside a freshly placed water source wets on
    /// water's own tick delay, not sooner** — the isolated half, checked
    /// without a live oracle.
    ///
    /// # Where the expected value comes from
    ///
    /// A live 26.2 server, probed directly over RCON with real-time
    /// timestamps (bypassing every tick-counting assumption this crate
    /// makes): a water source's immediate neighbour first read as water at
    /// 247 ms after placement, matching a 250 ms / 5-tick prediction from
    /// water's own tick delay to within the measurement's own noise floor.
    /// `FluidEnv::OVERWORLD.tick_delay(FluidKind::Water)` is that same `5`,
    /// read from this crate's own constant rather than restated as a literal,
    /// so a future change to the constant moves this test's expectation with
    /// it instead of silently decorrelating the two.
    ///
    /// # Why this test exists next to the live differential harness
    ///
    /// `tests/differential_live_fluid_spread.rs` compares this crate against
    /// a live server tick-for-tick, but its vanilla side is paced by a
    /// wall-clock sleep per nominal tick — accurate under ordinary load, and
    /// measurably not under heavy contention, where vanilla's independent
    /// tick loop can outrun the harness's nominal count. This test has no
    /// such dependency: both the seed and the read happen inside one
    /// process's own deterministic tick loop, so it is exact under any load
    /// and answers the question a flaky differential run cannot — whether
    /// *this crate itself*, in isolation, reproduces the externally-measured
    /// constant.
    #[test]
    fn a_water_source_s_neighbour_wets_on_water_s_own_tick_delay() {
        let rig = Rig::flat();
        let y = FLOOR_Y + 1;
        let source = BlockPos::new(0, y, 0);
        rig.set_block(source.x, source.y, source.z, "minecraft:water[level=0]");
        let neighbour = BlockPos::new(1, y, 0);

        let tick = first_wet_tick(&rig, source, neighbour, 50)
            .expect("the neighbour never wets within 50 ticks");

        assert_eq!(
            tick,
            FluidEnv::OVERWORLD.tick_delay(FluidKind::Water),
            "a live 26.2 server wets a water source's immediate neighbour at water's own \
             tick delay (measured 247ms, predicted 250ms/5 ticks) — this model must match \
             that in isolation, independent of any live-oracle harness's own timing"
        );
    }

    /// **Water falls one cell per [`FluidEnv::tick_delay`], starting at that
    /// delay rather than sooner.**
    ///
    /// # Where the expected values come from
    ///
    /// Arithmetic over [`FluidEnv::tick_delay`]'s own constant (`5` for water),
    /// not over this crate's own output: a source's own first spread cannot
    /// land before its own first scheduled tick comes due, so depth at tick
    /// `T` is `T / 5` (integer division) — zero until `T` reaches `5`, then
    /// one more cell every `5` ticks after that.
    ///
    /// # Why this discriminates the defect this pins
    ///
    /// [`ticks_after_edit`] used to seed the source's own first tick at a
    /// fixed delay of `1` rather than its own tick delay, and seeded every
    /// neighbour at that same short delay regardless of what it held. The air
    /// cell below a freshly placed source therefore got its own premature
    /// tick in the very same drain the source's first spread wrote water into
    /// it, so the front reached depth **2** after a single elapsed tick
    /// instead of staying at **0** until the fifth. `ticks in [1, 4]` below
    /// pin exactly that difference; `[5, 6, 9, 10, 11, 20, 21]` cover the
    /// steady cadence past the first cell.
    #[test]
    fn a_falling_column_advances_one_cell_per_tick_delay() {
        const DELAY: u64 = 5;
        let source_y = FLOOR_Y + 40;
        for ticks in [0_u64, 1, 4, 5, 6, 9, 10, 11, 20, 21] {
            let rig = Rig::flat();
            let source = BlockPos::new(0, source_y, 0);
            rig.set_block(source.x, source.y, source.z, "minecraft:water[level=0]");

            let depth = fall_depth_after(&rig, source, ticks);

            let expected = i32::try_from(ticks / DELAY).expect("small");
            assert_eq!(
                depth, expected,
                "after {ticks} ticks a falling water column must be {expected} cells \
                 deep (one per tick_delay={DELAY}, first at tick {DELAY} itself); it \
                 is {depth}"
            );
            assert!(
                source_y - depth > FLOOR_Y,
                "premise: the floor must not have capped the fall at {ticks} ticks, or \
                 this measures the floor rather than the spread"
            );
        }
    }

    /// Premise check for every gate below: the rig must reflect its own writes,
    /// or a settled flow would be invisible to the assertions.
    #[test]
    fn the_rig_retains_its_own_edits() {
        let rig = Rig::flat();
        assert_eq!(rig.block_state(0, FLOOR_Y, 0), "minecraft:stone");
        rig.set_block(0, FLOOR_Y + 1, 0, "minecraft:water[level=0]");
        assert_eq!(rig.block_state(0, FLOOR_Y + 1, 0), "minecraft:water[level=0]");
        // Across a chunk border too, since the border gate depends on it.
        rig.set_block(-1, FLOOR_Y + 1, 0, "minecraft:water[level=3]");
        assert_eq!(rig.block_state(-1, FLOOR_Y + 1, 0), "minecraft:water[level=3]");
    }

    /// The `level` ⇄ `(amount, falling)` mapping, both directions, against the
    /// two real functions that define it — the real legacy-level derivation and
    /// the real liquid block's own state cache.
    #[test]
    fn block_level_round_trips_through_amount_and_falling() {
        // Source: level 0, amount 8, not falling. A bare name is the default.
        for state in ["minecraft:water", "minecraft:water[level=0]"] {
            let fluid = fluid_state_of(state).expect("water is a fluid");
            assert_eq!(fluid.amount, 8, "{state}");
            assert!(!fluid.falling, "{state}");
            assert!(fluid.is_source(), "{state}");
            assert_eq!(fluid.legacy_level(), 0, "{state}");
        }
        // Flowing: level counts DOWN from the source, so level=1 is amount 7.
        for (level, amount) in [(1u8, 7u8), (2, 6), (3, 5), (4, 4), (5, 3), (6, 2), (7, 1)] {
            let state = format!("minecraft:water[level={level}]");
            let fluid = fluid_state_of(&state).expect("water is a fluid");
            assert_eq!(fluid.amount, amount, "level {level}");
            assert!(!fluid.falling, "level {level}");
            assert!(!fluid.is_source(), "level {level}");
            assert_eq!(fluid.legacy_level(), u32::from(level));
            assert_eq!(fluid.block_state(), state);
        }
        // Falling: 8..=15 all clamp to the one full falling state, which is
        // NOT a source despite amount 8.
        for level in 8u8..=15 {
            let fluid = fluid_state_of(&format!("minecraft:water[level={level}]"))
                .expect("water is a fluid");
            assert_eq!(fluid.amount, 8, "level {level}");
            assert!(fluid.falling, "level {level}");
            assert!(!fluid.is_source(), "level {level} must not read as a source");
            assert!(fluid.is_full(), "level {level} is full");
            assert_eq!(fluid.legacy_level(), 8, "the clamp writes back as 8");
        }
        // A waterlogged block's fluid state is a water source.
        let fluid = fluid_state_of("minecraft:oak_slab[type=bottom,waterlogged=true]")
            .expect("waterlogged is a fluid state");
        assert_eq!(fluid.kind, FluidKind::Water);
        assert!(fluid.is_source());
        assert!(fluid_state_of("minecraft:oak_slab[type=bottom,waterlogged=false]").is_none());
        assert!(fluid_state_of("minecraft:stone").is_none());
    }

    /// The dimension/fluid constants, straight off the jar table in this
    /// module's own doc comment. Predicts each value rather than asserting a
    /// relation between them.
    #[test]
    fn drop_off_and_delay_match_the_jar_per_fluid_and_dimension() {
        let over = FluidEnv::OVERWORLD;
        assert_eq!(over.drop_off(FluidKind::Water), 1);
        assert_eq!(over.slope_find_distance(FluidKind::Water), 4);
        assert_eq!(over.tick_delay(FluidKind::Water), 5);
        assert_eq!(over.drop_off(FluidKind::Lava), 2);
        assert_eq!(over.slope_find_distance(FluidKind::Lava), 2);
        assert_eq!(over.tick_delay(FluidKind::Lava), 30);

        let nether = FluidEnv::NETHER;
        assert_eq!(nether.drop_off(FluidKind::Lava), 1, "FAST_LAVA halves the drop-off");
        assert_eq!(nether.slope_find_distance(FluidKind::Lava), 4);
        assert_eq!(nether.tick_delay(FluidKind::Lava), 10);
        assert_eq!(nether.drop_off(FluidKind::Water), 1, "water is dimension-independent");
    }

    /// **The headline gate.** A water source on flat ground must settle into the
    /// exact level ramp the real drop-off predicts, and stop at the exact distance.
    ///
    /// The expected values come from outside this code: `amount` starts at `8`
    /// (the real source's own amount), each horizontal step costs
    /// the real drop-off, `1` for water, `amount <= 0` is empty
    /// (the real new-liquid derivation's last line), and the block stores `8 - amount`
    /// (the real legacy-level derivation). So the ramp along any axis is `level = 1..=7` and cell
    /// 8 is air — the seven-block reach every player knows, derived rather than
    /// remembered.
    ///
    /// Both hypotheses are computed: a `dropOff` of 2 (lava's number, the most
    /// plausible mis-port) would give levels `2,4,6` and stop at 4 cells. The
    /// assertion below can only be satisfied by one of them.
    #[test]
    fn flat_ground_water_spread_matches_vanilla_drop_off() {
        let rig = Rig::flat();
        let y = FLOOR_Y + 1;
        let source = BlockPos::new(0, y, 0);
        rig.set_block(source.x, source.y, source.z, "minecraft:water[level=0]");
        settle(&rig, source, 400);

        // The ramp, predicted from `dropOff = 1`.
        let expected_levels: Vec<u32> = (1..=7).collect();
        let wrong_hypothesis: Vec<u32> = vec![2, 4, 6];

        for (label, step) in [
            ("+x", (1, 0)),
            ("-x", (-1, 0)),
            ("+z", (0, 1)),
            ("-z", (0, -1)),
        ] {
            let observed: Vec<u32> = (1..=8)
                .map_while(|d| {
                    let state = rig.block_state(source.x + step.0 * d, y, source.z + step.1 * d);
                    fluid_state_of(&state).map(FluidState::legacy_level)
                })
                .collect();
            assert_eq!(
                observed, expected_levels,
                "{label}: water must ramp 1..=7 and stop (dropOff = 1); \
                 the dropOff = 2 hypothesis would read {wrong_hypothesis:?}"
            );
            // The negative half of the same claim, stated as its own
            // assertion so a failure says "it went too far" explicitly.
            let eighth = rig.block_state(source.x + step.0 * 8, y, source.z + step.1 * 8);
            assert!(
                fluid_state_of(&eighth).is_none(),
                "{label}: cell 8 must be dry, found {eighth}"
            );
        }
        // The source itself is untouched, and still a source.
        assert!(
            fluid_state_of(&rig.block_state(source.x, y, source.z))
                .expect("source survives")
                .is_source(),
            "the source must not have been converted to a flowing state"
        );
    }

    /// Lava's own reach in the overworld, from the same arithmetic: `dropOff =
    /// 2`, so amounts `6, 4, 2` and then empty — **three** cells, and the block
    /// levels are `2, 4, 6` rather than a contiguous ramp.
    ///
    /// This is the negative control for the water gate above: the two fluids run
    /// the same code and must land on different, separately-predicted patterns.
    /// A `drop_off` that ignored its `FluidKind` would fail exactly one of them.
    #[test]
    fn flat_ground_lava_reaches_three_cells_in_the_overworld() {
        let rig = Rig::flat();
        let y = FLOOR_Y + 1;
        let source = BlockPos::new(0, y, 0);
        rig.set_block(source.x, source.y, source.z, "minecraft:lava[level=0]");
        settle(&rig, source, 800);

        let observed: Vec<u32> = (1..=5)
            .map_while(|d| {
                fluid_state_of(&rig.block_state(source.x + d, y, source.z))
                    .map(FluidState::legacy_level)
            })
            .collect();
        assert_eq!(
            observed,
            vec![2, 4, 6],
            "overworld lava: dropOff = 2 gives amounts 6,4,2 -> levels 2,4,6, then dry"
        );
        assert!(
            fluid_state_of(&rig.block_state(source.x + 4, y, source.z)).is_none(),
            "lava must stop after three cells"
        );
    }

    /// Water must flow across a chunk border, because the tick operates on the
    /// [`ChunkSource`] in world coordinates rather than on one column.
    ///
    /// The source sits at `x = -3`, three cells inside chunk `-1`, so the ramp
    /// necessarily crosses `x = 0`. A column-bounded implementation — which is
    /// what every other reaction path in this crate is — would stop dead at the
    /// border, and this is the gate that would catch it.
    #[test]
    fn water_spreads_across_a_chunk_border() {
        let rig = Rig::flat();
        let y = FLOOR_Y + 1;
        let source = BlockPos::new(-3, y, 8);
        rig.set_block(source.x, source.y, source.z, "minecraft:water[level=0]");
        settle(&rig, source, 400);

        // Cells 1..=7 east of the source: x = -2 .. 4, crossing the border at 0.
        for step in 1..=7i32 {
            let x = source.x + step;
            let state = rig.block_state(x, y, source.z);
            let level = fluid_state_of(&state)
                .unwrap_or_else(|| panic!("x = {x} must hold water, found {state}"))
                .legacy_level();
            assert_eq!(level as i32, step, "x = {x} (chunk {})", x.div_euclid(16));
        }
        assert!(
            fluid_state_of(&rig.block_state(source.x + 8, y, source.z)).is_none(),
            "the ramp must still stop at 7 cells after crossing"
        );
    }

    /// Water must fall, and its base must spread the full seven cells because
    /// the falling cell spreads at `7` rather than at its own amount minus the
    /// drop-off (the real spread-to-sides step's own falling override).
    ///
    /// The source sits in a **walled shaft**, and that is not scene-dressing.
    /// `spread`'s fall-through is "if this is a source, or the cell below is not
    /// a water hole, spread to sides" — so once the column below a source is
    /// established, the
    /// down branch is refused (water never replaces water) and the source spreads
    /// sideways *at its own level*, making a seven-wide sheet that then falls in
    /// seven places. That is correct real behaviour and it is what this rig
    /// measured before the shaft was added; it just is not a test of the falling
    /// override. Walling the shaft isolates the one column.
    #[test]
    fn a_falling_column_spreads_seven_cells_at_its_base() {
        let rig = Rig::flat();
        let base_y = FLOOR_Y + 1;
        let source = BlockPos::new(0, base_y + 4, 0);
        for y in (base_y + 1)..=(source.y) {
            for (dx, dz) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
                rig.set_block(source.x + dx, y, source.z + dz, "minecraft:stone");
            }
        }
        rig.set_block(source.x, source.y, source.z, "minecraft:water[level=0]");
        settle(&rig, source, 800);

        // The column between the source and the floor is falling water.
        for y in base_y..source.y {
            let state = rig.block_state(0, y, 0);
            let fluid = fluid_state_of(&state)
                .unwrap_or_else(|| panic!("y = {y} must hold water, found {state}"));
            assert!(fluid.falling, "y = {y} must be falling water, found {state}");
            assert_eq!(fluid.legacy_level(), 8, "a falling cell stores level 8");
        }
        // And the base ramps a full 1..=7, not 2..=7.
        let observed: Vec<u32> = (1..=8)
            .map_while(|d| {
                fluid_state_of(&rig.block_state(d, base_y, 0)).map(FluidState::legacy_level)
            })
            .collect();
        assert_eq!(
            observed,
            (1..=7).collect::<Vec<u32>>(),
            "a falling cell spreads at 7, so its base reaches seven cells"
        );
    }

    /// The slope search: water one cell from a pit must flow **only** toward the
    /// pit, not evenly in four directions.
    ///
    /// This is the assertion the real "get spread" tie rule exists for, and it is the
    /// one a naive "spread to every direction that can hold fluid" port passes
    /// nothing of. The negative half — the other three directions stay dry — is
    /// what makes it a real test of the tie rule rather than of spreading.
    #[test]
    fn water_flows_toward_a_hole_and_not_elsewhere() {
        let rig = Rig::flat();
        let y = FLOOR_Y + 1;
        // A one-cell pit two east of the source: remove the floor at x = 2.
        rig.set_block(2, FLOOR_Y, 0, crate::chunk::AIR);
        let source = BlockPos::new(0, y, 0);
        rig.set_block(source.x, source.y, source.z, "minecraft:water[level=0]");
        settle(&rig, source, 400);

        assert!(
            fluid_state_of(&rig.block_state(1, y, 0)).is_some(),
            "the direction of the hole must receive water"
        );
        for (label, pos) in [
            ("west", BlockPos::new(-1, y, 0)),
            ("north", BlockPos::new(0, y, -1)),
            ("south", BlockPos::new(0, y, 1)),
        ] {
            let state = rig.block_state(pos.x, pos.y, pos.z);
            assert!(
                fluid_state_of(&state).is_none(),
                "{label} must stay dry while a nearer hole exists, found {state}"
            );
        }
    }

    /// Two adjacent sources with a solid floor make the cell between them a
    /// source — `getNewLiquid`'s first rule, under the default
    /// `water_source_conversion` gamerule.
    ///
    /// The control is the same rig with the gamerule off, which must produce a
    /// *flowing* cell instead. Without it the test would pass on an
    /// implementation that made every cell a source.
    #[test]
    fn two_sources_convert_the_cell_between_them_and_the_gamerule_gates_it() {
        for (conversion, expect_source) in [(true, true), (false, false)] {
            let rig = Rig::flat();
            let y = FLOOR_Y + 1;
            let env = FluidEnv {
                water_source_conversion: conversion,
                ..FluidEnv::OVERWORLD
            };
            rig.set_block(0, y, 0, "minecraft:water[level=0]");
            rig.set_block(2, y, 0, "minecraft:water[level=0]");

            let mut queue: ScheduledTickQueue<String> = ScheduledTickQueue::new();
            let mut changes = Vec::new();
            for seed in [BlockPos::new(0, y, 0), BlockPos::new(2, y, 0)] {
                for pending in ticks_after_edit(&rig, env, seed) {
                    queue.schedule(pending.pos, pending.kind, pending.trigger_tick, pending.priority);
                }
            }
            for tick in 1..=200u64 {
                let due = queue.drain_due(tick, usize::MAX);
                if due.is_empty() && queue.is_empty() {
                    break;
                }
                for entry in due {
                    let pos = BlockPos::new(entry.pos.0, entry.pos.1, entry.pos.2);
                    changes.clear();
                    run_scheduled_tick(&rig, env, pos, &mut queue, tick, &mut changes);
                }
            }

            let middle = rig.block_state(1, y, 0);
            let fluid = fluid_state_of(&middle).expect("the middle cell must hold water");
            assert_eq!(
                fluid.is_source(),
                expect_source,
                "water_source_conversion = {conversion}: middle cell is {middle}"
            );
        }
    }

    /// Lava beside water is quenched — the real should-spread-liquid check. A
    /// *source* becomes obsidian, a *flowing* cell becomes cobblestone, and the
    /// two must not be swapped (they are the single easiest pair here to invert).
    #[test]
    fn lava_beside_water_becomes_obsidian_or_cobblestone_by_source_ness() {
        for (lava_state, expected) in [
            ("minecraft:lava[level=0]", "minecraft:obsidian"),
            ("minecraft:lava[level=2]", "minecraft:cobblestone"),
        ] {
            let rig = Rig::flat();
            let y = FLOOR_Y + 1;
            rig.set_block(0, y, 0, lava_state);
            rig.set_block(1, y, 0, "minecraft:water[level=0]");
            let mut queue: ScheduledTickQueue<String> = ScheduledTickQueue::new();
            let mut changes = Vec::new();
            run_scheduled_tick(
                &rig,
                FluidEnv::OVERWORLD,
                BlockPos::new(0, y, 0),
                &mut queue,
                1,
                &mut changes,
            );
            assert_eq!(
                rig.block_state(0, y, 0),
                expected,
                "{lava_state} beside water"
            );
        }
    }

    /// Lava spreading **down** into water becomes stone — lava's own
    /// override of the real "spread to" step, a different rule from the obsidian/cobblestone one above and
    /// reached from a different callback.
    #[test]
    fn lava_falling_into_water_becomes_stone() {
        let rig = Rig::flat();
        let y = FLOOR_Y + 1;
        // Water on the floor, lava directly above it. `quench_lava` must NOT
        // fire (water is below, and `POSSIBLE_FLOW_DIRECTIONS` never probes
        // below), so the spread path runs and writes stone.
        rig.set_block(0, y, 0, "minecraft:water[level=0]");
        rig.set_block(0, y + 1, 0, "minecraft:lava[level=0]");
        let mut queue: ScheduledTickQueue<String> = ScheduledTickQueue::new();
        let mut changes = Vec::new();
        run_scheduled_tick(
            &rig,
            FluidEnv::OVERWORLD,
            BlockPos::new(0, y + 1, 0),
            &mut queue,
            1,
            &mut changes,
        );
        assert_eq!(
            rig.block_state(0, y, 0),
            "minecraft:stone",
            "lava spreading down into water makes stone"
        );
    }

    /// A flowing cell whose source is removed must drain to air rather than
    /// persist — `tick`'s `newFluidState.isEmpty()` branch. Without it every
    /// flow would be permanent, which is the failure mode a spread-only port
    /// has.
    #[test]
    fn a_flow_drains_when_its_source_is_removed() {
        let rig = Rig::flat();
        let y = FLOOR_Y + 1;
        let source = BlockPos::new(0, y, 0);
        rig.set_block(source.x, source.y, source.z, "minecraft:water[level=0]");
        settle(&rig, source, 400);
        assert!(
            fluid_state_of(&rig.block_state(3, y, 0)).is_some(),
            "precondition: the flow reached x = 3"
        );

        rig.set_block(source.x, source.y, source.z, crate::chunk::AIR);
        settle(&rig, source, 800);

        for x in 1..=7 {
            let state = rig.block_state(x, y, 0);
            assert!(
                fluid_state_of(&state).is_none(),
                "x = {x} must have drained to air, found {state}"
            );
        }
    }

    /// Water must not pass a full-cube wall, and must pass an open cell — the
    /// two ends of [`can_pass_through_wall`], asserted through the whole tick
    /// rather than on the predicate alone.
    #[test]
    fn a_solid_wall_stops_the_flow_and_a_gap_does_not() {
        let rig = Rig::flat();
        let y = FLOOR_Y + 1;
        // A full wall across z, not a single block: water flows *around* an
        // isolated block, which is correct vanilla behaviour, so a one-cell wall
        // would fail this gate for the wrong reason. The wall spans further than
        // the seven-cell reach on each side so there is no way round it.
        for z in -9..=9 {
            rig.set_block(2, y, z, "minecraft:stone");
        }
        let source = BlockPos::new(0, y, 0);
        rig.set_block(source.x, source.y, source.z, "minecraft:water[level=0]");
        settle(&rig, source, 400);

        assert_eq!(rig.block_state(2, y, 0), "minecraft:stone", "the wall survives");
        for z in -3..=3 {
            let state = rig.block_state(3, y, z);
            assert!(
                fluid_state_of(&state).is_none(),
                "water must not appear past a full-cube wall (x = 3, z = {z}), found {state}"
            );
        }
        assert!(
            fluid_state_of(&rig.block_state(1, y, 0)).is_some(),
            "control: the cell before the wall is wet, so the flow did run"
        );
        // And the other three directions are unaffected by the wall.
        assert!(fluid_state_of(&rig.block_state(-3, y, 0)).is_some());
    }

    /// [`covers_unit_square`]'s exactness, since it is the one piece of
    /// [`can_pass_through_wall`] that is a reduction rather than a
    /// transliteration.
    #[test]
    fn face_coverage_is_exact_for_partial_and_complementary_rects() {
        assert!(covers_unit_square(&[(0.0, 0.0, 1.0, 1.0)]), "one full face covers");
        assert!(!covers_unit_square(&[]), "nothing covers nothing");
        assert!(
            !covers_unit_square(&[(0.0, 0.0, 0.5, 1.0)]),
            "half a face does not cover"
        );
        assert!(
            covers_unit_square(&[(0.0, 0.0, 0.5, 1.0), (0.5, 0.0, 1.0, 1.0)]),
            "two complementary halves cover, with no gap at the seam"
        );
        assert!(
            !covers_unit_square(&[(0.0, 0.0, 0.4, 1.0), (0.5, 0.0, 1.0, 1.0)]),
            "a 0.1-wide gap is detected — a 16x16 raster would too, but a \
             coarser one would not"
        );
        assert!(
            !covers_unit_square(&[(0.0, 0.0, 0.09375, 1.0), (0.09375, 0.0, 0.9, 1.0)]),
            "a non-sixteenth gap at the far edge is detected"
        );
    }

    /// The seeding hook's shape: only a position that already holds a fluid is
    /// scheduled, each at *that* fluid's own tick delay rather than a fixed
    /// number — the delay for the lava neighbour must differ from the delay
    /// for the water cell being edited, or the two are copying one delay
    /// rather than reading each position.
    #[test]
    fn ticks_after_edit_schedules_only_positions_already_holding_a_fluid() {
        let rig = Rig::flat();
        let pos = BlockPos::new(5, 60, -7);
        rig.set_block(pos.x, pos.y, pos.z, "minecraft:water[level=0]");
        let lava_neighbour = BlockPos::new(5, 60, -8);
        rig.set_block(
            lava_neighbour.x,
            lava_neighbour.y,
            lava_neighbour.z,
            "minecraft:lava[level=0]",
        );
        // The other five neighbours are left as the flat rig's own air, so
        // they must not appear below at all.

        let pending = ticks_after_edit(&rig, FluidEnv::OVERWORLD, pos);

        assert!(pending.iter().all(|t| t.kind == TICK_FLUID));
        let mut by_pos: Vec<((i32, i32, i32), u64)> =
            pending.iter().map(|t| (t.pos, t.trigger_tick)).collect();
        by_pos.sort_unstable();
        assert_eq!(
            by_pos,
            vec![
                ((5, 60, -8), FluidEnv::OVERWORLD.tick_delay(FluidKind::Lava)),
                ((5, 60, -7), FluidEnv::OVERWORLD.tick_delay(FluidKind::Water)),
            ],
            "only the two fluid-holding positions are scheduled, each at its own \
             fluid's delay — the five dry neighbours must be absent entirely"
        );
        assert_ne!(
            FluidEnv::OVERWORLD.tick_delay(FluidKind::Lava),
            FluidEnv::OVERWORLD.tick_delay(FluidKind::Water),
            "premise: the two delays must differ, or this cannot tell a per-position \
             delay from one copied off the edited cell"
        );
    }

    /// An edit with no fluid anywhere in its blast radius schedules nothing at
    /// all — the direct counterpart of the fixed defect, where every one of
    /// the seven positions used to get a tick regardless of what it held.
    #[test]
    fn ticks_after_edit_on_an_entirely_dry_edit_schedules_nothing() {
        let rig = Rig::flat();
        let pending = ticks_after_edit(&rig, FluidEnv::OVERWORLD, BlockPos::new(0, FLOOR_Y + 5, 0));
        assert!(pending.is_empty(), "no fluid anywhere near the edit: {pending:?}");
    }

    /// A position holding no fluid must be a silent no-op — world state can
    /// change between a tick being scheduled and it coming due. A version that
    /// panicked or wrote something here would make every player edit corrupt
    /// terrain.
    #[test]
    fn a_fluid_tick_on_dry_land_changes_nothing() {
        let rig = Rig::flat();
        let mut queue: ScheduledTickQueue<String> = ScheduledTickQueue::new();
        let mut changes = Vec::new();
        for pos in [
            BlockPos::new(0, FLOOR_Y, 0),
            BlockPos::new(0, FLOOR_Y + 1, 0),
            BlockPos::new(0, FLOOR_Y + 40, 0),
        ] {
            run_scheduled_tick(&rig, FluidEnv::OVERWORLD, pos, &mut queue, 1, &mut changes);
        }
        assert!(changes.is_empty(), "no fluid, no writes: {changes:?}");
        assert!(queue.is_empty(), "no fluid, nothing rescheduled");
        assert_eq!(rig.block_state(0, FLOOR_Y, 0), "minecraft:stone");
    }

    // -----------------------------------------------------------------------
    // Waterlogging: the real waterloggable-block interface accepts the *source* fluid only
    // -----------------------------------------------------------------------

    const SLAB_DRY: &str = "minecraft:oak_slab[type=bottom,waterlogged=false]";
    const SLAB_WET: &str = "minecraft:oak_slab[type=bottom,waterlogged=true]";

    /// One cell, described the way a failing waterlog gate wants to read: the
    /// block name plus either its `waterlogged` value or its fluid level.
    ///
    /// Not a state-string comparison, deliberately — the property order a
    /// `ChunkColumn` reads back is not this module's to promise, and a mismatch
    /// message has to say *what went wrong* rather than print two long strings.
    fn describe(state: &str) -> String {
        if let Some(waterlogged) = property_of(state, "waterlogged") {
            return format!("{} waterlogged={waterlogged}", base_name(state));
        }
        match fluid_state_of(state) {
            Some(fluid) => format!("{} level {}", base_name(state), fluid.legacy_level()),
            None => base_name(state).to_owned(),
        }
    }

    /// A one-cell-wide east–west trench through the flat rig, walled at
    /// `y` on both `z` sides and capped at both ends.
    ///
    /// The confinement is what makes the reach gate below a *prediction* rather
    /// than a shape: in the open, four directions tie on
    /// [`slope_distance`] and the flow is a 2D disc whose footprint nobody can
    /// state from the real drop-off alone. Walls at `y` only, never at `y + 1` — a
    /// roofed cell would make the flow `falling`, which spreads at 7 regardless
    /// of its own amount and would mask exactly the arithmetic under test.
    fn trench(rig: &Rig, y: i32, from_x: i32, to_x: i32) {
        for x in (from_x - 1)..=(to_x + 1) {
            rig.set_block(x, y, 1, "minecraft:stone");
            rig.set_block(x, y, -1, "minecraft:stone");
        }
        rig.set_block(from_x - 1, y, 0, "minecraft:stone");
        rig.set_block(to_x + 1, y, 0, "minecraft:stone");
    }

    /// Settles the fluid queue from `seeds` and returns every **distinct**
    /// position written, sorted.
    ///
    /// The footprint is the counter the cascade needs: a flood is visible as a
    /// set of positions whose size can be predicted from the real drop-off, where
    /// "the water went too far" is a judgement about a screenshot.
    fn settle_footprint(rig: &Rig, seeds: &[BlockPos], max_ticks: u64) -> Vec<(i32, i32, i32)> {
        let mut queue: ScheduledTickQueue<String> = ScheduledTickQueue::new();
        for seed in seeds {
            for pending in ticks_after_edit(rig, FluidEnv::OVERWORLD, *seed) {
                queue.schedule(pending.pos, pending.kind, pending.trigger_tick, pending.priority);
            }
        }
        let mut written: Vec<(i32, i32, i32)> = Vec::new();
        let mut changes = Vec::new();
        for tick in 1..=max_ticks {
            let due = queue.drain_due(tick, usize::MAX);
            if due.is_empty() && queue.is_empty() {
                break;
            }
            for entry in due {
                let pos = BlockPos::new(entry.pos.0, entry.pos.1, entry.pos.2);
                changes.clear();
                run_scheduled_tick(rig, FluidEnv::OVERWORLD, pos, &mut queue, tick, &mut changes);
                written.extend(changes.iter().map(|(pos, _)| (pos.x, pos.y, pos.z)));
            }
        }
        written.sort_unstable();
        written.dedup();
        written
    }

    /// **The headline waterlog gate.** A *flowing* water state reaching a
    /// waterloggable block must leave it dry, and must therefore stop there.
    ///
    /// The rule is one instance comparison in the real engine and it is easy to
    /// read past:
    /// the real can-place-liquid check is an exact instance comparison against
    /// the water source, and a
    /// flowing state's own type is the flowing-water instance — a **different**
    /// fluid instance. So
    /// the real can-hold-specific-fluid check is false for every flowing state, the
    /// direction never enters the real "get spread" map, and the real place-liquid
    /// step's own
    /// instance check refuses it a second time.
    ///
    /// **A source arm cannot see this**: the real engine waterlogs a container when the
    /// new liquid *is* a source, so both hypotheses agree there — see
    /// [`a_source_spreading_into_a_container_still_waterlogs_it`], which is the
    /// same rig with the discriminating input removed.
    ///
    /// Both hypotheses are computed from outside constants. The slab sits four
    /// cells east of the source, so with a real drop-off of `1` the flow arrives
    /// there at `amount = 5` and the two answers differ at `x = 3` and `x = 4`:
    ///
    /// | | `x = 3` | `x = 4` | footprint |
    /// |---|---|---|---|
    /// | correct | `level = 3` (`amount = 5`) | slab dry | 10 cells |
    /// | waterlogs on flow | `level = 1` (`amount = 7`) | slab **wet** | 11 cells |
    ///
    /// `x = 3` is the discriminating cell and it is worth saying why, because the
    /// reasoning is the reverse of what the symptom suggests: the waterlogged slab
    /// reads as a *source*, so `getNewLiquid` at `x = 3` sees `amount = 8` beside
    /// it and **refills** `x = 3` from `5` to `7`. The relay runs *backwards* into
    /// the flow it came from, not only outward.
    ///
    /// Measured, and it corrects the obvious guess: `x >= 5` stays **air under
    /// both hypotheses** in this rig, so those cells are not discriminating. The
    /// relay needs a cell that already holds fluid, because
    /// [`run_scheduled_tick`]'s notify loop schedules a neighbour only when it
    /// does — east of the slab is air, which is never scheduled and so never
    /// evaluates its own new-liquid derivation. In open terrain, where the flow wraps
    /// around the container and arrives on its far side as real water, that limit
    /// does not apply and the refill continues outward; the trench isolates the
    /// arithmetic instead.
    ///
    /// The west half is unobstructed in both, and pins the reach at the seven
    /// cells the real drop-off predicts — so a failure that shortened *every* flow
    /// could not pass by shortening the east one.
    #[test]
    fn flowing_water_must_not_waterlog_a_container() {
        let rig = Rig::flat();
        let y = FLOOR_Y + 1;
        trench(&rig, y, -10, 20);
        rig.set_block(4, y, 0, SLAB_DRY);
        let source = BlockPos::new(0, y, 0);
        rig.set_block(source.x, source.y, source.z, "minecraft:water[level=0]");

        let footprint = settle_footprint(&rig, &[source], 600);

        // Expected cells, built from a real drop-off of `1` and the instance
        // comparison above rather than from this module's output.
        let mut expected: Vec<(i32, String)> = vec![(0, "minecraft:water level 0".to_owned())];
        for d in 1..=3 {
            expected.push((d, format!("minecraft:water level {d}")));
        }
        expected.push((4, "minecraft:oak_slab waterlogged=false".to_owned()));
        for x in 5..=13 {
            expected.push((x, "minecraft:air".to_owned()));
        }
        for d in 1..=7 {
            expected.push((-d, format!("minecraft:water level {d}")));
        }
        for x in -10..=-8 {
            expected.push((x, "minecraft:air".to_owned()));
        }

        // Both arms go into one collection, and the single assertion comes after.
        // An `assert!` between them would abort on the cell mismatch and leave
        // the footprint an argument rather than an observation — so the neuter
        // could only ever demonstrate one of the two.
        let mut mismatches: Vec<String> = Vec::new();
        for (x, want) in &expected {
            let got = describe(&rig.block_state(*x, y, 0));
            if &got != want {
                mismatches.push(format!("x = {x}: expected {want}, found {got}"));
            }
        }

        // The footprint, as a count with a verdict depending on the count: three
        // cells east plus seven west, and the slab is never written at all.
        // Waterlogging on flow writes the slab too, for 11.
        let expected_footprint: Vec<(i32, i32, i32)> = (-7..=3)
            .filter(|x| *x != 0)
            .map(|x| (x, y, 0))
            .collect();
        if footprint != expected_footprint {
            mismatches.push(format!(
                "footprint: expected {} cells (x = -7..=-1 and 1..=3), found {} — {footprint:?}",
                expected_footprint.len(),
                footprint.len()
            ));
        }

        assert!(
            mismatches.is_empty(),
            "a flowing state waterlogged the container. Under that hypothesis the \
             slab reads waterlogged=true and, because a waterlogged block reads as a \
             source, x = 3 refills from level 3 to level 1 and the footprint grows to \
             11; under vanilla's `type == Fluids.WATER` the flow stops dry at x = 3. \
             Mismatches:\n  {}",
            mismatches.join("\n  ")
        );
    }

    /// The other half of the same rule, and the arm that stops the fix
    /// over-correcting into "waterlogging never happens".
    ///
    /// The real engine really does waterlog a container from the spread path — what
    /// decides it is the new-liquid derivation's answer **at the target**, not
    /// what the flow
    /// started as. Two adjacent sources over a solid floor make the cell between
    /// them a source (the new-liquid derivation's first rule), that source *is*
    /// the water instance, and the real place-liquid step accepts it.
    ///
    /// Both hypotheses agree here — which is the point. This input cannot see the
    /// bug, and is exactly why it shipped.
    #[test]
    fn a_source_spreading_into_a_container_still_waterlogs_it() {
        let rig = Rig::flat();
        let y = FLOOR_Y + 1;
        trench(&rig, y, 0, 2);
        rig.set_block(1, y, 0, SLAB_DRY);
        rig.set_block(0, y, 0, "minecraft:water[level=0]");
        rig.set_block(2, y, 0, "minecraft:water[level=0]");

        settle_footprint(&rig, &[BlockPos::new(0, y, 0), BlockPos::new(2, y, 0)], 400);

        assert_eq!(
            describe(&rig.block_state(1, y, 0)),
            "minecraft:oak_slab waterlogged=true",
            "a container whose own `getNewLiquid` is a source must still be \
             waterlogged — the slab is still a slab, never replaced by water"
        );
        // And the block survives: the real "spread to" step must not fall
        // through to an ordinary block write
        // for a container, in either branch.
        assert_eq!(base_name(&rig.block_state(1, y, 0)), "minecraft:oak_slab");
    }

    /// A waterlogged block must keep reading as a water **source** for its
    /// neighbours' new-liquid derivation — the real waterloggable-block
    /// interface's own
    /// fluid-state query returns the water source.
    ///
    /// This is the arm a fix that removed waterlogging from
    /// [`fluid_state_of`] would fail. A thin flowing cell beside a waterlogged
    /// slab must be *refilled* to `amount = 8 - dropOff = 7`, not decay: the
    /// discriminating input is `level = 6` (`amount = 2`), which is neither the
    /// refilled value nor air, so a detector that did nothing at all is visible.
    #[test]
    fn a_waterlogged_block_still_reads_as_a_source_for_its_neighbours() {
        let rig = Rig::flat();
        let y = FLOOR_Y + 1;
        trench(&rig, y, 0, 2);
        rig.set_block(0, y, 0, SLAB_WET);
        rig.set_block(1, y, 0, "minecraft:water[level=6]");

        let mut queue: ScheduledTickQueue<String> = ScheduledTickQueue::new();
        let mut changes = Vec::new();
        run_scheduled_tick(
            &rig,
            FluidEnv::OVERWORLD,
            BlockPos::new(1, y, 0),
            &mut queue,
            1,
            &mut changes,
        );

        assert_eq!(
            describe(&rig.block_state(1, y, 0)),
            "minecraft:water level 1",
            "the waterlogged slab is amount 8 for `getNewLiquid`, so its \
             neighbour refills to amount 7 (level 1) rather than staying at \
             level 6 or draining"
        );
        // The slab itself originates nothing. This pins **our documented
        // reduction, not vanilla** — see the "named gaps" in this module's doc:
        // vanilla would run `spread` from this position as a source. The arm is
        // here so the reduction is a measured fact with a name rather than an
        // accident, and so that removing the early return fails a test instead of
        // silently changing how far water goes.
        let mut slab_changes = Vec::new();
        run_scheduled_tick(
            &rig,
            FluidEnv::OVERWORLD,
            BlockPos::new(0, y, 0),
            &mut queue,
            2,
            &mut slab_changes,
        );
        assert!(
            slab_changes.is_empty(),
            "a waterlogged block is not a fluid block and must not spread: {slab_changes:?}"
        );
        assert_eq!(describe(&rig.block_state(0, y, 0)), "minecraft:oak_slab waterlogged=true");
    }
}
