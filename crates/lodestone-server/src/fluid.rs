//! Fluid spread — water and lava flowing, on the scheduled-tick queue.
//!
//! # What this is
//!
//! A port of `net/minecraft/world/level/material/{FlowingFluid,WaterFluid,LavaFluid}.java`
//! plus the two pieces of `net/minecraft/world/level/block/LiquidBlock.java` that
//! drive them (`shouldSpreadLiquid`, and the `onPlace`/`neighborChanged`
//! scheduling), read out of `.cache/mc/26.2/src` as record definitions.
//!
//! Before this landed, nothing in this crate ticked a fluid at all. The
//! *classification* side was well covered — [`crate::chunk::is_water`],
//! [`crate::random_tick::has_full_fluid`] — so a placed water source was
//! correctly recognised as water and then sat there as a single cube forever.
//! `crate::tick::run_tick_loop`'s `fluid_ticks.drain_due` loop had an empty body
//! with a comment saying so.
//!
//! # The algorithm, in the order vanilla evaluates it
//!
//! [`run_scheduled_tick`] is `FlowingFluid.tick` (`FlowingFluid.java:427-444`):
//!
//! 1. if the cell is **not** a source, recompute what it *should* be with
//!    [`new_liquid`] (`getNewLiquid`, `:157-193`) and rewrite it — to air if the
//!    answer is empty, otherwise to the new level plus a fresh scheduled tick;
//! 2. then [`spread`] (`:117-137`) — try **down** first, and only fall back to
//!    the four horizontal directions when down is blocked (or when this is a
//!    source, or when the cell below is not a hole);
//! 3. `spreadToSides` (`:139-155`) consults [`spread_targets`] (`getSpread`,
//!    `:363-403`), which is the slope-finding: every horizontal direction is
//!    scored by [`slope_distance`] (`getSlopeDistance`, `:279-303`) — how many
//!    steps until a hole — and only the **joint minimum** directions receive
//!    fluid. That is why water on flat ground spreads outward evenly but water
//!    beside a one-block pit flows *only* toward the pit.
//!
//! Nothing here draws from an RNG, and that is a property of the algorithm
//! rather than of this port: fluid spread in vanilla is fully deterministic.
//! There is exactly one RNG consumer in the whole family and it is a *delay*,
//! not a decision — see "the named gaps" below.
//!
//! # The numbers, which are per-fluid and per-dimension
//!
//! | | `getDropOff` | `getSlopeFindDistance` | `getTickDelay` |
//! |---|---|---|---|
//! | water (`WaterFluid.java:106,90,110`) | 1 | 4 | 5 |
//! | lava, overworld (`LavaFluid.java:163,146,171`) | 2 | 2 | 30 |
//! | lava, nether (`fast_lava`) | 1 | 4 | 10 |
//!
//! `getDropOff` is what fixes the reach: a source has `amount = 8`, each step
//! horizontally costs `dropOff`, and an amount of `0` is empty. So **water
//! reaches 7 cells** from a source on flat ground (amounts 7,6,5,4,3,2,1) and
//! **overworld lava reaches 3** (amounts 6,4,2). Those two counts are the whole
//! visible signature of this module and `flat_ground_water_spread_matches_vanilla_drop_off`
//! predicts them from the table above rather than from our own output.
//!
//! `fast_lava` is `EnvironmentAttributes.FAST_LAVA`, a dimension attribute — not
//! a gamerule and not a difficulty. This crate hosts the overworld only
//! ([`FluidEnv::OVERWORLD`]); [`FluidEnv::NETHER`] exists so the arithmetic is
//! written down once rather than rediscovered when a second dimension lands.
//!
//! # The level encoding, which is the easiest thing here to get backwards
//!
//! The *block* carries `level` in `0..=15`; the *fluid* carries `amount` in
//! `1..=8` plus a `falling` flag. `FlowingFluid.getLegacyLevel` (`:446-448`) and
//! `LiquidBlock`'s `stateCache` (`LiquidBlock.java:67-77`, read back by
//! `getFluidState`, `:120-124`) are the two halves of the mapping:
//!
//! | block `level` | fluid |
//! |---|---|
//! | `0` (also a bare `minecraft:water`) | source, `amount = 8` |
//! | `1..=7` | flowing, `amount = 8 - level` |
//! | `8..=15` | **falling**, `amount = 8` |
//!
//! So `level` counts *down* from a full source, and `level=1` is the wettest
//! flowing state rather than the driest. `getFluidState` clamps with
//! `Math.min(level, 8)`, which is why `9..=15` are all the same falling state.
//!
//! # The named gaps
//!
//! Each of these is a deliberate reduction, not an oversight, and each is chosen
//! so the error direction is inert rather than plausible-looking:
//!
//! * **`LavaFluid.getSpreadDelay`'s RNG quadrupling** (`LavaFluid.java:174-185`)
//!   is not modelled. It multiplies the delay by 4 with probability 3/4 when a
//!   *non-falling* lava cell's height **rises**, and this crate's fluid tick has
//!   no RNG in scope (the tick loop's lives inside `RandomTickScheduler`). It
//!   affects lava's timing while deepening and never the final pattern, so the
//!   consequence is lava that settles slightly faster than vanilla, not lava
//!   that settles somewhere else.
//! * **`beforeDestroyingBlock`** is a plain overwrite here. Vanilla drops the
//!   destroyed block's loot for water (`WaterFluid.java:80-83`) and plays the
//!   `1501` fizz level-event for lava (`LavaFluid.java:186-188`). We destroy the
//!   block correctly and emit neither the drop nor the sound.
//! * **`shouldSpreadLiquid` runs at tick time, not at edit time.** Vanilla calls
//!   it from `LiquidBlock.onPlace`/`neighborChanged`; [`run_scheduled_tick`]
//!   evaluates it as its own first step instead. Same outcome, one scheduled-tick
//!   delay later, and it keeps the whole family reachable through one entry
//!   point.
//! * **Bubble columns** (`LiquidBlock.tick`, `shouldBubbleColumnOccupy`) are not
//!   modelled at all — this crate has no `BubbleColumnBlock`.
//! * **A waterlogged block does not originate a spread here.**
//!   [`run_scheduled_tick`] returns early unless the block is
//!   `minecraft:water`/`minecraft:lava`, so a waterlogged slab is a source for
//!   *reading* ([`fluid_state_of`], and so for every neighbour's
//!   [`new_liquid`]) but never runs [`spread`] itself. Vanilla does: `FlowingFluid.tick`
//!   takes a position rather than a `LiquidBlock`, `SimpleWaterloggedBlock.placeLiquid`
//!   schedules a tick at the container's own position, and 50 waterloggable block
//!   classes schedule `Fluids.WATER` there from `updateShape`. The consequence is
//!   water reaching *fewer* cells than vanilla past a waterlogged block, never
//!   more — chosen so the error is inert rather than a flood.
//!
//! # How to change it
//!
//! The geometry predicates are the part most likely to need work, and they are
//! the part that is a *reduction* rather than a transliteration:
//! [`can_pass_through_wall`] is `Shapes.mergedFaceOccludes` evaluated over
//! `lodestone_data::collision_shapes`' axis-aligned box lists with an exact
//! coordinate-sweep coverage test, because those boxes are all vanilla's own
//! `toAabbs()` output. It is exact for a static shape and **wrong for a
//! neighbour-dependent one** (stairs, fences, walls, panes): our census is keyed
//! by block state, and vanilla's `getCollisionShape` for those consults the
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
/// One kind for both fluids deliberately. Vanilla keys `fluidTicks` by the
/// `Fluid` *instance*, so water and lava at one position could in principle hold
/// two independent pending ticks — but they cannot both occupy one block, so the
/// distinction is unobservable, and collapsing it means
/// [`ScheduledTickQueue::has_scheduled`]'s `(pos, kind)` dedup does exactly what
/// `LevelChunkTicks.ticksPerPosition` does.
pub const TICK_FLUID: &str = "lodestone:fluid";

/// Vanilla's own horizontal iteration order, `Direction.Plane.HORIZONTAL`
/// (`Direction.java:577`): **north, east, south, west**.
///
/// Not load-bearing for any result here and written down anyway. Every
/// horizontal loop in `FlowingFluid` accumulates a `max` ([`new_liquid`]), a
/// `min` ([`slope_distance`]) or a `<=`-keeps-ties set ([`spread_targets`]), all
/// three of which are order-independent — so a wrong order would be invisible,
/// which is exactly why it is worth pinning rather than guessing.
const HORIZONTAL: [Direction; 4] =
    [Direction::North, Direction::East, Direction::South, Direction::West];

/// `LiquidBlock.POSSIBLE_FLOW_DIRECTIONS` (`LiquidBlock.java:56-58`), the set
/// `shouldSpreadLiquid` walks: **down, south, north, east, west** — no `up`.
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

/// Which fluid a cell holds. `EMPTY` is `Option::None` at every call site rather
/// than a third variant, matching how `FluidState.isEmpty` reads in practice.
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

/// One entry of vanilla's **fluid registry** — the distinction [`FluidKind`]
/// deliberately collapses. `minecraft:water` and `minecraft:flowing_water` are
/// two different `Fluid` *instances* (`WaterFluid.getSource`/`getFlowing`), and
/// `Fluid.isSame` is what treats them as one family.
///
/// It exists for exactly one predicate, and that predicate is load-bearing:
/// `SimpleWaterloggedBlock.canPlaceLiquid` is `type == Fluids.WATER`, an
/// instance comparison, so **no flowing state can ever waterlog a container**.
/// Passing a `FluidKind` there instead loses the distinction and waterlogs on
/// every flow — and because [`fluid_state_of`] correctly reports a
/// `waterlogged=true` block as a *source*, each newly waterlogged block then
/// feeds its neighbours at `amount = 8`. The level never decrements, so the
/// spread has no bound at all: the reach stops being `8 / getDropOff` cells and
/// becomes the size of the waterloggable terrain, at one block write and one
/// scheduled tick per cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FluidType {
    /// Which family — `Fluid.isSame`'s equivalence class.
    pub kind: FluidKind,
    /// `true` for `minecraft:water`/`minecraft:lava`, `false` for
    /// `minecraft:flowing_water`/`minecraft:flowing_lava`.
    pub source: bool,
}

impl FluidType {
    /// `WaterFluid.getSource` / `LavaFluid.getSource`.
    #[must_use]
    pub const fn source(kind: FluidKind) -> FluidType {
        FluidType { kind, source: true }
    }

    /// `WaterFluid.getFlowing` / `LavaFluid.getFlowing`.
    #[must_use]
    pub const fn flowing(kind: FluidKind) -> FluidType {
        FluidType { kind, source: false }
    }
}

/// One cell's fluid state — `net.minecraft.world.level.material.FluidState`
/// reduced to the two properties that decide spreading.
///
/// `amount` is `1..=8` (vanilla's `getAmount`); `8` with `falling == false` is a
/// source. See this module's own doc comment for the `level` ⇄ `(amount,
/// falling)` mapping, which is the easiest thing here to invert by accident.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FluidState {
    /// Which fluid.
    pub kind: FluidKind,
    /// `1..=8`. Vanilla's `FluidState.getAmount`.
    pub amount: u8,
    /// `FlowingFluid.FALLING` — set on a cell fed from directly above, which
    /// makes it spread sideways at the full `7` regardless of its own amount
    /// (`spreadToSides`, `FlowingFluid.java:140-143`).
    pub falling: bool,
}

impl FluidState {
    /// A source: `amount == 8` and not falling.
    ///
    /// The `!falling` half is not redundant. `getFlowing(8, true)` — the state a
    /// cell directly under a fluid takes — also has `amount == 8`, and it is
    /// **not** a source: `WaterFluid.Flowing.isSource` returns `false`
    /// unconditionally (`WaterFluid.java:143-146`). Treating it as one would
    /// make a waterfall's column self-sustaining and it would never drain.
    #[must_use]
    pub fn is_source(self) -> bool {
        self.amount == 8 && !self.falling
    }

    /// `FluidState.getType` — which fluid-registry instance this state belongs
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

    /// `FluidState.isFull` — `amount == 8`, falling included
    /// (`FluidState.java:57-59`).
    #[must_use]
    pub fn is_full(self) -> bool {
        self.amount == 8
    }

    /// `Fluid.getOwnHeight` — `amount / 9.0` (`FlowingFluid.java:497-499`).
    /// Note the divisor is **9**, not 8, so even a full non-stacked fluid is
    /// `0.888…` tall rather than `1.0`.
    #[must_use]
    pub fn own_height(self) -> f32 {
        f32::from(self.amount) / 9.0
    }

    /// `FlowingFluid.getLegacyLevel` (`FlowingFluid.java:446-448`) — the
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
    /// this fluid — `createLegacyBlock` (`WaterFluid.java:97-99`).
    #[must_use]
    pub fn block_state(self) -> String {
        format!("{}[level={}]", self.kind.block_name(), self.legacy_level())
    }
}

/// The two dimension-dependent constants `FlowingFluid` reads off the level, and
/// the two gamerules `canConvertToSource` reads.
///
/// `fast_lava` is `EnvironmentAttributes.FAST_LAVA` (`LavaFluid.java:265-267`),
/// a **dimension attribute** — nether lava is four times cheaper to cross and
/// three times faster. The two conversion flags are `GameRules`
/// `water_source_conversion` (default `true`) and `lava_source_conversion`
/// (default `false`), verified in `gamerules/GameRules.java:92,47`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FluidEnv {
    /// `EnvironmentAttributes.FAST_LAVA`.
    pub fast_lava: bool,
    /// `GameRules.WATER_SOURCE_CONVERSION`, vanilla default `true`.
    pub water_source_conversion: bool,
    /// `GameRules.LAVA_SOURCE_CONVERSION`, vanilla default `false`.
    pub lava_source_conversion: bool,
    /// Lowest world `y` that exists — the dimension's build-height floor.
    ///
    /// **Load-bearing, not decoration.** Every step of the spread reads the cell
    /// *below* the one it is looking at, so a fluid resting on the bottom of the
    /// world asks for `min_y - 1`. `ChunkColumn::block_state` indexes unguarded
    /// and **panics** there, and the panic would land on the world tick thread.
    /// [`block_at`] answers air outside these bounds instead, which is also
    /// vanilla's own behaviour (`Level.getBlockState` returns `VOID_AIR` when
    /// `isOutsideBuildHeight`).
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

    /// `true` iff `y` is inside this dimension's build height — vanilla's
    /// `LevelHeightAccessor.isInsideBuildHeight`.
    #[must_use]
    const fn contains_y(self, y: i32) -> bool {
        y >= self.min_y && y < self.min_y + self.height
    }

    /// `getDropOff` — how much `amount` one horizontal step costs
    /// (`WaterFluid.java:104-107`, `LavaFluid.java:161-164`).
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

    /// `getSlopeFindDistance` — how far [`slope_distance`] looks for a hole
    /// (`WaterFluid.java:88-91`, `LavaFluid.java:144-147`).
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

    /// `getTickDelay`, in game ticks (`WaterFluid.java:108-111`,
    /// `LavaFluid.java:169-172`).
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

    /// `canConvertToSource` — whether two adjacent sources make a third
    /// (`WaterFluid.java:75-78`, `LavaFluid.java:190-193`).
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
/// height — vanilla's `Level.getBlockState`, whose own first line is
/// `isOutsideBuildHeight(pos) ? Blocks.VOID_AIR.defaultBlockState() : …`.
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

/// `BlockState.getFluidState` — the fluid a block state holds, or `None` for a
/// block with no fluid.
///
/// Three producers, matching `LiquidBlock.getFluidState` (`LiquidBlock.java:120-124`)
/// and `SimpleWaterloggedBlock`:
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
    // `LiquidBlock.getFluidState`'s `Math.min(level, 8)` is why 9..=15 all read
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

/// `true` iff `state`'s fluid is `kind` — `Fluid.isSame`
/// (`WaterFluid.java:101-103`), which treats a fluid and its flowing twin as one.
fn is_same_fluid(state: &str, kind: FluidKind) -> bool {
    fluid_state_of(state).is_some_and(|fluid| fluid.kind == kind)
}

/// `FlowingFluid.isSourceBlockOfThisType` (`FlowingFluid.java:341-343`).
fn is_source_of_type(state: &str, kind: FluidKind) -> bool {
    fluid_state_of(state).is_some_and(|fluid| fluid.kind == kind && fluid.is_source())
}

// ---------------------------------------------------------------------------
// Bucket place/pickup — issue #578's remainder. This crate ticks fluid
// already in the world; these two functions are the missing entry point a
// dispenser (or, eventually, a player's direct use) needs to *start* one.
// Water and lava only — see `crate::redstone_dispenser`'s own behaviour
// table for why the powder-snow/fish/axolotl/tadpole buckets are not here
// (each needs an entity or block mechanic this module has nothing to do
// with fluid placement).
// ---------------------------------------------------------------------------

/// `DispensibleContainerItem.emptyContents`'s target check, reduced to the
/// case this crate can answer without a `canBeReplaced`/`Material` model: the
/// three air variants. Vanilla additionally empties onto any block whose
/// `canBeReplaced` is `true` for the fluid (a torch, tall grass, a flower,
/// …); refusing those here **under**-empties rather than over-empties —
/// naming a target as fillable that vanilla would refuse is the direction
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

/// `BucketPickup.pickupBlock`'s water/lava half: the fluid kind at
/// `target_state`, if it is a **source** (`FlowingFluid.pickupBlock` refuses
/// a flowing, non-source cell — an empty bucket dipped in a stream's middle
/// comes back empty, matching vanilla exactly).
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

/// `FluidState.getHeight` — `hasSameAbove ? 1.0 : ownHeight`
/// (`FlowingFluid.java:488-495`). Only lava's `canBeReplacedWith` needs it.
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

/// `FlowingFluid.canHoldAnyFluid`'s explicit block list
/// (`FlowingFluid.java:404-421`). Every one of these has no collision box, so
/// the `blocksMotion` test would let fluid in; vanilla names them individually.
///
/// `DoorBlock` and `BlockTags.SIGNS` are matched by suffix rather than listed,
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

/// `BlockState.blocksMotion`, out of `lodestone_data`'s jar-derived census.
///
/// `None` from the census means the state is not in the table, and the safe
/// answer is **yes it blocks** — a gap must stop fluid rather than let it
/// through a block we failed to classify.
fn blocks_motion(state: &str) -> bool {
    lodestone_data::block_solidity::blocks_motion(state_id(state)).unwrap_or(true)
}

/// `true` iff this block is a `LiquidBlockContainer` — in 26.2 that is
/// `SimpleWaterloggedBlock`, i.e. anything with a `waterlogged` property.
fn is_waterloggable(state: &str) -> bool {
    property_of(state, "waterlogged").is_some()
}

/// `FlowingFluid.canHoldAnyFluid` (`FlowingFluid.java:404-421`).
///
/// Order matters and is vanilla's: the `LiquidBlockContainer` test comes
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
    // `DoorBlock` and `BlockTags.SIGNS`, both per-wood families.
    !(name.ends_with("_door")
        || name.ends_with("_sign")
        || name.ends_with("_hanging_sign")
        || name.ends_with("_wall_sign"))
}

/// `FlowingFluid.canHoldSpecificFluid` → `LiquidBlockContainer.canPlaceLiquid`,
/// whose one 26.2 implementation is `SimpleWaterloggedBlock` and whose entire
/// body is `type == Fluids.WATER`.
///
/// **That is an instance comparison against the *source*, not a family test**,
/// and it is the only thing in the whole family stopping flowing water from
/// waterlogging every slab, fence and stair it reaches — see [`FluidType`].
/// `Fluid.isSame` would answer `true` for the flowing twin; `==` does not.
///
/// Vanilla's `canPlaceLiquid` deliberately says nothing about the block being
/// waterlogged *already*; that clause lives in `placeLiquid`, and so lives in
/// [`spread_to`] here. Putting it in this predicate instead is inert (an
/// already-waterlogged block reads as a source, so `canMaybePassThrough`
/// excludes it first) but it misplaces the rule, and the rule that matters is
/// the one above.
fn can_hold_specific_fluid(state: &str, fluid: FluidType) -> bool {
    if !is_waterloggable(state) {
        return true;
    }
    fluid == FluidType::source(FluidKind::Water)
}

/// `FlowingFluid.canHoldFluid` (`FlowingFluid.java:423-425`).
fn can_hold_fluid(state: &str, fluid: FluidType) -> bool {
    can_hold_any_fluid(state) && can_hold_specific_fluid(state, fluid)
}

/// `FluidState.canBeReplacedWith` — whether the fluid **already** at a cell
/// yields to `incoming` arriving from `direction`.
///
/// The two implementations disagree in an important way and neither is
/// symmetric:
///
/// * empty (`EmptyFluid.java:21-24`) — always yields;
/// * water (`WaterFluid.java:112-115`) — `direction == DOWN && !incoming.is(WATER)`,
///   so standing water yields **only** to lava falling into it, never to more
///   water. That single clause is what stops a fluid tick rewriting a cell with
///   the state it already holds, forever;
/// * lava (`LavaFluid.java:165-168`) — `height >= 0.4444 && incoming.is(WATER)`,
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

/// `Shapes.mergedFaceOccludes(sourceShape, targetShape, direction)`
/// (`Shapes.java:263-285`), evaluated over `lodestone_data::collision_shapes`.
///
/// # What vanilla actually computes, and how this reproduces it
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
/// vanilla's `shape == Shapes.block()` identity test.
fn is_full_cube(state: &str) -> bool {
    let boxes =
        lodestone_data::collision_shapes::collision_boxes(state_id(state)).unwrap_or(&[]);
    boxes.len() == 1
        && boxes[0].min.iter().all(|&c| c.abs() <= 1.0e-7)
        && boxes[0].max.iter().all(|&c| (c - 1.0).abs() <= 1.0e-7)
}

/// `FlowingFluid.canPassThroughWall` (`FlowingFluid.java:195-250`), minus the
/// two `SharedConstants.DEBUG_*` guards and the thread-local occlusion cache.
///
/// The three early exits are vanilla's own and they are the whole hot path: a
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

/// `FlowingFluid.canMaybePassThrough` (`FlowingFluid.java:330-339`).
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

/// `FlowingFluid.canPassThrough` (`FlowingFluid.java:318-328`) — the
/// [`can_maybe_pass_through`] test plus the fluid-specific holdability the
/// slope search needs.
///
/// The fluid it asks about is **always the flowing instance**: `getSlopeDistance`
/// is the only caller and it passes `this.getFlowing()`. The slope search is
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

/// `FlowingFluid.SpreadContext` (`FlowingFluid.java:519-536`) — the per-call
/// state and hole caches the slope search shares.
///
/// Keyed by horizontal offset from the origin only, exactly like vanilla's
/// `getCacheKey`, because [`slope_distance`] never changes `y`. The `isHole`
/// cache reads the cell *below* uncached, which is also vanilla.
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

/// `FlowingFluid.isWaterHole` (`FlowingFluid.java:305-316`) — "would fluid at
/// `top` fall out the bottom?".
///
/// Despite the name it is fluid-agnostic; lava uses it too.
fn is_water_hole(top_state: &str, bottom_state: &str, kind: FluidKind) -> bool {
    if !can_pass_through_wall(Direction::Down, top_state, bottom_state) {
        return false;
    }
    // `canHoldFluid(level, bottomPos, bottomState, this.getFlowing())` — the
    // flowing instance, so a dry slab underneath is **not** a hole even though
    // it is waterloggable.
    is_same_fluid(bottom_state, kind) || can_hold_fluid(bottom_state, FluidType::flowing(kind))
}

/// `FlowingFluid.getNewLiquid` (`FlowingFluid.java:157-193`) — what the fluid at
/// `pos` *should* be, derived only from its neighbours.
///
/// This is the function that makes the whole system converge. Three rules, in
/// vanilla's order:
///
/// 1. **two or more adjacent sources** (plus a solid or same-fluid floor, plus
///    the dimension's conversion gamerule) make this cell a source — the
///    infinite-water-pool rule;
/// 2. **fluid directly above** makes this cell `getFlowing(8, true)`, a full
///    falling cell, regardless of what is beside it;
/// 3. otherwise `highestNeighbour - dropOff`, and `<= 0` is empty.
///
/// Note rule 3 reads the highest neighbour that this cell can be reached
/// *from* — `canPassThroughWall` is consulted per direction, so a wall between
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
        // `belowState.isSolid()` — vanilla's `isSolid` is "the collision shape
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

/// `FlowingFluid.getSlopeDistance` (`FlowingFluid.java:279-303`) — how many
/// horizontal steps from `pos` until a hole, capped at `getSlopeFindDistance`.
///
/// Returns `1000` (vanilla's own sentinel) when no hole is within reach. The
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

/// `FlowingFluid.getSpread` (`FlowingFluid.java:363-403`) — which horizontal
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
        // `canHoldSpecificFluid(level, testPos, testState, newFluid.getType())`
        // — the *new liquid's own* instance, which is what makes a waterloggable
        // neighbour a candidate only when this cell's `getNewLiquid` answered
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

/// `FlowingFluid.spreadTo` (`FlowingFluid.java:264-274`) plus
/// `LavaFluid.spreadTo`'s override (`LavaFluid.java:195-208`).
///
/// Two behaviours worth naming:
///
/// * a `LiquidBlockContainer` target is **waterlogged rather than replaced**, and
///   only ever for a water **source** — `SimpleWaterloggedBlock.placeLiquid`'s
///   own guard is `!waterlogged && fluidState.is(Fluids.WATER)`, where
///   `TypedInstance.is` is instance identity, so a flowing state fails it. Note
///   both branches `return`: vanilla's `spreadTo` never falls through to
///   `setBlock` for a container, so a slab is never overwritten by water and a
///   refused `placeLiquid` writes **nothing at all**;
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

/// `FlowingFluid.sourceNeighborCount` (`FlowingFluid.java:345-357`).
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

/// `FlowingFluid.spreadToSides` (`FlowingFluid.java:139-155`).
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

/// `FlowingFluid.spread` (`FlowingFluid.java:117-137`) — down first, sides
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

/// `LiquidBlock.shouldSpreadLiquid` (`LiquidBlock.java:212-236`), inverted to
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
/// Both halves, always: `spread` reads the world back as it mutates (vanilla's
/// `setBlock` is immediate and `getNewLiquid` re-reads), so a version that only
/// collected changes and applied them afterwards would compute the *second*
/// cell of a flow against pre-flow terrain.
fn write_block<S: ChunkSource + ?Sized>(
    world: &S,
    env: FluidEnv,
    pos: BlockPos,
    state: &str,
    changes: &mut Vec<(BlockPos, String)>,
) {
    // Vanilla's `Level.setBlock` opens with the same guard and returns `false`
    // without doing anything (`isOutsideBuildHeight`). Silently dropping the
    // write is therefore faithful, and it is also the only safe answer:
    // `ChunkColumn::set_block` indexes unguarded.
    if !env.contains_y(pos.y) {
        return;
    }
    world.set_block(pos.x, pos.y, pos.z, state);
    changes.push((pos, state.to_owned()));
}

/// One due fluid tick — `FlowingFluid.tick` (`FlowingFluid.java:427-444`) with
/// `LiquidBlock.shouldSpreadLiquid` folded in front of it.
///
/// A position holding no fluid is a **silent no-op**, and that is deliberate:
/// [`ticks_after_edit`] over-schedules on purpose (it schedules the edited cell
/// and all six neighbours without reading any of them), so most of what reaches
/// this function is not a fluid at all. Filtering here rather than at schedule
/// time is what lets the seeding work across a chunk border without loading the
/// neighbour column.
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
    // vanilla** — see this module's "named gaps". `FlowingFluid.tick` takes a
    // position, not a `LiquidBlock`, and 26.2 really does schedule ticks at a
    // container's own position from two places: `SimpleWaterloggedBlock.placeLiquid`
    // ends with `level.scheduleTick(pos, fluidState.getType(), …)`, and 50
    // waterloggable block classes (`SlabBlock.updateShape`, `StairBlock`,
    // `WallBlock`, …) schedule `Fluids.WATER` at their own position while
    // `WATERLOGGED`. So vanilla spreads *out of* a waterlogged slab as a source
    // and we do not.
    //
    // Left as-is because the error direction is inert: water reaches fewer cells
    // than vanilla, never more, and removing this line without a gate on the
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
                // Vanilla falls through to `spread` with an *empty* fluid state
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
    // This is `Level.setBlock`'s flag-`2` half — `updateNeighborsAt` →
    // `LiquidBlock.neighborChanged` → `level.scheduleTick(pos, fluid,
    // getTickDelay)` (`LiquidBlock.java:190-200`). This crate has no
    // block-lifecycle callback, so the write list is the equivalent hook.
    //
    // **The neighbour half is not an optimisation, it is the only thing that
    // makes a flow drain.** Water never replaces water horizontally
    // (`WaterFluid.canBeReplacedWith` is `direction == DOWN && !other.is(WATER)`),
    // so a receding flow cannot be pushed back by the cell behind it — each cell
    // has to re-evaluate its *own* `getNewLiquid` and shrink. Measured while
    // building this: with only the written cells rescheduled, removing a source
    // left the ramp frozen at `level=3` forever, because nothing ever ticked the
    // cells the shrinking one no longer wrote to.
    //
    // The delay is the *ticking* fluid's, not each neighbour's own. Vanilla reads
    // `this.fluid` — the neighbour's block — and we would have to read the
    // neighbour to know it. The only case that differs is water and lava
    // adjacent, where a lava cell gets water's 5 instead of its own 30, so it
    // reacts sooner; `run_scheduled_tick` reschedules with the correct delay from
    // then on. `ScheduledTickQueue::schedule`'s `(pos, kind)` dedup absorbs the
    // overlap between neighbourhoods.
    // **A neighbour is only scheduled when it already holds a fluid**, and that
    // condition is the whole difference between vanilla's flow rate and twice it.
    //
    // `neighborChanged` is a method **on `LiquidBlock`** — the block at the
    // notified position — so vanilla only reaches
    // `level.scheduleTick(pos, …, getTickDelay)` when that position is itself a
    // liquid. An **air** cell's `neighborChanged` is `Block`'s default and
    // schedules no fluid tick at all.
    //
    // Scheduling air too made a falling column advance two cells per
    // `getTickDelay` instead of one. When a source spread down, the notify loop
    // handed the cell *below the newly written one* — the air the flow was about
    // to enter — a tick at the same `delay`. Both fell due in the same drain, and
    // `DRAIN_ORDER` puts the written cell first, so it spread into the air cell
    // and then the air cell, now liquid, spread one further inside that same
    // pass. Every constant was right, which is why reading `tick_delay` could not
    // find this: `FluidEnv::tick_delay` really is 5/30/10 and the queue really is
    // drained once per game tick.
    //
    // The receding case the unconditional form existed for is untouched: water
    // never replaces water horizontally, so a shrinking ramp drains only because
    // each cell re-evaluates its own `getNewLiquid`, and every one of those cells
    // *is* a fluid. It is exactly the air neighbours that were never vanilla's to
    // schedule. `changed` itself stays unconditional — that is
    // `LiquidBlock.onPlace`'s own `scheduleTick`, and on a receding write it is
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

/// The fluid ticks one block edit owes — a cell and its six neighbours, at the
/// **relative** delays [`crate::tick::run_tick_loop`] rebases onto its own
/// counter.
///
/// This is the seeding hook, and it stands in for vanilla's
/// `LiquidBlock.onPlace`/`neighborChanged`/`updateShape`, none of which this
/// crate has a block-lifecycle equivalent for. Breaking the block under an ocean
/// floor, or beside a spring, is exactly the `neighborChanged` case: the water
/// itself did not change, so only a notification can start it moving.
///
/// **It reads nothing and filters nothing**, which is the deliberate part.
/// Vanilla decides at schedule time whether the position holds a liquid;
/// [`run_scheduled_tick`] decides at run time instead. That costs up to seven
/// no-op drains per edit and buys a seeding path that works across a chunk
/// border without loading the neighbouring column — and a no-op drain schedules
/// nothing, so there is no runaway.
///
/// The delay is `1`, not the fluid's own `getTickDelay`. Vanilla's
/// `neighborChanged` passes `this.fluid.getTickDelay(level)`; using `1` here
/// makes the *first* reaction to a player's edit prompt and every subsequent
/// step pay the real delay, because [`run_scheduled_tick`] reschedules with
/// [`FluidEnv::tick_delay`]. The visible difference is one cell of flow starting
/// four ticks early once.
#[must_use]
pub fn ticks_after_edit(pos: BlockPos) -> Vec<ScheduledTick<String>> {
    // Built through a real queue rather than struct literals because
    // `ScheduledTick::sub_tick_order` is private — the same idiom
    // `server::propagate_placement` uses, and for the same reason.
    let mut pending: ScheduledTickQueue<String> = ScheduledTickQueue::new();
    pending.schedule((pos.x, pos.y, pos.z), TICK_FLUID.to_owned(), 1, TickPriority::Normal);
    for direction in [
        Direction::Down,
        Direction::Up,
        Direction::North,
        Direction::South,
        Direction::West,
        Direction::East,
    ] {
        let neighbour = direction.relative(pos);
        pending.schedule(
            (neighbour.x, neighbour.y, neighbour.z),
            TICK_FLUID.to_owned(),
            1,
            TickPriority::Normal,
        );
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
        for pending in ticks_after_edit(seed) {
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
        for pending in ticks_after_edit(source) {
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

    /// **Water falls one cell per `getTickDelay`, not two.**
    ///
    /// # Where the expected values come from
    ///
    /// Arithmetic over vanilla's own constant, not over our output.
    /// `WaterFluid.getTickDelay` is `5`. [`ticks_after_edit`] seeds the source
    /// **and its six neighbours** at delay `1`, so tick 1 reaches depth **2**,
    /// not 1: the source spreads into the cell below, and that cell already had a
    /// tick due in the same pass, so it spreads once more. That is the seed
    /// hook's own documented "one cell of flow starting four ticks early once",
    /// independent of anything below it. Every cell after that pays a full delay:
    /// depth at tick `T` is `2 + (T - 1) / 5`.
    ///
    /// The `2` was measured, not assumed — the first version of this gate
    /// predicted `1 + (T - 1) / 5` and failed at `T = 1` reporting depth 2. It is
    /// recorded here because the plausible round number was wrong in the
    /// direction that looks like a code bug.
    ///
    /// # Why these tick numbers
    ///
    /// Because the two hypotheses differ there. Scheduling the **air** cell below
    /// a freshly written one — which is what this code did before, and which
    /// vanilla never does, since `neighborChanged` is a method on `LiquidBlock`
    /// and air's default schedules nothing — let the front advance twice per
    /// delay: `1 + 2 * (T - 1) / 5`. At `T = 1` both predict depth 1, so a gate
    /// there would measure only that the code runs; from `T = 6` on they diverge
    /// and keep diverging. Both columns are tabulated so a future reader can see
    /// the discrimination rather than trust it:
    ///
    /// | tick | correct | doubled |
    /// |---|---|---|
    /// | 1 | 2 | 2 |
    /// | 6 | 3 | 4 |
    /// | 11 | 4 | 6 |
    /// | 21 | 6 | 10 |
    ///
    /// The column is 40 cells tall so the floor never caps the count within the
    /// range asserted — a capped depth would make the two hypotheses agree again
    /// and silently turn this back into a vacuous gate.
    #[test]
    fn a_falling_column_advances_one_cell_per_tick_delay() {
        const DELAY: u64 = 5;
        let source_y = FLOOR_Y + 40;
        // `1` is deliberately excluded: both hypotheses predict depth 2 there
        // (the seed cell is common to both), so it would measure only that the
        // code runs. It is asserted separately below as the seed's own premise.
        for ticks in [6_u64, 11, 21] {
            let rig = Rig::flat();
            let source = BlockPos::new(0, source_y, 0);
            rig.set_block(source.x, source.y, source.z, "minecraft:water[level=0]");

            let depth = fall_depth_after(&rig, source, ticks);

            let expected = 2 + i32::try_from((ticks - 1) / DELAY).expect("small");
            let doubled = 2 + 2 * i32::try_from((ticks - 1) / DELAY).expect("small");
            assert_eq!(
                depth, expected,
                "after {ticks} ticks a falling water column must be {expected} cells \
                 deep (one per getTickDelay={DELAY}); it is {depth}. The doubled \
                 hypothesis — scheduling the air cell below each write, so the front \
                 advances twice per delay — predicts {doubled} here."
            );
            assert_ne!(
                expected, doubled,
                "this tick count must discriminate the two hypotheses, or it is \
                 measuring nothing"
            );
            assert!(
                source_y - depth > FLOOR_Y,
                "premise: the floor must not have capped the fall at {ticks} ticks, \
                 or both hypotheses agree and this asserts nothing"
            );
        }

        // The seed's own contribution, asserted separately because the loop above
        // deliberately skips tick 1: `ticks_after_edit` schedules the source AND
        // its six neighbours at delay 1, so the very first pass reaches depth 2.
        // Pinning it means a future change to the seed hook fails here rather than
        // silently shifting every expectation above by one.
        let rig = Rig::flat();
        let source = BlockPos::new(0, source_y, 0);
        rig.set_block(source.x, source.y, source.z, "minecraft:water[level=0]");
        assert_eq!(
            fall_depth_after(&rig, source, 1),
            2,
            "the seed hook schedules the source and the cell below it in the same \
             pass, so tick 1 reaches depth 2 — the documented one-cell-early step"
        );
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
    /// two jar functions that define it — `FlowingFluid.getLegacyLevel` and
    /// `LiquidBlock`'s `stateCache`.
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
    /// exact level ramp `getDropOff` predicts, and stop at the exact distance.
    ///
    /// The expected values come from outside this code: `amount` starts at `8`
    /// (`WaterFluid.Source.getAmount`), each horizontal step costs
    /// `getDropOff() == 1` (`WaterFluid.java:104-107`), `amount <= 0` is empty
    /// (`getNewLiquid`'s last line), and the block stores `8 - amount`
    /// (`getLegacyLevel`). So the ramp along any axis is `level = 1..=7` and cell
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
    /// drop-off (`spreadToSides`' `FALLING` override).
    ///
    /// The source sits in a **walled shaft**, and that is not scene-dressing.
    /// `spread`'s fall-through is `if (fluidState.isSource() || !isWaterHole(...))
    /// spreadToSides` — so once the column below a source is established, the
    /// down branch is refused (water never replaces water) and the source spreads
    /// sideways *at its own level*, making a seven-wide sheet that then falls in
    /// seven places. That is correct vanilla behaviour and it is what this rig
    /// measured before the shaft was added; it just is not a test of the `FALLING`
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
    /// This is the assertion `getSpread`'s tie rule exists for, and it is the
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
                for pending in ticks_after_edit(seed) {
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

    /// Lava beside water is quenched — `LiquidBlock.shouldSpreadLiquid`. A
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

    /// Lava spreading **down** into water becomes stone — `LavaFluid.spreadTo`'s
    /// override, a different rule from the obsidian/cobblestone one above and
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

    /// The seeding hook's shape: seven positions, all at delay 1, deduped.
    #[test]
    fn ticks_after_edit_covers_the_cell_and_its_six_neighbours() {
        let pending = ticks_after_edit(BlockPos::new(5, 60, -7));
        assert_eq!(pending.len(), 7, "the cell plus six neighbours");
        assert!(pending.iter().all(|t| t.kind == TICK_FLUID));
        assert!(pending.iter().all(|t| t.trigger_tick == 1));
        let mut positions: Vec<(i32, i32, i32)> = pending.iter().map(|t| t.pos).collect();
        positions.sort_unstable();
        positions.dedup();
        assert_eq!(positions.len(), 7, "no duplicate positions");
        assert!(positions.contains(&(5, 60, -7)));
        assert!(positions.contains(&(5, 59, -7)));
        assert!(positions.contains(&(5, 61, -7)));
        assert!(positions.contains(&(4, 60, -7)));
        assert!(positions.contains(&(6, 60, -7)));
        assert!(positions.contains(&(5, 60, -8)));
        assert!(positions.contains(&(5, 60, -6)));
    }

    /// A position holding no fluid must be a silent no-op, since
    /// [`ticks_after_edit`] deliberately over-schedules. A version that panicked
    /// or wrote something here would make every player edit corrupt terrain.
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
    // Waterlogging: `SimpleWaterloggedBlock` accepts the *source* fluid only
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
    /// state from `getDropOff` alone. Walls at `y` only, never at `y + 1` — a
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
    /// set of positions whose size can be predicted from `getDropOff`, where
    /// "the water went too far" is a judgement about a screenshot.
    fn settle_footprint(rig: &Rig, seeds: &[BlockPos], max_ticks: u64) -> Vec<(i32, i32, i32)> {
        let mut queue: ScheduledTickQueue<String> = ScheduledTickQueue::new();
        for seed in seeds {
            for pending in ticks_after_edit(*seed) {
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
    /// The rule is one instance comparison in vanilla and it is easy to read past:
    /// `SimpleWaterloggedBlock.canPlaceLiquid` is `type == Fluids.WATER`, and a
    /// flowing state's `getType()` is `Fluids.FLOWING_WATER` — a **different**
    /// `Fluid` instance (`WaterFluid.getFlowing`/`getSource`). So
    /// `FlowingFluid.canHoldSpecificFluid` is false for every flowing state, the
    /// direction never enters `getSpread`'s map, and `placeLiquid`'s own
    /// `fluidState.is(Fluids.WATER)` refuses it a second time.
    ///
    /// **A source arm cannot see this**: vanilla waterlogs a container when the
    /// new liquid *is* a source, so both hypotheses agree there — see
    /// [`a_source_spreading_into_a_container_still_waterlogs_it`], which is the
    /// same rig with the discriminating input removed.
    ///
    /// Both hypotheses are computed from outside constants. The slab sits four
    /// cells east of the source, so with `getDropOff() == 1` the flow arrives
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
    /// evaluates its own `getNewLiquid`. In open terrain, where the flow wraps
    /// around the container and arrives on its far side as real water, that limit
    /// does not apply and the refill continues outward; the trench isolates the
    /// arithmetic instead.
    ///
    /// The west half is unobstructed in both, and pins the reach at the seven
    /// cells `getDropOff` predicts — so a failure that shortened *every* flow
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

        // Expected cells, built from `getDropOff() == 1` and the instance
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
    /// Vanilla really does waterlog a container from the spread path — what
    /// decides it is `getNewLiquid`'s answer **at the target**, not what the flow
    /// started as. Two adjacent sources over a solid floor make the cell between
    /// them a source (`getNewLiquid`'s first rule), that source *is*
    /// `Fluids.WATER`, and `placeLiquid` accepts it.
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
        // And the block survives: `spreadTo` must not fall through to `setBlock`
        // for a container, in either branch.
        assert_eq!(base_name(&rig.block_state(1, y, 0)), "minecraft:oak_slab");
    }

    /// A waterlogged block must keep reading as a water **source** for its
    /// neighbours' `getNewLiquid` — `SimpleWaterloggedBlock`'s own
    /// `getFluidState` is `Fluids.WATER.getSource(false)`.
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
