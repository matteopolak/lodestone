//! A bounded cache of generated chunk columns (`docs/plans/chunk-lifecycle.md`
//! unit **U3**).
//!
//! # What it is
//!
//! [`ChunkStore`] wraps any [`ChunkSource`] and *is* a [`ChunkSource`], so it
//! drops in wherever a source is constructed with no call-site changes
//! anywhere. It retains the columns it has been asked for, evicting the
//! least-recently-used one past a capacity bound, so a column is generated
//! **once** and thereafter read.
//!
//! # Why it exists: generation cost requires bounded retention
//!
//! [`crate::chunk::OverworldChunkSource`] retains **only edited** columns, while
//! deterministic generation can rebuild an unedited column on demand. The
//! generator combines carvers, ores, and vegetation, so repeating that work on
//! every request exceeds the 20 Hz tick budget; retaining generated columns
//! keeps reads within the bounded cache instead.
//!
//! What changed underneath it is that generation composed in carvers, ores and
//! vegetation, of which vegetation is ~62% of the cost and ore ~18%. Measured
//! here in release, four **cold** columns from four independently constructed
//! sources (so the generator's memo cache cannot absorb any of them), on a box
//! at load average 3.7:
//!
//! ```text
//! column 0: 803.0ms   column 1: 840.8ms   column 2: 1.001s   column 3: 991.4ms
//! mean: 909.2ms
//! ```
//!
//! A 20 Hz tick has a **50 ms** budget, so *one* regeneration is ~18 tick
//! budgets. `measure_real_column_generation_cost` below reproduces this.
//!
//! Two independent consumers request generated columns on repeating timers,
//! and an uncached read can starve both the world tick and a connection task:
//!
//! | site | cadence | columns per firing | task starved |
//! |---|---|---|---|
//! | [`crate::tick::run_tick_loop`]'s random-tick loop | every tick (50 ms) | the whole `tick_area` — **49** at the shell's `mob_radius.clamp(1, 3)` | the world tick |
//! | `crate::server`'s `vitals_tick` submersion probe | every 50 ms, once `player_pos` is `Some` | 1, to read a **single block** | the *connection*, i.e. chunk streaming |
//!
//! At 909 ms per column the world tick would spend ~44.5 s of generation per
//! 50 ms of budget — about **0.022 TPS** — while the connection task consumed
//! ~909 ms per 50 ms. That workload starves world ticks, saturates connection
//! I/O during movement, prevents timely view recentering, and leaves the player
//! without a loaded column beneath them. The connection-side collision probe
//! returns `PlayerCollision::Pending` while `is_chunk_loaded` is false.
//!
//! A `ChunkSource` default that answers `block_state` through `column` would
//! regenerate a whole 16×384×16 column for a one-block read. The connection
//! task begins this probe after the client reports a position, so that cost can
//! saturate streaming immediately after the join burst.
//!
//! [`ChunkStore`] therefore overrides `block_state` as well as `column`; the
//! override reads one cell out of the retained column without cloning, and
//! it is the model for the required method: a source that retains
//! columns should read a cell from them rather than regenerate.
//!
//! # How it works
//!
//! One `Mutex<Cache>` holding a `HashMap<(i32, i32), Entry>` plus a monotonic
//! use-stamp per entry. Three properties are load-bearing:
//!
//! - **Generation happens with the lock released.** A miss unlocks, calls
//!   `source.column()`, then re-locks to insert. Holding the lock across an
//!   ~909 ms generation would serialise
//!   [`crate::chunk::generate_columns_parallel`]'s whole worker-pool batch and
//!   undo the duplicated work.
//! - **An insert after that window never overwrites.** In the unlocked
//!   interval another thread may have inserted, and its entry may carry a
//!   [`set_block`](ChunkSource::set_block) edit that this thread's freshly
//!   generated column does not. First writer wins; the loser's column is
//!   dropped. (Both are otherwise byte-identical — generation is deterministic
//!   per chunk, see `generate_columns_parallel`'s doc comment.)
//! - **Eviction is lossless, so the bound needs no exception for edits.** A
//!   `set_block` is forwarded to the inner source *before* the cache is
//!   touched, and `OverworldChunkSource::edits` retains it there permanently.
//!   Dropping a cache entry therefore costs a regeneration and never a block:
//!   the regeneration goes back through `OverworldChunkSource::column`, which
//!   consults `edits` first. This is the single property that lets the store be
//!   bounded at all — `docs/plans/chunk-lifecycle.md`'s U6 needs a much more
//!   careful rule ("refuse to drop an edited column") because *it* drops the
//!   authoritative copy, where this only drops a cache.
//! - **Tick access is an atomic try operation, not a residency pre-check.**
//!   [`ChunkWriteGates`] protects the interval in which generation, light
//!   settlement, and block mutation update a coordinate. Tick-side callers use
//!   [`ChunkStore::try_resident_column`],
//!   [`ChunkStore::try_resident_block_state_id`], and
//!   [`ChunkStore::try_set_block`] to claim that gate without waiting; they get
//!   `Busy` while a generation owns it and `Absent` for a cold coordinate.
//!   The object-safe [`ChunkSource`] seam returns these same results inside
//!   `Some`; `None` means a legacy source has no atomic try capability and may
//!   keep its existing fallback. A source without a mutation-only edit ledger
//!   returns `Unsupported`, so tick code defers rather than reporting a
//!   cache-local edit as durable.
//!   The older `is_column_resident` plus blocking read/write pair remains for
//!   callers that explicitly accept loading.
//!
//! # The memory this costs, and how to change it
//!
//! Retention turns a column's representation into real resident memory, which is
//! `docs/plans/chunk-lifecycle.md`'s top risk and the reason this type is
//! bounded rather than a plain `HashMap`.
//!
//! **Measured, not assumed** (the plan's U2 question, answered here because
//! this is the unit that creates the cost). `/usr/bin/time -l` on the release
//! lib-test binary, one arm per configuration, `measure_rss_without_retention`
//! the shared control.
//!
//! ## Dense full-height storage measurement
//!
//! `16 × 384 × 16 × 2 B` = **192 KiB** per column, unconditionally — a column of
//! solid stone and a column of pure air cost the same. Three arms, the third at
//! [`MAX_CAPACITY`] so the ceiling rested on a measurement rather than on a 2.5×
//! extrapolation:
//!
//! | arm | peak RSS | delta | per column |
//! |---|---|---|---|
//! | retention off (the shared control) | 8.1 MiB | — | — |
//! | 512 retained | 105.4 MiB | 97.4 MiB | 194.8 KiB |
//! | **1,275 retained** ([`MAX_CAPACITY`]) | **250.1 MiB** | **242.0 MiB** | 194.4 KiB |
//!
//! ## Bit-packed per-section storage measurement (`crate::chunk_blocks`)
//!
//! Same three arms, same command, same box:
//!
//! | arm | peak RSS | delta | per column |
//! |---|---|---|---|
//! | retention off (the shared control) | 8.4 MiB | — | — |
//! | 512 retained | 24.0 MiB | 15.5 MiB | **31.1 KiB** |
//! | **1,275 retained** ([`MAX_CAPACITY`]) | **47.3 MiB** | **38.9 MiB** | **31.2 KiB** |
//!
//! **195.5 KiB → 31.1 KiB per retained column, a 6.3× cut**, and the rate stays
//! flat across the 2.5× range (31.1 vs 31.2), so residency stays linear in
//! the retained count. Of the 31.1 KiB, `ChunkColumn::blocks_heap_bytes` accounts
//! for ~24 KiB and the rest is palette `String`s, the 3-D biome grid (~3 KiB)
//! and the map entry; the biome grid is the second-largest resident term.
//!
//! ## Comparing the two tables is sound, and it is worth saying why
//!
//! `touched_column` uses a 48-writes-per-column shape that faults
//! every page of a contiguous 192 KiB allocation but packs to ~12 KiB under the
//! packed representation, so this synthetic fixture would exaggerate the saving
//! relative to a real column (see that function's own doc for the vacuous-test
//! warning). The two tables are nonetheless directly comparable **because
//! the dense representation's cost is independent of a column's content**: 192 KiB
//! was `vec![0u16; 16 * 16 * height]` whatever went in it, so the 194.8 KiB row is
//! valid for the packed fixture too. The fixture is calibrated against four
//! *real* generated columns (mean 24,112 packed bytes), so the packed table is not
//! flattered by its input either.
//!
//! The two arms of each table are also each other's control: a delta near zero
//! would mean the columns were dropped in both arms, or that the pages were never
//! faulted in, and the run would be a failure to measure rather than evidence that
//! residency is free.
//!
//! [`capacity_for_view_radius`] is the knob, and 15.5 MiB is what it buys at the
//! default render distance. A capacity of 128 (~4 MiB) still preserves the
//! starvation bound completely — only the
//! 49-column `tick_area` resident, and everything beyond that is avoided
//! *re*-generation as a player walks back over ground they have seen. 512 was
//! chosen to also cover the default streamed view (`render_distance` 8 ⇒
//! `view_radius` 9 ⇒ 361 columns) so that walking in a circle does not pay 909 ms
//! per column again.
//!
//! # Why the capacity is a function of the view radius
//!
//! The default capacity is 512 because it covers the default streamed view. The
//! shell serves
//! `view_radius = render_distance + 1` (`crates/lodestone-shell/src/app/session.rs`
//! — the `+ 1` is vanilla's `ChunkTrackingView` buffer ring and is correct), so
//! the streamed square is `(2 × (rd + 1) + 1)²`: 361 columns at `rd = 8`, 441 at
//! 9, **529 at 10**, **729 at vanilla's own default of 12**, 4,489 at the
//! slider's maximum of 32. One notch of the render-distance slider past 9 and the
//! a literal 512-entry cache would therefore under-provision the streamed set.
//!
//! [`capacity_for_view_radius`] derives it instead, floored at
//! [`DEFAULT_CAPACITY`] and capped at [`MAX_CAPACITY`]; both of those constants
//! carry their own argument. The gate is
//! `tests/view_radius_store_capacity.rs`, whose subject is `view_radius = 13`
//! (`render_distance` 12) precisely because a gate at the default radius is under
//! the capacity ceiling on *both* arms and can see nothing.
//!
//! # Two policies, because the ceiling is a question about whose memory it is
//!
//! The hosted ceiling is also used as a **bounded upper bound** for extreme
//! singleplayer distances. `render_distance` 32 still sizes the store at 4,539
//! columns, i.e. **139.2 MiB — measured directly** by
//! `measure_rss_at_the_singleplayer_slider_maximum` rather than extrapolated: the
//! packed representation uses 139.2 MiB at this capacity, versus 867 MiB for the
//! dense representation — the memory of the person who moved the
//! slider. At the selectable 256-chunk extreme, retaining the complete
//! 265,225-column square would be several gigabytes, so the integrated policy
//! saturates at the measured cache ceiling and lets the join scheduler stream
//! incrementally. Truncating the cache under a view they are already being streamed only buys
//! them re-generation of the ground under their feet (see
//! [`integrated_capacity_for_view_radius`] for why *innermost* rings are what a
//! short capacity drops). A hosted server spends an operator's memory on behalf
//! of players who did not choose the setting, so it keeps the cap.
//!
//! | path | constructor | policy |
//! |---|---|---|
//! | singleplayer (`open_in_memory*`) | `ChunkStore::for_integrated_view_radius` | bounded, floored at [`DEFAULT_CAPACITY`] and capped at [`MAX_CAPACITY`] |
//! | open-to-LAN (`IntegratedServer::bind`) | `ChunkStore::for_view_radius` | capped at [`MAX_CAPACITY`] |
//!
//! Packing in `crate::chunk_blocks` reduces the per-column cost rather than the
//! retained count; the table above records that measurement. Other
//! per-column terms are `Arc<ChunkSection>` copy-on-write sharing, the biome
//! grid (~3 KiB), and palette `String`s rather than the block grid.
//!
//! # The clone this keeps, deliberately
//!
//! [`ChunkSource::column`] returns a `ChunkColumn` **by value**, so a store
//! read is a deep copy (measured in the gate below, tens of microseconds) rather
//! than a refcount bump. Packed sections copy ~24 KiB instead of a flat 192 KiB,
//! while the result still uses `sections.len()` separate `Vec` allocations
//! rather than one, which is the term to watch if the clone ever shows up in a
//! profile. Handing back
//! `Arc<ChunkColumn>` instead — which the plan asks for, and which U8 wants —
//! cannot be done without either changing that signature or lending `&mut`
//! from inside the lock.
//!
//! Lending `&mut` is the trap, and it is worth writing down because it looks
//! like the obvious design: `run_tick_loop` mutates its column
//! (`random_tick::tick_chunk` takes `&mut ChunkColumn`) **and** calls
//! `world.set_block` for the same chunk in the same breath, to persist through
//! to the source. A `with_column_mut(cx, cz, f)` that holds the cache lock
//! across `f` therefore **deadlocks** on that nested `set_block`, and the
//! `try_lock` workaround silently skips a cache update on genuine contention,
//! which serves a stale block. So the closure API is not exposed even
//! privately in a re-entrant shape.
//!
//! The trade is not close: the clone is 3.1 µs (measured below) against the 909
//! ms it removes, and it needs **zero edits to `tick.rs`** — the most
//! contended file in this cluster, with concurrent redstone work in it.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::collections::hash_map::Entry as MapEntry;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, Weak};

use lodestone_data::block_states::StateId;

use crate::chunk::{
    ChunkColumn, ChunkGenerationStage, ColumnLightSettlement, ColumnLightSettlementError,
    ChunkSource,
};
use crate::chunk_lifecycle::{ChunkLifecycleHandoff, ChunkLifecyclePlan};
use crate::ticket::{TicketDelta, TicketStoreHandle};
use crate::worldgen_session::{
    AggregatePrefix, BlockCoordinate, ChunkCoordinate, FeatureSettlementProof,
    GenerationCheckpoint, GenerationSession, ImmutableProduct,
    ImmutableSidecar, ImmutableStageCompletion, MutationProvenance, ProvenanceMutation, SessionError,
    SourceCompletionRecord, TargetFeatureWrite, target_feature_write_precedes_mutation,
};
use lodestone_worldgen::stage_schedule::{
    BarrierPolicy, ChunkRequest, ColumnStage, Dimension, DimensionPipeline, GenerationTarget, PipelineIdentity,
    PipelineOptions, ResourceKey, StageFrontier, StageKey, StageRecord,
};
#[cfg(feature = "worldgen-stage-pmu")]
use lodestone_worldgen::counters::{RegionGuard, RegionPhase};
#[cfg(test)]
use crate::ticket::{TicketKind, TicketOwner};

/// The floor under [`capacity_for_view_radius`], and the capacity a radius-less
/// `ChunkStore::new` store retains before evicting the least-recently-used one.
///
/// 512 packed full-height columns measured **15.5 MiB** of resident memory (see
/// this module's memory section for the paired `/usr/bin/time -l` arms — that is
/// a measurement, not arithmetic; the dense representation measured 97.6 MiB, while
/// grid per section). It holds `run_tick_loop`'s
/// 49-column `tick_area` plus the default streamed view (`render_distance` 8 ⇒
/// `view_radius` 9 ⇒ 361 columns) with room to spare.
///
/// **It is the floor for served-connection capacity.** A bare literal is not a
/// function of the radius the connection serves, and at
/// `render_distance` 10 the streamed view alone (529 columns) passes it. See
/// [`capacity_for_view_radius`], which every constructor in
/// [`crate::integrated`] uses. This constant survives as that
/// function's **floor**, so the derivation can only ever move capacity *up*
/// from what was measured here, never down: at the default radius the store is
/// byte-for-byte the 512-column configuration (15.5 MiB in the packed
/// representation).
pub const DEFAULT_CAPACITY: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct GenerationRequestKey {
    dimension: Dimension,
    target: ChunkCoordinate,
    generation_target: GenerationTarget,
    dependency_radius: u8,
}

#[derive(Debug)]
struct GenerationInFlight {
    completed: AtomicBool,
    wake: Condvar,
    result: Mutex<Option<ChunkColumn>>,
    #[cfg(not(target_arch = "wasm32"))]
    gate: Mutex<()>,
}

impl GenerationInFlight {
    fn new() -> Self {
        Self {
            completed: AtomicBool::new(false),
            wake: Condvar::new(),
            result: Mutex::new(None),
            #[cfg(not(target_arch = "wasm32"))]
            gate: Mutex::new(()),
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn wait(&self) {
        let mut guard = self.gate.lock().expect("generation waiter lock poisoned");
        while !self.completed.load(Ordering::Acquire) {
            guard = self
                .wake
                .wait(guard)
                .expect("generation waiter lock poisoned");
        }
    }

    #[cfg(target_arch = "wasm32")]
    async fn wait_yielding(&self) {
        while !self.completed.load(Ordering::Acquire) {
            crate::chunk::yield_to_browser().await;
        }
    }

    fn complete(&self, result: Option<&crate::worldgen_session::GenerationRequestResult>) {
        let column = result.map(|result| match result {
            crate::worldgen_session::GenerationRequestResult::Existing(column) => column.clone(),
            crate::worldgen_session::GenerationRequestResult::Generated(snapshot) => {
                snapshot.column().clone()
            }
        });
        *self.result.lock().expect("generation result lock poisoned") = column;
        self.completed.store(true, Ordering::Release);
        self.wake.notify_all();
    }

    fn result(&self) -> Option<ChunkColumn> {
        self.result
            .lock()
            .expect("generation result lock poisoned")
            .clone()
    }
}

/// The columns in a square view of `radius`: the `[-radius, radius]²` window
/// `crate::server`'s `ViewTracker::window` and `join_view_rings` enumerate.
///
/// This is the *count* of what a connection actually streams, not an
/// approximation of it — both of those functions build the same square, and
/// `crate::server`'s `check_proximity_stream` gate already pins the join's
/// column total at `(2 * view_radius + 1)²`.
///
/// A negative radius is **0 columns**, matching `join_view_rings`, which returns
/// no rings rather than clamping to ring 0 (see its own doc comment for why the
/// clamp would be wrong).
///
/// Widened to `u64`/`usize` before squaring rather than after. `2 * radius + 1`
/// overflows `i32` past a radius of about 1.07 × 10⁹ and the square overflows a
/// 32-bit `usize` far sooner, and `IntegratedServer::bind` is public: the shell
/// clamps `render_distance` to 32 but a host embedding this crate has nothing
/// stopping it passing `i32::MAX`. In a `const fn` an overflow is a compile
/// error; at runtime it is a debug panic and a silently tiny capacity in release.
/// Saturating is right rather than merely safe — the result feeds
/// [`capacity_for_view_radius`], which clamps to [`MAX_CAPACITY`] anyway, so an
/// absurd radius lands on the cap instead of wrapping to nothing.
pub const fn view_columns(radius: i32) -> usize {
    if radius < 0 {
        return 0;
    }
    // `u128`, not `u64`: at `radius = i32::MAX` the side is ~2³² and the square
    // is ~2⁶⁴, which overflows `u64` itself. One more width costs nothing in a
    // `const fn` evaluated at compile time.
    let side = 2 * (radius as u128) + 1;
    let columns = side * side;
    if columns > usize::MAX as u128 {
        usize::MAX
    } else {
        columns as usize
    }
}

/// The radius of the largest concurrent scan over this store that is **not** the
/// streamed view: `crate::tick::run_tick_loop`'s random-tick `tick_area`.
///
/// The shell passes `mob_radius = view_radius.clamp(1, 3)`
/// (`crates/lodestone-shell/src/net.rs:1773`), so at any real view radius the
/// tick area is `-3..=3` on both axes — **49** columns, not the 9 a "radius 3"
/// reading suggests. `crate::integrated`'s LAN path is strictly smaller
/// (`LAN_TICK_RADIUS`, 2 ⇒ 25 columns), so 3 bounds both.
pub const CONCURRENT_TICK_RADIUS: i32 = 3;

/// Columns the capacity derivation reserves **on top of** the streamed view:
/// the 49-column `tick_area` plus the one column `crate::server`'s `vitals_tick`
/// probes every 50 ms.
///
/// # Why this is added to the view rather than assumed inside it
///
/// The tick area usually lies inside the streamed view, but movement, a
/// teleport, the initial deferral, and the playerless fallback can place it
/// outside the view while streaming catches up. The working set is therefore
/// the **union** of the concurrent scans, not the largest scan alone.
///
/// The headroom covers movement, teleport, initial deferral, and playerless
/// fallback cases where the tick area lies outside the streamed view while the
/// corresponding strip is being loaded. The union is smaller in steady state
/// but can exceed either individual scan during that interval.
///
/// The union matters rather than frequency. A column polled
/// at 20 Hz being regenerated **12** times over 12 random-tick passes, not once:
/// the block-entity scan runs before the random-tick pass, which then touches 49
/// columns after it, so by the end of a pass the polled column's stamp is the
/// *oldest* in the map. `an_over_capacity_store_makes_the_polled_column_cold_every_pass`
/// below is that measurement. **Access frequency does not confer LRU residency
/// — headroom does.**
///
/// # What this deliberately does *not* cover
///
/// The block-entity registry. `crate::tick`'s 20 Hz scan probes
/// `world.block_state` once per hopper, the registry has no chunk-unload path, so
/// the set it probes only grows with exploration — that is the unbounded-scan behavior in §12.110 and
/// it is unbounded by construction, so no constant here can size for it. Hopper
/// chunks past this headroom evict *view* columns rather than each other (the
/// view is touched once per column, the scan every 50 ms, so LRU's minimum stamp
/// always falls on a view column), which means they erode the "the whole view is
/// resident" property that `tests/view_radius_store_capacity.rs` gates with an
/// empty registry. Bounding the scan by the loaded view is the required
/// behavior; adding to this constant cannot bound an ever-growing registry.
pub const CONCURRENT_SCAN_COLUMNS: usize = view_columns(CONCURRENT_TICK_RADIUS) + 1;

/// Relative chunk coordinates whose retained light can depend on a block
/// mutation. The cross-column light footprint is one chunk in each horizontal
/// direction, so a mutation invalidates the complete 3x3 neighbourhood rather
/// than relying on a target coordinate or an admission order.
const RETAINED_LIGHT_NEIGHBOUR_OFFSETS: [(i32, i32); 8] = [
    (-1, -1),
    (0, -1),
    (1, -1),
    (-1, 0),
    (1, 0),
    (-1, 1),
    (0, 1),
    (1, 1),
];

/// The largest view radius whose whole square this store promises to hold
/// resident, and therefore where [`capacity_for_view_radius`] stops growing.
///
/// 17 is `render_distance` 16 (the shell serves `render_distance + 1`) — twice
/// our default of 8, and the midpoint of the supported `2..=32` slider
/// (`crates/lodestone-shell/src/config.rs`'s `MAX_RENDER_DISTANCE`). **The cap
/// is a memory decision and the number is the whole argument.** Both ends of the
/// table are `/usr/bin/time -l` readings on the release lib-test binary rather
/// than arithmetic — `measure_rss_without_retention` (8.4 MiB) is the control
/// both are differenced against. The packed block grid per section cuts the
/// per-column rate 6.3×; the dense column remains the comparison baseline:
///
/// | `render_distance` | `view_radius` | view columns | capacity | packed resident | unpacked resident |
/// |---|---|---|---|---|---|
/// | 8 (default) | 9 | 361 | 512 (floor) | **15.5 MiB, measured** | 97.4 MiB |
/// | 10 | 11 | 529 | 579 | 17.6 MiB | 110 MiB |
/// | 12 (typical default) | 13 | 729 | 779 | 23.7 MiB | 148 MiB |
/// | 16 | 17 | 1225 | 1275 | **38.9 MiB, measured** | 242.0 MiB |
/// | 24 | 25 | 2601 | 1275 (capped) | 38.9 MiB | 242.0 MiB |
/// | 32 (slider max) | 33 | 4489 | 1275 (capped) | 38.9 MiB | 242.0 MiB |
///
/// The two measured rows are 31.1 and 31.2 KiB per column, so residency is
/// linear in the count across a 2.5× range and the interpolated rows are safe to
/// read. That linearity is not a given and is why the cap row was measured
/// instead of extrapolated: a `HashMap` growing through several rehash thresholds
/// with large values in it could plausibly have been superlinear.
///
/// An *un*capped derivation costs 4,539 columns at `render_distance` 32, i.e.
/// **139.2 MiB, measured,** of resident chunk cache; the dense representation
/// requires 867 MiB at the same capacity inside a process that
/// also holds meshes, textures and a GPU allocator, on a machine whose whole
/// budget this repo's own operational notes put at 16 GB shared with everything
/// else.
///
/// **The cap is bounded by direct measurements.** The packed representation uses
/// 139 MiB at the maximum capacity, within what a hosted server can reasonably
/// spend. Moving a policy
/// constant is a separate decision from changing a representation; the measured
/// `867 MiB` dense cost explains why the packed cap is retained.
///
/// **That is the singleplayer policy** — see
/// [`integrated_capacity_for_view_radius`], which is that same derivation with
/// this ceiling removed, because there the memory belongs to the person who moved
/// the slider. This constant governs the *hosted* path only
/// (`IntegratedServer::bind`), where the setting is an operator's and the players
/// paying for it did not choose it.
///
/// # What degrades above it, precisely
///
/// The store holds 1,275 of the view's columns and no more, so columns
/// outside that set cost a regeneration when something asks for them again —
/// a `block_state` probe from redstone, a fluid tick, mob pathing, or the same
/// column re-entering the view after the player walked back over it. It is not
/// a per-access cost on the whole view (the view is *diffed* as the player
/// moves, never rescanned — see `ViewTracker::recenter`), and the 20 Hz scans
/// are covered by [`CONCURRENT_SCAN_COLUMNS`] at every radius. So the
/// degradation is bounded and localised. `tests/view_radius_store_capacity.rs`
/// measures the bound as this module's negative control.
///
/// To raise it, raise this constant and re-run
/// `measure_rss_with_retention`/`measure_rss_without_retention` first and put
/// the new pair of numbers in the table above. Reducing the *cost* per column
/// instead is unit U8 of `docs/plans/chunk-lifecycle.md`.
pub const FULLY_RESIDENT_VIEW_RADIUS: i32 = 17;

/// The ceiling [`capacity_for_view_radius`] saturates at [`MAX_CAPACITY`]; see
/// [`FULLY_RESIDENT_VIEW_RADIUS`] for the memory argument behind the number.
pub const MAX_CAPACITY: usize =
    view_columns(FULLY_RESIDENT_VIEW_RADIUS) + CONCURRENT_SCAN_COLUMNS;

/// The capacity a **hosted** store serving `view_radius` is built with —
/// this derived capacity, and what `IntegratedServer::bind` (open-to-LAN) calls.
///
/// Singleplayer uses [`integrated_capacity_for_view_radius`] instead, which is
/// this derivation with the same measured upper bound; read that function for
/// why the fork exists.
///
/// `view_columns(view_radius) + CONCURRENT_SCAN_COLUMNS`, clamped to
/// `DEFAULT_CAPACITY ..= MAX_CAPACITY`. Each of those three terms is load-bearing
/// and documented on its own constant; in short:
///
/// * the **view** term covers the streamed square, whose column count grows
///   with `render_distance`;
/// * the **scan** term is the union with `run_tick_loop`'s tick area, which is
///   not a subset of the view;
/// * the **floor** keeps every existing measurement in this module valid, since
///   the derivation can then only move capacity up;
/// * the **ceiling** bounds CPU work while keeping memory bounded. At
///   `render_distance` 32 the packed representation uses 139 MiB and the
///   dense representation uses 866 MiB.
///
/// `const fn` deliberately: `MAX_CAPACITY` is derived from it in a const
/// context, and `tests/view_radius_store_capacity.rs` computes both of its
/// competing hypotheses at compile time from these same constants.
pub const fn capacity_for_view_radius(view_radius: i32) -> usize {
    // `saturating_add`, because `view_columns` saturates at `usize::MAX` for an
    // absurd radius and a plain `+` would then wrap to a *tiny* capacity — the
    // worst possible failure mode for this function, and one that would look
    // like a thrashing cache rather than like arithmetic.
    let want = view_columns(view_radius).saturating_add(CONCURRENT_SCAN_COLUMNS);
    if want < DEFAULT_CAPACITY {
        DEFAULT_CAPACITY
    } else if want > MAX_CAPACITY {
        MAX_CAPACITY
    } else {
        want
    }
}

/// [`capacity_for_view_radius`] with the measured [`MAX_CAPACITY`] as a
/// bounded upper limit — the integrated (singleplayer) policy.
///
/// # Why the hosted derivation differs here
///
/// A hosted server's render distance is the *operator's* budget spent on behalf
/// of players who did not choose it, so capping it is right. Singleplayer is the
/// opposite: the person paying for the memory is the person who moved the slider,
/// and the slider goes to 32. At the selectable 256-chunk extreme, retaining the
/// complete square would make an unbounded memory commitment, so the cache
/// saturates at [`MAX_CAPACITY`] while the wire stream continues incrementally.
/// The cost above the measured fully-resident range is bounded re-generation,
/// not an allocation proportional to the full view.
///
/// # Memory cost by representation
///
/// This store uses a measured **31.1 KiB per retained column** in its packed
/// block-grid representation (measured with `/usr/bin/time -l` on the release
/// lib-test binary). The streamed set is `(2 × (rd + 1) + 1)²`, and capacity is
/// that plus [`CONCURRENT_SCAN_COLUMNS`]:
///
/// | `render_distance` | view columns | capacity | packed resident | dense resident |
/// |---|---|---|---|---|
/// | 8 (our default) | 361 | 512 (floor) | 15.5 MiB, measured | 97.4 MiB |
/// | 12 (vanilla default) | 729 | 779 | 23.7 MiB | 148 MiB |
/// | 16 | 1,225 | 1,275 | 38.9 MiB, measured | 242.0 MiB |
/// | 24 | 2,601 | 2,651 | 80.5 MiB | 506 MiB |
/// | **32 (slider max)** | **4,489** | **4,539** | **139.2 MiB, measured** | 867 MiB |
///
/// Residency measures 31.1–31.4 KiB per column across an **8.9×** range. The
/// final row comes from `measure_rss_at_the_singleplayer_slider_maximum`; it is
/// within 1% of the interpolated rows and validates that interpolation.
///
/// **The packed representation determines memory cost:** the measured
/// per-column rate, rather than the retention policy, determines the memory
/// cost. The packed rate is 31 MiB per 1,000 retained columns, compared with
/// 195 MiB for dense storage.
///
/// # Why the ceiling existed, so it is not reintroduced by accident
///
/// Removing a cap is safe; capping the *wrong* thing is not. `join_view_rings`
/// streams outward, so the least-recently-used entry under a short capacity is
/// the **innermost** ring — the player's own feet. At `render_distance` 10 the old
/// 512-column literal dropped 17 columns and they were rings 0–2, the band
/// `vitals_tick` probes every 50 ms and `run_tick_loop` random-ticks. A short
/// capacity here does not degrade the horizon; it degrades the ground underfoot.
#[must_use]
pub const fn integrated_capacity_for_view_radius(view_radius: i32) -> usize {
    // Same `saturating_add` reasoning as above, and it matters more here because
    // nothing downstream clamps it: a wrap would produce a *tiny* capacity that
    // looks like a thrashing cache rather than like arithmetic.
    let want = view_columns(view_radius).saturating_add(CONCURRENT_SCAN_COLUMNS);
    if want < DEFAULT_CAPACITY {
        DEFAULT_CAPACITY
    } else if want > MAX_CAPACITY {
        MAX_CAPACITY
    } else {
        want
    }
}

/// One retained column plus the stamp that orders eviction.
struct Entry {
    column: ChunkColumn,
    /// Value of `Cache::stamp` at this entry's most recent read or write.
    /// Smallest wins eviction.
    last_used: u64,
}

/// Which derivation a store re-applies when a connection changes its view radius
/// mid-session ([`ChunkStore::set_retention_radius`], the capacity policy).
///
/// The store has to *remember* which of the two policies built it, because the two
/// differ in their floor/ceiling policy and "whose memory is this" does not
/// change when the slider moves. A third arm is needed for the explicit-capacity
/// constructor, which must never resize at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CapacityPolicy {
    /// [`capacity_for_view_radius`] — the hosted (open-to-LAN) ceiling applies.
    Hosted,
    /// [`integrated_capacity_for_view_radius`] — singleplayer, bounded.
    Integrated,
    /// A capacity named outright by [`ChunkStore::with_capacity`], which a radius
    /// change must **not** override.
    ///
    /// Load-bearing rather than merely tidy: the negative control in this module
    /// is `with_capacity(source, 0)`, a store that retains nothing. If a radius
    /// change re-derived its capacity it would silently become a retaining store
    /// mid-test, the control would stop reproducing the pre-store behaviour, and
    /// the positive gate above it would stop measuring anything.
    ///
    /// `#[cfg(test)]` for the same reason [`ChunkStore::new`] is: every production
    /// constructor names a *radius*, so a fixed-capacity store cannot exist outside
    /// a gate, and saying so in a `cfg` is what stops a new call site
    /// reintroducing one by accident.
    #[cfg(test)]
    Fixed,
}

struct Cache {
    columns: HashMap<(i32, i32), Entry>,
    /// Logical halo pins. A pin is retained even before its column is inserted,
    /// so a generation commit cannot be evicted between admission and release.
    pins: HashMap<(i32, i32), usize>,
    /// The eviction bound. It lives inside the mutex because the capacity policy
    /// is mutable: a live render-distance change re-derives it — see
    /// [`ChunkStore::set_retention_radius`]. The `Arc` shares this cache across
    /// readers, so the bound and its policy must be updated together.
    capacity: usize,
    /// Monotonic counter handed out by [`Cache::next_stamp`]. Not a tick count
    /// and not comparable to anything outside this struct.
    stamp: u64,
    /// Cumulative count of calls that reached `source.column()`. This is a
    /// store-lifetime accumulator, so a gate must read it as a **delta** or
    /// against a freshly constructed store — see [`ChunkStore::generated`].
    generated: u64,
    /// Cumulative count of evictions, same accumulator caveat.
    evicted: u64,
    /// The `stamp` value at which [`ChunkStore::maybe_tick_tickets`] should
    /// next check the ticket graph — see that method's own doc for why this is
    /// piggybacked on cache-op traffic rather than a new `run_tick_loop`
    /// parameter. `0` so the very first op after construction always ticks.
    next_ticket_check: u64,
}

struct GenerationRegionState {
    next_ticket: u64,
    pending: BTreeMap<u64, Vec<(i32, i32)>>,
    active: BTreeMap<u64, Vec<(i32, i32)>>,
}

struct GenerationRegionCoordinator {
    state: Mutex<GenerationRegionState>,
    #[cfg(not(target_arch = "wasm32"))]
    wake: std::sync::Condvar,
}

struct GenerationRegionLease<'a> {
    coordinator: &'a GenerationRegionCoordinator,
    ticket: u64,
}

impl Default for GenerationRegionCoordinator {
    fn default() -> Self {
        Self {
            state: Mutex::new(GenerationRegionState {
                next_ticket: 0,
                pending: BTreeMap::new(),
                active: BTreeMap::new(),
            }),
            #[cfg(not(target_arch = "wasm32"))]
            wake: std::sync::Condvar::new(),
        }
    }
}

impl GenerationRegionCoordinator {
    fn enqueue(&self, coordinates: &[(i32, i32)]) -> u64 {
        let mut state = self.state.lock().expect("generation region lock poisoned");
        state.next_ticket = state.next_ticket.wrapping_add(1);
        let ticket = state.next_ticket;
        let mut coordinates = coordinates.to_vec();
        coordinates.sort_unstable();
        coordinates.dedup();
        state.pending.insert(ticket, coordinates);
        ticket
    }

    fn ready(state: &GenerationRegionState, ticket: u64) -> bool {
        let Some(coordinates) = state.pending.get(&ticket) else {
            return false;
        };
        let overlaps = |other: &[(i32, i32)]| {
            coordinates.iter().any(|coordinate| other.contains(coordinate))
        };
        !state
            .active
            .values()
            .any(|active| overlaps(active))
            && !state
                .pending
                .range(..ticket)
                .any(|(_, pending)| overlaps(pending))
    }

    #[cfg(target_arch = "wasm32")]
    fn try_activate(&self, ticket: u64) -> bool {
        let mut state = self.state.lock().expect("generation region lock poisoned");
        if !Self::ready(&state, ticket) {
            return false;
        }
        let coordinates = state
            .pending
            .remove(&ticket)
            .expect("ready ticket must remain pending");
        state.active.insert(ticket, coordinates);
        true
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn acquire(
        &self,
        coordinates: &[(i32, i32)],
        cancellation: &crate::worldgen_session::RequestCancellation,
    ) -> Result<GenerationRegionLease<'_>, ()> {
        let ticket = self.enqueue(coordinates);
        let mut state = self.state.lock().expect("generation region lock poisoned");
        while !Self::ready(&state, ticket) {
            if cancellation.is_cancelled() {
                state.pending.remove(&ticket);
                self.wake.notify_all();
                return Err(());
            }
            let (next_state, _) = self
                .wake
                .wait_timeout(state, std::time::Duration::from_millis(10))
                .expect("generation region lock poisoned");
            state = next_state;
        }
        let coordinates = state
            .pending
            .remove(&ticket)
            .expect("ready ticket must remain pending");
        state.active.insert(ticket, coordinates);
        Ok(GenerationRegionLease {
            coordinator: self,
            ticket,
        })
    }

    #[cfg(target_arch = "wasm32")]
    async fn acquire_yielding(
        &self,
        coordinates: &[(i32, i32)],
        cancellation: &crate::worldgen_session::RequestCancellation,
    ) -> Result<GenerationRegionLease<'_>, ()> {
        let ticket = self.enqueue(coordinates);
        while !self.try_activate(ticket) {
            if cancellation.is_cancelled() {
                let mut state = self.state.lock().expect("generation region lock poisoned");
                state.pending.remove(&ticket);
                return Err(());
            }
            crate::chunk::yield_to_browser().await;
        }
        Ok(GenerationRegionLease {
            coordinator: self,
            ticket,
        })
    }
}

impl Drop for GenerationRegionLease<'_> {
    fn drop(&mut self) {
        let mut state = self
            .coordinator
            .state
            .lock()
            .expect("generation region lock poisoned");
        state.active.remove(&self.ticket);
        #[cfg(not(target_arch = "wasm32"))]
        self.coordinator.wake.notify_all();
    }
}

impl Cache {
    fn next_stamp(&mut self) -> u64 {
        self.stamp += 1;
        self.stamp
    }

    fn retained_bytes(&self) -> usize {
        let columns = self
            .columns
            .values()
            .map(|entry| {
                entry
                    .column
                    .memory_census()
                    .logical_total()
                    .saturating_add(std::mem::size_of::<Entry>())
            })
            .fold(0usize, usize::saturating_add);
        let metadata = self
            .columns
            .capacity()
            .saturating_mul(std::mem::size_of::<((i32, i32), Entry)>())
            .saturating_add(
                self.pins
                    .capacity()
                    .saturating_mul(std::mem::size_of::<((i32, i32), usize)>()),
            );
        columns.saturating_add(metadata)
    }

    /// Drops least-recently-used entries until `len() <= self.capacity`.
    ///
    /// Linear scan per eviction rather than an intrusive LRU list: it runs only
    /// on a **miss**, which has just paid a generation three to four orders of
    /// magnitude more expensive than 512 integer comparisons. A real LRU here
    /// would be optimising the cheap half.
    /// Returns the evicted coordinates so the caller can pass them to
    /// [`ChunkSource::unload`] **after releasing the cache lock**. Notifying
    /// from in here would call out into the source while holding this mutex,
    /// which is both a lock-ordering hazard and a way to put the source's own
    /// work on the critical section every miss pays.
    ///
    /// Ticket- or halo-resident entries are protected even when they are the
    /// oldest cache entries. A periodic ticket sweep is too late for this check: a
    /// capacity miss can happen between sweeps, and unloading a still-ticketed
    /// column would make the next tick or packet path regenerate it. If every
    /// entry is pinned, the cache may temporarily exceed its soft capacity;
    /// preserving the ticket invariant is more important than dropping the
    /// authoritative resident value.
    fn evict_down_to_capacity(
        &mut self,
        ticket_resident: &HashSet<(i32, i32)>,
    ) -> Vec<(i32, i32)> {
        let capacity = self.capacity;
        let mut evicted = Vec::new();
        while self.columns.len() > capacity {
            let Some(victim) = self
                .columns
                .iter()
                .filter(|(key, _)| {
                    !ticket_resident.contains(*key) && !self.pins.contains_key(*key)
                })
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(&key, _)| key)
            else {
                break;
            };
            self.columns.remove(&victim);
            self.evicted += 1;
            evicted.push(victim);
        }
        evicted
    }
}

const DEFAULT_LEDGER_PIPELINES: usize = 4;
const DEFAULT_LEDGER_COORDINATES: usize = 4096;
const DEFAULT_LEDGER_SOURCES: usize = 16_384;
const DEFAULT_LEDGER_OVERLAYS: usize = 131_072;
const DEFAULT_LEDGER_PRODUCTS: usize = 32_768;
const DEFAULT_LEDGER_SIDECARS: usize = 16_384;

/// Explicit memory ceilings for the world-owned generation ledger.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct GenerationLedgerLimits {
    pub(crate) pipelines: usize,
    pub(crate) coordinates_per_pipeline: usize,
    pub(crate) source_completions_per_pipeline: usize,
    pub(crate) overlays_per_pipeline: usize,
    pub(crate) products_per_pipeline: usize,
    pub(crate) sidecars_per_pipeline: usize,
}

impl Default for GenerationLedgerLimits {
    fn default() -> Self {
        Self {
            pipelines: DEFAULT_LEDGER_PIPELINES,
            coordinates_per_pipeline: DEFAULT_LEDGER_COORDINATES,
            source_completions_per_pipeline: DEFAULT_LEDGER_SOURCES,
            overlays_per_pipeline: DEFAULT_LEDGER_OVERLAYS,
            products_per_pipeline: DEFAULT_LEDGER_PRODUCTS,
            sidecars_per_pipeline: DEFAULT_LEDGER_SIDECARS,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct SourceCompletionKey {
    pub(crate) target: (i32, i32),
    pub(crate) source: (i32, i32),
    pub(crate) stage: StageKey,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg(test)]
pub(crate) struct GenerationLedgerStats {
    pub(crate) pipelines: usize,
    pub(crate) coordinates: usize,
    pub(crate) sources: usize,
    pub(crate) overlays: usize,
    pub(crate) products: usize,
    pub(crate) aggregates: usize,
    pub(crate) stamp: u64,
    /// Structural retention plus the sizes declared by typed products,
    /// sidecars, and mutations.
    pub(crate) retained_bytes: usize,
}

fn generation_ledger_trace_enabled() -> bool {
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::env::var_os("LODESTONE_WORLDGEN_LEDGER_TRACE").is_some()
    }
    #[cfg(target_arch = "wasm32")]
    {
        false
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub(crate) enum GenerationLedgerError {
    #[error("generation ledger pipeline capacity exhausted")]
    PipelineCapacity,
    #[error("generation ledger coordinate capacity exhausted")]
    CoordinateCapacity,
    #[error("generation ledger admission cannot be empty")]
    EmptyAdmission,
    #[error("generation ledger source completion capacity exhausted")]
    SourceCapacity,
    #[error("generation ledger overlay capacity exhausted")]
    OverlayCapacity,
    #[error("generation ledger product capacity exhausted")]
    ProductCapacity,
    #[error("generation ledger sidecar capacity exhausted")]
    SidecarCapacity,
    #[error("unexpected product {resource:?} for stage {stage:?}")]
    UnexpectedProduct { stage: StageKey, resource: ResourceKey },
    #[error("missing product {resource:?} for stage {stage:?}")]
    MissingProduct { stage: StageKey, resource: ResourceKey },
    #[error("duplicate product {resource:?} for stage {stage:?}")]
    DuplicateProduct { stage: StageKey, resource: ResourceKey },
    #[error("unexpected sidecar for stage {stage:?}")]
    UnexpectedSidecar { stage: StageKey },
    #[error("missing sidecar for stage {stage:?}")]
    MissingSidecar { stage: StageKey },
    #[error("duplicate sidecar for stage {stage:?}")]
    DuplicateSidecar { stage: StageKey },
    #[error("generation ledger has no pipeline {0:?}")]
    UnknownPipeline(PipelineIdentity),
    #[error("coordinate {0:?} is outside the generation ledger")]
    UnknownCoordinate((i32, i32)),
    #[error("stage {0:?} does not belong to the generation pipeline")]
    ForeignStage(StageKey),
    #[error("stage {0:?} is not a source-ordered stage")]
    NotSourceOrdered(StageKey),
    #[error("source order for {stage:?} expected {expected}, found {found}")]
    SourceOrder {
        stage: StageKey,
        expected: u64,
        found: u64,
    },
    #[error("generation frontier rejected a stage: {0:?}")]
    Frontier(lodestone_worldgen::stage_schedule::FrontierError),
    #[error("generation ledger revision conflict at {coordinate:?}: expected {expected:?}, found {found:?}")]
    RevisionConflict {
        coordinate: (i32, i32),
        expected: u64,
        found: u64,
    },
    #[error("generation ledger checkpoint does not extend its current frontier")]
    CheckpointMismatch,
}

struct PipelineLedger {
    frontiers: BTreeMap<(i32, i32), StageFrontier>,
    products: BTreeMap<crate::worldgen_session::ProductKey, ImmutableProduct>,
    sidecars: BTreeMap<crate::worldgen_session::SidecarProductKey, ImmutableSidecar>,
    aggregates: BTreeMap<(i32, i32), AggregatePrefix>,
    sources: BTreeMap<SourceCompletionKey, u64>,
    source_next: BTreeMap<((i32, i32), StageKey), u64>,
    overlays: BTreeMap<(i32, i32), BTreeMap<BlockCoordinate, ProvenanceMutation>>,
    feature_settlements: BTreeMap<ChunkCoordinate, FeatureSettlementProof>,
    feature_winner_receipts: BTreeMap<ChunkCoordinate, Vec<TargetFeatureWrite>>,
    revisions: BTreeMap<(i32, i32), u64>,
    coordinate_last_used: BTreeMap<(i32, i32), u64>,
    last_used: u64,
}

impl PipelineLedger {
    fn overlays_len(&self) -> usize {
        self.overlays.values().map(BTreeMap::len).sum()
    }

    fn overlays_iter(&self) -> impl Iterator<Item = (&BlockCoordinate, &ProvenanceMutation)> {
        self.overlays.values().flat_map(|bucket| bucket.iter())
    }

    fn overlay(&self, destination: BlockCoordinate) -> Option<&ProvenanceMutation> {
        let coordinate = (
            destination.x().div_euclid(16),
            destination.z().div_euclid(16),
        );
        self.overlays
            .get(&coordinate)
            .and_then(|bucket| bucket.get(&destination))
    }

    fn has_pending_overlay_for(&self, coordinate: (i32, i32)) -> bool {
        self.overlays.get(&coordinate).is_some_and(|bucket| !bucket.is_empty())
            || self.overlays.values().any(|bucket| {
                bucket
                    .values()
                    .any(|mutation| mutation.provenance().target() == coordinate)
            })
    }

    fn accepts_settled_feature_replay(
        &self,
        coordinate: ChunkCoordinate,
        mutation: &ProvenanceMutation,
    ) -> bool {
        let provenance = mutation.provenance();
        self.products
            .get(&crate::worldgen_session::ProductKey::new(
                coordinate,
                StageKey::new(provenance.stage().dimension(), ColumnStage::Output),
                ResourceKey::OutputColumn,
            ))
            .and_then(|product| product.get::<ChunkColumn>())
            .is_some()
            && provenance.stage().stage() == ColumnStage::Features
            && provenance.target() == provenance.source()
            && self
                .feature_settlements
                .get(&coordinate)
                .is_some_and(|proof| {
                    proof.output() == coordinate && proof.contains(provenance.target())
                })
    }

    fn evict_coordinate(&mut self, coordinate: (i32, i32)) {
        self.frontiers.remove(&coordinate);
        self.revisions.remove(&coordinate);
        self.coordinate_last_used.remove(&coordinate);
        self.products
            .retain(|key, _| key.coordinate() != coordinate);
        self.sidecars
            .retain(|key, _| key.coordinate() != coordinate);
        self.aggregates.remove(&coordinate);
        self.sources.retain(|key, _| {
            key.target != coordinate && key.source != coordinate
        });
        self.source_next
            .retain(|(target, _), _| *target != coordinate);
        self.overlays.remove(&coordinate);
        self.overlays.retain(|_, bucket| {
            bucket.retain(|_, mutation| mutation.provenance().target() != coordinate);
            !bucket.is_empty()
        });
        self.feature_settlements.remove(&coordinate);
        self.feature_winner_receipts.remove(&coordinate);
    }
}

struct GenerationLedgerJournal {
    identity: PipelineIdentity,
    stamp: u64,
    pipeline_last_used: u64,
    frontiers: BTreeMap<(i32, i32), Option<StageFrontier>>,
    products: BTreeMap<crate::worldgen_session::ProductKey, Option<ImmutableProduct>>,
    sidecars: BTreeMap<crate::worldgen_session::SidecarProductKey, Option<ImmutableSidecar>>,
    aggregates: BTreeMap<(i32, i32), Option<AggregatePrefix>>,
    sources: BTreeMap<SourceCompletionKey, Option<u64>>,
    source_next: BTreeMap<((i32, i32), StageKey), Option<u64>>,
    overlays: BTreeMap<(i32, i32), BTreeMap<BlockCoordinate, Option<ProvenanceMutation>>>,
    feature_settlements: BTreeMap<ChunkCoordinate, Option<FeatureSettlementProof>>,
    feature_winner_receipts: BTreeMap<ChunkCoordinate, Option<Vec<TargetFeatureWrite>>>,
    revisions: BTreeMap<(i32, i32), Option<u64>>,
}

#[derive(Debug)]
enum GenerationPublicationCommitError {
    Ledger(GenerationLedgerError),
    Commit(GenerationCommitError),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum GenerationCommitMode {
    Standard,
    Cohort,
}

impl GenerationCommitMode {
    const fn checks_full_halo(self) -> bool {
        matches!(self, Self::Cohort)
    }

    const fn defers_eviction(self) -> bool {
        matches!(self, Self::Cohort)
    }
}

impl GenerationLedgerJournal {
    fn new(ledger: &GenerationLedger, identity: PipelineIdentity) -> Self {
        let pipeline_last_used = ledger
            .pipelines
            .get(&identity)
            .expect("publish journal requires an admitted pipeline")
            .last_used;
        Self {
            identity,
            stamp: ledger.stamp,
            pipeline_last_used,
            frontiers: BTreeMap::new(),
            products: BTreeMap::new(),
            sidecars: BTreeMap::new(),
            aggregates: BTreeMap::new(),
            sources: BTreeMap::new(),
            source_next: BTreeMap::new(),
            overlays: BTreeMap::new(),
            feature_settlements: BTreeMap::new(),
            feature_winner_receipts: BTreeMap::new(),
            revisions: BTreeMap::new(),
        }
    }

    fn frontier(&mut self, state: &PipelineLedger, coordinate: (i32, i32)) {
        self.frontiers
            .entry(coordinate)
            .or_insert_with(|| state.frontiers.get(&coordinate).cloned());
    }

    fn product(
        &mut self,
        state: &PipelineLedger,
        key: crate::worldgen_session::ProductKey,
    ) {
        self.products
            .entry(key)
            .or_insert_with(|| state.products.get(&key).cloned());
    }

    fn sidecar(
        &mut self,
        state: &PipelineLedger,
        key: crate::worldgen_session::SidecarProductKey,
    ) {
        self.sidecars
            .entry(key)
            .or_insert_with(|| state.sidecars.get(&key).cloned());
    }

    fn aggregate(&mut self, state: &PipelineLedger, coordinate: (i32, i32)) {
        self.aggregates
            .entry(coordinate)
            .or_insert_with(|| state.aggregates.get(&coordinate).cloned());
    }

    fn source(&mut self, state: &PipelineLedger, key: SourceCompletionKey) {
        self.sources
            .entry(key)
            .or_insert_with(|| state.sources.get(&key).copied());
    }

    fn source_next(&mut self, state: &PipelineLedger, key: ((i32, i32), StageKey)) {
        self.source_next
            .entry(key)
            .or_insert_with(|| state.source_next.get(&key).copied());
    }

    fn overlay(&mut self, state: &PipelineLedger, destination: BlockCoordinate) {
        let coordinate = (
            destination.x().div_euclid(16),
            destination.z().div_euclid(16),
        );
        self.overlays
            .entry(coordinate)
            .or_default()
            .entry(destination)
            .or_insert_with(|| {
                state
                    .overlays
                    .get(&coordinate)
                    .and_then(|bucket| bucket.get(&destination))
                    .cloned()
            });
    }

    fn revision(&mut self, state: &PipelineLedger, coordinate: (i32, i32)) {
        self.revisions
            .entry(coordinate)
            .or_insert_with(|| state.revisions.get(&coordinate).copied());
    }

    fn feature_settlement(&mut self, state: &PipelineLedger, coordinate: ChunkCoordinate) {
        self.feature_settlements
            .entry(coordinate)
            .or_insert_with(|| state.feature_settlements.get(&coordinate).copied());
    }

    fn feature_winner_receipts(&mut self, state: &PipelineLedger, coordinate: ChunkCoordinate) {
        self.feature_winner_receipts
            .entry(coordinate)
            .or_insert_with(|| state.feature_winner_receipts.get(&coordinate).cloned());
    }

    fn rollback(self, ledger: &mut GenerationLedger) {
        ledger.stamp = self.stamp;
        let state = ledger
            .pipelines
            .get_mut(&self.identity)
            .expect("publish journal pipeline survived the transaction");
        state.last_used = self.pipeline_last_used;
        Self::restore(&mut state.frontiers, self.frontiers);
        Self::restore(&mut state.products, self.products);
        Self::restore(&mut state.sidecars, self.sidecars);
        Self::restore(&mut state.aggregates, self.aggregates);
        Self::restore(&mut state.sources, self.sources);
        Self::restore(&mut state.source_next, self.source_next);
        Self::restore_overlays(&mut state.overlays, self.overlays);
        Self::restore(&mut state.feature_settlements, self.feature_settlements);
        Self::restore(&mut state.feature_winner_receipts, self.feature_winner_receipts);
        Self::restore(&mut state.revisions, self.revisions);
    }

    fn restore<K: Ord, V>(map: &mut BTreeMap<K, V>, entries: BTreeMap<K, Option<V>>) {
        for (key, value) in entries {
            match value {
                Some(value) => {
                    map.insert(key, value);
                }
                None => {
                    map.remove(&key);
                }
            }
        }
    }

    fn restore_overlays(
        map: &mut BTreeMap<(i32, i32), BTreeMap<BlockCoordinate, ProvenanceMutation>>,
        buckets: BTreeMap<(i32, i32), BTreeMap<BlockCoordinate, Option<ProvenanceMutation>>>,
    ) {
        for (coordinate, entries) in buckets {
            let bucket = map.entry(coordinate).or_default();
            for (destination, value) in entries {
                match value {
                    Some(value) => {
                        bucket.insert(destination, value);
                    }
                    None => {
                        bucket.remove(&destination);
                    }
                }
            }
            if bucket.is_empty() {
                map.remove(&coordinate);
            }
        }
    }

}

/// World-owned, bounded generation state shared by future streaming sessions.
/// The cache remains a packet-column store; this ledger retains the typed
/// frontier, source completion identity, and sparse mutable products that a
/// request may reuse after its session is cancelled or dropped.
pub(crate) struct GenerationLedger {
    pipelines: HashMap<PipelineIdentity, PipelineLedger>,
    pipeline_pins: HashMap<PipelineIdentity, usize>,
    limits: GenerationLedgerLimits,
    stamp: u64,
}

impl GenerationLedger {
    pub(crate) fn new() -> Self {
        Self::with_limits(GenerationLedgerLimits::default())
    }

    pub(crate) fn with_limits(limits: GenerationLedgerLimits) -> Self {
        assert!(limits.pipelines > 0, "generation ledger needs one pipeline slot");
        assert!(limits.coordinates_per_pipeline > 0, "generation ledger needs one coordinate slot");
        Self {
            pipelines: HashMap::new(),
            pipeline_pins: HashMap::new(),
            limits,
            stamp: 0,
        }
    }

    fn next_stamp(&mut self) -> u64 {
        self.stamp = self.stamp.saturating_add(1);
        self.stamp
    }

    fn pipeline_mut(
        &mut self,
        pipeline: DimensionPipeline,
    ) -> Result<&mut PipelineLedger, GenerationLedgerError> {
        let identity = pipeline.identity(lodestone_worldgen::stage_schedule::PipelineOptions::ALL);
        let stamp = self.next_stamp();
        if !self.pipelines.contains_key(&identity) {
            if self.pipelines.len() >= self.limits.pipelines {
                let victim = self
                    .pipelines
                    .iter()
                    .filter(|entry| {
                        let (key, _) = *entry;
                        !self.pipeline_pins.contains_key(key)
                    })
                    .min_by_key(|(_, state)| state.last_used)
                    .map(|(key, _)| *key)
                    .ok_or(GenerationLedgerError::PipelineCapacity)?;
                self.pipelines.remove(&victim);
            }
            self.pipelines.insert(identity, PipelineLedger {
                frontiers: BTreeMap::new(),
                products: BTreeMap::new(),
                sidecars: BTreeMap::new(),
                aggregates: BTreeMap::new(),
                sources: BTreeMap::new(),
                source_next: BTreeMap::new(),
                overlays: BTreeMap::new(),
                feature_settlements: BTreeMap::new(),
                feature_winner_receipts: BTreeMap::new(),
                revisions: BTreeMap::new(),
                coordinate_last_used: BTreeMap::new(),
                last_used: stamp,
            });
        }
        let state = self.pipelines.get_mut(&identity).expect("pipeline was inserted");
        state.last_used = stamp;
        Ok(state)
    }

    fn pipeline_ref(
        &self,
        identity: PipelineIdentity,
    ) -> Result<&PipelineLedger, GenerationLedgerError> {
        self.pipelines.get(&identity).ok_or(GenerationLedgerError::UnknownPipeline(identity))
    }

    pub(crate) fn admit(
        &mut self,
        pipeline: DimensionPipeline,
        coordinates: &[(i32, i32)],
    ) -> Result<Vec<(i32, i32)>, GenerationLedgerError> {
        let limits = self.limits;
        let mut canonical = coordinates.to_vec();
        canonical.sort_unstable();
        canonical.dedup();
        if canonical.is_empty() {
            return Err(GenerationLedgerError::EmptyAdmission);
        }
        let identity = pipeline.identity(
            lodestone_worldgen::stage_schedule::PipelineOptions::ALL,
        );
        if let Some(state) = self.pipelines.get(&identity) {
            let new_coordinates = canonical
                .iter()
                .filter(|coordinate| !state.frontiers.contains_key(coordinate))
                .count();
            let required_evictions = state
                .frontiers
                .len()
                .saturating_add(new_coordinates)
                .saturating_sub(limits.coordinates_per_pipeline);
            if required_evictions != 0 {
                if self.pipeline_pins.contains_key(&identity) {
                    return Err(GenerationLedgerError::CoordinateCapacity);
                }
                let mut candidates = state
                    .frontiers
                    .keys()
                    .filter(|coordinate| {
                        !canonical.contains(coordinate)
                            && !state.has_pending_overlay_for(**coordinate)
                            && !state.feature_winner_receipts.contains_key(coordinate)
                    })
                    .copied()
                    .collect::<Vec<_>>();
                candidates.sort_unstable_by_key(|coordinate| {
                    (
                        state.coordinate_last_used.get(coordinate).copied().unwrap_or(0),
                        *coordinate,
                    )
                });
                if candidates.len() < required_evictions {
                    return Err(GenerationLedgerError::CoordinateCapacity);
                }
                let state = self
                    .pipelines
                    .get_mut(&identity)
                    .expect("pipeline was checked before coordinate eviction");
                for coordinate in candidates.into_iter().take(required_evictions) {
                    state.evict_coordinate(coordinate);
                }
            }
        } else if canonical.len() > limits.coordinates_per_pipeline {
            return Err(GenerationLedgerError::CoordinateCapacity);
        }
        let state = self.pipeline_mut(pipeline)?;
        let new_coordinates = canonical
            .iter()
            .filter(|coordinate| !state.frontiers.contains_key(coordinate))
            .copied()
            .collect::<Vec<_>>();
        for coordinate in &new_coordinates {
            if !state.frontiers.contains_key(coordinate) {
                state.frontiers.insert(
                    *coordinate,
                    pipeline.frontier(
                        *coordinate,
                        lodestone_worldgen::stage_schedule::PipelineOptions::ALL,
                    ),
                );
                state.revisions.insert(*coordinate, 0);
            }
            state
                .coordinate_last_used
                .insert(*coordinate, state.last_used);
        }
        for coordinate in canonical.iter().filter(|coordinate| !new_coordinates.contains(coordinate)) {
            state
                .coordinate_last_used
                .insert(*coordinate, state.last_used);
        }
        Ok(new_coordinates)
    }

    fn rollback_admission(
        &mut self,
        pipeline: DimensionPipeline,
        coordinates: &[(i32, i32)],
    ) {
        if coordinates.is_empty() {
            return;
        }
        let identity = pipeline.identity(
            lodestone_worldgen::stage_schedule::PipelineOptions::ALL,
        );
        let remove = coordinates.iter().copied().collect::<HashSet<_>>();
        let mut remove_pipeline = false;
        if let Some(state) = self.pipelines.get_mut(&identity) {
            for coordinate in coordinates {
                let has_committed_state = state
                    .frontiers
                    .get(coordinate)
                    .is_some_and(|frontier| !frontier.records().is_empty())
                    || state.products.keys().any(|key| key.coordinate() == *coordinate)
                    || state.sidecars.keys().any(|key| key.coordinate() == *coordinate)
                    || state.aggregates.contains_key(coordinate)
                    || state.sources.keys().any(|key| {
                        key.target == *coordinate || key.source == *coordinate
                    })
                    || state.overlays.values().flat_map(|bucket| bucket.values()).any(|mutation| {
                        mutation.provenance().target() == *coordinate
                    });
                if !has_committed_state {
                    state.frontiers.remove(coordinate);
                    state.revisions.remove(coordinate);
                    state.coordinate_last_used.remove(coordinate);
                }
            }
            state.products.retain(|key, _| {
                !remove.contains(&key.coordinate())
                    || state
                        .frontiers
                        .get(&key.coordinate())
                        .is_some_and(|frontier| !frontier.records().is_empty())
            });
            state.sidecars.retain(|key, _| {
                !remove.contains(&key.coordinate())
                    || state
                        .frontiers
                        .get(&key.coordinate())
                        .is_some_and(|frontier| !frontier.records().is_empty())
            });
            state.aggregates.retain(|coordinate, _| {
                !remove.contains(coordinate)
                    || state
                        .frontiers
                        .get(coordinate)
                        .is_some_and(|frontier| !frontier.records().is_empty())
            });
            state.sources.retain(|key, _| {
                !remove.contains(&key.target) && !remove.contains(&key.source)
                    || state
                        .frontiers
                        .get(&key.target)
                        .is_some_and(|frontier| !frontier.records().is_empty())
            });
            state.source_next.retain(|(target, _), _| {
                !remove.contains(target)
                    || state
                        .frontiers
                        .get(target)
                        .is_some_and(|frontier| !frontier.records().is_empty())
            });
            state.overlays.retain(|_, bucket| {
                bucket.retain(|_, mutation| {
                    !remove.contains(&mutation.provenance().target())
                        || state
                            .frontiers
                            .get(&mutation.provenance().target())
                            .is_some_and(|frontier| !frontier.records().is_empty())
                });
                !bucket.is_empty()
            });
            remove_pipeline = state.frontiers.is_empty();
        }
        if remove_pipeline {
            self.pipelines.remove(&identity);
            self.pipeline_pins.remove(&identity);
        }
    }

    pub(crate) fn frontier(
        &self,
        identity: PipelineIdentity,
        coordinate: (i32, i32),
    ) -> Result<&StageFrontier, GenerationLedgerError> {
        self.pipeline_ref(identity)?
            .frontiers
            .get(&coordinate)
            .ok_or(GenerationLedgerError::UnknownCoordinate(coordinate))
    }

    pub(crate) fn revision(
        &self,
        identity: PipelineIdentity,
        coordinate: (i32, i32),
    ) -> Result<u64, GenerationLedgerError> {
        self.pipeline_ref(identity)?
            .revisions
            .get(&coordinate)
            .copied()
            .ok_or(GenerationLedgerError::UnknownCoordinate(coordinate))
    }

    fn pin_pipeline(
        &mut self,
        pipeline: DimensionPipeline,
    ) -> Result<PipelineIdentity, GenerationLedgerError> {
        let identity = pipeline.identity(
            lodestone_worldgen::stage_schedule::PipelineOptions::ALL,
        );
        self.pipeline_ref(identity)?;
        *self.pipeline_pins.entry(identity).or_insert(0) += 1;
        Ok(identity)
    }

    fn unpin_pipeline(&mut self, identity: PipelineIdentity) {
        if let Some(count) = self.pipeline_pins.get_mut(&identity) {
            *count -= 1;
            if *count == 0 {
                self.pipeline_pins.remove(&identity);
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn complete_source(
        &mut self,
        pipeline: DimensionPipeline,
        target: (i32, i32),
        source: (i32, i32),
        stage: StageKey,
        source_order: u64,
    ) -> Result<bool, GenerationLedgerError> {
        self.complete_source_with_journal(pipeline, target, source, stage, source_order, None)
    }

    fn complete_source_with_journal(
        &mut self,
        pipeline: DimensionPipeline,
        target: (i32, i32),
        source: (i32, i32),
        stage: StageKey,
        source_order: u64,
        mut journal: Option<&mut GenerationLedgerJournal>,
    ) -> Result<bool, GenerationLedgerError> {
        if stage.dimension() != pipeline.dimension() {
            return Err(GenerationLedgerError::ForeignStage(stage));
        }
        let Some(descriptor) = pipeline.descriptor(stage.stage()) else {
            return Err(GenerationLedgerError::ForeignStage(stage));
        };
        if descriptor.barrier() != BarrierPolicy::SourceOrdered {
            return Err(GenerationLedgerError::NotSourceOrdered(stage));
        }
        let limits = self.limits;
        let identity = pipeline.identity(
            lodestone_worldgen::stage_schedule::PipelineOptions::ALL,
        );
        let state = self.pipeline_ref(identity)?;
        if !state.frontiers.contains_key(&source) {
            return Err(GenerationLedgerError::UnknownCoordinate(source));
        }
        if !state.frontiers.contains_key(&target) {
            return Err(GenerationLedgerError::UnknownCoordinate(target));
        }
        let target_frontier = &state.frontiers[&target];
        let stage_is_committed = target_frontier
            .records()
            .iter()
            .any(|record| record.key() == stage);
        if !stage_is_committed && target_frontier.next_stage() != Some(stage) {
            return Err(GenerationLedgerError::Frontier(
                lodestone_worldgen::stage_schedule::FrontierError::OutOfOrder {
                    expected: target_frontier.next_stage(),
                    found: stage,
                },
            ));
        }
        let key = SourceCompletionKey { target, source, stage };
        if state.sources.contains_key(&key) {
            return Ok(false);
        }
        let expected_order = state
            .source_next
            .get(&(target, stage))
            .copied()
            .unwrap_or(0);
        if source_order != expected_order {
            return Err(GenerationLedgerError::SourceOrder {
                stage,
                expected: expected_order,
                found: source_order,
            });
        }
        if state.sources.len() >= limits.source_completions_per_pipeline {
            return Err(GenerationLedgerError::SourceCapacity);
        }
        if let Some(journal) = journal.as_deref_mut() {
            journal.source(state, key);
            journal.source_next(state, (target, stage));
            journal.revision(state, target);
        }
        let state = self.pipeline_mut(pipeline)?;
        state.sources.insert(key, expected_order);
        state.source_next.insert((target, stage), expected_order + 1);
        Self::bump_revision(state, target);
        Ok(true)
    }

    #[cfg(test)]
    pub(crate) fn commit_immutable(
        &mut self,
        pipeline: DimensionPipeline,
        completion: &ImmutableStageCompletion,
    ) -> Result<(), GenerationLedgerError> {
        self.commit_stage_completion_with_journal(pipeline, completion, false, None)
    }

    fn commit_stage_completion_with_journal(
        &mut self,
        pipeline: DimensionPipeline,
        completion: &ImmutableStageCompletion,
        source_ordered: bool,
        mut journal: Option<&mut GenerationLedgerJournal>,
    ) -> Result<(), GenerationLedgerError> {
        let limits = self.limits;
        if completion.stage().dimension() != pipeline.dimension() {
            return Err(GenerationLedgerError::ForeignStage(completion.stage()));
        }
        let descriptor = pipeline
            .descriptor(completion.stage().stage())
            .ok_or(GenerationLedgerError::ForeignStage(completion.stage()))?;
        if (descriptor.barrier() == BarrierPolicy::SourceOrdered) != source_ordered {
            return Err(GenerationLedgerError::NotSourceOrdered(completion.stage()));
        }
        let mut products = BTreeSet::new();
        for product in completion.products() {
            if !products.insert(product.resource()) {
                return Err(GenerationLedgerError::DuplicateProduct {
                    stage: completion.stage(),
                    resource: product.resource(),
                });
            }
            if !descriptor.outputs().contains(&product.resource()) {
                return Err(GenerationLedgerError::UnexpectedProduct {
                    stage: completion.stage(),
                    resource: product.resource(),
                });
            }
        }
        for &resource in descriptor.outputs() {
            if !products.contains(&resource) {
                return Err(GenerationLedgerError::MissingProduct {
                    stage: completion.stage(),
                    resource,
                });
            }
        }
        let mut sidecars = BTreeSet::new();
        for sidecar in completion.sidecars() {
            if !sidecars.insert(sidecar.sidecar()) {
                return Err(GenerationLedgerError::DuplicateSidecar {
                    stage: completion.stage(),
                });
            }
            if !descriptor.retained_sidecars().contains(&sidecar.sidecar()) {
                return Err(GenerationLedgerError::UnexpectedSidecar {
                    stage: completion.stage(),
                });
            }
        }
        for &sidecar in descriptor.retained_sidecars() {
            if !sidecars.contains(&sidecar) {
                return Err(GenerationLedgerError::MissingSidecar {
                    stage: completion.stage(),
                });
            }
        }
        let identity = pipeline.identity(
            lodestone_worldgen::stage_schedule::PipelineOptions::ALL,
        );
        let state = self.pipeline_ref(identity)?;
        if !state.frontiers.contains_key(&completion.coordinate()) {
            return Err(GenerationLedgerError::UnknownCoordinate(completion.coordinate()));
        }
        if state.products.len()
            + state.aggregates.len()
            + completion.products().len()
            > limits.products_per_pipeline
        {
            return Err(GenerationLedgerError::ProductCapacity);
        }
        if state.sidecars.len() + completion.sidecars().len() > limits.sidecars_per_pipeline {
            return Err(GenerationLedgerError::SidecarCapacity);
        }
        let record = StageRecord::for_descriptor(
            descriptor,
            completion.input_fingerprint(),
            completion.output_fingerprint(),
            completion.executor_version(),
        );
        let product_keys = completion
            .products()
            .iter()
            .map(|product| {
                crate::worldgen_session::ProductKey::new(
                    completion.coordinate(),
                    completion.stage(),
                    product.resource(),
                )
            })
            .collect::<Vec<_>>();
        let sidecar_keys = completion
            .sidecars()
            .iter()
            .map(|sidecar| {
                crate::worldgen_session::SidecarProductKey::new(
                    completion.coordinate(),
                    completion.stage(),
                    sidecar.sidecar(),
                )
            })
            .collect::<Vec<_>>();
        if let Some(journal) = journal.as_deref_mut() {
            journal.frontier(state, completion.coordinate());
            for key in &product_keys {
                journal.product(state, *key);
            }
            for key in &sidecar_keys {
                journal.sidecar(state, *key);
            }
            journal.revision(state, completion.coordinate());
        }
        let state = self.pipeline_mut(pipeline)?;
        state
            .frontiers
            .get_mut(&completion.coordinate())
            .expect("coordinate was checked")
            .commit(record)
            .map_err(GenerationLedgerError::Frontier)?;
        for product in completion.products() {
            state.products.insert(
                crate::worldgen_session::ProductKey::new(
                    completion.coordinate(),
                    completion.stage(),
                    product.resource(),
                ),
                product.clone(),
            );
        }
        for sidecar in completion.sidecars() {
            state.sidecars.insert(
                crate::worldgen_session::SidecarProductKey::new(
                    completion.coordinate(),
                    completion.stage(),
                    sidecar.sidecar(),
                ),
                sidecar.clone(),
            );
        }
        Self::bump_revision(state, completion.coordinate());
        Ok(())
    }

    fn commit_mutable_stage_with_journal(
        &mut self,
        pipeline: DimensionPipeline,
        completion: &ImmutableStageCompletion,
        journal: Option<&mut GenerationLedgerJournal>,
    ) -> Result<(), GenerationLedgerError> {
        if completion.stage().dimension() != pipeline.dimension() {
            return Err(GenerationLedgerError::ForeignStage(completion.stage()));
        }
        let identity = pipeline.identity(PipelineOptions::ALL);
        let state = self.pipeline_ref(identity)?;
        let source_count = state
            .source_next
            .get(&(completion.coordinate(), completion.stage()))
            .copied()
            .unwrap_or(0);
        if source_count == 0
            || state
                .sources
                .keys()
                .filter(|key| {
                    key.target == completion.coordinate() && key.stage == completion.stage()
                })
                .count() as u64
                != source_count
        {
            return Err(GenerationLedgerError::SourceOrder {
                stage: completion.stage(),
                expected: source_count,
                found: state
                    .sources
                    .keys()
                    .filter(|key| {
                        key.target == completion.coordinate() && key.stage == completion.stage()
                    })
                    .count() as u64,
            });
        }
        self.commit_stage_completion_with_journal(pipeline, completion, true, journal)
    }

    /// Retain an externally materialized shaped prefix without manufacturing
    /// one product per covered stage. The prefix's frontier records remain
    /// useful validation metadata, while the aggregate column is the single
    /// value future sessions consume.
    fn commit_aggregate_prefix(
        &mut self,
        pipeline: DimensionPipeline,
        aggregate: &AggregatePrefix,
        records: &[StageRecord],
        sidecars: &[(crate::worldgen_session::SidecarProductKey, ImmutableSidecar)],
        journal: &mut GenerationLedgerJournal,
    ) -> Result<(), GenerationLedgerError> {
        let coordinate = aggregate.coordinate();
        let boundary = aggregate.boundary();
        let schedule = pipeline.schedule();
        let boundary_index = schedule
            .index_of(boundary)
            .ok_or(GenerationLedgerError::ForeignStage(StageKey::new(
                pipeline.dimension(),
                boundary,
            )))?;
        if aggregate.product().resource() != ResourceKey::MaterializedWorld
            || (aggregate.product().get::<ChunkColumn>().is_none()
                && aggregate
                    .product()
                    .get::<lodestone_worldgen::overworld::GeneratedColumn>()
                    .is_none())
            || records.len() <= boundary_index
        {
            return Err(GenerationLedgerError::CheckpointMismatch);
        }
        let prefix = &records[..boundary_index + 1];
        if prefix.iter().any(|record| {
            record.input_fingerprint() != aggregate.input_fingerprint()
                || record.output_fingerprint() != aggregate.output_fingerprint()
                || record.executor_version() != aggregate.executor_version()
        }) {
            return Err(GenerationLedgerError::CheckpointMismatch);
        }
        for record in records.iter().take(boundary_index + 1) {
            if record.key().dimension() != pipeline.dimension()
                || schedule.index_of(record.key().stage()).is_none()
            {
                return Err(GenerationLedgerError::ForeignStage(record.key()));
            }
        }
        let identity = pipeline.identity(PipelineOptions::ALL);
        let limits = self.limits;
        let state = self.pipeline_ref(identity)?;
        if !state.frontiers.contains_key(&coordinate) {
            return Err(GenerationLedgerError::UnknownCoordinate(coordinate));
        }
        if let Some(existing) = state.aggregates.get(&coordinate) {
            if existing.boundary() != boundary
                || existing.input_fingerprint() != aggregate.input_fingerprint()
                || existing.output_fingerprint() != aggregate.output_fingerprint()
                || existing.executor_version() != aggregate.executor_version()
            {
                return Err(GenerationLedgerError::CheckpointMismatch);
            }
        } else if state.products.len() + state.aggregates.len() >= limits.products_per_pipeline {
            return Err(GenerationLedgerError::ProductCapacity);
        }
        let frontier_len = state
            .frontiers
            .get(&coordinate)
            .expect("coordinate was checked")
            .records()
            .len();
        let current_records = state.frontiers[&coordinate].records();
        let compatible_prefix = if frontier_len >= prefix.len() {
            &current_records[..prefix.len()] == prefix
        } else {
            &prefix[..frontier_len] == current_records
        };
        if !compatible_prefix {
            return Err(GenerationLedgerError::CheckpointMismatch);
        }
        let mut required_sidecars = Vec::new();
        for record in records.iter().take(boundary_index + 1) {
            let descriptor = pipeline
                .descriptor(record.key().stage())
                .ok_or(GenerationLedgerError::ForeignStage(record.key()))?;
            for &sidecar in descriptor.retained_sidecars() {
                let key = crate::worldgen_session::SidecarProductKey::new(
                    coordinate,
                    record.key(),
                    sidecar,
                );
                let Some((_, value)) = sidecars.iter().find(|(candidate, _)| *candidate == key)
                else {
                    return Err(GenerationLedgerError::MissingSidecar {
                        stage: record.key(),
                    });
                };
                required_sidecars.push((key, value.clone()));
            }
        }
        let has_aggregate = state.aggregates.contains_key(&coordinate);
        let missing_sidecar = required_sidecars
            .iter()
            .any(|(key, _)| !state.sidecars.contains_key(key));
        let needs_publication = frontier_len < prefix.len() || !has_aggregate || missing_sidecar;
        if !needs_publication {
            return Ok(());
        }

        journal.frontier(state, coordinate);
        journal.aggregate(state, coordinate);
        for (key, _) in &required_sidecars {
            journal.sidecar(state, *key);
        }
        let state = self.pipeline_mut(pipeline)?;
        let frontier = state
            .frontiers
            .get_mut(&coordinate)
            .expect("coordinate was checked");
        for record in records
            .iter()
            .skip(frontier_len)
            .take(prefix.len().saturating_sub(frontier_len))
        {
            frontier
                .commit(record.clone())
                .map_err(GenerationLedgerError::Frontier)?;
        }
        state
            .aggregates
            .entry(coordinate)
            .or_insert_with(|| aggregate.clone());
        for (key, value) in required_sidecars {
            state.sidecars.entry(key).or_insert(value);
        }
        Self::bump_revision(state, coordinate);
        Ok(())
    }

    /// Build a request-scoped checkpoint from the world-owned state. The
    /// ledger may retain several targets under one pipeline, so products,
    /// overlays, and source completions are filtered to this request's target
    /// while every admitted halo frontier is included for validation.
    pub(crate) fn checkpoint(
        &self,
        pipeline: DimensionPipeline,
        request: crate::worldgen_session::GenerationRequest,
    ) -> Result<GenerationCheckpoint, GenerationLedgerError> {
        let identity = pipeline.identity(PipelineOptions::ALL);
        let state = self.pipeline_ref(identity)?;
        let coordinates = ChunkRequest::single(
            request.target().0,
            request.target().1,
            i32::from(request.dependency_radius()),
        )
        .admission_order();
        let mut frontiers = Vec::with_capacity(coordinates.len());
        for coordinate in &coordinates {
            let frontier = state
                .frontiers
                .get(coordinate)
                .ok_or(GenerationLedgerError::UnknownCoordinate(*coordinate))?;
            frontiers.push((*coordinate, frontier.records().to_vec()));
        }
        let in_halo = coordinates.iter().copied().collect::<BTreeSet<_>>();
        let products: Vec<_> = state
            .products
            .iter()
            .filter(|(key, _)| in_halo.contains(&key.coordinate()))
            .map(|(key, product)| (*key, product.clone()))
            .collect();
        let aggregates: Vec<_> = state
            .aggregates
            .iter()
            .filter(|(coordinate, _)| in_halo.contains(coordinate))
            .map(|(coordinate, aggregate)| (*coordinate, aggregate.clone()))
            .collect();
        let sidecars: Vec<_> = state
            .sidecars
            .iter()
            .filter(|(key, _)| in_halo.contains(&key.coordinate()))
            .map(|(key, sidecar)| (*key, sidecar.clone()))
            .collect();
        let committed_mutations = coordinates
            .iter()
            .filter_map(|coordinate| state.overlays.get(coordinate))
            .flat_map(|bucket| bucket.values())
            .cloned()
            .collect::<Vec<_>>();
        let source_completions = state
            .sources
            .iter()
            .filter(|(key, _)| {
                key.target == request.target() && in_halo.contains(&key.source)
            })
            .map(|(key, source_order)| {
                SourceCompletionRecord::new(key.stage, *source_order, key.source)
            })
            .collect();
        let current_revision = committed_mutations
            .iter()
            .map(|mutation| mutation.provenance().revision().value())
            .max()
            .unwrap_or(0);
        let feature_winner_receipts = state
            .feature_winner_receipts
            .get(&request.target())
            .cloned()
            .unwrap_or_default();
        Ok(GenerationCheckpoint::from_ledger(
            request,
            identity,
            frontiers,
            products,
            sidecars,
            aggregates,
            committed_mutations,
            source_completions,
            state.feature_settlements.get(&request.target()).copied(),
            feature_winner_receipts,
            current_revision,
        ))
    }

    /// Publish a session's committed report atomically. The journal records
    /// only entries touched by this request, so a rejected report can restore
    /// the prior state without cloning unrelated pipelines or products.
    pub(crate) fn publish_session(
        &mut self,
        pipeline: DimensionPipeline,
        session: &GenerationSession,
    ) -> Result<(), GenerationLedgerError> {
        let checkpoint = {
            #[cfg(feature = "worldgen-stage-pmu")]
            let _checkpoint_export = RegionGuard::enter(RegionPhase::CheckpointExport);
            session.export_checkpoint()
        };
        if checkpoint
            .frontiers()
            .iter()
            .all(|(_, records)| records.is_empty())
            && checkpoint.products().is_empty()
            && checkpoint.sidecars().is_empty()
            && checkpoint.aggregates().is_empty()
            && checkpoint.committed_mutations().is_empty()
            && checkpoint.source_completions().is_empty()
            && checkpoint.feature_settlement().is_none()
            && checkpoint.feature_winner_receipts().is_empty()
        {
            return Ok(());
        }
        let identity = pipeline.identity(PipelineOptions::ALL);
        if !self.pipelines.contains_key(&identity) {
            return Err(GenerationLedgerError::UnknownPipeline(identity));
        }
        let mut journal = GenerationLedgerJournal::new(self, identity);
        let result = self.publish_session_inner(pipeline, &checkpoint, &mut journal, None);
        if result.is_err() {
            journal.rollback(self);
        }
        result
    }

    /// Publish several completed sessions against one authoritative set of
    /// final output columns. Cross-target writes already folded into those
    /// columns are validated before any frontier is advanced and are omitted
    /// from the overlay ledger; every other write keeps its provenance.
    pub(crate) fn publish_sessions_with_final_outputs(
        &mut self,
        sessions: &[(DimensionPipeline, &GenerationSession)],
        final_outputs: &BTreeMap<ChunkCoordinate, &ChunkColumn>,
    ) -> Result<(), GenerationLedgerError> {
        self.publish_sessions_with_final_outputs_journaled(sessions, final_outputs)
            .map(|_| ())
    }

    fn publish_sessions_with_final_outputs_and_commit(
        &mut self,
        sessions: &[(DimensionPipeline, &GenerationSession)],
        final_outputs: &BTreeMap<ChunkCoordinate, &ChunkColumn>,
        commit: impl FnOnce() -> Result<GenerationCommitReport, GenerationCommitError>,
    ) -> Result<GenerationCommitReport, GenerationPublicationCommitError> {
        let journals = self
            .publish_sessions_with_final_outputs_journaled(sessions, final_outputs)
            .map_err(GenerationPublicationCommitError::Ledger)?;
        match commit() {
            Ok(report) => Ok(report),
            Err(error) => {
                for journal in journals.into_iter().rev() {
                    journal.rollback(self);
                }
                Err(GenerationPublicationCommitError::Commit(error))
            }
        }
    }

    fn publish_sessions_with_final_outputs_journaled(
        &mut self,
        sessions: &[(DimensionPipeline, &GenerationSession)],
        final_outputs: &BTreeMap<ChunkCoordinate, &ChunkColumn>,
    ) -> Result<Vec<GenerationLedgerJournal>, GenerationLedgerError> {
        if sessions.is_empty() {
            return Ok(Vec::new());
        }

        let finalized = final_outputs.keys().copied().collect::<BTreeSet<_>>();
        let audit = finalized_mutation_audit(self, sessions, &finalized);
        validate_finalized_mutation_audit(&audit, |coordinate, destination| {
            final_outputs.get(&coordinate).map(|column| {
                column.block_state_id(
                    destination.x().rem_euclid(16),
                    destination.y(),
                    destination.z().rem_euclid(16),
                )
            })
        })
        .map_err(|_| GenerationLedgerError::CheckpointMismatch)?;

        let checked = audit
            .winners
            .values()
            .map(|(pipeline, mutation)| (*pipeline, *mutation));
        for (pipeline, mutation) in checked {
            let destination = mutation.provenance().destination();
            let coordinate = (
                destination.x().div_euclid(16),
                destination.z().div_euclid(16),
            );
            let Some(state) = mutation.get::<StateId>() else {
                return Err(GenerationLedgerError::CheckpointMismatch);
            };

            let identity = pipeline.identity(PipelineOptions::ALL);
            let output_key = crate::worldgen_session::ProductKey::new(
                coordinate,
                StageKey::new(pipeline.dimension(), ColumnStage::Output),
                ResourceKey::OutputColumn,
            );
            if let Some(existing) = self
                .pipeline_ref(identity)?
                .products
                .get(&output_key)
                .and_then(|product| product.get::<ChunkColumn>())
            {
                if existing.block_state_id(
                    destination.x().rem_euclid(16),
                    destination.y(),
                    destination.z().rem_euclid(16),
                ) != *state
                    && !self
                        .pipeline_ref(identity)?
                        .accepts_settled_feature_replay(coordinate, mutation)
                {
                    return Err(GenerationLedgerError::CheckpointMismatch);
                }
            }
        }

        let mut journals = Vec::new();
        for &(pipeline, session) in sessions {
            let identity = pipeline.identity(PipelineOptions::ALL);
            let journal_index = if let Some(index) = journals
                .iter()
                .position(|journal: &GenerationLedgerJournal| journal.identity == identity)
            {
                index
            } else {
                if !self.pipelines.contains_key(&identity) {
                    for journal in journals.into_iter().rev() {
                        journal.rollback(self);
                    }
                    return Err(GenerationLedgerError::UnknownPipeline(identity));
                }
                journals.push(GenerationLedgerJournal::new(self, identity));
                journals.len() - 1
            };
            let checkpoint = {
                #[cfg(feature = "worldgen-stage-pmu")]
                let _checkpoint_export = RegionGuard::enter(RegionPhase::CheckpointExport);
                session.export_checkpoint()
            };
            if let Err(error) = self.publish_session_inner(
                pipeline,
                &checkpoint,
                &mut journals[journal_index],
                Some(final_outputs),
            ) {
                for journal in journals.into_iter().rev() {
                    journal.rollback(self);
                }
                return Err(error);
            }
        }
        Ok(journals)
    }

    fn publish_session_inner(
        &mut self,
        pipeline: DimensionPipeline,
        checkpoint: &GenerationCheckpoint,
        journal: &mut GenerationLedgerJournal,
        final_outputs: Option<&BTreeMap<ChunkCoordinate, &ChunkColumn>>,
    ) -> Result<(), GenerationLedgerError> {
        #[cfg(feature = "worldgen-stage-pmu")]
        let _ledger_publish = RegionGuard::enter(RegionPhase::LedgerPublishInner);
        let identity = pipeline.identity(PipelineOptions::ALL);
        if checkpoint.pipeline_identity() != identity {
            return Err(GenerationLedgerError::UnknownPipeline(identity));
        }
        let coordinates = ChunkRequest::single(
            checkpoint.request().target().0,
            checkpoint.request().target().1,
            i32::from(checkpoint.request().dependency_radius()),
        )
        .admission_order();
        {
            let state = self.pipeline_ref(identity)?;
            if coordinates
                .iter()
                .any(|coordinate| !state.frontiers.contains_key(coordinate))
            {
                return Err(GenerationLedgerError::UnknownCoordinate(
                    coordinates
                        .iter()
                        .copied()
                        .find(|coordinate| !state.frontiers.contains_key(coordinate))
                        .expect("the missing coordinate was found"),
                ));
            }
        }

        let mut published_source_stages = BTreeSet::new();
        for (coordinate, records) in checkpoint.frontiers() {
            if let Some(aggregate) = checkpoint
                .aggregates()
                .iter()
                .find(|(candidate, _)| candidate == coordinate)
                .map(|(_, aggregate)| aggregate)
            {
                self.commit_aggregate_prefix(
                    pipeline,
                    aggregate,
                    records,
                    checkpoint.sidecars(),
                    journal,
                )?;
            }
            let existing_len = self
                .frontier(identity, *coordinate)?
                .records()
                .len();
            let current_records = self.frontier(identity, *coordinate)?.records();
            if existing_len >= records.len() {
                if &current_records[..records.len()] != records {
                    return Err(GenerationLedgerError::CheckpointMismatch);
                }
                continue;
            }
            if current_records != &records[..existing_len] {
                return Err(GenerationLedgerError::CheckpointMismatch);
            }
            for record in &records[existing_len..] {
                let aggregate_covered = checkpoint
                    .aggregates()
                    .iter()
                    .find(|(candidate, _)| candidate == coordinate)
                    .and_then(|(_, aggregate)| {
                        self.pipeline_ref(identity)
                            .ok()
                            .and_then(|_| pipeline.schedule().index_of(aggregate.boundary()))
                    })
                    .is_some_and(|boundary| {
                        pipeline
                            .schedule()
                            .index_of(record.key().stage())
                            .is_some_and(|index| index <= boundary)
                    });
                if aggregate_covered {
                    continue;
                }
                let products = checkpoint
                    .products()
                    .iter()
                    .filter(|(key, _)| {
                        key.coordinate() == *coordinate && key.stage() == record.key()
                    })
                    .map(|(_, product)| product.clone())
                    .collect();
                let sidecars = checkpoint
                    .sidecars()
                    .iter()
                    .filter(|(key, _)| {
                        key.coordinate() == *coordinate && key.stage() == record.key()
                    })
                    .map(|(_, sidecar)| sidecar.clone())
                    .collect();
                let completion = ImmutableStageCompletion::new(
                    *coordinate,
                    record.key(),
                    record.input_fingerprint(),
                    record.output_fingerprint(),
                    record.executor_version(),
                    products,
                    sidecars,
                );
                if record.key().dimension() != pipeline.dimension() {
                    return Err(GenerationLedgerError::ForeignStage(record.key()));
                }
                let descriptor = pipeline
                    .descriptor(record.key().stage())
                    .ok_or(GenerationLedgerError::ForeignStage(record.key()))?;
                if descriptor.barrier() == BarrierPolicy::SourceOrdered {
                    if published_source_stages.insert(record.key()) {
                        for completion in checkpoint
                            .source_completions()
                            .iter()
                            .filter(|completion| completion.stage() == record.key())
                        {
                            self.complete_source_with_journal(
                                pipeline,
                                checkpoint.request().target(),
                                completion.source(),
                                completion.stage(),
                                completion.source_order(),
                                Some(journal),
                            )?;
                        }
                    }
                    self.commit_mutable_stage_with_journal(
                        pipeline,
                        &completion,
                        Some(journal),
                    )?;
                } else {
                    self.commit_stage_completion_with_journal(
                        pipeline,
                        &completion,
                        false,
                        Some(journal),
                    )?;
                }
            }
        }

        for mutation in checkpoint.committed_mutations() {
            let destination = mutation.provenance().destination();
            let coordinate = (
                destination.x().div_euclid(16),
                destination.z().div_euclid(16),
            );
            if final_outputs.is_some_and(|outputs| outputs.contains_key(&coordinate)) {
                continue;
            }
            let expected = self.revision(identity, coordinate)?;
            self.commit_overlay_with_journal(
                pipeline,
                coordinate,
                expected,
                mutation.clone(),
                Some(journal),
            )?;
        }
        if let Some(proof) = checkpoint.feature_settlement() {
            let coordinate = proof.output();
            let features_stage = StageKey::new(pipeline.dimension(), ColumnStage::Features);
            let output_stage = StageKey::new(pipeline.dimension(), ColumnStage::Output);
            let expected_proof = pipeline
                .descriptor(ColumnStage::Features)
                .map(|descriptor| {
                    FeatureSettlementProof::square(
                        coordinate,
                        i32::from(descriptor.mutable_write_radius().chunks_value()),
                    )
                });
            let output_key = crate::worldgen_session::ProductKey::new(
                coordinate,
                output_stage,
                ResourceKey::OutputColumn,
            );
            let state = self.pipeline_ref(identity)?;
            let output_frontier = state
                .frontiers
                .get(&coordinate)
                .is_some_and(|frontier| {
                    frontier
                        .records()
                        .iter()
                        .any(|record| record.key() == output_stage)
                });
            let output_exists = state
                .products
                .get(&output_key)
                .and_then(|product| product.get::<ChunkColumn>())
                .is_some();
            let output_is_finalized = final_outputs
                .is_some_and(|outputs| outputs.contains_key(&coordinate));
            if coordinate != checkpoint.request().target()
                || expected_proof != Some(proof)
                || !state
                    .frontiers
                    .get(&coordinate)
                    .is_some_and(|frontier| {
                        frontier
                            .records()
                            .iter()
                            .any(|record| record.key() == features_stage)
                        })
                || output_frontier != output_exists
                || (output_is_finalized && !output_exists)
                || state
                    .feature_settlements
                    .get(&coordinate)
                    .is_some_and(|existing| *existing != proof)
            {
                return Err(GenerationLedgerError::CheckpointMismatch);
            }
            journal.feature_settlement(state, coordinate);
            self.pipelines
                .get_mut(&identity)
                .expect("the checkpoint pipeline remains admitted")
                .feature_settlements
                .insert(coordinate, proof);
        }

        let target = checkpoint.request().target();
        let checkpoint_receipts = checkpoint.feature_winner_receipts();
        let state = self.pipeline_ref(identity)?;
        let retained_receipts = state.feature_winner_receipts.get(&target).cloned();
        if !checkpoint_receipts.is_empty()
            && retained_receipts
                .as_deref()
                .is_some_and(|retained| retained != checkpoint_receipts)
        {
            return Err(GenerationLedgerError::CheckpointMismatch);
        }
        let output_is_finalized = final_outputs.is_some_and(|outputs| outputs.contains_key(&target));
        if output_is_finalized && checkpoint.feature_settlement().is_some() {
            let receipts = if checkpoint_receipts.is_empty() {
                retained_receipts.as_deref().unwrap_or_default()
            } else {
                checkpoint_receipts
            };
            for &receipt in receipts {
                self.retire_settled_feature_overlay(pipeline, receipt, journal)?;
            }
            let state = self.pipeline_ref(identity)?;
            journal.feature_winner_receipts(state, target);
            self.pipelines
                .get_mut(&identity)
                .expect("settled feature pipeline remains admitted")
                .feature_winner_receipts
                .remove(&target);
        } else if !checkpoint_receipts.is_empty() {
            let state = self.pipeline_ref(identity)?;
            journal.feature_winner_receipts(state, target);
            self.pipelines
                .get_mut(&identity)
                .expect("feature receipt pipeline remains admitted")
                .feature_winner_receipts
                .insert(target, checkpoint_receipts.to_vec());
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn product(
        &self,
        identity: PipelineIdentity,
        coordinate: (i32, i32),
        stage: StageKey,
        resource: ResourceKey,
    ) -> Result<Option<ImmutableProduct>, GenerationLedgerError> {
        let state = self.pipeline_ref(identity)?;
        if !state.frontiers.contains_key(&coordinate) {
            return Err(GenerationLedgerError::UnknownCoordinate(coordinate));
        }
        Ok(state
            .products
            .get(&crate::worldgen_session::ProductKey::new(coordinate, stage, resource))
            .cloned())
    }

    /// Return a detached packet output retained by the ledger. This is the
    /// terminal reuse path after a cache eviction: a completed output must be
    /// consumed directly instead of invoking the whole source driver again.
    pub(crate) fn output_column(
        &self,
        pipeline: DimensionPipeline,
        coordinate: (i32, i32),
    ) -> Option<ChunkColumn> {
        let identity = pipeline.identity(PipelineOptions::ALL);
        let state = self.pipelines.get(&identity)?;
        let stage = StageKey::new(pipeline.dimension(), lodestone_worldgen::stage_schedule::ColumnStage::Output);
        state
            .products
            .get(&crate::worldgen_session::ProductKey::new(
                coordinate,
                stage,
                ResourceKey::OutputColumn,
            ))
            .and_then(|product| product.get::<ChunkColumn>())
            .map(|column| (*column).clone())
    }

    /// Retained payload and per-entry state owned by this ledger, including
    /// aggregate shaped residents.
    pub(crate) fn retained_bytes(&self) -> usize {
        self.pipelines
            .values()
            .map(|state| {
                let structural = state.frontiers.len()
                    * std::mem::size_of::<((i32, i32), StageFrontier)>()
                    + state.products.len()
                        * std::mem::size_of::<(crate::worldgen_session::ProductKey, ImmutableProduct)>()
                    + state.sidecars.len()
                        * std::mem::size_of::<(crate::worldgen_session::SidecarProductKey, ImmutableSidecar)>()
                    + state.aggregates.len()
                        * std::mem::size_of::<((i32, i32), AggregatePrefix)>()
                    + state.sources.len() * std::mem::size_of::<(SourceCompletionKey, u64)>()
                    + state.source_next.len()
                        * std::mem::size_of::<((i32, i32), StageKey, u64)>()
                    + state.overlays_len()
                        * std::mem::size_of::<(BlockCoordinate, ProvenanceMutation)>()
                    + state.feature_settlements.len()
                        * std::mem::size_of::<(ChunkCoordinate, FeatureSettlementProof)>()
                    + state.feature_winner_receipts.len()
                        * std::mem::size_of::<(ChunkCoordinate, Vec<TargetFeatureWrite>)>()
                    + state
                        .feature_winner_receipts
                        .values()
                        .map(|receipts| {
                            receipts.len() * std::mem::size_of::<TargetFeatureWrite>()
                        })
                        .sum::<usize>()
                    + state.coordinate_last_used.len()
                        * std::mem::size_of::<((i32, i32), u64)>();
                let payload = state
                    .products
                    .values()
                    .map(ImmutableProduct::retained_bytes)
                    .chain(
                        state
                            .aggregates
                            .values()
                            .map(|aggregate| aggregate.product().retained_bytes()),
                    )
                    .chain(state.sidecars.values().map(ImmutableSidecar::retained_bytes))
                    .chain(state.overlays_iter().map(|(_, mutation)| {
                        mutation.retained_bytes()
                    }))
                    .fold(0usize, usize::saturating_add);
                structural.saturating_add(payload)
            })
            .fold(0usize, usize::saturating_add)
    }

    #[cfg(test)]
    pub(crate) fn commit_overlay(
        &mut self,
        pipeline: DimensionPipeline,
        coordinate: (i32, i32),
        expected: u64,
        mutation: ProvenanceMutation,
    ) -> Result<u64, GenerationLedgerError> {
        self.commit_overlay_with_journal(pipeline, coordinate, expected, mutation, None)
    }

    fn validate_overlay_context(
        &self,
        pipeline: DimensionPipeline,
        coordinate: (i32, i32),
        expected: u64,
        provenance: MutationProvenance,
    ) -> Result<(PipelineIdentity, u64), GenerationLedgerError> {
        if provenance.stage().dimension() != pipeline.dimension() {
            return Err(GenerationLedgerError::ForeignStage(provenance.stage()));
        }
        let descriptor = pipeline
            .descriptor(provenance.stage().stage())
            .ok_or(GenerationLedgerError::ForeignStage(provenance.stage()))?;
        if descriptor.barrier() != BarrierPolicy::SourceOrdered {
            return Err(GenerationLedgerError::NotSourceOrdered(provenance.stage()));
        }
        let destination = provenance.destination();
        let destination_coordinate = (
            destination.x().div_euclid(16),
            destination.z().div_euclid(16),
        );
        if destination_coordinate != coordinate {
            return Err(GenerationLedgerError::UnknownCoordinate(coordinate));
        }
        let identity = pipeline.identity(
            lodestone_worldgen::stage_schedule::PipelineOptions::ALL,
        );
        let state = self.pipeline_ref(identity)?;
        if !state.frontiers.contains_key(&coordinate)
            || !state.frontiers.contains_key(&provenance.target())
        {
            return Err(GenerationLedgerError::UnknownCoordinate(coordinate));
        }
        let found = state
            .revisions
            .get(&coordinate)
            .copied()
            .ok_or(GenerationLedgerError::UnknownCoordinate(coordinate))?;
        if found != expected {
            return Err(GenerationLedgerError::RevisionConflict { coordinate, expected, found });
        }
        Ok((identity, found))
    }

    fn commit_overlay_with_journal(
        &mut self,
        pipeline: DimensionPipeline,
        coordinate: (i32, i32),
        expected: u64,
        mutation: ProvenanceMutation,
        mut journal: Option<&mut GenerationLedgerJournal>,
    ) -> Result<u64, GenerationLedgerError> {
        let limits = self.limits;
        let provenance = mutation.provenance();
        let (identity, found) =
            self.validate_overlay_context(pipeline, coordinate, expected, provenance)?;
        let destination = provenance.destination();
        let state = self.pipeline_ref(identity)?;
        let key = crate::worldgen_session::ProductKey::new(
            coordinate,
            StageKey::new(
                pipeline.dimension(),
                lodestone_worldgen::stage_schedule::ColumnStage::Output,
            ),
            ResourceKey::OutputColumn,
        );
        if let Some(column) = state
            .products
            .get(&key)
            .and_then(|product| product.get::<ChunkColumn>())
        {
            let same_state = mutation.get::<StateId>().is_some_and(|value| {
                column.block_state_id(
                    destination.x().rem_euclid(16),
                    destination.y(),
                    destination.z().rem_euclid(16),
                ) == *value
            });
            if !same_state {
                if state.accepts_settled_feature_replay(coordinate, &mutation) {
                    return Ok(found);
                }
                if generation_ledger_trace_enabled() {
                    let current = column.block_state_id(
                        destination.x().rem_euclid(16),
                        destination.y(),
                        destination.z().rem_euclid(16),
                    );
                    let next = mutation
                        .get::<StateId>()
                        .expect("worldgen block mutation carries StateId");
                    eprintln!(
                        "worldgen ledger mismatch: late output write target={:?} source={:?} destination={destination:?} current={current:?} next={next:?}",
                        provenance.target(),
                        provenance.source(),
                    );
                }
                return Err(GenerationLedgerError::CheckpointMismatch);
            }
            if same_state {
                return Ok(found);
            }
        }
        if let Some(existing) = state.overlay(destination) {
            if existing.provenance() == provenance {
                return Ok(found);
            }
            // Admission order, not worker completion order, owns a shared
            // destination. Provenance is the authenticated canonical order;
            // retain the smaller winner regardless of which result arrived
            // first. A later, lower provenance therefore replaces an earlier
            // speculative write, while a higher one is a deterministic no-op.
            if existing.provenance() < provenance {
                return Ok(found);
            }
        } else if state.overlays_len() >= limits.overlays_per_pipeline {
            return Err(GenerationLedgerError::OverlayCapacity);
        }
        if let Some(journal) = journal.as_deref_mut() {
            journal.overlay(state, destination);
            journal.revision(state, coordinate);
        }
        let state = self.pipeline_mut(pipeline)?;
        state
            .overlays
            .entry(coordinate)
            .or_default()
            .insert(destination, mutation);
        Ok(Self::bump_revision(state, coordinate))
    }

    fn retire_settled_feature_overlay(
        &mut self,
        pipeline: DimensionPipeline,
        receipt: TargetFeatureWrite,
        journal: &mut GenerationLedgerJournal,
    ) -> Result<(), GenerationLedgerError> {
        let identity = pipeline.identity(PipelineOptions::ALL);
        let destination = receipt.destination();
        let coordinate = (
            destination.x().div_euclid(16),
            destination.z().div_euclid(16),
        );
        let state = self.pipeline_ref(identity)?;
        let Some(existing) = state.overlay(destination) else {
            return Ok(());
        };
        if existing.provenance().stage().stage() != ColumnStage::Features
            || existing.provenance().stage().dimension() != pipeline.dimension()
            || !target_feature_write_precedes_mutation(receipt, existing.provenance())
        {
            return Ok(());
        }
        journal.overlay(state, destination);
        journal.revision(state, coordinate);
        let state = self
            .pipelines
            .get_mut(&identity)
            .expect("the feature overlay pipeline remains admitted");
        let empty = state
            .overlays
            .get_mut(&coordinate)
            .is_some_and(|bucket| {
                bucket.remove(&destination);
                bucket.is_empty()
            });
        if empty {
            state.overlays.remove(&coordinate);
        }
        Self::bump_revision(state, coordinate);
        Ok(())
    }

    /// Retire mutations only after their source state is persisted or their
    /// destination is already present in the immutable output product.
    pub(crate) fn settle_mutations(
        &mut self,
        pipeline: DimensionPipeline,
        mutations: &[ProvenanceMutation],
        source_stored: bool,
        ready_destinations: &[(i32, i32)],
    ) -> usize {
        let identity = pipeline.identity(PipelineOptions::ALL);
        let Some(state) = self.pipelines.get_mut(&identity) else {
            return 0;
        };
        let mut by_coordinate = BTreeMap::<
            ChunkCoordinate,
            BTreeMap<BlockCoordinate, ProvenanceMutation>,
        >::new();
        let mut source_candidates = HashSet::new();
        for mutation in mutations {
            let destination = mutation.provenance().destination();
            let coordinate = (
                destination.x().div_euclid(16),
                destination.z().div_euclid(16),
            );
            let current = state
                .overlays
                .get(&coordinate)
                .and_then(|bucket| bucket.get(&destination));
            if current.is_some_and(|existing| {
                existing.provenance() == mutation.provenance()
            }) {
                by_coordinate
                    .entry(coordinate)
                    .or_default()
                    .insert(destination, mutation.clone());
                source_candidates.insert((coordinate, destination));
            }
        }
        for &coordinate in ready_destinations {
            if !state.products.contains_key(&crate::worldgen_session::ProductKey::new(
                coordinate,
                StageKey::new(
                    pipeline.dimension(),
                    lodestone_worldgen::stage_schedule::ColumnStage::Output,
                ),
                ResourceKey::OutputColumn,
            )) {
                continue;
            }
            if let Some(bucket) = state.overlays.get(&coordinate) {
                by_coordinate
                    .entry(coordinate)
                    .or_default()
                    .extend(bucket.iter().map(|(destination, mutation)| {
                        (*destination, mutation.clone())
                    }));
            }
        }
        let mut settled = 0;
        for (coordinate, mutations) in by_coordinate {
            let key = crate::worldgen_session::ProductKey::new(
                coordinate,
                StageKey::new(
                    pipeline.dimension(),
                    lodestone_worldgen::stage_schedule::ColumnStage::Output,
                ),
                ResourceKey::OutputColumn,
            );
            let output = state
                .products
                .get(&key)
                .and_then(|product| product.get::<ChunkColumn>());
            let output_matches = output.as_ref().is_some_and(|column| {
                mutations.values().all(|mutation| {
                    let destination = mutation.provenance().destination();
                    let expected = mutation
                        .get::<StateId>()
                        .expect("worldgen block mutation carries StateId");
                    column.block_state_id(
                        destination.x().rem_euclid(16),
                        destination.y(),
                        destination.z().rem_euclid(16),
                    ) == *expected
                })
            });
            for mutation in mutations.values() {
                let destination = mutation.provenance().destination();
                let is_current = state
                    .overlays
                    .get(&coordinate)
                    .and_then(|bucket| bucket.get(&destination))
                    .is_some_and(|existing| {
                        existing.provenance() == mutation.provenance()
                    });
                let source_persisted = source_stored
                    && output.is_none()
                    && source_candidates.contains(&(coordinate, destination));
                if !is_current || (!output_matches && !source_persisted) {
                    continue;
                }
                let empty = state
                    .overlays
                    .get_mut(&coordinate)
                    .map(|bucket| {
                        bucket.remove(&destination);
                        bucket.is_empty()
                    })
                    .unwrap_or(false);
                if empty {
                    state.overlays.remove(&coordinate);
                }
                settled += 1;
            }
        }
        settled
    }

    #[cfg(test)]
    pub(crate) fn stats(&self) -> GenerationLedgerStats {
        let retained_bytes = self.retained_bytes();
        GenerationLedgerStats {
            pipelines: self.pipelines.len(),
            coordinates: self.pipelines.values().map(|state| state.frontiers.len()).sum(),
            sources: self.pipelines.values().map(|state| state.sources.len()).sum(),
            overlays: self.pipelines.values().map(PipelineLedger::overlays_len).sum(),
            products: self.pipelines.values().map(|state| state.products.len()).sum(),
            aggregates: self.pipelines.values().map(|state| state.aggregates.len()).sum(),
            stamp: self.stamp,
            retained_bytes,
        }
    }

    fn bump_revision(state: &mut PipelineLedger, coordinate: (i32, i32)) -> u64 {
        let next = state
            .revisions
            .get(&coordinate)
            .copied()
            .unwrap_or(0);
        let revision = next.saturating_add(1);
        state.revisions.insert(coordinate, revision);
        revision
    }
}

struct GenerationPipelineLease<'a> {
    ledger: &'a Mutex<GenerationLedger>,
    identity: PipelineIdentity,
}

impl Drop for GenerationPipelineLease<'_> {
    fn drop(&mut self) {
        self.ledger
            .lock()
            .expect("generation ledger lock poisoned")
            .unpin_pipeline(self.identity);
    }
}

struct GenerationBatchEntry {
    index: usize,
    pipeline: DimensionPipeline,
    admitted: Vec<(i32, i32)>,
}

struct PreparedGenerationBatch<'a, S: ChunkSource> {
    region_lease: GenerationRegionLease<'a>,
    halo: ChunkHaloLease<'a, S>,
    entries: Vec<GenerationBatchEntry>,
    reused_columns: Vec<(usize, ChunkColumn)>,
    leases: Vec<GenerationPipelineLease<'a>>,
    results: Vec<
        Option<
            Result<
                Option<crate::worldgen_session::GenerationRequestResult>,
                crate::worldgen_session::GenerationRequestError,
            >,
        >,
    >,
    active_sessions: Vec<GenerationSession>,
}

enum GenerationBatchFinish {
    Complete(
        Vec<
            Result<
                Option<crate::worldgen_session::GenerationRequestResult>,
                crate::worldgen_session::GenerationRequestError,
            >,
        >,
    ),
    RevisionConflict,
}

/// Per-coordinate serialization for writes that update both retention layers.
///
/// The cache mutex cannot cover the wrapped source callback: a persistent
/// source may perform filesystem work, and holding the global cache lock across
/// that callback would stall unrelated columns. A plain unlock between the two
/// layers is also incorrect, though: an older light snapshot can reach the
/// source after a newer block mutation and overwrite it. The table keeps a
/// small revision number for resident coordinates after their active gate is
/// released; eviction prunes idle entries.
#[derive(Debug)]
struct ChunkWriteState {
    held: AtomicBool,
    revision: AtomicU64,
}

struct ChunkWriteObservation {
    chunk: (i32, i32),
    state: Arc<ChunkWriteState>,
    revision: u64,
    column: ChunkColumn,
}

struct ChunkWriteSnapshot<'a> {
    gates: &'a ChunkWriteGates,
    observations: Vec<ChunkWriteObservation>,
}

impl Drop for ChunkWriteSnapshot<'_> {
    fn drop(&mut self) {
        let coordinates = self
            .observations
            .iter()
            .map(|observation| observation.chunk)
            .collect::<Vec<_>>();
        let observations = std::mem::take(&mut self.observations);
        drop(observations);
        self.gates.forget_if_idle(&coordinates);
    }
}

/// The result of a resident-only read attempt.
///
/// `Busy` is distinct from `Absent`: a coordinate can be resident immediately
/// before a generation or mutation claims its write gate, and a caller that
/// observes only `Option` would re-check the cache and then fall through to a
/// blocking `column`/`set_block` call. Tick code uses this three-way result to
/// defer either case without entering generation.
#[derive(Debug, PartialEq, Eq)]
pub enum TryResident<T> {
    /// A writer owns one of the coordinate gates (or the short cache lock was
    /// unavailable), so the caller should retry later.
    Busy,
    /// No complete resident snapshot exists at the requested coordinate.
    Absent,
    /// A resident snapshot was captured while the coordinate gate was held.
    Present(T),
}

/// The result of attempting to retain a block-edit snapshot in a source's
/// mutation-only edit ledger.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TryResidentEdit {
    /// The source's edit ledger is contended; no source or cache state changed.
    Busy,
    /// The source retained this exact post-edit snapshot durably.
    Applied,
}

/// The result of a resident-only block mutation attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TryBlockMutation {
    /// A generation, light settlement, or other write currently owns the
    /// target footprint. No state was changed.
    Busy,
    /// The target is resident, but its source has no nonblocking edit ledger.
    /// No state was changed; the caller may defer to the ordinary blocking
    /// mutation path when that is acceptable.
    Unsupported,
    /// The target column is not resident (or the requested `y` is outside its
    /// height). No state was changed and no generation was started.
    Absent,
    /// The resident cache and the source's mutation-only edit ledger both
    /// accepted the same post-edit snapshot.
    Applied,
}

/// A canonical, multi-coordinate write lease. The table mutex is held only
/// while the lease claims or releases its coordinate records; the lease itself
/// does not hold the global cache mutex, so an expensive light computation can
/// run with the cache available to unrelated coordinates.
struct ChunkWriteLease<'a> {
    gates: &'a ChunkWriteGates,
    coordinates: Vec<(i32, i32)>,
    states: Vec<Arc<ChunkWriteState>>,
    bump_revision: bool,
    revision_filter: Option<Vec<(i32, i32)>>,
}

impl ChunkWriteLease<'_> {
    fn release_and_prune(self) {
        let gates = self.gates;
        let coordinates = self.coordinates.clone();
        drop(self);
        gates.forget_if_idle(&coordinates);
    }
}

impl Drop for ChunkWriteLease<'_> {
    fn drop(&mut self) {
        let mut table = self
            .gates
            .state
            .lock()
            .expect("chunk write-gate table poisoned");
        let retain_revisions = self.bump_revision
            && self
                .states
                .iter()
                .any(|state| Arc::strong_count(state) > 1);
        for (chunk, state) in self.coordinates.iter().copied().zip(&self.states) {
            let selected = self
                .revision_filter
                .as_ref()
                .is_none_or(|coordinates| coordinates.binary_search(&chunk).is_ok());
            if retain_revisions && selected {
                let revision = state.revision.fetch_add(1, Ordering::AcqRel) + 1;
                if let Some(record) = table.get_mut(&chunk) {
                    record.revision = revision;
                }
            }
            state.held.store(false, Ordering::Release);
        }
        drop(table);
        self.gates.wake.notify_all();
    }
}

#[derive(Debug, Default)]
struct ChunkWriteGates {
    state: Mutex<HashMap<(i32, i32), ChunkWriteGateRecord>>,
    wake: Condvar,
}

#[derive(Debug)]
struct ChunkWriteGateRecord {
    state: Weak<ChunkWriteState>,
    revision: u64,
}

impl ChunkWriteGates {
    fn state_for_locked(
        state: &mut HashMap<(i32, i32), ChunkWriteGateRecord>,
        chunk: (i32, i32),
    ) -> Arc<ChunkWriteState> {
        if let Some(record) = state.get(&chunk) {
            if let Some(gate) = record.state.upgrade() {
                return gate;
            }
        }
        let revision = state.get(&chunk).map_or(0, |record| record.revision);
        let gate = Arc::new(ChunkWriteState {
            held: AtomicBool::new(false),
            revision: AtomicU64::new(revision),
        });
        state.insert(
            chunk,
            ChunkWriteGateRecord {
                state: Arc::downgrade(&gate),
                revision,
            },
        );
        gate
    }

    fn forget_if_idle(&self, chunks: &[(i32, i32)]) {
        let mut state = self
            .state
            .lock()
            .expect("chunk write-gate table poisoned");
        for &chunk in chunks {
            if state.get(&chunk).is_some_and(|record| {
                record.state.strong_count() == 0 && record.revision == 0
            })
            {
                state.remove(&chunk);
            }
        }
    }

    /// Acquires one or more coordinate gates in canonical `(cx, cz)` order.
    /// Claiming the sorted set under the table mutex avoids lock inversion when
    /// adjacent columns settle concurrently, while the condition variable
    /// keeps a contended admission finite rather than spinning.
    fn acquire_many<'a>(
        &'a self,
        chunks: &[(i32, i32)],
        bump_revision: bool,
    ) -> ChunkWriteLease<'a> {
        let mut canonical = chunks.to_vec();
        canonical.sort_unstable();
        canonical.dedup();
        loop {
            let mut state = self
                .state
                .lock()
                .expect("chunk write-gate table poisoned");
            state.retain(|_, record| record.state.strong_count() != 0 || record.revision != 0);
            let records = canonical
                .iter()
                .map(|&chunk| (chunk, Self::state_for_locked(&mut state, chunk)))
                .collect::<Vec<_>>();
            if records
                .iter()
                .any(|(_, record)| record.held.load(Ordering::Acquire))
            {
                state = self
                    .wake
                    .wait(state)
                    .expect("chunk write-gate table poisoned");
                drop(state);
                continue;
            }
            let states = records
                .into_iter()
                .map(|(_, record)| record)
                .collect::<Vec<_>>();
            for record in &states {
                record.held.store(true, Ordering::Release);
            }
            drop(state);
            return ChunkWriteLease {
                gates: self,
                coordinates: canonical,
                states,
                bump_revision,
                revision_filter: None,
            };
        }
    }

    fn acquire_many_with_revision_filter<'a>(
        &'a self,
        chunks: &[(i32, i32)],
        revision_filter: &[(i32, i32)],
    ) -> ChunkWriteLease<'a> {
        let mut lease = self.acquire_many(chunks, false);
        let mut revision_filter = revision_filter.to_vec();
        revision_filter.sort_unstable();
        revision_filter.dedup();
        lease.bump_revision = true;
        lease.revision_filter = Some(revision_filter);
        lease
    }

    /// Attempts to claim one or more coordinate gates without waiting.
    ///
    /// The canonical set is checked and claimed while the gate-table mutex is
    /// held, so a caller gets an all-or-nothing answer: it cannot observe a
    /// resident column and then race a generation that claims the same
    /// coordinate before the snapshot is taken. `None` means at least one
    /// coordinate is currently held; no condition-variable wait or generation
    /// is performed.
    fn try_acquire_many<'a>(
        &'a self,
        chunks: &[(i32, i32)],
        bump_revision: bool,
    ) -> Option<ChunkWriteLease<'a>> {
        let mut canonical = chunks.to_vec();
        canonical.sort_unstable();
        canonical.dedup();
        let mut state = self
            .state
            .lock()
            .expect("chunk write-gate table poisoned");
        state.retain(|_, record| record.state.strong_count() != 0 || record.revision != 0);
        let records = canonical
            .iter()
            .map(|&chunk| (chunk, Self::state_for_locked(&mut state, chunk)))
            .collect::<Vec<_>>();
        if records
            .iter()
            .any(|(_, record)| record.held.load(Ordering::Acquire))
        {
            return None;
        }
        let states = records
            .into_iter()
            .map(|(_, record)| record)
            .collect::<Vec<_>>();
        for record in &states {
            record.held.store(true, Ordering::Release);
        }
        drop(state);
        Some(ChunkWriteLease {
            gates: self,
            coordinates: canonical,
            states,
            bump_revision,
            revision_filter: None,
        })
    }

    /// Runs one same-coordinate cache/source write while holding its gate.
    /// Callbacks must not re-enter a write for the same coordinate; source
    /// implementations only receive the complete operation and do not call
    /// back into the outer store.
    fn with<R>(&self, chunk: (i32, i32), operation: impl FnOnce() -> R) -> R {
        let lease = self.acquire_many(&[chunk], true);
        let result = operation();
        lease.release_and_prune();
        result
    }

    /// Captures all requested columns while their coordinate gates are held.
    /// The cache lock is held only by the caller's short `capture` operation;
    /// no gate remains held while light computation runs.
    fn snapshot_many(
        &self,
        chunks: &[(i32, i32)],
        mut capture: impl FnMut((i32, i32)) -> Option<ChunkColumn>,
    ) -> Result<ChunkWriteSnapshot<'_>, ()> {
        let lease = self.acquire_many(chunks, false);
        let observations = lease
            .states
            .iter()
            .zip(lease.coordinates.iter().copied())
            .map(|(state, chunk)| {
                let column = capture(chunk)?;
                let revision = state.revision.load(Ordering::Acquire);
                Some(ChunkWriteObservation {
                    chunk,
                    state: Arc::clone(state),
                    revision,
                    column,
                })
            })
            .collect::<Option<Vec<_>>>();
        lease.release_and_prune();
        observations
            .map(|observations| ChunkWriteSnapshot {
                gates: self,
                observations,
            })
            .ok_or(())
    }

    /// Captures all requested columns under an already-held lease. This is
    /// used only by the bounded final settlement attempt, which keeps the
    /// coordinate gates through compute and commit for guaranteed progress.
    fn snapshot_while_held(
        &self,
        lease: &ChunkWriteLease<'_>,
        mut capture: impl FnMut((i32, i32)) -> Option<ChunkColumn>,
    ) -> Result<ChunkWriteSnapshot<'_>, ()> {
        let observations = lease
            .states
            .iter()
            .zip(lease.coordinates.iter().copied())
            .map(|(state, chunk)| {
                let column = capture(chunk)?;
                let revision = state.revision.load(Ordering::Acquire);
                Some(ChunkWriteObservation {
                    chunk,
                    state: Arc::clone(state),
                    revision,
                    column,
                })
            })
            .collect::<Option<Vec<_>>>();
        observations
            .map(|observations| ChunkWriteSnapshot {
                gates: self,
                observations,
            })
            .ok_or(())
    }

    /// Commits a computed snapshot only if no dependency write completed after
    /// capture. All nine coordinate gates are claimed in canonical order before
    /// validation, so the check and source/cache commit are one transaction.
    fn try_commit<R>(
        &self,
        snapshot: ChunkWriteSnapshot<'_>,
        commit: impl FnOnce() -> R,
    ) -> Result<R, ()> {
        let chunks = snapshot
            .observations
            .iter()
            .map(|observation| observation.chunk)
            .collect::<Vec<_>>();
        let mut lease = self.acquire_many(&chunks, false);
        if snapshot.observations.iter().any(|observation| {
            observation.state.revision.load(Ordering::Acquire) != observation.revision
        }) {
            drop(snapshot);
            lease.release_and_prune();
            return Err(());
        }
        let result = commit();
        drop(snapshot);
        lease.bump_revision = true;
        lease.release_and_prune();
        Ok(result)
    }

}

/// A [`ChunkSource`] that retains what it generates. See the module docs.
pub(crate) struct ChunkStore<S> {
    source: S,
    /// Which derivation [`set_retention_radius`](Self::set_retention_radius)
    /// re-applies. Immutable for the life of the store, unlike the capacity it
    /// produces — the *policy* is a question about whose memory this is, and that
    /// does not change when the slider moves.
    policy: CapacityPolicy,
    cache: Mutex<Cache>,
    /// Same-coordinate write gate used for revision capture and cache commits.
    /// Cold source generation keeps this gate so a mutation cannot become its
    /// input snapshot; the cache mutex remains released during the callback.
    write_gates: ChunkWriteGates,
    /// The only source-facing lifecycle owner. Cache mutation selects bounded
    /// load/release work first; this hand-off serializes source transitions for
    /// one coordinate through their acknowledgement without serializing
    /// independent generation on other coordinates.
    lifecycle: ChunkLifecycleHandoff,
    /// The ticket graph this store's residency answers to — the ticket-driven eviction path. See
    /// [`maybe_tick_tickets`](Self::maybe_tick_tickets) for how it is driven and
    /// this module's own doc section on the ticket/status pipeline for the
    /// design (why this is a plain field rather than a new
    /// [`ChunkSource`] trait method).
    tickets: TicketStoreHandle,
    /// World-owned typed generation state. It is intentionally independent of
    /// packet-column retention so a cancelled request can leave reusable
    /// products and source completions behind.
    generation_ledger: Mutex<GenerationLedger>,
    generation_regions: GenerationRegionCoordinator,
    /// One admission slot per request identity. Waiters hold the slot's
    /// detached signal, so the map may forget it as soon as the leader ends.
    generation_in_flight: Mutex<HashMap<GenerationRequestKey, Arc<GenerationInFlight>>>,
}

impl<S> ChunkStore<S> {
    /// Wraps `source`, retaining up to [`DEFAULT_CAPACITY`] columns.
    ///
    /// **`#[cfg(test)]` because production callers supply a view radius.** Every
    /// production caller is in [`crate::integrated`] and every
    /// one of them already has a `view_radius` in scope, so a store built without
    /// one is exactly the defect: a capacity chosen for a radius, in a different
    /// file from the radius. Removing this from the non-test build means a new
    /// call site cannot reintroduce it by accident — it has to name a capacity or
    /// a radius. The gates below keep it because a store with no view attached
    /// has no radius to derive from.
    #[cfg(test)]
    pub(crate) fn new(source: S) -> Self {
        Self::with_capacity(source, DEFAULT_CAPACITY)
    }

    /// Wraps `source` with the capacity the connection's `view_radius` needs —
    /// [`capacity_for_view_radius`], which carries the derivation and the two
    /// clamps.
    ///
    /// Takes the radius rather than the column count so the *policy* lives in
    /// one place: a call site that computed `(2r+1)² + 50` itself would be a
    /// second copy of the derivation, and the reason this helper exists is
    /// that the number and the radius it was chosen for lived in two different
    /// files.
    pub(crate) fn for_view_radius(source: S, view_radius: i32) -> Self {
        Self::with_policy(
            source,
            CapacityPolicy::Hosted,
            capacity_for_view_radius(view_radius),
        )
    }

    /// [`for_view_radius`](Self::for_view_radius) for the **integrated** server:
    /// the same derivation with no [`MAX_CAPACITY`] ceiling.
    ///
    /// The two constructors are the two halves of one decision, and which one a
    /// call site picks is a question about *whose* memory is being spent — see
    /// [`integrated_capacity_for_view_radius`] for the numbers. Singleplayer is
    /// this one; open-to-LAN (`IntegratedServer::bind`) is the capped one.
    pub(crate) fn for_integrated_view_radius(source: S, view_radius: i32) -> Self {
        Self::with_policy(
            source,
            CapacityPolicy::Integrated,
            integrated_capacity_for_view_radius(view_radius),
        )
    }

    /// Wraps `source` with an explicit capacity that **no later radius change
    /// overrides** ([`CapacityPolicy::Fixed`]). A capacity of 0 disables
    /// retention entirely, which is the pre-store behaviour and is what the
    /// gate below uses as its negative control.
    #[cfg(test)]
    pub(crate) fn with_capacity(source: S, capacity: usize) -> Self {
        Self::with_policy(source, CapacityPolicy::Fixed, capacity)
    }

    fn with_policy(source: S, policy: CapacityPolicy, capacity: usize) -> Self {
        Self {
            source,
            policy,
            cache: Mutex::new(Cache {
                columns: HashMap::new(),
                pins: HashMap::new(),
                capacity,
                stamp: 0,
                generated: 0,
                evicted: 0,
                next_ticket_check: 0,
            }),
            write_gates: ChunkWriteGates::default(),
            lifecycle: ChunkLifecycleHandoff::default(),
            tickets: TicketStoreHandle::new(),
            generation_ledger: Mutex::new(GenerationLedger::new()),
            generation_regions: GenerationRegionCoordinator::default(),
            generation_in_flight: Mutex::new(HashMap::new()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Cache> {
        self.cache.lock().expect("chunk store lock poisoned")
    }

    /// Access the world-owned generation ledger. The guard keeps ledger
    /// updates atomic without sharing a cache lock with expensive generation.
    pub(crate) fn generation_ledger(
        &self,
    ) -> std::sync::MutexGuard<'_, GenerationLedger> {
        self.generation_ledger
            .lock()
            .expect("generation ledger lock poisoned")
    }

    fn begin_generation(
        &self,
        key: GenerationRequestKey,
    ) -> (Arc<GenerationInFlight>, bool) {
        let mut in_flight = self
            .generation_in_flight
            .lock()
            .expect("generation in-flight lock poisoned");
        if let Some(slot) = in_flight.get(&key) {
            return (Arc::clone(slot), false);
        }
        let slot = Arc::new(GenerationInFlight::new());
        in_flight.insert(key, Arc::clone(&slot));
        (slot, true)
    }

    fn finish_generation(
        &self,
        key: GenerationRequestKey,
        slot: &Arc<GenerationInFlight>,
        result: &Result<
            crate::worldgen_session::GenerationRequestResult,
            GenerationSessionExecutionError,
        >,
    ) {
        // Publish before removal so a caller that observes this slot cannot
        // become a second leader between completion and map cleanup.
        slot.complete(result.as_ref().ok());
        let mut in_flight = self
            .generation_in_flight
            .lock()
            .expect("generation in-flight lock poisoned");
        if in_flight
            .get(&key)
            .is_some_and(|current| Arc::ptr_eq(current, slot))
        {
            in_flight.remove(&key);
        }
    }

    // The four accessors below are `#[cfg(test)]` rather than
    // `#[allow(dead_code)]`: nothing in production reads them, and pretending
    // otherwise is how dead code accumulates. Production observability goes
    // through the `Debug` impl, which reports all four. Units U6 (unloading)
    // and U8 (sectioned storage) of `docs/plans/chunk-lifecycle.md` are the
    // ones that will want them for real; drop the `cfg` then.

    /// Columns currently retained. A live halo lease may temporarily exceed
    /// the soft capacity until its pins are released.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.lock().columns.len()
    }

    /// Sum of the packed block buffers retained in the cache.
    ///
    /// This deliberately excludes palettes and map overhead: it is a stable
    /// lower-bound counter for comparing retention regimes without turning a
    /// measurement helper into a second memory model.
    #[cfg(test)]
    pub(crate) fn retained_blocks_heap_bytes(&self) -> usize {
        self.lock()
            .columns
            .values()
            .map(|entry| entry.column.blocks_heap_bytes())
            .sum()
    }

    /// The store's **current** eviction bound. [`set_retention_radius`](Self::set_retention_radius)
    /// can move it after construction.
    #[cfg(test)]
    pub(crate) fn capacity(&self) -> usize {
        self.lock().capacity
    }

    /// Cumulative calls that reached the inner source's `column()`.
    ///
    /// A store-lifetime accumulator: read it as a delta, or from a store
    /// constructed inside the gate. It is a convenience cross-check only — the
    /// gate below counts on its own hand-written source instead, because the
    /// real `OverworldGenerator` carries a 512-entry memo cache that would
    /// absorb a second request and make any count measured *above* it vacuous.
    #[cfg(test)]
    pub(crate) fn generated(&self) -> u64 {
        self.lock().generated
    }

    /// Cumulative evictions. Same accumulator caveat as
    /// [`generated`](Self::generated).
    #[cfg(test)]
    pub(crate) fn evicted(&self) -> u64 {
        self.lock().evicted
    }
}

impl<S> std::fmt::Debug for ChunkStore<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let cache = self.lock();
        let resident_bytes = cache.retained_bytes();
        let resident = cache.columns.len();
        let capacity = cache.capacity;
        let generated = cache.generated;
        let evicted = cache.evicted;
        drop(cache);
        let retained_bytes = resident_bytes
            .saturating_add(self.generation_ledger().retained_bytes());
        f.debug_struct("ChunkStore")
            .field("resident", &resident)
            .field("capacity", &capacity)
            .field("policy", &self.policy)
            .field("generated", &generated)
            .field("evicted", &evicted)
            .field("retained_bytes", &retained_bytes)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub(crate) enum HaloLeaseError {
    #[error("generation halo cannot be empty")]
    Empty,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum GenerationSessionExecutionError {
    #[error("the wrapped source has no request-scoped generation driver")]
    MissingDriver,
    #[error("request generation failed: {0}")]
    Request(#[from] crate::worldgen_session::GenerationRequestError),
    #[error("generation halo admission failed: {0}")]
    Halo(#[from] HaloLeaseError),
    #[error("generation session failed: {0}")]
    Session(#[from] SessionError),
    #[error("generation ledger admission failed: {0}")]
    Ledger(#[from] GenerationLedgerError),
    #[error("generation column commit failed: {0}")]
    Commit(#[from] GenerationCommitError),
}

/// A cache pin and revision snapshot for one canonical generation halo.
/// Write gates are held only while this value is created or checked; they are
/// never held across generation.
pub(crate) struct ChunkHaloLease<'a, S: ChunkSource> {
    store: &'a ChunkStore<S>,
    coordinates: Vec<(i32, i32)>,
    revisions: Vec<u64>,
    states: Vec<Arc<ChunkWriteState>>,
}

impl<S: ChunkSource> ChunkHaloLease<'_, S> {
    pub(crate) fn revision(&self, coordinate: (i32, i32)) -> Option<u64> {
        self.coordinates
            .iter()
            .position(|candidate| *candidate == coordinate)
            .map(|index| self.revisions[index])
    }

    fn acknowledge(&mut self, report: &GenerationCommitReport) {
        for &(coordinate, before, after) in &report.revisions {
            let index = self
                .coordinates
                .binary_search(&coordinate)
                .expect("committed coordinate belongs to its halo");
            assert_eq!(self.revisions[index], before);
            self.revisions[index] = after;
        }
    }
}

impl<S: ChunkSource> Drop for ChunkHaloLease<'_, S> {
    fn drop(&mut self) {
        let mut cache = self.store.lock();
        for coordinate in &self.coordinates {
            if let Some(count) = cache.pins.get_mut(coordinate) {
                *count -= 1;
                if *count == 0 {
                    cache.pins.remove(coordinate);
                }
            }
        }
        drop(cache);
        self.store.evict_excess();
        let states = std::mem::take(&mut self.states);
        drop(states);
        self.store.write_gates.forget_if_idle(&self.coordinates);
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub(crate) enum GenerationCommitError {
    #[error("generation commit has no columns")]
    Empty,
    #[error("generation commit contains duplicate coordinate {0:?}")]
    DuplicateCoordinate((i32, i32)),
    #[error("generation commit coordinate {0:?} is outside its halo")]
    OutsideHalo((i32, i32)),
    #[error("generation mutation destination {0:?} is absent from its commit columns")]
    MissingMutationDestination((i32, i32)),
    #[error("finalized output {coordinate:?} differs from mutation winner at {destination:?}: expected {expected:?}, found {actual:?}")]
    FinalizedMutationMismatch {
        coordinate: ChunkCoordinate,
        destination: BlockCoordinate,
        expected: StateId,
        actual: StateId,
    },
    #[error("generation commit revision conflict at {coordinate:?}: expected {expected}, found {found}")]
    RevisionConflict { coordinate: (i32, i32), expected: u64, found: u64 },
    #[error("finalized output has conflicting feature winner receipts")]
    ConflictingFeatureWinnerReceipts,
}

struct FinalizedMutationAudit<'a> {
    winners: BTreeMap<BlockCoordinate, (DimensionPipeline, &'a ProvenanceMutation)>,
    settled_winners: BTreeMap<BlockCoordinate, (DimensionPipeline, TargetFeatureWrite)>,
    conflicting_receipts: bool,
}

fn finalized_mutation_audit<'a>(
    ledger: &GenerationLedger,
    sessions: &[(DimensionPipeline, &'a GenerationSession)],
    finalized: &BTreeSet<ChunkCoordinate>,
) -> FinalizedMutationAudit<'a> {
    let mut pending_proofs = HashMap::new();
    let mut pipelines = Vec::<PipelineIdentity>::new();
    for &(pipeline, session) in sessions {
        let identity = pipeline.identity(PipelineOptions::ALL);
        if !pipelines.contains(&identity) {
            pipelines.push(identity);
        }
        let target = session.request().target();
        if finalized.contains(&target) {
            if let Some(proof) = session.feature_settlement() {
                if proof.output() == target {
                    pending_proofs.insert((identity, target), proof);
                }
            }
        }
    }
    let mut proofs = HashMap::new();
    for &identity in &pipelines {
        for &coordinate in finalized {
            let pending = pending_proofs.get(&(identity, coordinate)).copied();
            let persisted = ledger
                .pipelines
                .get(&identity)
                .and_then(|state| state.feature_settlements.get(&coordinate))
                .copied();
            if let Some(proof) = pending
                .or(persisted)
                .filter(|proof| proof.output() == coordinate)
            {
                proofs.insert((identity, coordinate), proof);
            }
        }
    }

    let mut audit = FinalizedMutationAudit {
        winners: BTreeMap::new(),
        settled_winners: BTreeMap::new(),
        conflicting_receipts: false,
    };
    for &(pipeline, session) in sessions {
        let emitted_target = session.request().target();
        let identity = pipeline.identity(PipelineOptions::ALL);
        for &receipt in session.feature_winner_receipts() {
            let destination = receipt.destination();
            let coordinate = (
                destination.x().div_euclid(16),
                destination.z().div_euclid(16),
            );
            if !finalized.contains(&coordinate)
                || receipt.owner() != coordinate
                || !proofs
                    .get(&(identity, coordinate))
                    .is_some_and(|proof| proof.contains(receipt.source()))
            {
                audit.conflicting_receipts = true;
                continue;
            }
            match audit.settled_winners.entry(destination) {
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert((pipeline, receipt));
                }
                std::collections::btree_map::Entry::Occupied(entry)
                    if *entry.get() != (pipeline, receipt) =>
                {
                    audit.conflicting_receipts = true;
                }
                std::collections::btree_map::Entry::Occupied(_) => {}
            }
        }
        for mutation in session.committed_mutations() {
            let provenance = mutation.provenance();
            let destination = provenance.destination();
            let coordinate = (
                destination.x().div_euclid(16),
                destination.z().div_euclid(16),
            );
            if !finalized.contains(&coordinate) {
                continue;
            }
            let proof = proofs.get(&(identity, coordinate));
            let projected_feature = proof.is_some_and(|proof| {
                provenance.stage().stage() == ColumnStage::Features
                    && emitted_target == coordinate
                    && proof.contains(provenance.target())
                    && proof.contains(provenance.source())
            });
            if !projected_feature
                && let Some(proof) = proof
                && provenance.stage().stage() == ColumnStage::Features
            {
                if emitted_target != coordinate
                    && provenance.target() == emitted_target
                    && provenance.source() == emitted_target
                    && proof.contains(emitted_target)
                {
                    continue;
                }
            }
            let replaces_winner = audit
                .winners
                .get(&destination)
                .is_none_or(|(_, current)| provenance < current.provenance());
            if replaces_winner {
                audit.winners.insert(destination, (pipeline, mutation));
            }
        }
    }
    audit
}

fn validate_finalized_mutation_audit(
    audit: &FinalizedMutationAudit<'_>,
    mut state_at: impl FnMut(ChunkCoordinate, BlockCoordinate) -> Option<StateId>,
) -> Result<(), GenerationCommitError> {
    if audit.conflicting_receipts {
        return Err(GenerationCommitError::ConflictingFeatureWinnerReceipts);
    }
    for (&destination, (_, receipt)) in &audit.settled_winners {
        let coordinate = (
            destination.x().div_euclid(16),
            destination.z().div_euclid(16),
        );
        validate_finalized_mutation_value(coordinate, destination, receipt.state(), &mut state_at)?;
    }
    for (&destination, (pipeline, mutation)) in &audit.winners {
        let dominated = audit
            .settled_winners
            .get(&destination)
            .is_some_and(|(receipt_pipeline, receipt)| {
                receipt_pipeline == pipeline
                    && target_feature_write_precedes_mutation(
                        *receipt,
                        mutation.provenance(),
                    )
            });
        if dominated {
            continue;
        }
        let coordinate = (
            destination.x().div_euclid(16),
            destination.z().div_euclid(16),
        );
        let expected = *mutation
            .get::<StateId>()
            .expect("worldgen mutations carry StateId");
        validate_finalized_mutation_value(coordinate, destination, expected, &mut state_at)?;
    }
    Ok(())
}

fn validate_finalized_mutation_value(
    coordinate: ChunkCoordinate,
    destination: BlockCoordinate,
    expected: StateId,
    state_at: &mut impl FnMut(ChunkCoordinate, BlockCoordinate) -> Option<StateId>,
) -> Result<(), GenerationCommitError> {
    let actual = state_at(coordinate, destination)
        .ok_or(GenerationCommitError::MissingMutationDestination(coordinate))?;
    if actual != expected {
        return Err(GenerationCommitError::FinalizedMutationMismatch {
            coordinate,
            destination,
            expected,
            actual,
        });
    }
    Ok(())
}

fn cohort_mutation_destination_coordinates(
    target: ChunkCoordinate,
    mutations: &[ProvenanceMutation],
) -> BTreeSet<ChunkCoordinate> {
    mutations
        .iter()
        .map(|mutation| mutation.provenance().destination())
        .map(|destination| {
            (
                destination.x().div_euclid(16),
                destination.z().div_euclid(16),
            )
        })
        .filter(|coordinate| *coordinate != target)
        .collect()
}

#[derive(Debug)]
pub(crate) struct GenerationCommitReport {
    pub(crate) coordinates: Vec<(i32, i32)>,
    revisions: Vec<(ChunkCoordinate, u64, u64)>,
    /// Post-mutation copies of just the columns which the source must retain.
    /// They are captured after the halo revision check, before the gathered
    /// commit columns move into the resident cache.
    persistence_columns: Vec<(ChunkCoordinate, ChunkColumn)>,
}

impl<S: ChunkSource> ChunkStore<S> {
    /// Persist only columns that carry a committed cross-target mutation. A
    /// generated neighbour is normally cache-only, but a spill into that
    /// neighbour is authoritative state and must outlive cache eviction.
    fn persist_generation_mutations(
        &self,
        mutations: &[ProvenanceMutation],
        captured_columns: &[(ChunkCoordinate, ChunkColumn)],
    ) -> bool {
        let destinations = mutations
            .iter()
            .map(|mutation| {
                let destination = mutation.provenance().destination();
                (
                    destination.x().div_euclid(16),
                    destination.z().div_euclid(16),
                )
            })
            .collect::<BTreeSet<_>>();
        if destinations.is_empty() {
            return true;
        }
        let retained = captured_columns
            .iter()
            .filter(|(coordinate, _)| destinations.contains(coordinate))
            .map(|(coordinate, column)| (coordinate.0, coordinate.1, column.clone()))
            .collect::<Vec<_>>();
        debug_assert_eq!(
            retained.len(),
            destinations.len(),
            "a validated generation commit must capture every mutation destination"
        );
        self.source.store_resident_columns(&retained)
    }

    fn emit_completed_cohort_results(
        &self,
        sessions: &[GenerationSession],
        results: Vec<
            Result<
                Option<crate::worldgen_session::GenerationRequestResult>,
                crate::worldgen_session::GenerationRequestError,
            >,
        >,
        emit: &mut dyn FnMut(
            usize,
            &GenerationSession,
            crate::worldgen_session::GenerationRequestResult,
        ) -> Result<(), crate::worldgen_session::GenerationRequestError>,
    ) -> Result<Vec<Result<(), crate::worldgen_session::GenerationRequestError>>, crate::worldgen_session::GenerationRequestError> {
        if results.len() != sessions.len() {
            return Err(crate::worldgen_session::GenerationRequestError::Boundary(
                "generation cohort returned the wrong result count".to_owned(),
            ));
        }
        let mut statuses = Vec::with_capacity(results.len());
        for (index, result) in results.into_iter().enumerate() {
            match result {
                Ok(Some(result)) if !sessions[index].cancellation().is_cancelled() => {
                    emit(index, &sessions[index], result)?;
                    statuses.push(Ok(()));
                }
                Ok(Some(_)) => statuses.push(Err(
                    crate::worldgen_session::GenerationRequestError::Session(
                        SessionError::Cancelled,
                    ),
                )),
                Ok(None) => statuses.push(Err(
                    crate::worldgen_session::GenerationRequestError::Unsupported,
                )),
                Err(error) => statuses.push(Err(error)),
            }
        }
        Ok(statuses)
    }

    fn commit_cohort_output(
        &self,
        halo: &mut ChunkHaloLease<'_, S>,
        entry: &GenerationBatchEntry,
        session: &GenerationSession,
        result: &crate::worldgen_session::GenerationRequestResult,
    ) -> Result<(), crate::worldgen_session::GenerationRequestError> {
        if session.cancellation().is_cancelled() {
            return Err(crate::worldgen_session::GenerationRequestError::Session(
                SessionError::Cancelled,
            ));
        }
        let crate::worldgen_session::GenerationRequestResult::Generated(snapshot) = result else {
            let crate::worldgen_session::GenerationRequestResult::Existing(column) = result else {
                unreachable!();
            };
            let report = self
                .commit_generation_with_mutations_and_persistence_destinations_and_receipts_policy(
                    halo,
                    vec![(session.request().target(), column.clone())],
                    &[],
                    &BTreeSet::new(),
                    &BTreeSet::new(),
                    &[],
                    GenerationCommitMode::Cohort,
                )
                .map_err(|error| {
                    crate::worldgen_session::GenerationRequestError::Boundary(error.to_string())
                })?;
            halo.acknowledge(&report);
            self.evict_excess();
            return Ok(());
        };
        let target = session.request().target();
        if snapshot.coordinate() != target {
            return Err(crate::worldgen_session::GenerationRequestError::Boundary(
                "generation cohort output does not match its admitted target".to_owned(),
            ));
        }
        let finalized = BTreeSet::from([target]);
        let mutations = session
            .committed_mutations()
            .cloned()
            .collect::<Vec<_>>();
        let mutation_coordinates = cohort_mutation_destination_coordinates(target, &mutations);
        let mut columns = Vec::with_capacity(mutation_coordinates.len() + 1);
        columns.push((target, snapshot.column().clone()));
        for coordinate in &mutation_coordinates {
            let neighbour = snapshot
                .neighbours()
                .iter()
                .find(|neighbour| neighbour.coordinate() == *coordinate)
                .ok_or_else(|| {
                    crate::worldgen_session::GenerationRequestError::Boundary(format!(
                        "generation output omitted mutation destination {coordinate:?}"
                    ))
                })?;
            columns.push((*coordinate, neighbour.column().clone()));
        }
        let commit_mutations = mutations
            .iter()
            .filter(|mutation| {
                let destination = mutation.provenance().destination();
                (
                    destination.x().div_euclid(16),
                    destination.z().div_euclid(16),
                ) != target
            })
            .cloned()
            .collect::<Vec<_>>();
        let persistence_destinations = mutations
            .iter()
            .map(|mutation| mutation.provenance().destination())
            .map(|destination| {
                (
                    destination.x().div_euclid(16),
                    destination.z().div_euclid(16),
                )
            })
            .collect::<BTreeSet<_>>();
        let final_outputs = BTreeMap::from([(target, snapshot.column())]);
        let receipts = session.feature_winner_receipts().copied().collect::<Vec<_>>();
        let report = self
            .publish_and_commit_generation(
                &[(entry.pipeline, session)],
                &final_outputs,
                halo,
                columns,
                &commit_mutations,
                &persistence_destinations,
                &finalized,
                &receipts,
            )
            .map_err(|error| match error {
                GenerationPublicationCommitError::Ledger(error) => {
                    crate::worldgen_session::GenerationRequestError::Boundary(error.to_string())
                }
                GenerationPublicationCommitError::Commit(error) => {
                    crate::worldgen_session::GenerationRequestError::Boundary(error.to_string())
                }
            })?;
        halo.acknowledge(&report);
        let source_stored = self.persist_generation_mutations(&mutations, &report.persistence_columns);
        self.generation_ledger().settle_mutations(
            entry.pipeline,
            &mutations,
            source_stored,
            &report.coordinates,
        );
        crate::world_spawn::record_packet_neighbour_admissions(snapshot.neighbours().len());
        Ok(())
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn execute_generation_cohort(
        &self,
        sessions: &mut [GenerationSession],
        emit: &mut dyn FnMut(
            usize,
            &GenerationSession,
            crate::worldgen_session::GenerationRequestResult,
        ) -> Result<(), crate::worldgen_session::GenerationRequestError>,
    ) -> Result<Vec<Result<(), crate::worldgen_session::GenerationRequestError>>, crate::worldgen_session::GenerationRequestError> {
        if sessions.len() >= 2 && !Self::batch_pipeline_identities_match(sessions) {
            return Err(crate::worldgen_session::GenerationRequestError::Boundary(
                "generation cohort requires one pipeline identity".to_owned(),
            ));
        }
        let has_retained_target = sessions.iter().any(|session| {
            let target = session.request().target();
            self.resident_column(target.0, target.1)
                .is_some_and(|column| column.generation_stage() >= ChunkGenerationStage::Full)
                || self
                    .retained_output_column(session.pipeline(), target)
                    .is_some()
        });
        if sessions.len() < 2
            || has_retained_target
            || sessions.iter().any(|session| {
                session.request().generation_target() != GenerationTarget::Full
            })
        {
            let outcomes = self.execute_generation_batch(sessions);
            return self.emit_completed_cohort_results(sessions, outcomes, emit);
        }

        let prepared = match self.prepare_generation_batch(sessions) {
            Ok(prepared) => prepared,
            Err(results) => {
                if results.len() != sessions.len() {
                    return Err(crate::worldgen_session::GenerationRequestError::Boundary(
                        "generation cohort admission returned the wrong result count".to_owned(),
                    ));
                }
                return self.emit_completed_cohort_results(sessions, results, emit);
            }
        };
        if prepared.entries.is_empty() {
            let outcomes = prepared
                .results
                .into_iter()
                .map(|result| result.unwrap_or(Ok(None)))
                .collect();
            return self.emit_completed_cohort_results(sessions, outcomes, emit);
        }
        if !prepared.reused_columns.is_empty()
            || prepared.results.iter().any(|result| {
                matches!(result, Some(Ok(Some(_))))
            })
        {
            let PreparedGenerationBatch {
                entries,
                active_sessions,
                ..
            } = prepared;
            for entry in &entries {
                self.generation_ledger()
                    .rollback_admission(entry.pipeline, &entry.admitted);
            }
            for (entry, active) in entries.into_iter().zip(active_sessions) {
                sessions[entry.index] = active;
            }
            let outcomes = self.execute_generation_batch(sessions);
            return self.emit_completed_cohort_results(sessions, outcomes, emit);
        }

        let PreparedGenerationBatch {
            region_lease: _region_lease,
            mut halo,
            entries,
            leases,
            mut results,
            mut active_sessions,
            ..
        } = prepared;
        let mut statuses = (0..sessions.len()).map(|_| None).collect::<Vec<_>>();
        for (index, result) in results.iter_mut().enumerate() {
            if let Some(result) = result.take() {
                statuses[index] = Some(match result {
                    Ok(Some(_)) | Ok(None) => {
                        Err(crate::worldgen_session::GenerationRequestError::Unsupported)
                    }
                    Err(error) => Err(error),
                });
            }
        }
        let mut committed = vec![false; entries.len()];
        let mut committed_existing = vec![false; entries.len()];
        let mut discard_uncommitted = vec![false; entries.len()];
        let mut on_stable = |active_index: usize,
                             session: &GenerationSession,
                             result: crate::worldgen_session::GenerationRequestResult| {
            let Some(entry) = entries.get(active_index) else {
                return Err(crate::worldgen_session::GenerationRequestError::Boundary(
                    "generation cohort emitted an unknown owner index".to_owned(),
                ));
            };
            if session.cancellation().is_cancelled() {
                discard_uncommitted[active_index] = true;
                return Err(crate::worldgen_session::GenerationRequestError::Session(
                    SessionError::Cancelled,
                ));
            }
            if let Err(error) = self.commit_cohort_output(&mut halo, entry, session, &result) {
                discard_uncommitted[active_index] = true;
                return Err(error);
            }
            committed_existing[active_index] = matches!(
                &result,
                crate::worldgen_session::GenerationRequestResult::Existing(_)
            );
            committed[active_index] = true;
            emit(entry.index, session, result)
        };
        let cohort_result = self
            .source
            .request_generation_cohort(&mut active_sessions, &mut on_stable);
        drop(on_stable);
        for (entry, active) in entries.iter().zip(active_sessions) {
            sessions[entry.index] = active;
        }

        let mut cohort_error = None;
        let mut source_statuses = match cohort_result {
            Ok(source_statuses) if source_statuses.len() == entries.len() => Some(source_statuses),
            Ok(_) => {
                cohort_error = Some(
                    crate::worldgen_session::GenerationRequestError::Boundary(
                        "generation cohort returned the wrong status count".to_owned(),
                    ),
                );
                None
            }
            Err(error) => {
                cohort_error = Some(error);
                None
            }
        };
        let cohort_failed = cohort_error.is_some();

        for (active_index, entry) in entries.iter().enumerate() {
            if committed[active_index] {
                statuses[entry.index] = Some(Ok(()));
                continue;
            }
            let session = &sessions[entry.index];
            let source_status = source_statuses
                .as_mut()
                .and_then(|statuses| statuses.get_mut(active_index))
                .map(|status| std::mem::replace(status, Ok(())));
            let was_cancelled = session.cancellation().is_cancelled()
                || source_status.as_ref().is_some_and(|status| {
                    matches!(
                        status,
                        Err(crate::worldgen_session::GenerationRequestError::Session(
                            SessionError::Cancelled
                        ))
                    )
                });
            discard_uncommitted[active_index] |= was_cancelled || cohort_failed;
            if !discard_uncommitted[active_index] {
                if let Err(error) = self
                    .generation_ledger()
                    .publish_session(entry.pipeline, session)
                {
                    statuses[entry.index] = Some(Err(
                        crate::worldgen_session::GenerationRequestError::Boundary(
                            error.to_string(),
                        ),
                    ));
                    self.generation_ledger()
                        .rollback_admission(entry.pipeline, &entry.admitted);
                    continue;
                }
            }
            self.generation_ledger()
                .rollback_admission(entry.pipeline, &entry.admitted);
            if let Some(source_status) = source_status {
                statuses[entry.index] = Some(source_status);
            } else if discard_uncommitted[active_index] {
                statuses[entry.index] = Some(Err(
                    crate::worldgen_session::GenerationRequestError::Session(
                        SessionError::Cancelled,
                    ),
                ));
            }
        }
        for (active_index, entry) in entries.iter().enumerate() {
            if committed_existing[active_index] {
                self.generation_ledger()
                    .rollback_admission(entry.pipeline, &entry.admitted);
            }
        }
        drop(leases);
        if let Some(error) = cohort_error {
            return Err(error);
        }
        Ok(statuses
            .into_iter()
            .map(|status| {
                status.unwrap_or_else(|| {
                    Err(crate::worldgen_session::GenerationRequestError::Boundary(
                        "generation cohort did not settle a target".to_owned(),
                    ))
                })
            })
            .collect())
    }

    fn publish_and_commit_generation(
        &self,
        sessions: &[(DimensionPipeline, &GenerationSession)],
        final_outputs: &BTreeMap<ChunkCoordinate, &ChunkColumn>,
        halo: &ChunkHaloLease<'_, S>,
        columns: Vec<(ChunkCoordinate, ChunkColumn)>,
        mutations: &[ProvenanceMutation],
        persistence_destinations: &BTreeSet<ChunkCoordinate>,
        finalized: &BTreeSet<ChunkCoordinate>,
        receipts: &[TargetFeatureWrite],
    ) -> Result<GenerationCommitReport, GenerationPublicationCommitError> {
        let mut ledger = self.generation_ledger();
        let result = ledger.publish_sessions_with_final_outputs_and_commit(
            sessions,
            final_outputs,
            || {
                self.commit_generation_with_mutations_and_persistence_destinations_and_receipts_policy(
                    halo,
                    columns,
                    mutations,
                    persistence_destinations,
                    finalized,
                    receipts,
                    GenerationCommitMode::Cohort,
                )
            },
        );
        drop(ledger);
        if result.is_ok() {
            self.evict_excess();
        }
        result
    }

    fn apply_generation_mutations(
        columns: &mut [(ChunkCoordinate, ChunkColumn)],
        mutations: &[ProvenanceMutation],
        finalized: &BTreeSet<ChunkCoordinate>,
    ) -> Result<(), GenerationCommitError> {
        Self::apply_generation_mutations_with_receipts(columns, mutations, finalized, &[])
    }

    fn apply_generation_mutations_with_receipts(
        columns: &mut [(ChunkCoordinate, ChunkColumn)],
        mutations: &[ProvenanceMutation],
        finalized: &BTreeSet<ChunkCoordinate>,
        receipts: &[TargetFeatureWrite],
    ) -> Result<(), GenerationCommitError> {
        let finalized_winners = Self::finalized_mutation_winners(mutations, finalized);
        Self::validate_finalized_mutation_winners_with_receipts(
            &finalized_winners,
            receipts,
            |coordinate, destination| {
                columns
                    .iter()
                    .find(|(candidate, _)| *candidate == coordinate)
                    .map(|(_, column)| {
                        column.block_state_id(
                            destination.x().rem_euclid(16),
                            destination.y(),
                            destination.z().rem_euclid(16),
                        )
                    })
            },
        )?;
        let mut writes = BTreeMap::<ChunkCoordinate, Vec<(i32, i32, i32, StateId)>>::new();
        for mutation in mutations {
            let state = *mutation
                .get::<StateId>()
                .expect("worldgen mutations carry StateId");
            let destination = mutation.provenance().destination();
            let coordinate = (
                destination.x().div_euclid(16),
                destination.z().div_euclid(16),
            );
            if finalized.contains(&coordinate) {
                continue;
            }
            if columns.iter().any(|(candidate, _)| *candidate == coordinate) {
                writes.entry(coordinate).or_default().push((
                    destination.x().rem_euclid(16),
                    destination.y(),
                    destination.z().rem_euclid(16),
                    state,
                ));
            }
        }
        for (coordinate, writes) in writes {
            if let Some((_, column)) = columns
                .iter_mut()
                .find(|(candidate, _)| *candidate == coordinate)
            {
                Self::apply_generation_writes(column, &writes);
            }
        }
        Ok(())
    }

    fn finalized_mutation_winners(
        mutations: &[ProvenanceMutation],
        finalized: &BTreeSet<ChunkCoordinate>,
    ) -> BTreeMap<BlockCoordinate, (MutationProvenance, StateId)> {
        let mut winners = BTreeMap::new();
        for mutation in mutations {
            let destination = mutation.provenance().destination();
            let coordinate = (
                destination.x().div_euclid(16),
                destination.z().div_euclid(16),
            );
            if !finalized.contains(&coordinate) {
                continue;
            }
            let provenance = mutation.provenance();
            let state = *mutation
                .get::<StateId>()
                .expect("worldgen mutations carry StateId");
            if winners
                .get(&destination)
                .is_none_or(|(current, _)| provenance < *current)
            {
                winners.insert(destination, (provenance, state));
            }
        }
        winners
    }

    fn validate_finalized_mutation_winners_with_receipts(
        winners: &BTreeMap<BlockCoordinate, (MutationProvenance, StateId)>,
        receipts: &[TargetFeatureWrite],
        mut state_at: impl FnMut(ChunkCoordinate, BlockCoordinate) -> Option<StateId>,
    ) -> Result<(), GenerationCommitError> {
        let mut settled_winners = BTreeMap::new();
        for &receipt in receipts {
            let destination = receipt.destination();
            let coordinate = (
                destination.x().div_euclid(16),
                destination.z().div_euclid(16),
            );
            if receipt.owner() != coordinate
                || settled_winners.insert(destination, receipt).is_some()
            {
                return Err(GenerationCommitError::ConflictingFeatureWinnerReceipts);
            }
            let actual = state_at(coordinate, destination)
                .ok_or(GenerationCommitError::MissingMutationDestination(coordinate))?;
            if actual != receipt.state() {
                return Err(GenerationCommitError::FinalizedMutationMismatch {
                    coordinate,
                    destination,
                    expected: receipt.state(),
                    actual,
                });
            }
        }
        for (&destination, (provenance, expected)) in winners {
            let coordinate = (
                destination.x().div_euclid(16),
                destination.z().div_euclid(16),
            );
            if settled_winners.get(&destination).is_some_and(|receipt| {
                target_feature_write_precedes_mutation(*receipt, *provenance)
            }) {
                continue;
            }
            let actual = state_at(coordinate, destination)
                .ok_or(GenerationCommitError::MissingMutationDestination(coordinate))?;
            if actual != *expected {
                return Err(GenerationCommitError::FinalizedMutationMismatch {
                    coordinate,
                    destination,
                    expected: *expected,
                    actual,
                });
            }
        }
        Ok(())
    }

    fn apply_generation_writes(
        column: &mut ChunkColumn,
        writes: &[(i32, i32, i32, StateId)],
    ) {
        let writes = writes
            .iter()
            .map(|(x, y, z, state)| (*x, *y, *z, *state))
            .collect::<Vec<_>>();
        column.apply_ordered_block_id_batch(&writes);
    }

    fn generation_key(request: crate::worldgen_session::GenerationRequest) -> GenerationRequestKey {
        GenerationRequestKey {
            dimension: request.dimension(),
            target: request.target(),
            generation_target: request.generation_target(),
            dependency_radius: request.dependency_radius(),
        }
    }

    fn retained_output_column(
        &self,
        pipeline: DimensionPipeline,
        coordinate: (i32, i32),
    ) -> Option<ChunkColumn> {
        self.generation_ledger().output_column(pipeline, coordinate)
    }

    fn prepare_generation_batch(
        &self,
        sessions: &mut [GenerationSession],
    ) -> Result<
        PreparedGenerationBatch<'_, S>,
        Vec<
            Result<
                Option<crate::worldgen_session::GenerationRequestResult>,
                crate::worldgen_session::GenerationRequestError,
            >,
        >,
    > {
        let coordinates = match crate::production_worldgen_session::required_generation_halo(
            sessions,
        ) {
            Ok(coordinates) => coordinates,
            Err(error) => {
                let message = error.to_string();
                return Err((0..sessions.len())
                    .map(|_| {
                        Err(crate::worldgen_session::GenerationRequestError::Boundary(
                            message.clone(),
                        ))
                    })
                    .collect());
            }
        };
        let Some(cancellation) = sessions
            .iter()
            .find(|session| !session.cancellation().is_cancelled())
            .map(GenerationSession::cancellation)
        else {
            return Err(sessions
                .iter()
                .map(|_| {
                    Err(crate::worldgen_session::GenerationRequestError::Session(
                        SessionError::Cancelled,
                    ))
                })
                .collect());
        };
        #[cfg(target_arch = "wasm32")]
        let _ = &cancellation;
        #[cfg(not(target_arch = "wasm32"))]
        let region_lease = match self.generation_regions.acquire(&coordinates, &cancellation) {
            Ok(lease) => lease,
            Err(_) => {
                return Err(sessions
                    .iter()
                    .map(|_| {
                        Err(crate::worldgen_session::GenerationRequestError::Session(
                            SessionError::Cancelled,
                        ))
                    })
                    .collect());
            }
        };
        #[cfg(target_arch = "wasm32")]
        let region_lease = {
            let ticket = self.generation_regions.enqueue(&coordinates);
            if !self.generation_regions.try_activate(ticket) {
                return Err(sessions
                    .iter()
                    .map(|_| {
                        Err(crate::worldgen_session::GenerationRequestError::Session(
                            SessionError::Cancelled,
                        ))
                    })
                    .collect());
            }
            GenerationRegionLease {
                coordinator: &self.generation_regions,
                ticket,
            }
        };
        let halo = match self.lease_halo(&coordinates) {
            Ok(halo) => halo,
            Err(error) => {
                let error = error.to_string();
                return Err(sessions
                    .iter()
                    .map(|_| {
                        Err(crate::worldgen_session::GenerationRequestError::Boundary(
                            error.clone(),
                        ))
                    })
                    .collect());
            }
        };

        let mut entries: Vec<GenerationBatchEntry> = Vec::new();
        let mut reused_columns = Vec::new();
        let mut leases = Vec::new();
        let mut results = (0..sessions.len())
            .map(|_| None)
            .collect::<Vec<Option<Result<
                Option<crate::worldgen_session::GenerationRequestResult>,
                crate::worldgen_session::GenerationRequestError,
            >>>>();

        for index in 0..sessions.len() {
            let session = &mut sessions[index];
            if session.cancellation().is_cancelled() {
                results[index] = Some(Err(
                    crate::worldgen_session::GenerationRequestError::Session(
                        SessionError::Cancelled,
                    ),
                ));
                continue;
            }
            let request = session.request();
            let required_stage = ChunkGenerationStage::Full;
            if let Some(column) = self.resident_column(request.target().0, request.target().1)
                && column.generation_stage() >= required_stage
            {
                crate::world_spawn::record_existing_hit();
                results[index] = Some(Ok(Some(
                    crate::worldgen_session::GenerationRequestResult::Existing(column),
                )));
                continue;
            }
            let pipeline = session.pipeline();
            let (admitted, checkpoint) = {
                let mut ledger = self.generation_ledger();
                let admitted = match ledger.admit(pipeline, &coordinates) {
                    Ok(admitted) => admitted,
                    Err(error) => {
                        let message = error.to_string();
                        drop(ledger);
                        for entry in &entries {
                            self.generation_ledger()
                                .rollback_admission(entry.pipeline, &entry.admitted);
                        }
                        return Err((0..sessions.len())
                            .map(|_| {
                                Err(crate::worldgen_session::GenerationRequestError::Boundary(
                                    message.clone(),
                                ))
                            })
                            .collect());
                    }
                };
                if let Err(error) = ledger.publish_session(pipeline, session) {
                    let message = error.to_string();
                    ledger.rollback_admission(pipeline, &admitted);
                    drop(ledger);
                    for entry in &entries {
                        self.generation_ledger()
                            .rollback_admission(entry.pipeline, &entry.admitted);
                    }
                    return Err((0..sessions.len())
                        .map(|_| {
                            Err(crate::worldgen_session::GenerationRequestError::Boundary(
                                message.clone(),
                            ))
                        })
                        .collect());
                }
                let checkpoint = match {
                    #[cfg(feature = "worldgen-stage-pmu")]
                    let _checkpoint_capture =
                        RegionGuard::enter(RegionPhase::LedgerCheckpointCapture);
                    ledger.checkpoint(pipeline, request)
                } {
                    Ok(checkpoint) => checkpoint,
                    Err(error) => {
                        let message = error.to_string();
                        ledger.rollback_admission(pipeline, &admitted);
                        drop(ledger);
                        for entry in &entries {
                            self.generation_ledger()
                                .rollback_admission(entry.pipeline, &entry.admitted);
                        }
                        return Err((0..sessions.len())
                            .map(|_| {
                                Err(crate::worldgen_session::GenerationRequestError::Boundary(
                                    message.clone(),
                                ))
                            })
                            .collect());
                    }
                };
                let identity = match ledger.pin_pipeline(pipeline) {
                    Ok(identity) => identity,
                    Err(error) => {
                        let message = error.to_string();
                        ledger.rollback_admission(pipeline, &admitted);
                        drop(ledger);
                        for entry in &entries {
                            self.generation_ledger()
                                .rollback_admission(entry.pipeline, &entry.admitted);
                        }
                        return Err((0..sessions.len())
                            .map(|_| {
                                Err(crate::worldgen_session::GenerationRequestError::Boundary(
                                    message.clone(),
                                ))
                            })
                            .collect());
                    }
                };
                leases.push(GenerationPipelineLease {
                    ledger: &self.generation_ledger,
                    identity,
                });
                (admitted, checkpoint)
            };
            let hydration = {
                #[cfg(feature = "worldgen-stage-pmu")]
                let _session_hydration = RegionGuard::enter(RegionPhase::SessionHydration);
                GenerationSession::from_checkpoint_with_budget_and_cancellation(
                    checkpoint,
                    session.budget(),
                    session.cancellation(),
                )
            };
            let hydration = match hydration {
                Ok(hydration) => hydration,
                Err(error) => {
                    self.generation_ledger()
                        .rollback_admission(pipeline, &admitted);
                    for entry in &entries {
                        self.generation_ledger()
                            .rollback_admission(entry.pipeline, &entry.admitted);
                    }
                    let message = error.to_string();
                    return Err((0..sessions.len())
                        .map(|_| {
                            Err(crate::worldgen_session::GenerationRequestError::Boundary(
                                message.clone(),
                            ))
                        })
                        .collect());
                }
            };
            *session = hydration;
            if let Some(column) = self.retained_output_column(pipeline, request.target()) {
                self.generation_ledger()
                    .rollback_admission(pipeline, &admitted);
                let mutations = session.committed_mutations().cloned().collect::<Vec<_>>();
                let mut retained = vec![(request.target(), column)];
                if let Err(error) =
                    Self::apply_generation_mutations(&mut retained, &mutations, &BTreeSet::new())
                {
                    results[index] = Some(Err(
                        crate::worldgen_session::GenerationRequestError::Boundary(
                            error.to_string(),
                        ),
                    ));
                    continue;
                }
                let column = retained
                    .pop()
                    .expect("retained target column was installed")
                    .1;
                reused_columns.push((index, column.clone()));
                results[index] = Some(Ok(Some(
                    crate::worldgen_session::GenerationRequestResult::Existing(column),
                )));
                crate::world_spawn::record_existing_hit();
                continue;
            }
            entries.push(GenerationBatchEntry {
                index,
                pipeline,
                admitted,
            });
        }

        let mut active_sessions = Vec::with_capacity(entries.len());
        for entry in &entries {
            let session = &mut sessions[entry.index];
            let placeholder = GenerationSession::with_budget_and_cancellation(
                session.request(),
                session.budget(),
                session.cancellation(),
            );
            active_sessions.push(std::mem::replace(session, placeholder));
        }
        Ok(PreparedGenerationBatch {
            region_lease,
            halo,
            entries,
            reused_columns,
            leases,
            results,
            active_sessions,
        })
    }

    fn finish_generation_batch(
        &self,
        sessions: &mut [GenerationSession],
        prepared: PreparedGenerationBatch<'_, S>,
        generated: Vec<
            Result<
                Option<crate::worldgen_session::GenerationRequestResult>,
                crate::worldgen_session::GenerationRequestError,
            >,
        >,
    ) -> GenerationBatchFinish {
        let PreparedGenerationBatch {
            region_lease: _region_lease,
            halo,
            entries,
            reused_columns,
            leases,
            mut results,
            active_sessions,
        } = prepared;
        if generated.len() != entries.len() {
            for (entry, session) in entries.iter().zip(active_sessions) {
                sessions[entry.index] = session;
            }
            for entry in &entries {
                self.generation_ledger()
                    .rollback_admission(entry.pipeline, &entry.admitted);
            }
            return GenerationBatchFinish::Complete((0..sessions.len())
                .map(|_| {
                    Err(crate::worldgen_session::GenerationRequestError::Boundary(
                        "generation batch returned the wrong result count".to_owned(),
                    ))
                })
                .collect());
        }

        let mut commit_columns = reused_columns
            .iter()
            .map(|(index, column)| (sessions[*index].request().target(), column.clone()))
            .collect::<BTreeMap<_, _>>();
        let mut finalized_coordinates = reused_columns
            .iter()
            .map(|(index, _)| sessions[*index].request().target())
            .collect::<BTreeSet<_>>();
        let mut generated_snapshots = Vec::new();
        let mut failed_entries = Vec::<GenerationBatchEntry>::new();
        let mut committed_mutations = Vec::new();
        for (index, _) in &reused_columns {
            let target = sessions[*index].request().target();
            committed_mutations.extend(
                sessions[*index]
                    .committed_mutations()
                    .filter(|mutation| {
                        let destination = mutation.provenance().destination();
                        (
                            destination.x().div_euclid(16),
                            destination.z().div_euclid(16),
                        ) != target
                    })
                    .cloned(),
            );
        }
        let mut commit_indices = reused_columns
            .iter()
            .map(|(index, _)| *index)
            .collect::<Vec<_>>();
        for ((entry, session), generation) in entries
            .into_iter()
            .zip(active_sessions)
            .zip(generated)
        {
            let index = entry.index;
            sessions[index] = session;
            let session = &mut sessions[index];
            match generation {
                Err(error) => {
                    results[index] = Some(Err(error));
                    failed_entries.push(entry);
                }
                Ok(None) => {
                    self.generation_ledger()
                        .rollback_admission(entry.pipeline, &entry.admitted);
                    results[index] = Some(Ok(None));
                }
                Ok(Some(crate::worldgen_session::GenerationRequestResult::Existing(column))) => {
                    crate::world_spawn::record_existing_hit();
                    self.generation_ledger()
                        .rollback_admission(entry.pipeline, &entry.admitted);
                    commit_columns.insert(session.request().target(), column.clone());
                    finalized_coordinates.insert(session.request().target());
                    commit_indices.push(index);
                    results[index] = Some(Ok(Some(
                        crate::worldgen_session::GenerationRequestResult::Existing(column),
                    )));
                }
                Ok(Some(crate::worldgen_session::GenerationRequestResult::Generated(snapshot))) => {
                    if session.cancellation().is_cancelled() {
                        self.generation_ledger()
                            .rollback_admission(entry.pipeline, &entry.admitted);
                        results[index] = Some(Err(
                            crate::worldgen_session::GenerationRequestError::Session(
                                SessionError::Cancelled,
                            ),
                        ));
                        continue;
                    }
                    commit_columns.insert(snapshot.coordinate(), snapshot.column().clone());
                    finalized_coordinates.insert(snapshot.coordinate());
                    for neighbour in snapshot.neighbours() {
                        commit_columns
                            .entry(neighbour.coordinate())
                            .or_insert_with(|| neighbour.column().clone());
                    }
                    committed_mutations.extend(session.committed_mutations().cloned());
                    commit_indices.push(index);
                    generated_snapshots.push((index, entry, snapshot));
                }
            }
        }

        let final_outputs: BTreeMap<ChunkCoordinate, &ChunkColumn> = finalized_coordinates
            .iter()
            .filter_map(|&coordinate| {
                commit_columns
                    .get(&coordinate)
                    .map(|column| (coordinate, column))
            })
            .collect();
        let mut publication_sessions = Vec::with_capacity(
            generated_snapshots.len() + failed_entries.len(),
        );
        publication_sessions.extend(generated_snapshots.iter().map(|(_, entry, _)| {
            (entry.pipeline, &sessions[entry.index])
        }));
        publication_sessions.extend(
            failed_entries
                .iter()
                .map(|entry| (entry.pipeline, &sessions[entry.index])),
        );
        let publication_error = if generated_snapshots.is_empty() && failed_entries.is_empty() {
            None
        } else {
            self.generation_ledger()
                .publish_sessions_with_final_outputs(&publication_sessions, &final_outputs)
                .err()
        };
        let publication_failed = publication_error.is_some();
        if let Some(error) = publication_error {
            for (_, entry, _) in &generated_snapshots {
                self.generation_ledger()
                    .rollback_admission(entry.pipeline, &entry.admitted);
            }
            for entry in &failed_entries {
                self.generation_ledger()
                    .rollback_admission(entry.pipeline, &entry.admitted);
            }
            for &index in &commit_indices {
                results[index] = Some(Err(
                    crate::worldgen_session::GenerationRequestError::Boundary(
                        error.to_string(),
                    ),
                ));
            }
        } else {
            for entry in &failed_entries {
                self.generation_ledger()
                    .rollback_admission(entry.pipeline, &entry.admitted);
            }
        }

        if publication_failed {
            drop(leases);
            return GenerationBatchFinish::Complete(results
                .into_iter()
                .map(|result| {
                    result.unwrap_or_else(|| {
                        Err(crate::worldgen_session::GenerationRequestError::Boundary(
                            "generation batch did not produce a result".to_owned(),
                        ))
                    })
                })
                .collect());
        }

        let persistence_destinations = generated_snapshots
            .iter()
            .flat_map(|(index, _, _)| sessions[*index].committed_mutations())
            .map(|mutation| mutation.provenance().destination())
            .map(|destination| {
                (
                    destination.x().div_euclid(16),
                    destination.z().div_euclid(16),
                )
            })
            .collect::<BTreeSet<_>>();
        let committed_mutations = committed_mutations
            .into_iter()
            .filter(|mutation| {
                let destination = mutation.provenance().destination();
                let coordinate = (
                    destination.x().div_euclid(16),
                    destination.z().div_euclid(16),
                );
                !finalized_coordinates.contains(&coordinate)
            })
            .collect::<Vec<_>>();
        drop(final_outputs);
        if !commit_columns.is_empty() {
            let columns = commit_columns.into_iter().collect::<Vec<_>>();
            match self.commit_generation_with_mutations_and_persistence_destinations(
                &halo,
                columns,
                &committed_mutations,
                &persistence_destinations,
                &finalized_coordinates,
            ) {
                Err(error) => {
                    if matches!(error, GenerationCommitError::RevisionConflict { .. }) {
                        for (_, entry, _) in &generated_snapshots {
                            self.generation_ledger()
                                .rollback_admission(entry.pipeline, &entry.admitted);
                        }
                        return GenerationBatchFinish::RevisionConflict;
                    }
                    for index in commit_indices {
                        results[index] = Some(Err(
                            crate::worldgen_session::GenerationRequestError::Boundary(
                                error.to_string(),
                            ),
                        ));
                    }
                    for (index, entry, _) in generated_snapshots {
                        self.generation_ledger()
                            .rollback_admission(entry.pipeline, &entry.admitted);
                        results[index] = Some(Err(
                            crate::worldgen_session::GenerationRequestError::Boundary(
                                error.to_string(),
                            ),
                        ));
                    }
                }
                Ok(report) => {
                    for (index, _) in &reused_columns {
                        let mutations = sessions[*index]
                            .committed_mutations()
                            .cloned()
                            .collect::<Vec<_>>();
                        self.generation_ledger().settle_mutations(
                            sessions[*index].pipeline(),
                            &mutations,
                            false,
                            &report.coordinates,
                        );
                    }
                    let packet_neighbour_admissions = generated_snapshots
                        .iter()
                        .map(|(_, _, snapshot)| snapshot.neighbours().len())
                        .sum();
                    crate::world_spawn::record_packet_neighbour_admissions(
                        packet_neighbour_admissions,
                    );
                    for (index, entry, snapshot) in generated_snapshots {
                        let mutations = sessions[index]
                            .committed_mutations()
                            .cloned()
                            .collect::<Vec<_>>();
                        let source_stored = self.persist_generation_mutations(
                            &mutations,
                            &report.persistence_columns,
                        );
                        self.generation_ledger().settle_mutations(
                            entry.pipeline,
                            &mutations,
                            source_stored,
                            &report.coordinates,
                        );
                        results[index] = Some(Ok(Some(
                            crate::worldgen_session::GenerationRequestResult::Generated(snapshot),
                        )));
                    }
                }
            }
        }
        drop(leases);
        GenerationBatchFinish::Complete(results
            .into_iter()
            .map(|result| {
                result.unwrap_or_else(|| {
                    Err(crate::worldgen_session::GenerationRequestError::Boundary(
                        "generation batch did not produce a result".to_owned(),
                    ))
                })
            })
            .collect())
    }

    fn batch_pipeline_identities_match(sessions: &[GenerationSession]) -> bool {
        let Some(first) = sessions.first() else {
            return true;
        };
        let identity = first.pipeline().identity(PipelineOptions::ALL);
        sessions
            .iter()
            .skip(1)
            .all(|session| session.pipeline().identity(PipelineOptions::ALL) == identity)
    }

    fn execute_generation_batch(
        &self,
        sessions: &mut [GenerationSession],
    ) -> Vec<
        Result<
            Option<crate::worldgen_session::GenerationRequestResult>,
            crate::worldgen_session::GenerationRequestError,
        >,
    > {
        if sessions.len() >= 2 && !Self::batch_pipeline_identities_match(sessions) {
            let message = "generation batch requires one pipeline identity";
            return sessions
                .iter()
                .map(|_| {
                    Err(crate::worldgen_session::GenerationRequestError::Boundary(
                        message.to_owned(),
                    ))
                })
                .collect();
        }
        if sessions.len() < 2
            || sessions.iter().any(|session| {
                session.request().generation_target() != GenerationTarget::Full
            })
        {
            return sessions
                .iter_mut()
                .map(|session| self.request_generation(session.request(), Some(session)))
                .collect();
        }
        loop {
            let mut prepared = match self.prepare_generation_batch(sessions) {
                Ok(prepared) => prepared,
                Err(results) => return results,
            };
            let generated = self.source.request_generation_batch(&mut prepared.active_sessions);
            match self.finish_generation_batch(sessions, prepared, generated) {
                GenerationBatchFinish::Complete(results) => return results,
                GenerationBatchFinish::RevisionConflict => std::thread::yield_now(),
            }
        }
    }

    #[cfg(target_arch = "wasm32")]
    async fn execute_generation_batch_yielding(
        &self,
        sessions: &mut [GenerationSession],
    ) -> Vec<
        Result<
            Option<crate::worldgen_session::GenerationRequestResult>,
            crate::worldgen_session::GenerationRequestError,
        >,
    > {
        if sessions.len() >= 2 && !Self::batch_pipeline_identities_match(sessions) {
            let message = "generation batch requires one pipeline identity";
            return sessions
                .iter()
                .map(|_| {
                    Err(crate::worldgen_session::GenerationRequestError::Boundary(
                        message.to_owned(),
                    ))
                })
                .collect();
        }
        if sessions.len() < 2
            || sessions.iter().any(|session| {
                session.request().generation_target() != GenerationTarget::Full
            })
        {
            let mut results = Vec::with_capacity(sessions.len());
            for session in sessions {
                results.push(
                    self.request_generation_yielding(session.request(), Some(session))
                        .await,
                );
            }
            return results;
        }
        loop {
            let mut prepared = match self.prepare_generation_batch(sessions) {
                Ok(prepared) => prepared,
                Err(results) => return results,
            };
            let generated = self
                .source
                .request_generation_batch_yielding(&mut prepared.active_sessions)
                .await;
            match self.finish_generation_batch(sessions, prepared, generated) {
                GenerationBatchFinish::Complete(results) => return results,
                GenerationBatchFinish::RevisionConflict => crate::chunk::yield_to_browser().await,
            }
        }
    }

    /// Execute one request with a canonical halo pin. Same-target callers
    /// share one leader and wait for its cache or retained-output result.
    #[cfg(test)]
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn execute_generation_session(
        &self,
        session: &mut GenerationSession,
    ) -> Result<crate::worldgen_session::GenerationRequestResult, GenerationSessionExecutionError> {
        let request = session.request();
        let key = Self::generation_key(request);
        let required_stage = match request.generation_target() {
            GenerationTarget::Shaped => ChunkGenerationStage::Shaped,
            GenerationTarget::Full => ChunkGenerationStage::Full,
        };
        loop {
            let (slot, leader) = self.begin_generation(key);
            if !leader {
                slot.wait();
                if let Some(column) = slot.result() {
                    return Ok(crate::worldgen_session::GenerationRequestResult::Existing(column));
                }
                if let Some(column) = self.resident_column(request.target().0, request.target().1)
                    && column.generation_stage() >= required_stage
                {
                    return Ok(crate::worldgen_session::GenerationRequestResult::Existing(column));
                }
                if let Some(column) = self.retained_output_column(session.pipeline(), request.target()) {
                    return Ok(crate::worldgen_session::GenerationRequestResult::Existing(column));
                }
                continue;
            }
            let result = self.execute_generation_session_inner(session);
            self.finish_generation(key, &slot, &result);
            return result;
        }
    }

    /// The leader-only body behind [`Self::execute_generation_session`].
    #[cfg(not(target_arch = "wasm32"))]
    fn execute_generation_session_inner(
        &self,
        session: &mut GenerationSession,
    ) -> Result<crate::worldgen_session::GenerationRequestResult, GenerationSessionExecutionError> {
        let coordinates = crate::production_worldgen_session::required_generation_halo(
            std::slice::from_ref(session),
        )
        .map_err(GenerationSessionExecutionError::Session)?;
        let cancellation = session.cancellation();
        let _region_lease = self
            .generation_regions
            .acquire(&coordinates, &cancellation)
            .map_err(|_| GenerationSessionExecutionError::Session(SessionError::Cancelled))?;
        let halo = self.lease_halo(&coordinates)?;
        let pipeline = session.pipeline();
        let (admitted, checkpoint, pipeline_lease) = {
            let mut ledger = self.generation_ledger();
            let admitted = ledger.admit(pipeline, &coordinates)?;
            // A caller may have completed a prefix before handing the session
            // to the store. Publish that committed local state first so
            // hydration never silently discards useful work.
            ledger.publish_session(pipeline, session)?;
            let checkpoint = ledger.checkpoint(pipeline, session.request())?;
            let identity = ledger.pin_pipeline(pipeline)?;
            (
                admitted,
                checkpoint,
                GenerationPipelineLease {
                    ledger: &self.generation_ledger,
                    identity,
                },
            )
        };
        let cancellation = session.cancellation();
        let budget = session.budget();
        let hydration = GenerationSession::from_checkpoint_with_budget_and_cancellation(
            checkpoint,
            budget,
            cancellation,
        );
        let result = (|| {
            *session = hydration?;
            if let Some(column) = self.retained_output_column(pipeline, session.request().target()) {
                self.generation_ledger()
                    .rollback_admission(pipeline, &admitted);
                let mutations = session.committed_mutations().cloned().collect::<Vec<_>>();
                let target = session.request().target();
                let mut retained = vec![(target, column)];
                Self::apply_generation_mutations(&mut retained, &mutations, &BTreeSet::new())?;
                let output = retained[0].1.clone();
                self.commit_generation(&halo, retained)?;
                let ready_destinations = vec![target];
                self.generation_ledger().settle_mutations(
                    pipeline,
                    &mutations,
                    false,
                    &ready_destinations,
                );
                return Ok(crate::worldgen_session::GenerationRequestResult::Existing(output));
            }
            let generation = match self
                .source
                .request_generation(session.request(), Some(session))
            {
                Ok(generation) => generation,
                Err(error) => {
                    // Preserve any reusable prefix before removing empty
                    // admissions on cancellation or another driver error.
                    self.generation_ledger()
                        .publish_session(pipeline, session)?;
                    return Err(GenerationSessionExecutionError::Request(error));
                }
            };
            let Some(generation) = generation else {
                return Err(GenerationSessionExecutionError::MissingDriver);
            };
            if let crate::worldgen_session::GenerationRequestResult::Existing(column) = generation {
                // A persistence hit is terminal, not generation state: remove
                // the empty admission before caching its authoritative column.
                self.generation_ledger()
                    .rollback_admission(pipeline, &admitted);
                self.commit_generation(&halo, vec![(session.request().target(), column.clone())])?;
                return Ok(crate::worldgen_session::GenerationRequestResult::Existing(column));
            }
            let crate::worldgen_session::GenerationRequestResult::Generated(snapshot) = generation
            else {
                unreachable!("existing generation result returned above")
            };
            let final_outputs = BTreeMap::from([(snapshot.coordinate(), snapshot.column())]);
            self.generation_ledger().publish_sessions_with_final_outputs(
                &[(pipeline, session)],
                &final_outputs,
            )?;
            // Do not cache a generated packet after cancellation; the ledger
            // publication above intentionally remains reusable.
            if session.cancellation().is_cancelled() {
                return Err(GenerationSessionExecutionError::Session(SessionError::Cancelled));
            }
            let mut columns = Vec::with_capacity(snapshot.neighbours().len() + 1);
            columns.push((snapshot.coordinate(), snapshot.column().clone()));
            columns.extend(
                snapshot
                    .neighbours()
                    .iter()
                    .map(|neighbour| (neighbour.coordinate(), neighbour.column().clone())),
            );
            let committed_mutations = session.committed_mutations().cloned().collect::<Vec<_>>();
            let feature_winner_receipts = session.feature_winner_receipts().copied().collect::<Vec<_>>();
            let finalized = BTreeSet::from([snapshot.coordinate()]);
            let report = self.commit_generation_with_finalized_mutations_and_receipts(
                &halo,
                columns,
                &committed_mutations,
                &finalized,
                &feature_winner_receipts,
            )?;
            crate::world_spawn::record_packet_neighbour_admissions(snapshot.neighbours().len());
            let source_stored = self
                .persist_generation_mutations(&committed_mutations, &report.persistence_columns);
            let mutations = session.committed_mutations().cloned().collect::<Vec<_>>();
            self.generation_ledger()
                .settle_mutations(
                    pipeline,
                    &mutations,
                    source_stored,
                    &report.coordinates,
                );
            Ok(crate::worldgen_session::GenerationRequestResult::Generated(snapshot))
        })();
        if result.is_err() {
            self.generation_ledger()
                .rollback_admission(pipeline, &admitted);
        }
        drop(pipeline_lease);
        result
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) async fn execute_generation_session_yielding(
        &self,
        session: &mut GenerationSession,
    ) -> Result<crate::worldgen_session::GenerationRequestResult, GenerationSessionExecutionError> {
        let request = session.request();
        let key = Self::generation_key(request);
        let required_stage = match request.generation_target() {
            GenerationTarget::Shaped => ChunkGenerationStage::Shaped,
            GenerationTarget::Full => ChunkGenerationStage::Full,
        };
        loop {
            let (slot, leader) = self.begin_generation(key);
            if !leader {
                slot.wait_yielding().await;
                if let Some(column) = slot.result() {
                    return Ok(crate::worldgen_session::GenerationRequestResult::Existing(column));
                }
                if let Some(column) = self.resident_column(request.target().0, request.target().1)
                    && column.generation_stage() >= required_stage
                {
                    return Ok(crate::worldgen_session::GenerationRequestResult::Existing(column));
                }
                if let Some(column) = self.retained_output_column(session.pipeline(), request.target()) {
                    return Ok(crate::worldgen_session::GenerationRequestResult::Existing(column));
                }
                continue;
            }
            let result = self.execute_generation_session_yielding_inner(session).await;
            self.finish_generation(key, &slot, &result);
            return result;
        }
    }

    #[cfg(target_arch = "wasm32")]
    async fn execute_generation_session_yielding_inner(
        &self,
        session: &mut GenerationSession,
    ) -> Result<crate::worldgen_session::GenerationRequestResult, GenerationSessionExecutionError> {
        let coordinates = crate::production_worldgen_session::required_generation_halo(
            std::slice::from_ref(session),
        )
        .map_err(GenerationSessionExecutionError::Session)?;
        let cancellation = session.cancellation();
        let _region_lease = self
            .generation_regions
            .acquire_yielding(&coordinates, &cancellation)
            .await
            .map_err(|_| GenerationSessionExecutionError::Session(SessionError::Cancelled))?;
        let halo = self.lease_halo(&coordinates)?;
        let pipeline = session.pipeline();
        let (admitted, checkpoint, pipeline_lease) = {
            let mut ledger = self.generation_ledger();
            let admitted = ledger.admit(pipeline, &coordinates)?;
            ledger.publish_session(pipeline, session)?;
            let checkpoint = ledger.checkpoint(pipeline, session.request())?;
            let identity = ledger.pin_pipeline(pipeline)?;
            (
                admitted,
                checkpoint,
                GenerationPipelineLease {
                    ledger: &self.generation_ledger,
                    identity,
                },
            )
        };
        let cancellation = session.cancellation();
        let budget = session.budget();
        let hydration = GenerationSession::from_checkpoint_with_budget_and_cancellation(
            checkpoint,
            budget,
            cancellation,
        );
        let result = async {
            *session = hydration?;
            if let Some(column) = self.retained_output_column(pipeline, session.request().target()) {
                self.generation_ledger()
                    .rollback_admission(pipeline, &admitted);
                let mutations = session.committed_mutations().cloned().collect::<Vec<_>>();
                let target = session.request().target();
                let mut retained = vec![(target, column)];
                Self::apply_generation_mutations(&mut retained, &mutations, &BTreeSet::new())?;
                let output = retained[0].1.clone();
                self.commit_generation(&halo, retained)?;
                let ready_destinations = vec![target];
                self.generation_ledger().settle_mutations(
                    pipeline,
                    &mutations,
                    false,
                    &ready_destinations,
                );
                return Ok(crate::worldgen_session::GenerationRequestResult::Existing(output));
            }
            let generation = match self
                .source
                .request_generation_yielding(session.request(), Some(session))
                .await
            {
                Ok(generation) => generation,
                Err(error) => {
                    self.generation_ledger()
                        .publish_session(pipeline, session)?;
                    return Err(GenerationSessionExecutionError::Request(error));
                }
            };
            let Some(generation) = generation else {
                return Err(GenerationSessionExecutionError::MissingDriver);
            };
            if let crate::worldgen_session::GenerationRequestResult::Existing(column) = generation {
                self.generation_ledger()
                    .rollback_admission(pipeline, &admitted);
                self.commit_generation(&halo, vec![(session.request().target(), column.clone())])?;
                return Ok(crate::worldgen_session::GenerationRequestResult::Existing(column));
            }
            let crate::worldgen_session::GenerationRequestResult::Generated(snapshot) = generation
            else {
                unreachable!("existing generation result returned above")
            };
            let final_outputs = BTreeMap::from([(snapshot.coordinate(), snapshot.column())]);
            self.generation_ledger()
                .publish_sessions_with_final_outputs(&[(pipeline, session)], &final_outputs)?;
            if session.cancellation().is_cancelled() {
                return Err(GenerationSessionExecutionError::Session(SessionError::Cancelled));
            }
            let mut columns = Vec::with_capacity(snapshot.neighbours().len() + 1);
            columns.push((snapshot.coordinate(), snapshot.column().clone()));
            columns.extend(
                snapshot
                    .neighbours()
                    .iter()
                    .map(|neighbour| (neighbour.coordinate(), neighbour.column().clone())),
            );
            let committed_mutations = session.committed_mutations().cloned().collect::<Vec<_>>();
            let feature_winner_receipts = session.feature_winner_receipts().copied().collect::<Vec<_>>();
            let finalized = BTreeSet::from([snapshot.coordinate()]);
            let report = self.commit_generation_with_finalized_mutations_and_receipts(
                &halo,
                columns,
                &committed_mutations,
                &finalized,
                &feature_winner_receipts,
            )?;
            crate::world_spawn::record_packet_neighbour_admissions(snapshot.neighbours().len());
            let source_stored = self
                .persist_generation_mutations(&committed_mutations, &report.persistence_columns);
            let mutations = session.committed_mutations().cloned().collect::<Vec<_>>();
            self.generation_ledger()
                .settle_mutations(
                    pipeline,
                    &mutations,
                    source_stored,
                    &report.coordinates,
                );
            Ok(crate::worldgen_session::GenerationRequestResult::Generated(snapshot))
        }
        .await;
        if result.is_err() {
            self.generation_ledger()
                .rollback_admission(pipeline, &admitted);
        }
        drop(pipeline_lease);
        result
    }

    /// Pin a canonical halo and capture its write revisions. Pins include
    /// currently cold coordinates, allowing a later multi-column commit to
    /// insert them without being evicted before the lease is released.
    pub(crate) fn lease_halo(
        &self,
        coordinates: &[(i32, i32)],
    ) -> Result<ChunkHaloLease<'_, S>, HaloLeaseError> {
        let mut canonical = coordinates.to_vec();
        canonical.sort_unstable();
        canonical.dedup();
        if canonical.is_empty() {
            return Err(HaloLeaseError::Empty);
        }
        let gate = self.write_gates.acquire_many(&canonical, false);
        let states = gate.states.clone();
        let revisions = states
            .iter()
            .map(|state| state.revision.load(Ordering::Acquire))
            .collect::<Vec<_>>();
        let mut cache = self.lock();
        for coordinate in &canonical {
            *cache.pins.entry(*coordinate).or_insert(0) += 1;
        }
        drop(cache);
        drop(gate);
        Ok(ChunkHaloLease {
            store: self,
            coordinates: canonical,
            revisions,
            states,
        })
    }

    /// Atomically install generated columns when every coordinate still has
    /// the revision captured by `halo`. The write gates cover validation and
    /// cache insertion only; callers generate before entering this method.
    pub(crate) fn commit_generation(
        &self,
        halo: &ChunkHaloLease<'_, S>,
        columns: Vec<((i32, i32), ChunkColumn)>,
    ) -> Result<GenerationCommitReport, GenerationCommitError> {
        self.commit_generation_with_mutations(halo, columns, &[])
    }

    fn commit_generation_with_mutations(
        &self,
        halo: &ChunkHaloLease<'_, S>,
        columns: Vec<((i32, i32), ChunkColumn)>,
        mutations: &[ProvenanceMutation],
    ) -> Result<GenerationCommitReport, GenerationCommitError> {
        self.commit_generation_with_finalized_mutations(
            halo,
            columns,
            mutations,
            &BTreeSet::new(),
        )
    }

    fn commit_generation_with_finalized_mutations(
        &self,
        halo: &ChunkHaloLease<'_, S>,
        columns: Vec<((i32, i32), ChunkColumn)>,
        mutations: &[ProvenanceMutation],
        finalized: &BTreeSet<ChunkCoordinate>,
    ) -> Result<GenerationCommitReport, GenerationCommitError> {
        self.commit_generation_with_finalized_mutations_and_receipts(
            halo,
            columns,
            mutations,
            finalized,
            &[],
        )
    }

    fn commit_generation_with_finalized_mutations_and_receipts(
        &self,
        halo: &ChunkHaloLease<'_, S>,
        columns: Vec<((i32, i32), ChunkColumn)>,
        mutations: &[ProvenanceMutation],
        finalized: &BTreeSet<ChunkCoordinate>,
        receipts: &[TargetFeatureWrite],
    ) -> Result<GenerationCommitReport, GenerationCommitError> {
        let persistence_destinations = mutations
            .iter()
            .map(|mutation| mutation.provenance().destination())
            .map(|destination| {
                (
                    destination.x().div_euclid(16),
                    destination.z().div_euclid(16),
                )
            })
            .collect::<BTreeSet<_>>();
        self.commit_generation_with_mutations_and_persistence_destinations_and_receipts(
            halo,
            columns,
            mutations,
            &persistence_destinations,
            finalized,
            receipts,
        )
    }

    fn commit_generation_with_mutations_and_persistence_destinations(
        &self,
        halo: &ChunkHaloLease<'_, S>,
        columns: Vec<((i32, i32), ChunkColumn)>,
        mutations: &[ProvenanceMutation],
        persistence_destinations: &BTreeSet<ChunkCoordinate>,
        finalized: &BTreeSet<ChunkCoordinate>,
    ) -> Result<GenerationCommitReport, GenerationCommitError> {
        self.commit_generation_with_mutations_and_persistence_destinations_and_receipts(
            halo,
            columns,
            mutations,
            persistence_destinations,
            finalized,
            &[],
        )
    }

    fn commit_generation_with_mutations_and_persistence_destinations_and_receipts(
        &self,
        halo: &ChunkHaloLease<'_, S>,
        columns: Vec<((i32, i32), ChunkColumn)>,
        mutations: &[ProvenanceMutation],
        persistence_destinations: &BTreeSet<ChunkCoordinate>,
        finalized: &BTreeSet<ChunkCoordinate>,
        receipts: &[TargetFeatureWrite],
    ) -> Result<GenerationCommitReport, GenerationCommitError> {
        self.commit_generation_with_mutations_and_persistence_destinations_and_receipts_policy(
            halo,
            columns,
            mutations,
            persistence_destinations,
            finalized,
            receipts,
            GenerationCommitMode::Standard,
        )
    }

    fn commit_generation_with_mutations_and_persistence_destinations_and_receipts_policy(
        &self,
        halo: &ChunkHaloLease<'_, S>,
        columns: Vec<((i32, i32), ChunkColumn)>,
        mutations: &[ProvenanceMutation],
        persistence_destinations: &BTreeSet<ChunkCoordinate>,
        finalized: &BTreeSet<ChunkCoordinate>,
        receipts: &[TargetFeatureWrite],
        mode: GenerationCommitMode,
    ) -> Result<GenerationCommitReport, GenerationCommitError> {
        if columns.is_empty() {
            return Err(GenerationCommitError::Empty);
        }
        let mut columns = columns;
        Self::apply_generation_mutations_with_receipts(
            &mut columns,
            mutations,
            finalized,
            receipts,
        )?;
        let mutation_destinations = mutations
            .iter()
            .map(|mutation| mutation.provenance().destination())
            .map(|destination| {
                (
                    destination.x().div_euclid(16),
                    destination.z().div_euclid(16),
                )
            })
            .collect::<HashSet<_>>();
        columns.sort_unstable_by_key(|(coordinate, _)| *coordinate);
        let mut seen = HashSet::new();
        let coordinates = columns
            .iter()
            .map(|(coordinate, _)| {
                if !seen.insert(*coordinate) {
                    Err(GenerationCommitError::DuplicateCoordinate(*coordinate))
                } else if halo.revision(*coordinate).is_none() {
                    Err(GenerationCommitError::OutsideHalo(*coordinate))
                } else {
                    Ok(*coordinate)
                }
            })
            .collect::<Result<Vec<_>, _>>()?;
        if let Some(coordinate) = mutations
            .iter()
            .map(|mutation| mutation.provenance().destination())
            .map(|destination| {
                (
                    destination.x().div_euclid(16),
                    destination.z().div_euclid(16),
                )
            })
            .find(|coordinate| coordinates.binary_search(coordinate).is_err())
        {
            return Err(GenerationCommitError::MissingMutationDestination(coordinate));
        }
        if let Some(coordinate) = persistence_destinations
            .iter()
            .copied()
            .find(|coordinate| coordinates.binary_search(coordinate).is_err())
        {
            return Err(GenerationCommitError::MissingMutationDestination(coordinate));
        }
        let gate = if mode.checks_full_halo() {
            self.write_gates
                .acquire_many_with_revision_filter(&halo.coordinates, &coordinates)
        } else {
            self.write_gates.acquire_many(&coordinates, false)
        };
        let checked_coordinates = if mode.checks_full_halo() {
            &halo.coordinates
        } else {
            &coordinates
        };
        for &coordinate in checked_coordinates {
            let lease_index = halo
                .coordinates
                .binary_search(&coordinate)
                .expect("checked generation coordinate belongs to the halo");
            let gate_index = gate
                .coordinates
                .binary_search(&coordinate)
                .expect("checked generation coordinate has a write gate");
            let expected = halo.revisions[lease_index];
            let found = gate.states[gate_index].revision.load(Ordering::Acquire);
            if found != expected {
                drop(gate);
                return Err(GenerationCommitError::RevisionConflict {
                    coordinate,
                    expected,
                    found,
                });
            }
        }
        let mut revisions = Vec::with_capacity(coordinates.len());
        for coordinate in coordinates.iter().copied() {
            let expected = halo.revisions[halo
                .coordinates
                .binary_search(&coordinate)
                .expect("coordinate was checked")];
            revisions.push((coordinate, expected, expected));
        }
        let persistence_columns = columns
            .iter()
            .filter(|(coordinate, _)| persistence_destinations.contains(coordinate))
            .map(|(coordinate, column)| (*coordinate, column.clone()))
            .collect();
        let mut cache = self.lock();
        let mut changed = false;
        for (coordinate, column) in columns {
            let stamp = cache.next_stamp();
            match cache.columns.entry(coordinate) {
                MapEntry::Occupied(mut occupied) => {
                    let entry = occupied.get_mut();
                    entry.last_used = stamp;
                    if column.generation_stage() > entry.column.generation_stage()
                        || (column.generation_stage() == entry.column.generation_stage()
                            && (mutation_destinations.contains(&coordinate)
                                || (mode == GenerationCommitMode::Cohort
                                    && finalized.contains(&coordinate))))
                    {
                        entry.column = column;
                        changed = true;
                    }
                }
                MapEntry::Vacant(vacant) => {
                    vacant.insert(Entry { column, last_used: stamp });
                    changed = true;
                }
            }
        }
        drop(cache);
        let mut gate = gate;
        gate.bump_revision = changed;
        if changed {
            for (_, _, after) in &mut revisions {
                *after += 1;
            }
        }
        drop(gate);
        if !mode.defers_eviction() {
            self.evict_excess();
        }
        Ok(GenerationCommitReport {
            coordinates,
            revisions,
            persistence_columns,
        })
    }

    fn evict_excess(&self) {
        let ticket_resident = {
            let guard = self.lock();
            (guard.columns.len() > guard.capacity).then(|| {
                self.tickets
                    .cache_protected_positions()
                    .into_iter()
                    .collect::<HashSet<_>>()
            })
        };
        let Some(ticket_resident) = ticket_resident else {
            return;
        };
        let mut guard = self.lock();
        let evicted = guard.evict_down_to_capacity(&ticket_resident);
        drop(guard);
        self.write_gates.forget_if_idle(&evicted);
        self.lifecycle.execute(ChunkLifecyclePlan::unload(evicted), |assignment| {
            debug_assert_eq!(
                assignment.owner,
                crate::chunk_lifecycle::ChunkLifecycleOwner::Chunk {
                    cx: assignment.chunk.0,
                    cz: assignment.chunk.1,
                }
            );
            self.source.unload(assignment.chunk.0, assignment.chunk.1);
        });
    }

    /// Makes `(cx, cz)` resident, generating it **with the lock released** if
    /// it is not. See the module docs for why that matters and why the insert
    /// does not overwrite.
    ///
    /// Returns `Some(column)` only when retention is disabled
    /// (`capacity == 0`), handing the freshly generated column straight back so
    /// the caller does not generate it a **second** time. That double
    /// generation is what the first draft of this did, and the negative control
    /// below caught it: the control measured `49 × 12 × 2` where the predicted
    /// pre-store figure is `49 × 12`. Harmless in production (capacity is never
    /// 0 there) but it made the control a 2× overstatement of the bug rather
    /// than an exact reproduction of it.
    ///
    /// For any `capacity >= 1` the just-inserted entry carries the highest
    /// `last_used` in the map, so [`Cache::evict_down_to`] can never choose it
    /// and the following [`read`](Self::read) is guaranteed to hit.
    fn ensure(
        &self,
        cx: i32,
        cz: i32,
        stage: crate::chunk::ChunkGenerationStage,
    ) -> Option<ChunkColumn> {
        crate::world_spawn::record_raw_ensure();
        loop {
            // A cheap, rate-limited check-in with the ticket graph on every
            // real op through this store; see `maybe_tick_tickets`'s own doc.
            self.maybe_tick_tickets();
            // Keep the coordinate gate through source generation and the cache
            // insertion. A mutation must not become the source snapshot while
            // a cold load is in flight; independent coordinates still proceed
            // concurrently because this is not the cache mutex.
            let mut gate = self.write_gates.acquire_many(&[(cx, cz)], false);
            let expected_revision = gate.states[0].revision.load(Ordering::Acquire);
            {
                let mut guard = self.lock();
                let cache = &mut *guard;
                let stamp = cache.next_stamp();
                if let Some(entry) = cache.columns.get_mut(&(cx, cz)) {
                    entry.last_used = stamp;
                    if entry.column.generation_stage() >= stage {
                        drop(guard);
                        drop(gate);
                        return None;
                    }
                }
            }

            let mut fresh = self.lifecycle.execute(ChunkLifecyclePlan::load((cx, cz)), |assignment| {
                debug_assert_eq!(assignment.chunk, (cx, cz));
                debug_assert_eq!(
                    assignment.owner,
                    crate::chunk_lifecycle::ChunkLifecycleOwner::Chunk { cx, cz }
                );
                self.source
                    .column_at(assignment.chunk.0, assignment.chunk.1, stage)
            });
            let fresh = fresh
                .pop()
                .expect("an on-demand lifecycle load returns exactly one column");
            self.lock().generated += 1;

            debug_assert_eq!(
                gate.states[0].revision.load(Ordering::Acquire),
                expected_revision,
                "the coordinate gate excludes mutations during generation",
            );
            let mut guard = self.lock();
            let cache = &mut *guard;
            let stamp = cache.next_stamp();
            if cache.capacity == 0 {
                drop(guard);
                drop(gate);
                return Some(fresh);
            }
            match cache.columns.entry((cx, cz)) {
                // Another thread won the race while this one generated. Keep an
                // equal-or-higher result (it may carry an edit); otherwise replace
                // a shaped entry with the requested full upgrade.
                MapEntry::Occupied(mut occupied) => {
                    let entry = occupied.get_mut();
                    entry.last_used = stamp;
                    if fresh.generation_stage() > entry.column.generation_stage() {
                        entry.column = fresh;
                    }
                }
                MapEntry::Vacant(vacant) => {
                    vacant.insert(Entry {
                        column: fresh,
                        last_used: stamp,
                    });
                }
            }
            drop(guard);
            gate.bump_revision = true;
            drop(gate);
            self.evict_excess();
            return None;
        }
    }

    /// Reads a retained column in place, without cloning it. `None` if it is
    /// not resident (a capacity of 0, or an eviction in the window since
    /// [`ensure`](Self::ensure)).
    fn read<R>(&self, cx: i32, cz: i32, f: impl FnOnce(&ChunkColumn) -> R) -> Option<R> {
        let mut guard = self.lock();
        let cache = &mut *guard;
        let stamp = cache.next_stamp();
        let entry = cache.columns.get_mut(&(cx, cz))?;
        entry.last_used = stamp;
        Some(f(&entry.column))
    }

    /// Reads one cache entry without waiting for the cache mutex.
    ///
    /// Resident-only tick access already owns the coordinate write gate, which
    /// closes the important generation race. `try_lock` also keeps the helper
    /// honest if a short eviction or mutation is holding the cache mutex: the
    /// caller gets `Busy` rather than turning a supposedly nonblocking probe
    /// into an unbounded wait.
    fn try_read<R>(
        &self,
        cx: i32,
        cz: i32,
        f: impl FnOnce(&ChunkColumn) -> R,
    ) -> Result<Option<R>, ()> {
        let mut guard = match self.cache.try_lock() {
            Ok(guard) => guard,
            Err(std::sync::TryLockError::WouldBlock) => return Err(()),
            Err(std::sync::TryLockError::Poisoned(_)) => {
                panic!("chunk store lock poisoned")
            }
        };
        let cache = &mut *guard;
        let stamp = cache.next_stamp();
        let Some(entry) = cache.columns.get_mut(&(cx, cz)) else {
            return Ok(None);
        };
        entry.last_used = stamp;
        Ok(Some(f(&entry.column)))
    }

    /// How many cache ops [`maybe_tick_tickets`](Self::maybe_tick_tickets)
    /// waits between ticket-graph check-ins. A `Cache::stamp` unit, not a
    /// tick or a wall-clock duration — see that method's doc for what this
    /// trades away and why the trade is deliberate.
    const TICKET_CHECK_PERIOD: u64 = 20;

    /// A shared handle to this store's own ticket graph, for a caller that
    /// wants to grant, move or remove tickets — [`set_spawn_ticket`],
    /// [`set_forced_ticket`] and friends below cover the common cases; this is
    /// the escape hatch for anything else (e.g. a future player-loading
    /// ticket once a connection-scoped resource exists to carry it — see this
    /// module's own doc for why that wiring is not in this store).
    #[must_use]
    pub(crate) fn tickets(&self) -> TicketStoreHandle {
        self.tickets.clone()
    }

    /// Grants (or refreshes, or moves) the world's one spawn ticket —
    /// vanilla's `TicketType.PLAYER_SPAWN`: loading-only, expires after 20
    /// ticks without a refresh (`docs/plans/chunk-lifecycle.md` U7,
    /// `crate::ticket::ticket_type::PLAYER_SPAWN`).
    #[cfg(test)]
    pub(crate) fn set_spawn_ticket(&self, pos: (i32, i32), radius: i32) {
        self.tickets.set_ticket_with_radius(
            TicketOwner::Spawn,
            TicketKind::PlayerSpawn,
            pos,
            radius,
        );
    }

    /// Grants a persistent, simulating `FORCED` ticket at `pos`. Its level is
    /// `31`, the level used for entity-ticking residency.
    /// `id` distinguishes more than one forced region; the caller owns
    /// uniqueness (a serial counter is enough).
    #[cfg(test)]
    pub(crate) fn set_forced_ticket(&self, id: u64, pos: (i32, i32)) {
        self.tickets.set_ticket_at_level(
            TicketOwner::Forced(id),
            TicketKind::Forced,
            pos,
            crate::ticket::ENTITY_TICKING_LEVEL,
        );
    }

    /// Withdraws a forced ticket. Its chunk is not dropped synchronously —
    /// see [`maybe_tick_tickets`](Self::maybe_tick_tickets) — but it becomes
    /// an eviction candidate on the next check-in.
    #[cfg(test)]
    pub(crate) fn remove_forced_ticket(&self, id: u64) -> bool {
        self.tickets
            .remove_ticket(TicketOwner::Forced(id), TicketKind::Forced)
    }

    /// The ticket graph's answer for `(cx, cz)` — `Full` iff some active
    /// ticket's propagated level reaches it at or below
    /// [`crate::ticket::MAX_LEVEL`]. Independent of whether the column is
    /// *actually* cached right now: a chunk can be ticket-resident and still
    /// cold (nothing has read it since the ticket was granted) or cached and
    /// ticket-`Empty` (read once, ticket since removed, not yet swept).
    #[must_use]
    #[cfg(test)]
    pub(crate) fn ticket_status(&self, cx: i32, cz: i32) -> crate::ticket::ChunkStatus {
        self.tickets.status((cx, cz))
    }

    /// Rate-limited ticket-graph check-in, called from every real op through
    /// this store ([`ensure`](Self::ensure)).
    ///
    /// # Why this, and not a `run_tick_loop` parameter
    ///
    /// The obvious design threads a `TicketStoreHandle` into
    /// `crate::tick::run_tick_loop` and ticks it once per game tick, exactly
    /// like `BlockTickFeed`/`ExplosionFeed`. That is the *more correct* design
    /// — ticket expiry would then mean exactly "N real game ticks," matching
    /// vanilla's own `purgeStaleTickets` — but `run_tick_loop`'s signature has
    /// eleven direct-or-wrapped call sites across `tick.rs`,
    /// `redstone_placement_gate.rs` and `integrated.rs`, `tick.rs` carries
    /// concurrent in-flight redstone work, and this crate's own hazard notes
    /// name exactly this file as the one to touch with named-anchor
    /// insertions, never a signature change, when avoidable. Piggybacking on
    /// this store's own read traffic needs **zero edits to `tick.rs`** and
    /// still ticks the graph at a real cadence: [`TICKET_CHECK_PERIOD`]
    /// cache ops is a few generations' worth of traffic in any dimension a
    /// connection or the tick loop is actually touching, which is the only
    /// case eviction matters for.
    ///
    /// The cost, named rather than hidden: a ticket's expiry is now
    /// "approximately N ticks, decided by read cadence" rather than exactly
    /// N game ticks, and a dimension nobody reads from never checks in at
    /// all (which is also exactly when nothing needs evicting). Tests that
    /// need exact-tick semantics drive [`TicketStoreHandle::tick`] directly
    /// rather than going through a store — see `crate::ticket`'s own test
    /// module.
    ///
    /// A loading-ticket withdrawal only changes eviction eligibility. The
    /// ordinary capacity path performs any needed lifecycle hand-off, while
    /// simulation and world-owned loading tickets remain protected.
    fn maybe_tick_tickets(&self) {
        let due = {
            let mut guard = self.lock();
            let stamp = guard.stamp;
            if stamp < guard.next_ticket_check && !self.tickets.needs_propagation() {
                false
            } else {
                guard.next_ticket_check = stamp + Self::TICKET_CHECK_PERIOD;
                true
            }
        };
        if !due {
            return;
        }
        let delta: TicketDelta = self.tickets.tick();
        if delta.newly_unresident.is_empty() {
            return;
        }
        // A loading ticket controls eligibility, not immediate cache lifetime.
        // Let the bounded eviction path choose an unprotected LRU entry so a
        // high-water view survives a temporary shrink.
        self.evict_excess();
    }

    /// Replaces the cache entry and forwards the same complete value to the
    /// wrapped source. The caller owns the coordinate write gate.
    fn store_resident_column_inner(&self, cx: i32, cz: i32, column: &ChunkColumn) -> bool {
        let cached = {
            let mut guard = self.lock();
            let cache = &mut *guard;
            let stamp = cache.next_stamp();
            if let Some(entry) = cache.columns.get_mut(&(cx, cz)) {
                entry.column = column.clone();
                entry.last_used = stamp;
                true
            } else {
                false
            }
        };
        // The cache and wrapped source are two distinct retention layers. Do
        // not short-circuit on a cache hit: a persistent source still needs the
        // exact snapshot for a future reload.
        let stored = self.source.store_resident_column(cx, cz, column);
        cached || stored
    }

    /// Clears retained light in this cache and in every wrapped persistence
    /// layer while the caller owns the footprint gates. The gate keeps a
    /// concurrent settlement from observing the old snapshot after the block
    /// write has been accepted.
    fn invalidate_retained_light_neighbourhood_while_held(
        &self,
        cx: i32,
        cz: i32,
        coordinates: &[(i32, i32)],
    ) {
        let mut guard = self.lock();
        for &coordinate in coordinates {
            if let Some(entry) = guard.columns.get_mut(&coordinate) {
                entry.column.clear_retained_light();
            }
        }
        drop(guard);
        self.source
            .invalidate_retained_light_neighbourhood(cx, cz);
    }

    /// Replaces every cached column in one footprint, then forwards the same
    /// complete batch to the wrapped source. The caller owns all coordinate
    /// gates, so no block mutation can observe a partially committed light
    /// admission between these two retention layers.
    fn store_resident_columns_inner(&self, columns: &[(i32, i32, ChunkColumn)]) -> bool {
        let cached = {
            let mut guard = self.lock();
            let cache = &mut *guard;
            let stamp = cache.next_stamp();
            let mut cached = false;
            for &(cx, cz, ref column) in columns {
                if let Some(entry) = cache.columns.get_mut(&(cx, cz)) {
                    entry.column = column.clone();
                    entry.last_used = stamp;
                    cached = true;
                }
            }
            cached
        };
        let stored = self.source.store_resident_columns(columns);
        cached || stored
    }

    fn light_coordinates(
        cx: i32,
        cz: i32,
        neighbour_offsets: &[(i32, i32)],
    ) -> Vec<(i32, i32)> {
        let mut coordinates = Vec::with_capacity(neighbour_offsets.len() + 1);
        coordinates.push((cx, cz));
        coordinates.extend(
            neighbour_offsets
                .iter()
                .map(|&(dx, dz)| (cx + dx, cz + dz)),
        );
        coordinates.sort_unstable();
        coordinates.dedup();
        coordinates
    }

    fn capture_light_snapshot(
        &self,
        coordinates: &[(i32, i32)],
        centre: (i32, i32),
        fallback: &ChunkColumn,
        resident_only: bool,
    ) -> Result<ChunkWriteSnapshot<'_>, ColumnLightSettlementError> {
        self.write_gates
            .snapshot_many(coordinates, |(cx, cz)| {
                self.read(cx, cz, ChunkColumn::clone)
                    .or_else(|| self.source.resident_column(cx, cz))
                    .or_else(|| {
                        (!resident_only).then(|| self.source.column(cx, cz))
                    })
                    .or_else(|| (cx, cz).eq(&centre).then(|| fallback.clone()))
            })
            .map_err(|()| ColumnLightSettlementError::MissingFootprint)
    }

    fn capture_light_snapshot_while_held(
        &self,
        lease: &ChunkWriteLease<'_>,
        centre: (i32, i32),
        fallback: &ChunkColumn,
        resident_only: bool,
    ) -> Result<ChunkWriteSnapshot<'_>, ColumnLightSettlementError> {
        self.write_gates
            .snapshot_while_held(lease, |(cx, cz)| {
                self.read(cx, cz, ChunkColumn::clone)
                    .or_else(|| self.source.resident_column(cx, cz))
                    .or_else(|| {
                        (!resident_only).then(|| self.source.column(cx, cz))
                    })
                    .or_else(|| (cx, cz).eq(&centre).then(|| fallback.clone()))
            })
            .map_err(|()| ColumnLightSettlementError::MissingFootprint)
    }

    fn light_columns<'a>(
        snapshot: &'a ChunkWriteSnapshot<'_>,
        centre: (i32, i32),
        neighbour_offsets: &[(i32, i32)],
    ) -> Result<(&'a ChunkColumn, Vec<(i32, i32, &'a ChunkColumn)>), ColumnLightSettlementError> {
        let centre_column = snapshot
            .observations
            .iter()
            .find(|observation| observation.chunk == centre)
            .map(|observation| &observation.column)
            .ok_or(ColumnLightSettlementError::MissingFootprint)?;
        let neighbours = neighbour_offsets
            .iter()
            .map(|&(dx, dz)| {
                snapshot
                    .observations
                    .iter()
                    .find(|observation| observation.chunk == (centre.0 + dx, centre.1 + dz))
                    .map(|observation| (dx, dz, &observation.column))
                    .ok_or(ColumnLightSettlementError::MissingFootprint)
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok((centre_column, neighbours))
    }

    fn settled_columns(
        snapshot: &ChunkWriteSnapshot<'_>,
        centre: (i32, i32),
        neighbour_offsets: &[(i32, i32)],
        settlement: &ColumnLightSettlement,
    ) -> Result<(Vec<(i32, i32, ChunkColumn)>, ChunkColumn), ColumnLightSettlementError> {
        let mut updates = Vec::new();
        let mut settled_centre = None;
        for (offset, light) in settlement.iter() {
            if offset != (0, 0) && !neighbour_offsets.contains(&offset) {
                return Err(ColumnLightSettlementError::MissingFootprint);
            }
            let coordinate = (centre.0 + offset.0, centre.1 + offset.1);
            let observation = snapshot
                .observations
                .iter()
                .find(|observation| observation.chunk == coordinate)
                .ok_or(ColumnLightSettlementError::MissingFootprint)?;
            let mut column = observation.column.clone();
            if offset != (0, 0)
                && column.retained_light_status()
                    == Some(crate::chunk::RetainedLightStatus::CentreSettled)
            {
                // A later footprint may read this column as a dependency, but
                // must not downgrade its already admitted centre snapshot.
                // Block mutations clear the status across the dependency
                // neighbourhood before a refresh, so retaining it here cannot
                // hide a changed terrain state.
                updates.push((coordinate.0, coordinate.1, column));
                continue;
            }
            let status = if offset == (0, 0) {
                crate::chunk::RetainedLightStatus::CentreSettled
            } else {
                crate::chunk::RetainedLightStatus::DependencyInitialized
            };
            column.set_retained_light_with_status(light.clone(), status);
            if offset == (0, 0) {
                settled_centre = Some(column.clone());
            }
            updates.push((coordinate.0, coordinate.1, column));
        }
        let settled_centre = settled_centre
            .or_else(|| {
                snapshot
                    .observations
                    .iter()
                    .find(|observation| observation.chunk == centre)
                    .map(|observation| observation.column.clone())
            })
            .ok_or(ColumnLightSettlementError::MissingFootprint)?;
        Ok((updates, settled_centre))
    }

    fn snapshot_is_centre_settled(
        snapshot: &ChunkWriteSnapshot<'_>,
        centre: (i32, i32),
    ) -> bool {
        snapshot
            .observations
            .iter()
            .find(|observation| observation.chunk == centre)
            .is_some_and(|observation| {
                observation.column.retained_light_status()
                    == Some(crate::chunk::RetainedLightStatus::CentreSettled)
            })
    }

    /// Captures a retained column without waiting or starting generation.
    ///
    /// The coordinate gate is claimed before the cache is inspected, so this
    /// is one atomic admission boundary rather than the racy pair
    /// `is_column_resident` followed by `resident_column`. A concurrent
    /// generation or mutation returns [`TryResident::Busy`]; a cold coordinate
    /// returns [`TryResident::Absent`]. The existing blocking
    /// [`ChunkSource::resident_column`] implementation remains available to
    /// callers that explicitly want its ordinary behavior.
    pub(crate) fn try_resident_column(
        &self,
        cx: i32,
        cz: i32,
    ) -> TryResident<ChunkColumn> {
        let Some(lease) = self.write_gates.try_acquire_many(&[(cx, cz)], false) else {
            return TryResident::Busy;
        };
        let result = match self.try_read(cx, cz, ChunkColumn::clone) {
            Err(()) => TryResident::Busy,
            Ok(Some(column)) => TryResident::Present(column),
            Ok(None) => self
                .source
                .resident_column(cx, cz)
                .map_or(TryResident::Absent, TryResident::Present),
        };
        lease.release_and_prune();
        result
    }

    /// Reads one resident block state without waiting or starting generation.
    ///
    /// This returns a state snapshot rather than a borrowed cell because the
    /// cache lock must be released before the caller can do any tick work. The
    /// coordinate gate and the cache lock are both part of the admission
    /// boundary, so a result cannot be invalidated between the residency check
    /// and the cell read.
    pub(crate) fn try_resident_block_state_id(
        &self,
        x: i32,
        y: i32,
        z: i32,
    ) -> TryResident<lodestone_data::block_states::StateId> {
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        let Some(lease) = self.write_gates.try_acquire_many(&[(cx, cz)], false) else {
            return TryResident::Busy;
        };
        let result = match self.try_read(cx, cz, |column| {
            let local_y = i64::from(y) - i64::from(column.min_y);
            (0..i64::from(column.height))
                .contains(&local_y)
                .then(|| column.block_state_id(lx, y, lz))
        }) {
            Err(()) => TryResident::Busy,
            Ok(None) | Ok(Some(None)) => self
                .source
                .resident_block_state_id(x, y, z)
                .map_or(TryResident::Absent, TryResident::Present),
            Ok(Some(Some(state))) => TryResident::Present(state),
        };
        lease.release_and_prune();
        result
    }

    /// Attempts a resident block mutation without waiting or starting a cold
    /// generation.
    ///
    /// The whole 3×3 retained-light footprint is claimed atomically before the
    /// target is inspected. `Busy` therefore covers a generation, source edit
    /// ledger, or neighbour settlement that would otherwise make a tick-side
    /// `set_block` wait on a condition variable. `Absent` leaves the world
    /// untouched. On a resident target, the source's mutation-only edit ledger
    /// accepts the post-edit snapshot before the cache commits it, so
    /// `Applied` is durable across cache eviction. A source without that
    /// nonblocking edit capability returns `Unsupported` and leaves both layers
    /// unchanged; it is deliberately not sent through the blocking writer,
    /// which could re-enter generation.
    pub(crate) fn try_set_block(
        &self,
        x: i32,
        y: i32,
        z: i32,
        state: lodestone_data::block_states::StateId,
    ) -> TryBlockMutation {
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        let coordinates = Self::light_coordinates(cx, cz, &RETAINED_LIGHT_NEIGHBOUR_OFFSETS);
        let Some(lease) = self.write_gates.try_acquire_many(&coordinates, true) else {
            return TryBlockMutation::Busy;
        };

        let result = {
            let mut guard = match self.cache.try_lock() {
                Ok(guard) => guard,
                Err(std::sync::TryLockError::WouldBlock) => {
                    lease.release_and_prune();
                    return TryBlockMutation::Busy;
                }
                Err(std::sync::TryLockError::Poisoned(_)) => {
                    panic!("chunk store lock poisoned")
                }
            };
            let cache = &mut *guard;
            let stamp = cache.next_stamp();
            let Some(entry) = cache.columns.get_mut(&(cx, cz)) else {
                drop(guard);
                lease.release_and_prune();
                return TryBlockMutation::Absent;
            };
            if y < entry.column.min_y || y >= entry.column.min_y + entry.column.height {
                drop(guard);
                lease.release_and_prune();
                return TryBlockMutation::Absent;
            }
            let mut retained = entry.column.clone();
            retained.set_block_id(lx, y, lz, state);
            retained.clear_retained_light();

            // Keep the cache lock while the typed hook commits the source edit
            // so a successful source write cannot be observed with an old
            // resident cache entry. The hook contract is try-only: source
            // implementations use short `try_lock` sections and never call
            // back into this store.
            let Some(retention) = self
                .source
                .try_store_resident_edit(cx, cz, &retained)
            else {
                drop(guard);
                    lease.release_and_prune();
                return TryBlockMutation::Unsupported;
            };
            if retention == TryResidentEdit::Busy {
                drop(guard);
                lease.release_and_prune();
                return TryBlockMutation::Busy;
            }

            entry.column = retained;
            entry.last_used = stamp;
            // The changed column invalidates every retained-light snapshot in
            // the footprint. Do this while the same short cache lock is held
            // instead of calling the blocking helper, so the try path never
            // waits for a second cache lock.
            for &coordinate in &coordinates {
                if let Some(entry) = cache.columns.get_mut(&coordinate) {
                    entry.column.clear_retained_light();
                }
            }
            TryBlockMutation::Applied
        };
        lease.release_and_prune();
        result
    }
}

impl<S: ChunkSource> ChunkSource for ChunkStore<S> {
    fn horizon_sample(&self, x: i32, z: i32) -> Option<crate::chunk::HorizonSample> {
        self.source.horizon_sample(x, z)
    }

    fn columns(&self, coords: &[(i32, i32)]) -> Vec<ChunkColumn> {
        coords.iter().map(|&(cx, cz)| self.column(cx, cz)).collect()
    }

    fn request_generation(
        &self,
        request: crate::worldgen_session::GenerationRequest,
        session: Option<&mut GenerationSession>,
    ) -> Result<
        Option<crate::worldgen_session::GenerationRequestResult>,
        crate::worldgen_session::GenerationRequestError,
    > {
        // Reuse complete retained columns without reopening a ledger halo.
        let required_stage = match request.generation_target() {
            lodestone_worldgen::stage_schedule::GenerationTarget::Shaped => {
                ChunkGenerationStage::Shaped
            }
            lodestone_worldgen::stage_schedule::GenerationTarget::Full => {
                ChunkGenerationStage::Full
            }
        };
        if let Some(column) = self.resident_column(request.target().0, request.target().1)
            && column.generation_stage() >= required_stage
        {
            crate::world_spawn::record_existing_hit();
            return Ok(Some(
                crate::worldgen_session::GenerationRequestResult::Existing(column),
            ));
        }
        let pipeline = session
            .as_deref()
            .map(GenerationSession::pipeline)
            .unwrap_or_else(|| GenerationSession::new(request).pipeline());
        if let Some(column) = self.retained_output_column(pipeline, request.target()) {
            crate::world_spawn::record_existing_hit();
            return Ok(Some(
                crate::worldgen_session::GenerationRequestResult::Existing(column),
            ));
        }

        #[cfg(target_arch = "wasm32")]
        {
            let _ = session;
            Ok(None)
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let key = Self::generation_key(request);
            let mut session = session;
            let execution = loop {
                let (slot, leader) = self.begin_generation(key);
                if !leader {
                    slot.wait();
                    if let Some(column) = slot.result() {
                        crate::world_spawn::record_existing_hit();
                        break Ok(crate::worldgen_session::GenerationRequestResult::Existing(column));
                    }
                    if let Some(column) = self.resident_column(request.target().0, request.target().1)
                        && column.generation_stage() >= required_stage
                    {
                        crate::world_spawn::record_existing_hit();
                        break Ok(crate::worldgen_session::GenerationRequestResult::Existing(column));
                    }
                    continue;
                }
                crate::world_spawn::record_request_session_leader();
                let execution = match session.as_deref_mut() {
                    Some(session) => self.execute_generation_session_inner(session),
                    None => {
                        let mut owned = GenerationSession::new(request);
                        self.execute_generation_session_inner(&mut owned)
                    }
                };
                self.finish_generation(key, &slot, &execution);
                if matches!(
                    &execution,
                    Err(GenerationSessionExecutionError::Commit(
                        GenerationCommitError::RevisionConflict { .. }
                    ))
                ) {
                    if let Some(column) =
                        self.resident_column(request.target().0, request.target().1)
                        && column.generation_stage() >= required_stage
                    {
                        break Ok(
                            crate::worldgen_session::GenerationRequestResult::Existing(column),
                        );
                    }
                    std::thread::yield_now();
                    continue;
                }
                break execution;
            };
            if matches!(
                &execution,
                Ok(crate::worldgen_session::GenerationRequestResult::Existing(_))
            ) {
                crate::world_spawn::record_existing_hit();
            }
            match execution {
                Ok(result) => Ok(Some(result)),
                Err(GenerationSessionExecutionError::MissingDriver) => Ok(None),
                Err(GenerationSessionExecutionError::Request(
                    crate::worldgen_session::GenerationRequestError::Session(
                        SessionError::CheckpointPipelineMismatch,
                    ),
                )) if request.generation_target()
                    == lodestone_worldgen::stage_schedule::GenerationTarget::Shaped =>
                {
                    Ok(None)
                }
                Err(error) => Err(crate::worldgen_session::GenerationRequestError::Boundary(
                    error.to_string(),
                )),
            }
        }
    }

    fn request_generation_batch(
        &self,
        sessions: &mut [GenerationSession],
    ) -> Vec<
        Result<
            Option<crate::worldgen_session::GenerationRequestResult>,
            crate::worldgen_session::GenerationRequestError,
    >,
    > {
        self.execute_generation_batch(sessions)
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn generation_cohort_width_hint(&self) -> Option<usize> {
        self.source.generation_cohort_width_hint()
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn request_generation_cohort(
        &self,
        sessions: &mut [GenerationSession],
        emit: &mut dyn FnMut(
            usize,
            &GenerationSession,
            crate::worldgen_session::GenerationRequestResult,
        ) -> Result<(), crate::worldgen_session::GenerationRequestError>,
    ) -> Result<Vec<Result<(), crate::worldgen_session::GenerationRequestError>>, crate::worldgen_session::GenerationRequestError> {
        self.execute_generation_cohort(sessions, emit)
    }

    #[cfg(target_arch = "wasm32")]
    fn request_generation_yielding<'a>(
        &'a self,
        request: crate::worldgen_session::GenerationRequest,
        session: Option<&'a mut GenerationSession>,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<
                        Option<crate::worldgen_session::GenerationRequestResult>,
                        crate::worldgen_session::GenerationRequestError,
                    >,
                > + 'a,
        >,
    > {
        Box::pin(async move {
            let required_stage = match request.generation_target() {
                lodestone_worldgen::stage_schedule::GenerationTarget::Shaped => {
                    ChunkGenerationStage::Shaped
                }
                lodestone_worldgen::stage_schedule::GenerationTarget::Full => {
                    ChunkGenerationStage::Full
                }
            };
            if let Some(column) = self.resident_column(request.target().0, request.target().1)
                && column.generation_stage() >= required_stage
            {
                crate::world_spawn::record_existing_hit();
                return Ok(Some(
                    crate::worldgen_session::GenerationRequestResult::Existing(column),
                ));
            }
            let key = Self::generation_key(request);
            let mut session = session;
            let execution = loop {
                let (slot, leader) = self.begin_generation(key);
                if !leader {
                    slot.wait_yielding().await;
                    if let Some(column) = slot.result() {
                        crate::world_spawn::record_existing_hit();
                        break Ok(crate::worldgen_session::GenerationRequestResult::Existing(column));
                    }
                    if let Some(column) = self.resident_column(request.target().0, request.target().1)
                        && column.generation_stage() >= required_stage
                    {
                        crate::world_spawn::record_existing_hit();
                        break Ok(crate::worldgen_session::GenerationRequestResult::Existing(column));
                    }
                    continue;
                }
                crate::world_spawn::record_request_session_leader();
                let execution = match session.as_deref_mut() {
                    Some(session) => self.execute_generation_session_yielding_inner(session).await,
                    None => {
                        let mut owned = GenerationSession::new(request);
                        self.execute_generation_session_yielding_inner(&mut owned).await
                    }
                };
                self.finish_generation(key, &slot, &execution);
                if matches!(
                    &execution,
                    Err(GenerationSessionExecutionError::Commit(
                        GenerationCommitError::RevisionConflict { .. }
                    ))
                ) {
                    if let Some(column) =
                        self.resident_column(request.target().0, request.target().1)
                        && column.generation_stage() >= required_stage
                    {
                        break Ok(
                            crate::worldgen_session::GenerationRequestResult::Existing(column),
                        );
                    }
                    crate::chunk::yield_to_browser().await;
                    continue;
                }
                break execution;
            };
            if matches!(
                &execution,
                Ok(crate::worldgen_session::GenerationRequestResult::Existing(_))
            ) {
                crate::world_spawn::record_existing_hit();
            }
            match execution {
                Ok(result) => Ok(Some(result)),
                Err(GenerationSessionExecutionError::MissingDriver) => Ok(None),
                Err(GenerationSessionExecutionError::Request(
                    crate::worldgen_session::GenerationRequestError::Session(
                        SessionError::CheckpointPipelineMismatch,
                    ),
                )) if request.generation_target()
                    == lodestone_worldgen::stage_schedule::GenerationTarget::Shaped =>
                {
                    Ok(None)
                }
                Err(error) => Err(crate::worldgen_session::GenerationRequestError::Boundary(
                    error.to_string(),
                )),
            }
        })
    }

    #[cfg(target_arch = "wasm32")]
    fn request_generation_batch_yielding<'a>(
        &'a self,
        sessions: &'a mut [GenerationSession],
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Vec<
                        Result<
                            Option<crate::worldgen_session::GenerationRequestResult>,
                            crate::worldgen_session::GenerationRequestError,
                        >,
                    >,
                > + 'a,
        >,
    > {
        Box::pin(async move {
            if sessions.len() < 2
                || sessions.iter().any(|session| {
                    session.request().generation_target() != GenerationTarget::Full
                })
            {
                let mut results = Vec::with_capacity(sessions.len());
                for session in sessions {
                    results.push(
                        self.request_generation_yielding(session.request(), Some(session))
                            .await,
                    );
                }
                return results;
            }
            self.execute_generation_batch_yielding(sessions).await
        })
    }

    fn resident_block_state_id(&self, x: i32, y: i32, z: i32) -> Option<lodestone_data::block_states::StateId> {
        self.read(x.div_euclid(16), z.div_euclid(16), |column| {
            let local_y = i64::from(y) - i64::from(column.min_y);
            if !(0..i64::from(column.height)).contains(&local_y) {
                return None;
            }
            Some(column.block_state_id(x.rem_euclid(16), y, z.rem_euclid(16)))
        })
        .flatten()
        .or_else(|| self.source.resident_block_state_id(x, y, z))
    }

    /// Prefer the retained authoritative copy, then preserve the wrapped
    /// source's explicitly resident columns without calling `column()`.
    fn resident_column(&self, cx: i32, cz: i32) -> Option<ChunkColumn> {
        self.read(cx, cz, ChunkColumn::clone)
            .or_else(|| self.source.resident_column(cx, cz))
    }

    fn retain_generation_input(&self, cx: i32, cz: i32, column: &ChunkColumn) -> bool {
        self.source.retain_generation_input(cx, cz, column)
    }

    fn release_generation_input(&self, cx: i32, cz: i32) {
        self.source.release_generation_input(cx, cz);
    }

    fn try_resident_column(
        &self,
        cx: i32,
        cz: i32,
    ) -> Option<crate::chunk_store::TryResident<ChunkColumn>> {
        Some(ChunkStore::try_resident_column(self, cx, cz))
    }

    fn try_resident_block_state_id(
        &self,
        x: i32,
        y: i32,
        z: i32,
    ) -> Option<crate::chunk_store::TryResident<lodestone_data::block_states::StateId>> {
        Some(ChunkStore::try_resident_block_state_id(self, x, y, z))
    }

    fn try_set_block(
        &self,
        x: i32,
        y: i32,
        z: i32,
        state: lodestone_data::block_states::StateId,
    ) -> Option<crate::chunk_store::TryBlockMutation> {
        Some(ChunkStore::try_set_block(self, x, y, z, state))
    }

    fn try_store_resident_edit(
        &self,
        cx: i32,
        cz: i32,
        column: &ChunkColumn,
    ) -> Option<crate::chunk_store::TryResidentEdit> {
        self.source.try_store_resident_edit(cx, cz, column)
    }

    /// Replaces the cached column with the caller's complete snapshot and
    /// forwards that same value to the wrapped source.
    fn store_resident_column(&self, cx: i32, cz: i32, column: &ChunkColumn) -> bool {
        self.write_gates
            .with((cx, cz), || self.store_resident_column_inner(cx, cz, column))
    }

    fn invalidate_retained_light_neighbourhood(&self, cx: i32, cz: i32) {
        let coordinates = Self::light_coordinates(cx, cz, &RETAINED_LIGHT_NEIGHBOUR_OFFSETS);
        let lease = self.write_gates.acquire_many(&coordinates, true);
        self.invalidate_retained_light_neighbourhood_while_held(cx, cz, &coordinates);
        lease.release_and_prune();
    }

    fn store_resident_columns(&self, columns: &[(i32, i32, ChunkColumn)]) -> bool {
        let coordinates = columns
            .iter()
            .map(|&(cx, cz, _)| (cx, cz))
            .collect::<Vec<_>>();
        let lease = self.write_gates.acquire_many(&coordinates, true);
        let stored = self.store_resident_columns_inner(columns);
        lease.release_and_prune();
        stored
    }

    /// Captures the complete light footprint, computes outside the cache lock,
    /// then commits only when every captured coordinate revision is still
    /// current. The exclusive path is the bounded final attempt: all
    /// dependency gates remain held through compute and commit, so it cannot
    /// lose a race to a neighbour mutation.
    fn settle_resident_column_light_with_neighbours(
        &self,
        cx: i32,
        cz: i32,
        fallback: &ChunkColumn,
        neighbour_offsets: &[(i32, i32)],
        resident_only: bool,
        replace_existing: bool,
        exclusive: bool,
        compute: &mut dyn FnMut(
            &ChunkColumn,
            &[(i32, i32, &ChunkColumn)],
        ) -> Option<lodestone_world::ColumnLight>,
    ) -> Result<ChunkColumn, ColumnLightSettlementError> {
        let mut compute_batch = |centre: &ChunkColumn,
                                 neighbours: &[(i32, i32, &ChunkColumn)]| {
            compute(centre, neighbours).map(ColumnLightSettlement::centre)
        };
        self.settle_resident_column_lights_with_neighbours(
            cx,
            cz,
            fallback,
            neighbour_offsets,
            resident_only,
            replace_existing,
            exclusive,
            &mut compute_batch,
        )
    }

    /// Captures a complete light footprint, computes outside the cache lock,
    /// then commits every returned snapshot only when every captured coordinate
    /// revision is still current. The exclusive path holds every dependency
    /// gate through compute and commit, giving the bounded retry loop a
    /// guaranteed-progress final attempt.
    fn settle_resident_column_lights_with_neighbours(
        &self,
        cx: i32,
        cz: i32,
        fallback: &ChunkColumn,
        neighbour_offsets: &[(i32, i32)],
        resident_only: bool,
        replace_existing: bool,
        exclusive: bool,
        compute: &mut dyn FnMut(
            &ChunkColumn,
            &[(i32, i32, &ChunkColumn)],
        ) -> Option<ColumnLightSettlement>,
    ) -> Result<ChunkColumn, ColumnLightSettlementError> {
        let centre = (cx, cz);
        if !replace_existing {
            let snapshot = self.capture_light_snapshot(
                &[centre],
                centre,
                fallback,
                resident_only,
            )?;
            let current = snapshot
                .observations
                .first()
                .expect("the centre snapshot is always requested")
                .column
                .clone();
            if Self::snapshot_is_centre_settled(&snapshot, centre) {
                return Ok(current);
            }
        }
        let coordinates = Self::light_coordinates(cx, cz, neighbour_offsets);
        // Ensure all generated dependencies are ordered before the optimistic
        // capture. The subsequent multi-coordinate snapshot still validates
        // every dependency, so a mutation between these calls is detected.
        if !resident_only {
            for &(column_cx, column_cz) in &coordinates {
                let _ = self.column(column_cx, column_cz);
            }
        }

        if exclusive {
            let mut lease = self.write_gates.acquire_many(&coordinates, false);
            let snapshot = match self.capture_light_snapshot_while_held(
                &lease,
                centre,
                fallback,
                resident_only,
            ) {
                Ok(snapshot) => snapshot,
                Err(error) => {
                    lease.release_and_prune();
                    return Err(error);
                }
            };
            let (centre_column, neighbours) = match Self::light_columns(
                &snapshot,
                centre,
                neighbour_offsets,
            ) {
                Ok(columns) => columns,
                Err(error) => {
                    drop(snapshot);
                    lease.release_and_prune();
                    return Err(error);
                }
            };
            if !replace_existing && Self::snapshot_is_centre_settled(&snapshot, centre) {
                lease.release_and_prune();
                return Ok(centre_column.clone());
            }
            let Some(settlement) = compute(&centre_column, &neighbours) else {
                lease.release_and_prune();
                return Err(ColumnLightSettlementError::NoLight);
            };
            let (updates, settled) = match Self::settled_columns(
                &snapshot,
                centre,
                neighbour_offsets,
                &settlement,
            ) {
                Ok(result) => result,
                Err(error) => {
                    drop(snapshot);
                    lease.release_and_prune();
                    return Err(error);
                }
            };
            let _ = self.store_resident_columns_inner(&updates);
            drop(snapshot);
            lease.bump_revision = true;
            lease.release_and_prune();
            return Ok(settled);
        }

        let snapshot = self.capture_light_snapshot(
            &coordinates,
            centre,
            fallback,
            resident_only,
        )?;
        let (centre_column, neighbours) = Self::light_columns(
            &snapshot,
            centre,
            neighbour_offsets,
        )?;
        if !replace_existing && Self::snapshot_is_centre_settled(&snapshot, centre) {
            return Ok(centre_column.clone());
        }
        let Some(settlement) = compute(&centre_column, &neighbours) else {
            return Err(ColumnLightSettlementError::NoLight);
        };
        let (updates, settled) = Self::settled_columns(
            &snapshot,
            centre,
            neighbour_offsets,
            &settlement,
        )?;
        self.write_gates
            .try_commit(snapshot, || self.store_resident_columns_inner(&updates))
            .map(|_| settled)
            .map_err(|()| ColumnLightSettlementError::Conflict)
    }

    /// Forwarded, not answered: a cache owns no registries of its own, and a
    /// constructor that wraps before asking would otherwise see `None` and build
    /// a private pair the save path cannot read.
    fn world_registries(&self) -> Option<crate::chunk::WorldRegistries> {
        self.source.world_registries()
    }

    /// Forwarded for the same reason [`world_registries`](ChunkSource::world_registries)
    /// is: a cache is transparent, and a wrapper that answered the default here
    /// would hide the world's other dimensions from every connection — the store
    /// sits *between* `crate::dimension::DimensionalSource` and the generator on
    /// one of the two paths and *above* it on the other, so neither position may
    /// swallow these.
    fn dimension(&self) -> Option<crate::dimension::Dimension> {
        self.source.dimension()
    }

    fn sibling(
        &self,
        dimension: crate::dimension::Dimension,
    ) -> Option<std::sync::Arc<dyn ChunkSource>> {
        self.source.sibling(dimension)
    }

    fn portal_index(&self) -> Option<&crate::portal::PortalIndex> {
        self.source.portal_index()
    }

    /// Forwarded for the same reason [`world_registries`](ChunkSource::world_registries)
    /// is: a cache is transparent, and answering the default here would hide a
    /// dimension's own tick-scheduling feed from a connection asking through
    /// this layer.
    fn block_tick_feed(&self) -> Option<crate::tick::BlockTickFeed> {
        self.source.block_tick_feed()
    }

    fn column(&self, cx: i32, cz: i32) -> ChunkColumn {
        self.column_at(cx, cz, crate::chunk::ChunkGenerationStage::Full)
    }

    fn column_at(
        &self,
        cx: i32,
        cz: i32,
        stage: crate::chunk::ChunkGenerationStage,
    ) -> ChunkColumn {
        // `Some` means retention is off (the negative-control configuration) —
        // the column was just generated and there is nothing to read it from.
        if let Some(fresh) = self.ensure(cx, cz, stage) {
            return fresh;
        }
        // The fallback below is reachable only if another thread evicted this
        // entry in the window since `ensure` inserted it, which needs a
        // capacity-worth of concurrent misses. Correct rather than dead, and it
        // costs a regeneration, never a wrong block.
        self.read(cx, cz, ChunkColumn::clone)
            .filter(|column| column.generation_stage() >= stage)
            .unwrap_or_else(|| self.source.column_at(cx, cz, stage))
    }

    fn packet_generation_stage(
        &self,
        stage: crate::chunk::ChunkGenerationStage,
    ) -> Option<crate::chunk::ChunkGenerationStage> {
        self.source.packet_generation_stage(stage)
    }

    fn generation_request_dependency_radius(
        &self,
        target: lodestone_worldgen::stage_schedule::GenerationTarget,
    ) -> u8 {
        self.source.generation_request_dependency_radius(target)
    }

    fn request_stage_driver(
        &self,
    ) -> Option<&dyn crate::worldgen_session::RequestStageDriver> {
        self.source.request_stage_driver()
    }

    /// One block, without regenerating or cloning a column.
    ///
    /// This override avoids whole-column regeneration per probe: the
    /// column-regenerating form (`self.column(cx, cz).block_state_id(..)`) would
    /// regenerate a whole column for every read, and
    /// `crate::server`'s `vitals_tick` calls this every 50 ms on the
    /// connection task. See the module docs.
    fn block_state_id(&self, x: i32, y: i32, z: i32) -> lodestone_data::block_states::StateId {
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        if let Some(fresh) = self.ensure(
            cx,
            cz,
            crate::chunk::ChunkGenerationStage::Full,
        ) {
            return fresh.block_state_id(lx, y, lz);
        }
        self.read(cx, cz, |column| column.block_state_id(lx, y, lz))
            .unwrap_or_else(|| self.source.block_state_id(x, y, z))
    }

    /// One biome cell out of the retained column, without cloning the whole
    /// thing — the same reason [`block_state`](Self::block_state) is
    /// overridden here rather than left at `self.column(..).biome_state_at(..)`.
    fn biome_state_at(&self, x: i32, y: i32, z: i32) -> String {
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        if let Some(fresh) = self.ensure(
            cx,
            cz,
            crate::chunk::ChunkGenerationStage::Full,
        ) {
            return fresh.biome_state_at(lx, y, lz).to_string();
        }
        self.read(cx, cz, |column| column.biome_state_at(lx, y, lz).to_string())
            .unwrap_or_else(|| self.source.biome_state_at(x, y, z))
    }

    /// One generated block entity out of the retained column, without cloning the
    /// whole thing — the same reason [`block_state`](Self::block_state) is
    /// overridden here.
    fn block_entity(&self, x: i32, y: i32, z: i32) -> Option<crate::block_entities::BlockEntity> {
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let pos = lodestone_model::BlockPos::new(x, y, z);
        let find = |column: &crate::chunk::ChunkColumn| {
            column
                .block_entities()
                .iter()
                .find(|(at, _)| *at == pos)
                .map(|(_, entity)| entity.clone())
        };
        if let Some(fresh) = self.ensure(
            cx,
            cz,
            crate::chunk::ChunkGenerationStage::Full,
        ) {
            if let Some(entity) = find(&fresh) {
                return Some(entity);
            }
        }
        if let Some(entity) = self.read(cx, cz, find).flatten() {
            return Some(entity);
        }
        self.source.block_entity(x, y, z)
    }

    /// The one real override of [`ChunkSource::is_column_resident`] — a plain map lookup,
    /// no generation, and no `last_used` bump.
    ///
    /// Deliberately does **not** touch recency: this exists to be probed at
    /// 20 Hz by a caller that must not itself influence what stays resident
    /// (see `an_over_capacity_store_makes_the_polled_column_cold_every_pass`'s
    /// own doc for the "polling pins a column" argument this module already
    /// found and rejected once — `read`/`ensure` bump the stamp because a hit
    /// there is real work being reused; a residency *check* is not, and must
    /// not buy the column another lap of LRU life it did not earn).
    fn is_column_resident(&self, cx: i32, cz: i32) -> bool {
        self.lock().columns.contains_key(&(cx, cz))
    }

    fn reconcile_ticket_residency(&self) {
        self.maybe_tick_tickets();
    }

    /// Re-derives the capacity for `view_radius` under this store's
    /// [`CapacityPolicy`], evicting down to it if the radius-derived value shrank.
    ///
    /// # Why this recalculation is required
    ///
    /// The whole of [`integrated_capacity_for_view_radius`]'s "why the ceiling
    /// existed" section applies to an *over-subscribed* store just as it does to a
    /// capped one, because they are the same condition: capacity below the streamed
    /// view. `crate::server`'s `join_view_rings` streams outward, so the
    /// least-recently-used entry is the **innermost** ring — the column the player
    /// is standing in, which `vitals_tick` probes every 50 ms and `run_tick_loop`
    /// random-ticks. Raising render distance mid-session keeps those columns in
    /// the cache instead of allowing the retained set to collapse and forcing
    /// ~909 ms regeneration for the ground underfoot.
    ///
    /// # Grow-only: capacity follows the session's **high-water mark**
    ///
    /// A *lowering* is recorded as nothing at all. That looks like a missed
    /// opportunity to hand memory back and it is a deliberate choice, because a
    /// shrinking policy is not the safe-looking option it appears to be:
    ///
    /// * `tests/view_radius_store_capacity.rs`'s subject drags the slider **down
    ///   to 0 and back up** and asserts the re-grow costs **zero** regenerations.
    ///   A store that shrank on the way down would evict 217 of the 729 columns at
    ///   the subject radius — and by `join_view_rings`' outward order those 217 are
    ///   the *innermost* rings, the player's own feet. That gate's `== 0` would
    ///   become a non-zero, and it would be right to: nudging the render-distance
    ///   slider would cost a regeneration of the ground you are standing on. The
    ///   gate is not in the way of a shrinking policy; it is the argument against
    ///   one.
    /// * The memory it would reclaim is small. The dense representation requires
    ///   867 MiB for dense storage at the ceiling; the packed representation
    ///   requires 139 MiB when a player requests `render_distance` 32.
    ///
    /// So the residual cost is explicit and bounded: **a session that ever raised
    /// render distance to `N` keeps `N`'s capacity for the rest of the session.**
    /// If that ever needs to change, the thing to add is a shrink that refuses to
    /// drop any column *inside the current view* — not a plain `evict_down_to`,
    /// which drops exactly the wrong ones.
    ///
    /// Because it only grows, this never evicts, so there is no `unload`
    /// notification to make and no lock-ordering hazard to route around — unlike
    /// [`ensure`](Self::ensure), which does both.
    ///
    /// [`CapacityPolicy::Fixed`] stores ignore this entirely — see that variant.
    fn set_retention_radius(&self, view_radius: i32) {
        let want = match self.policy {
            CapacityPolicy::Hosted => capacity_for_view_radius(view_radius),
            CapacityPolicy::Integrated => integrated_capacity_for_view_radius(view_radius),
            #[cfg(test)]
            CapacityPolicy::Fixed => return,
        };

        let mut cache = self.lock();
        cache.capacity = cache.capacity.max(want);
    }

    fn prepare_packet_replay(&self, targets: &[(i32, i32)]) -> Option<usize> {
        self.source.prepare_packet_replay(targets)
    }

    fn reset_packet_replay(&self) {
        self.source.reset_packet_replay();
    }

    /// Persists a retained mutation through the inner source, then leaves the
    /// matching cache entry current.
    ///
    /// A source that can retain a complete snapshot receives the cache's
    /// already-mutated column, avoiding a second cold generation merely to make
    /// an edit record. If no entry is resident this deliberately does not create
    /// one: the next read regenerates through the inner source, which for
    /// [`crate::chunk::OverworldChunkSource`] consults its `edits` map and so
    /// returns the edited column.
    fn set_block(&self, x: i32, y: i32, z: i32, state: lodestone_data::block_states::StateId) {
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        let coordinates = Self::light_coordinates(cx, cz, &RETAINED_LIGHT_NEIGHBOUR_OFFSETS);
        // A mutation invalidates every retained snapshot that could have read
        // the changed column. Holding the whole footprint prevents a settlement
        // on an adjacent centre from racing the invalidation.
        let lease = self.write_gates.acquire_many(&coordinates, true);
        let retained = {
            let mut guard = self.lock();
            let cache = &mut *guard;
            let stamp = cache.next_stamp();
            if let Some(entry) = cache.columns.get_mut(&(cx, cz)) {
                // A `y` outside the column's vertical extent is a no-op rather
                // than an index panic. `ChunkColumn::set_block` indexes
                // unguarded, and the inner source's own `set_block` may have
                // accepted the edit (or rejected it its own way) without this
                // retained column being able to hold it — so the store guards its
                // own update rather than relying on the source to reject
                // out-of-range `y`.
                if y >= entry.column.min_y && y < entry.column.min_y + entry.column.height {
                    entry.column.set_block_id(lx, y, lz, state);
                    entry.last_used = stamp;
                    Some(entry.column.clone())
                } else {
                    None
                }
            } else {
                None
            }
        };
        self.invalidate_retained_light_neighbourhood_while_held(cx, cz, &coordinates);
        let retained = retained.map(|mut column| {
            // The clone was taken immediately after the block write, before
            // the cache-wide invalidation. Do not forward the old retained
            // light to a wrapped source: the mutation makes that snapshot
            // stale even for the edited centre itself.
            column.clear_retained_light();
            column
        });
        if !retained
            .as_ref()
            .is_some_and(|column| self.source.store_resident_column(cx, cz, column))
        {
            self.source.set_block(x, y, z, state);
        }
        lease.release_and_prune();
        for &coordinate in &coordinates {
            if !self.is_column_resident(coordinate.0, coordinate.1) {
                self.write_gates.forget_if_idle(std::slice::from_ref(&coordinate));
            }
        }
    }

    /// Forwarded for the same reason `world_registries`/`dimension` above are:
    /// a cache is transparent, and answering the trait's own default here
    /// would let a fresh End sibling's dragon fight re-initialise on every
    /// join once wrapped behind this store.
    fn claim_dragon_fight_start(&self) -> bool {
        self.source.claim_dragon_fight_start()
    }
}

#[cfg(test)]
mod tests {
    use std::ops::RangeInclusive;
    use std::sync::{Arc, Barrier, Condvar, Mutex};
    use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

    use lodestone_data::block::Block;
    use lodestone_model::BlockPos;

    use super::*;
    use crate::block_entities::BlockEntityHandle;
    use crate::mobs::{ChunkWorld, MobHandle};
    use crate::tick::{
        BlockTickFeed, ExplosionFeed, INITIAL_RANDOM_TICK_DEFERRAL_TICKS, TICK_PERIOD, TickClock,
        run_tick_loop,
    };

    /// The world height a real overworld column has, so the clone-cost
    /// measurement below is about the representation production actually pays
    /// for rather than a toy.
    const REAL_MIN_Y: i32 = -64;
    const REAL_HEIGHT: i32 = 384;

    /// A [`ChunkSource`] that counts `column()` calls and nothing else.
    ///
    /// **Hand-written on purpose: every call counts.** The production source
    /// carries a per-instance 512-entry memo cache keyed on exact `(cx, cz)`,
    /// so a generation-count gate using that source could hide a broken store
    /// behind a cache hit. This source has no cache, so every call it receives
    /// is observable.
    struct CountingSource {
        calls: Arc<AtomicU64>,
        /// Recorded per coordinate too, so a failure can say *which* chunk was
        /// regenerated rather than only that the total was wrong — per
        /// CLAUDE.md's "make failure output say *where*". Shared by `Arc` so a
        /// gate can keep reading it after the source is moved into the store.
        per_chunk: PerChunk,
        min_y: i32,
        height: i32,
        /// Every `(cx, cz)` this source's `unload` was called with, in call
        /// order — the ticket-driven eviction gate needs to observe
        /// that `ChunkStore::maybe_tick_tickets` actually reaches the source's
        /// `unload`, not merely that the cache entry disappeared (which a
        /// dropped `Entry` alone would also show).
        unloaded: Arc<Mutex<Vec<(i32, i32)>>>,
    }

    type PerChunk = Arc<Mutex<HashMap<(i32, i32), u64>>>;

    impl CountingSource {
        fn new() -> Self {
            Self::sized(0, 16)
        }

        /// Same shape, but full overworld height — used where the *size* of a
        /// column matters (the clone-cost and residency measurements).
        fn full_height() -> Self {
            Self::sized(REAL_MIN_Y, REAL_HEIGHT)
        }

        fn sized(min_y: i32, height: i32) -> Self {
            Self {
                calls: Arc::new(AtomicU64::new(0)),
                per_chunk: Arc::new(Mutex::new(HashMap::new())),
                min_y,
                height,
                unloaded: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn calls(&self) -> u64 {
            self.calls.load(Ordering::Relaxed)
        }

    }

    struct BatchStoreSource {
        batch_calls: Arc<AtomicUsize>,
        scalar_calls: Arc<AtomicUsize>,
    }

    impl ChunkSource for BatchStoreSource {
        fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
            self.scalar_calls.fetch_add(1, Ordering::Relaxed);
            ChunkColumn::new(0, 1)
        }

        fn request_generation(
            &self,
            _request: crate::worldgen_session::GenerationRequest,
            _session: Option<&mut GenerationSession>,
        ) -> Result<
            Option<crate::worldgen_session::GenerationRequestResult>,
            crate::worldgen_session::GenerationRequestError,
        > {
            Ok(Some(
                crate::worldgen_session::GenerationRequestResult::Existing(
                    ChunkColumn::new(0, 1),
                ),
            ))
        }

        fn request_generation_batch(
            &self,
            sessions: &mut [GenerationSession],
        ) -> Vec<
            Result<
                Option<crate::worldgen_session::GenerationRequestResult>,
                crate::worldgen_session::GenerationRequestError,
            >,
        > {
            self.batch_calls.fetch_add(1, Ordering::Relaxed);
            sessions
                .iter()
                .map(|_| {
                    Ok(Some(
                        crate::worldgen_session::GenerationRequestResult::Existing(
                            ChunkColumn::new(0, 1),
                        ),
                    ))
                })
                .collect()
        }

        fn block_state_id(&self, _x: i32, _y: i32, _z: i32) -> lodestone_data::block_states::StateId {
            crate::chunk::air_state()
        }

        fn biome_state_at(&self, _x: i32, _y: i32, _z: i32) -> String {
            crate::chunk::DEFAULT_BIOME.to_owned()
        }

        fn set_block(&self, _x: i32, _y: i32, _z: i32, _state: lodestone_data::block_states::StateId) {}
    }

    /// The worst per-coordinate generation count, with its coordinate — the
    /// figure that distinguishes "generated once each" from "regenerated every
    /// tick" without depending on how many chunks the loop happened to visit.
    fn worst_chunk(per_chunk: &PerChunk) -> ((i32, i32), u64) {
        per_chunk
            .lock()
            .expect("per-chunk map poisoned")
            .iter()
            .max_by_key(|&(_, &n)| n)
            .map(|(&k, &n)| (k, n))
            .unwrap_or(((0, 0), 0))
    }

    /// How many distinct coordinates were ever generated.
    fn distinct_chunks(per_chunk: &PerChunk) -> usize {
        per_chunk.lock().expect("per-chunk map poisoned").len()
    }

    impl ChunkSource for CountingSource {
        fn column(&self, cx: i32, cz: i32) -> ChunkColumn {
            self.calls.fetch_add(1, Ordering::Relaxed);
            *self
                .per_chunk
                .lock()
                .expect("per-chunk map poisoned")
                .entry((cx, cz))
                .or_insert(0) += 1;
            ChunkColumn::new(self.min_y, self.height)
        }

        // Goes through `column()` on purpose: the control half of
        // `repeated_single_block_probes_generate_one_column_not_forty` relies
        // on one probe costing exactly one generation. This is the explicit
        // column-regenerating form.
        fn block_state_id(&self, x: i32, y: i32, z: i32) -> lodestone_data::block_states::StateId {
            let cx = x.div_euclid(16);
            let cz = z.div_euclid(16);
            let lx = x.rem_euclid(16);
            let lz = z.rem_euclid(16);
            self.column(cx, cz).block_state_id(lx, y, lz)
        }

        fn biome_state_at(&self, x: i32, y: i32, z: i32) -> String {
            let cx = x.div_euclid(16);
            let cz = z.div_euclid(16);
            let lx = x.rem_euclid(16);
            let lz = z.rem_euclid(16);
            self.column(cx, cz).biome_state_at(lx, y, lz).to_string()
        }

        // `run_tick_loop` forwards random-tick and grazing mutations through
        // the store to the inner source, so this must not panic; but the
        // source has no storage (its `column()` is a fresh blank column plus a
        // counter), so the edit is deliberately discarded. Explicit rather than
        // inherited — this stub must make the discarded-edit behavior explicit.
        fn set_block(&self, _x: i32, _y: i32, _z: i32, _state: lodestone_data::block_states::StateId) {
            // No storage; edits are discarded by design for this counting stub.
        }

        fn unload(&self, cx: i32, cz: i32) {
            self.unloaded
                .lock()
                .expect("unloaded log poisoned")
                .push((cx, cz));
        }
    }

    #[test]
    fn store_batch_path_reuses_one_region_and_preserves_results() {
        let batch_calls = Arc::new(AtomicUsize::new(0));
        let scalar_calls = Arc::new(AtomicUsize::new(0));
        let source = BatchStoreSource {
            batch_calls: Arc::clone(&batch_calls),
            scalar_calls: Arc::clone(&scalar_calls),
        };
        let store = ChunkStore::new(source);
        let requests = [(0, 0), (1, 0)]
            .into_iter()
            .map(|target| {
                GenerationSession::new(crate::worldgen_session::GenerationRequest::new(
                    Dimension::Overworld,
                    target,
                    GenerationTarget::Full,
                    1,
                ))
            })
            .collect::<Vec<_>>();
        let mut sessions = requests;
        let results = ChunkSource::request_generation_batch(&store, &mut sessions);
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|result| {
            matches!(
                result,
                Ok(Some(crate::worldgen_session::GenerationRequestResult::Existing(_)))
            )
        }));
        assert_eq!(batch_calls.load(Ordering::Relaxed), 1);
        assert_eq!(scalar_calls.load(Ordering::Relaxed), 0);
        assert!(store.is_column_resident(0, 0));
        assert!(store.is_column_resident(1, 0));
    }

    #[test]
    fn store_batch_path_rejects_mixed_pipeline_identities_before_admission() {
        let batch_calls = Arc::new(AtomicUsize::new(0));
        let scalar_calls = Arc::new(AtomicUsize::new(0));
        let store = ChunkStore::with_capacity(
            BatchStoreSource {
                batch_calls: Arc::clone(&batch_calls),
                scalar_calls: Arc::clone(&scalar_calls),
            },
            0,
        );
        let mut sessions = vec![
            GenerationSession::new(crate::worldgen_session::GenerationRequest::new(
                Dimension::Overworld,
                (0, 0),
                GenerationTarget::Full,
                1,
            )),
            GenerationSession::new(crate::worldgen_session::GenerationRequest::new(
                Dimension::End,
                (1, 0),
                GenerationTarget::Full,
                1,
            )),
        ];

        let results = ChunkSource::request_generation_batch(&store, &mut sessions);

        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|result| {
            matches!(
                result,
                Err(crate::worldgen_session::GenerationRequestError::Boundary(message))
                    if message == "generation batch requires one pipeline identity"
            )
        }));
        assert_eq!(batch_calls.load(Ordering::Relaxed), 0);
        assert_eq!(scalar_calls.load(Ordering::Relaxed), 0);
        let stats = store.generation_ledger().stats();
        assert_eq!(stats.pipelines, 0);
        assert_eq!(stats.coordinates, 0);
        assert_eq!(stats.overlays, 0);
        assert_eq!(stats.products, 0);
        assert!(!store.is_column_resident(0, 0));
        assert!(!store.is_column_resident(1, 0));
    }

    #[test]
    fn mixed_batch_coalesces_ledger_output_before_revision_check() {
        use lodestone_worldgen::stage_schedule::{Dimension, GenerationTarget, END_PIPELINE};

        let batch_calls = Arc::new(AtomicUsize::new(0));
        let scalar_calls = Arc::new(AtomicUsize::new(0));
        let store = ChunkStore::with_capacity(
            BatchStoreSource {
                batch_calls: Arc::clone(&batch_calls),
                scalar_calls: Arc::clone(&scalar_calls),
            },
            0,
        );
        let mut ledger = GenerationLedger::new();
        ledger.admit(END_PIPELINE, &[(0, 0)]).unwrap();
        let mut seeded = end_shaped_session(1);
        complete_end_suffix(&mut seeded, 1);
        ledger.publish_session(END_PIPELINE, &seeded).unwrap();
        *store.generation_ledger() = ledger;
        assert!(store
            .generation_ledger()
            .output_column(END_PIPELINE, (0, 0))
            .is_some());

        let reused_request = crate::worldgen_session::GenerationRequest::new(
            Dimension::End,
            (0, 0),
            GenerationTarget::Full,
            0,
        );
        let generated_request = crate::worldgen_session::GenerationRequest::new(
            Dimension::End,
            (1, 0),
            GenerationTarget::Full,
            0,
        );
        let mut sessions = vec![
            GenerationSession::new(reused_request),
            GenerationSession::new(generated_request),
        ];
        let results = ChunkSource::request_generation_batch(&store, &mut sessions);

        assert_eq!(results.len(), 2);
        assert!(matches!(
            &results[0],
            Ok(Some(crate::worldgen_session::GenerationRequestResult::Existing(_)))
        ));
        assert!(matches!(
            &results[1],
            Ok(Some(crate::worldgen_session::GenerationRequestResult::Existing(_)))
        ));
        assert_eq!(batch_calls.load(Ordering::Relaxed), 1);
        assert_eq!(scalar_calls.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn mixed_batch_persists_a_generated_spill_into_a_source_existing_target() {
        use lodestone_worldgen::stage_schedule::{Dimension, GenerationTarget, END_PIPELINE};

        let source_target = (0, 0);
        let existing_target = (1, 0);
        let destination = BlockCoordinate::new(16, 4, 0);
        let store = ChunkStore::with_capacity(
            MixedBatchSource {
                driver: ResumableDriver::with_feature_spill(
                    source_target,
                    destination,
                    Block::Dirt.default_state(),
                ),
                existing_target,
                persisted: Arc::new(Mutex::new(HashMap::new())),
            },
            0,
        );
        let mut sessions = [source_target, existing_target]
            .into_iter()
            .map(|target| {
                GenerationSession::new(crate::worldgen_session::GenerationRequest::new(
                    Dimension::End,
                    target,
                    GenerationTarget::Full,
                    1,
                ))
            })
            .collect::<Vec<_>>();

        let results = ChunkSource::request_generation_batch(&store, &mut sessions);

        assert!(matches!(
            &results[0],
            Ok(Some(crate::worldgen_session::GenerationRequestResult::Generated(_)))
        ));
        assert!(matches!(
            &results[1],
            Ok(Some(crate::worldgen_session::GenerationRequestResult::Existing(_)))
        ));
        assert!(
            store
                .generation_ledger()
                .output_column(END_PIPELINE, existing_target)
                .is_none(),
            "the source-existing target has no retained output product"
        );
        assert!(
            !store.is_column_resident(existing_target.0, existing_target.1),
            "capacity zero forces the mixed-batch target out of the resident cache"
        );
        assert_eq!(
            ChunkSource::block_state_id(&store, destination.x(), destination.y(), destination.z()),
            Block::Dirt.default_state(),
            "the generated spill survives eviction through the captured source payload"
        );
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn persisted_existing_cohort_target_releases_empty_admission_and_failed_suffix() {
        use lodestone_worldgen::stage_schedule::{
            Dimension, END_PIPELINE, GenerationTarget, PipelineOptions,
        };
        use crate::worldgen_session::GenerationRequestResult;

        let existing_target = (0, 0);
        let persisted = Arc::new(Mutex::new(HashMap::from([(
            existing_target,
            ChunkColumn::new(0, 16),
        )])));
        let store = ChunkStore::with_capacity(
            MixedBatchSource {
                driver: ResumableDriver::new(false),
                existing_target,
                persisted,
            },
            0,
        );
        let mut sessions = [existing_target, (1, 0), (2, 0)]
            .into_iter()
            .map(|target| {
                GenerationSession::new(crate::worldgen_session::GenerationRequest::new(
                    Dimension::End,
                    target,
                    GenerationTarget::Full,
                    1,
                ))
            })
            .collect::<Vec<_>>();
        let admission_union = sessions
            .iter()
            .flat_map(|session| session.admission_order().iter().copied())
            .collect::<BTreeSet<_>>();
        let mut emitted = Vec::new();

        let error = ChunkSource::request_generation_cohort(
            &store,
            &mut sessions,
            &mut |index, session, result| {
                emitted.push((
                    index,
                    session.request().target(),
                    matches!(&result, GenerationRequestResult::Existing(_)),
                ));
                if index == 1 {
                    Err(crate::worldgen_session::GenerationRequestError::Boundary(
                        "consumer stopped after stable neighbour".to_owned(),
                    ))
                } else {
                    Ok(())
                }
            },
        )
        .unwrap_err();

        assert!(matches!(
            error,
            crate::worldgen_session::GenerationRequestError::Boundary(message)
                if message == "consumer stopped after stable neighbour"
        ));
        assert_eq!(admission_union.len(), 15);
        assert_eq!(
            emitted,
            vec![(0, existing_target, true), (1, (1, 0), false)]
        );

        let ledger = store.generation_ledger();
        let stats = ledger.stats();
        assert_eq!(stats.coordinates, sessions[1].admission_order().len());
        assert!(stats.coordinates < admission_union.len());
        assert!(stats.sources > 0);
        assert!(ledger
            .output_column(END_PIPELINE, (1, 0))
            .is_some());
        assert!(ledger.output_column(END_PIPELINE, existing_target).is_none());
        assert!(ledger.output_column(END_PIPELINE, (2, 0)).is_none());
        assert!(ledger
            .frontier(END_PIPELINE.identity(PipelineOptions::ALL), existing_target)
            .is_ok(), "the adjacent generated target's shared source state must survive cleanup");
    }

    struct RejectingDriver;

    impl crate::worldgen_session::RequestStageDriver for RejectingDriver {
        fn generate(
            &self,
            _session: &mut crate::worldgen_session::GenerationSession,
        ) -> Result<crate::worldgen_session::PacketSnapshot, crate::worldgen_session::SessionError>
        {
            Err(crate::worldgen_session::SessionError::Cancelled)
        }
    }

    struct DriverSource {
        driver: RejectingDriver,
    }

    impl ChunkSource for DriverSource {
        fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
            ChunkColumn::new(0, 16)
        }

        fn block_state_id(&self, x: i32, y: i32, z: i32) -> lodestone_data::block_states::StateId {
            ChunkColumn::new(0, 16)
                .block_state_id(x.rem_euclid(16), y, z.rem_euclid(16))
        }

        fn biome_state_at(&self, x: i32, y: i32, z: i32) -> String {
            ChunkColumn::new(0, 16)
                .biome_state_at(x.rem_euclid(16), y, z.rem_euclid(16))
                .to_owned()
        }

        fn set_block(&self, _x: i32, _y: i32, _z: i32, _state: lodestone_data::block_states::StateId) {}

        fn request_stage_driver(
            &self,
        ) -> Option<&dyn crate::worldgen_session::RequestStageDriver> {
            Some(&self.driver)
        }
    }

    struct ResumableDriver {
        calls: Arc<AtomicUsize>,
        fill_completions: Arc<AtomicUsize>,
        cancel_after_fill: AtomicBool,
        feature_spill: Option<(ChunkCoordinate, BlockCoordinate, StateId)>,
    }

    impl ResumableDriver {
        fn new(cancel_after_fill: bool) -> Self {
            Self {
                calls: Arc::new(AtomicUsize::new(0)),
                fill_completions: Arc::new(AtomicUsize::new(0)),
                cancel_after_fill: AtomicBool::new(cancel_after_fill),
                feature_spill: None,
            }
        }

        fn with_feature_spill(
            target: ChunkCoordinate,
            destination: BlockCoordinate,
            state: StateId,
        ) -> Self {
            let mut driver = Self::new(false);
            driver.feature_spill = Some((target, destination, state));
            driver
        }
    }

    impl crate::worldgen_session::RequestStageDriver for ResumableDriver {
        fn generate(
            &self,
            session: &mut crate::worldgen_session::GenerationSession,
        ) -> Result<crate::worldgen_session::PacketSnapshot, crate::worldgen_session::SessionError>
        {
            self.calls.fetch_add(1, Ordering::Relaxed);
            let target = session.request().target();
            for &stage in session
                .pipeline()
                .schedule()
                .stages_for(session.request().generation_target())
            {
                let key = StageKey::new(session.pipeline().dimension(), stage);
                if session.frontier(target).is_some_and(|frontier| {
                    frontier.records().iter().any(|record| record.key() == key)
                }) {
                    continue;
                }
                let descriptor = session
                    .pipeline()
                    .descriptor(stage)
                    .expect("the selected stage has a descriptor");
                if descriptor.barrier() == BarrierPolicy::SourceOrdered {
                    let sources = session.admission_order().to_vec();
                    session.declare_mutable_sources(
                        key,
                        sources
                            .iter()
                            .copied()
                            .enumerate()
                            .map(|(order, source)| (order as u64, source)),
                    )?;
                    for (source_order, source) in sources.into_iter().enumerate() {
                        if session.source_order_committed(source_order as u64) {
                            continue;
                        }
                        let transaction = session.begin_mutable_source(
                            source,
                            key,
                            source_order as u64,
                        )?;
                        let mut transaction = transaction;
                        if stage == lodestone_worldgen::stage_schedule::ColumnStage::Features
                            && self.feature_spill.is_some_and(|(target, _, _)| {
                                target == session.request().target() && source == target
                            })
                        {
                            let (_, destination, state) =
                                self.feature_spill.expect("feature spill remains configured");
                            transaction.push(0, destination, state)?;
                        }
                        session.complete_mutable_source(transaction)?;
                    }
                    let products = descriptor
                        .outputs()
                        .iter()
                        .copied()
                        .map(|resource| ImmutableProduct::new(resource, stage as u8))
                        .collect();
                    let sidecars = descriptor
                        .retained_sidecars()
                        .iter()
                        .copied()
                        .map(|sidecar| ImmutableSidecar::new(sidecar, stage as u8))
                        .collect();
                    session.commit_mutable_stage(
                        key,
                        [stage as u8; 32],
                        [stage as u8; 32],
                        1,
                        products,
                        sidecars,
                    )?;
                } else {
                    let products = descriptor
                        .outputs()
                        .iter()
                        .copied()
                        .map(|resource| {
                            if resource == ResourceKey::OutputColumn {
                                ImmutableProduct::new(resource, ChunkColumn::new(0, 16))
                            } else {
                                ImmutableProduct::new(resource, stage as u8)
                            }
                        })
                        .collect();
                    let sidecars = descriptor
                        .retained_sidecars()
                        .iter()
                        .copied()
                        .map(|sidecar| ImmutableSidecar::new(sidecar, stage as u8))
                        .collect();
                    session.complete_immutable(ImmutableStageCompletion::new(
                        target,
                        key,
                        [stage as u8; 32],
                        [stage as u8; 32],
                        1,
                        products,
                        sidecars,
                    ))?;
                    session.advance_ready_immutable()?;
                    if stage == lodestone_worldgen::stage_schedule::ColumnStage::Fill {
                        self.fill_completions.fetch_add(1, Ordering::Relaxed);
                        if self.cancel_after_fill.swap(false, Ordering::AcqRel) {
                            session.cancel();
                        }
                    }
                }
            }
            session.complete_light_domain()?;
            session.finalize_packet(ChunkColumn::new(0, 16))
        }
    }

    struct ResumableSource {
        driver: ResumableDriver,
    }

    impl ChunkSource for ResumableSource {
        fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
            ChunkColumn::new(0, 16)
        }

        fn block_state_id(&self, _x: i32, _y: i32, _z: i32) -> lodestone_data::block_states::StateId {
            crate::chunk::air_state()
        }

        fn biome_state_at(&self, _x: i32, _y: i32, _z: i32) -> String {
            "minecraft:the_void".to_owned()
        }

        fn set_block(&self, _x: i32, _y: i32, _z: i32, _state: lodestone_data::block_states::StateId) {}

        fn request_stage_driver(
            &self,
        ) -> Option<&dyn crate::worldgen_session::RequestStageDriver> {
            Some(&self.driver)
        }
    }

    struct MixedBatchSource {
        driver: ResumableDriver,
        existing_target: ChunkCoordinate,
        persisted: Arc<Mutex<HashMap<ChunkCoordinate, ChunkColumn>>>,
    }

    impl ChunkSource for MixedBatchSource {
        fn column(&self, cx: i32, cz: i32) -> ChunkColumn {
            self.persisted
                .lock()
                .unwrap()
                .get(&(cx, cz))
                .cloned()
                .unwrap_or_else(|| ChunkColumn::new(0, 16))
        }

        fn request_generation_batch(
            &self,
            sessions: &mut [GenerationSession],
        ) -> Vec<
            Result<
                Option<crate::worldgen_session::GenerationRequestResult>,
                crate::worldgen_session::GenerationRequestError,
            >,
        > {
            sessions
                .iter_mut()
                .map(|session| {
                    if session.request().target() == self.existing_target {
                        Ok(Some(
                            crate::worldgen_session::GenerationRequestResult::Existing(
                                self.column(self.existing_target.0, self.existing_target.1),
                            ),
                        ))
                    } else {
                        crate::worldgen_session::RequestStageDriver::generate(&self.driver, session)
                            .map(crate::worldgen_session::GenerationRequestResult::Generated)
                            .map(Some)
                            .map_err(Into::into)
                    }
                })
                .collect()
        }

        fn block_state_id(
            &self,
            x: i32,
            y: i32,
            z: i32,
        ) -> lodestone_data::block_states::StateId {
            self.column(x.div_euclid(16), z.div_euclid(16)).block_state_id(
                x.rem_euclid(16),
                y,
                z.rem_euclid(16),
            )
        }

        fn biome_state_at(&self, _x: i32, _y: i32, _z: i32) -> String {
            crate::chunk::DEFAULT_BIOME.to_owned()
        }

        fn set_block(&self, _x: i32, _y: i32, _z: i32, _state: StateId) {}

        fn store_resident_columns(&self, columns: &[(i32, i32, ChunkColumn)]) -> bool {
            let mut persisted = self.persisted.lock().unwrap();
            for (cx, cz, column) in columns {
                persisted.insert((*cx, *cz), column.clone());
            }
            true
        }
    }

    struct BlockingDriver {
        inner: ResumableDriver,
        entered: Arc<Barrier>,
        release: Arc<Barrier>,
    }

    impl crate::worldgen_session::RequestStageDriver for BlockingDriver {
        fn generate(
            &self,
            session: &mut crate::worldgen_session::GenerationSession,
        ) -> Result<crate::worldgen_session::PacketSnapshot, crate::worldgen_session::SessionError>
        {
            self.entered.wait();
            self.release.wait();
            self.inner.generate(session)
        }
    }

    struct BlockingSource {
        driver: BlockingDriver,
    }

    impl ChunkSource for BlockingSource {
        fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
            ChunkColumn::new(0, 16)
        }

        fn block_state_id(&self, _x: i32, _y: i32, _z: i32) -> lodestone_data::block_states::StateId {
            crate::chunk::air_state()
        }

        fn biome_state_at(&self, _x: i32, _y: i32, _z: i32) -> String {
            "minecraft:the_void".to_owned()
        }

        fn set_block(&self, _x: i32, _y: i32, _z: i32, _state: lodestone_data::block_states::StateId) {}

        fn request_stage_driver(
            &self,
        ) -> Option<&dyn crate::worldgen_session::RequestStageDriver> {
            Some(&self.driver)
        }
    }

    /// The `tick_area` the shell actually produces for singleplayer:
    /// `crates/lodestone-shell/src/net.rs` passes
    /// `mob_radius = view_radius.clamp(1, 3)`, so at any real view radius this
    /// is `-3..=3` on both axes — **49** columns, not the 9 a 3×3 reading
    /// suggests. Transcribed from that call site rather than invented, because
    /// the whole magnitude of the bug is this number times the per-column cost.
    const SHELL_TICK_RADIUS: i32 = 3;

    fn shell_tick_area() -> (RangeInclusive<i32>, RangeInclusive<i32>) {
        (
            -SHELL_TICK_RADIUS..=SHELL_TICK_RADIUS,
            -SHELL_TICK_RADIUS..=SHELL_TICK_RADIUS,
        )
    }

    fn preload_tick_area<S: ChunkSource>(store: &ChunkStore<S>) {
        for cx in -SHELL_TICK_RADIUS..=SHELL_TICK_RADIUS {
            for cz in -SHELL_TICK_RADIUS..=SHELL_TICK_RADIUS {
                let _ = store.column(cx, cz);
            }
        }
    }

    const EXPECTED_TICK_AREA_COLUMNS: usize =
        ((2 * SHELL_TICK_RADIUS + 1) * (2 * SHELL_TICK_RADIUS + 1)) as usize;

    /// How many random-tick *passes* the gates below observe — **not** how many
    /// ticks they drive.
    ///
    /// [`crate::tick::run_tick_loop`] is the only thing that calls
    /// `world.column()` here, and since the initial-deferral rule it skips its random-tick pass
    /// while `game_tick <= INITIAL_RANDOM_TICK_DEFERRAL_TICKS`. `game_tick` is
    /// incremented at the top of each iteration, so driving [`TICKS`] periods
    /// yields passes on ticks `INITIAL_RANDOM_TICK_DEFERRAL_TICKS + 1 ..= TICKS`,
    /// i.e. exactly this many.
    ///
    /// 12 rather than some other number because it is the figure this module's
    /// own doc comment and the negative control's `49 × 12 = 588` observation
    /// were recorded against — the *passes* count is what those numbers were
    /// always about; only the tick count had to move.
    const RANDOM_TICK_PASSES: u32 = 12;

    /// Tick periods to drive, derived from the deferral rather than restated:
    /// the deferral is a production knob, and a gate that hardcoded a tick
    /// count went to **zero** observed generations when it was introduced.
    const TICKS: u32 = INITIAL_RANDOM_TICK_DEFERRAL_TICKS as u32 + RANDOM_TICK_PASSES;

    // The deferral must not swallow the whole window, or both gates below
    // measure nothing while still reading as rigorous. Checked at compile time
    // so raising `INITIAL_RANDOM_TICK_DEFERRAL_TICKS` past `TICKS` is a build
    // failure rather than a silent pair of zeroes.
    const _: () = assert!(RANDOM_TICK_PASSES > 0);
    const _: () = assert!(TICKS as u64 > INITIAL_RANDOM_TICK_DEFERRAL_TICKS);

    /// Drives `run_tick_loop` for `ticks` virtual tick periods against `world`,
    /// returning nothing — the caller reads its own counter afterwards.
    ///
    /// Virtual time (`start_paused`), so this is immune to the box's load. The
    /// `yield_now` before and after are both required, not defensive: see
    /// `crate::tick`'s own tests for why the first one (the spawned task must
    /// reach its `Instant::now()` baseline before the first `advance`) and the
    /// second (the woken task must actually run its synchronous body).
    async fn drive_tick_loop<W: ChunkSource + 'static>(
        world: Arc<W>,
        area: (RangeInclusive<i32>, RangeInclusive<i32>),
        ticks: u32,
    ) -> Arc<TickClock> {
        drive_tick_loop_with_block_entities(world, area, ticks, BlockEntityHandle::default()).await
    }

    /// [`drive_tick_loop`], with the registry supplied by the caller — the arm
    /// the block-entity gates below need, since the whole question there is what
    /// a *populated* registry makes the tick loop ask the store for.
    ///
    /// A separate entry point rather than a fourth parameter on
    /// [`drive_tick_loop`] so the two column-generation gates above keep the
    /// exact call they were measured with.
    async fn drive_tick_loop_with_block_entities<W: ChunkSource + 'static>(
        world: Arc<W>,
        area: (RangeInclusive<i32>, RangeInclusive<i32>),
        ticks: u32,
        block_entities: BlockEntityHandle,
    ) -> Arc<TickClock> {
        let clock = Arc::new(TickClock::new());
        tokio::spawn(run_tick_loop(
            MobHandle::new(ChunkWorld::new(REAL_MIN_Y, REAL_HEIGHT)),
            crate::mobs::LiveMobSource::default(),
            block_entities,
            Arc::clone(&clock),
            world,
            BlockTickFeed::default(),
            area,
            ExplosionFeed::default(),
            // this gate measures column generation, not persistence,
            // so a fresh handle -- behaviourally the locals this replaced.
            crate::region_source::ScheduledTickHandle::default(),
            crate::tick_area::TickFollow::default(),
        ));
        tokio::task::yield_now().await;
        for _ in 0..ticks {
            tokio::time::advance(TICK_PERIOD).await;
            tokio::task::yield_now().await;
        }
        clock
    }

    /// **The load-bearing gate.** A column is generated exactly **once**, no
    /// matter how many ticks run over it.
    ///
    /// # Why a count and not a duration
    ///
    /// Counts are immune to machine load; durations are not — a 2.3× spread
    /// was measured on an identical release binary from load alone, while every
    /// count stayed byte-identical. So the assertion is
    /// `generated == distinct chunks`, never "the tick got faster".
    ///
    /// # Predicting the value, not the sign
    ///
    /// The two competing hypotheses are computed rather than compared: with a
    /// a preloaded store, repeated resident-only visits produce **49** total
    /// generations. A cold store is deliberately not touched by the tick loop.
    ///
    /// # Duration species
    ///
    /// `CountingSource` is constructed inside this test, so its counter has no
    /// life outside the gate. `TickClock` would have been the wrong instrument:
    /// it accumulates over a whole server lifetime. It is read here only as a
    /// precondition (did the loop actually run?), never as the measurement.
    #[tokio::test(start_paused = true)]
    async fn the_store_generates_each_column_exactly_once_across_many_ticks() {
        let counting = CountingSource::new();
        let calls = Arc::clone(&counting.calls);
        let per_chunk = Arc::clone(&counting.per_chunk);
        let store = Arc::new(ChunkStore::new(counting));

        preload_tick_area(store.as_ref());
        let clock = drive_tick_loop(Arc::clone(&store), shell_tick_area(), TICKS).await;

        // Precondition, failing rather than skipping: if the loop did not
        // really run many ticks, "generated once" is trivially true and this
        // gate measures nothing.
        assert!(
            clock.tick_count() >= u64::from(TICKS) - 1,
            "the tick loop only advanced {} ticks of {TICKS}; the count below would be \
             trivially satisfied",
            clock.tick_count()
        );

        let generated = calls.load(Ordering::Relaxed);
        assert_eq!(
            distinct_chunks(&per_chunk),
            EXPECTED_TICK_AREA_COLUMNS,
            "precondition: the loop must have visited the whole tick area, or the total \
             below could be right for the wrong reason"
        );
        assert_eq!(
            store.len(),
            EXPECTED_TICK_AREA_COLUMNS,
            "the store should hold the whole tick area ({EXPECTED_TICK_AREA_COLUMNS} columns)"
        );

        // The per-chunk figure, not just the total: a total can be right while
        // one chunk is regenerated N times and another never visited.
        let (worst_coord, worst_count) = worst_chunk(&per_chunk);
        assert_eq!(
            worst_count, 1,
            "chunk {worst_coord:?} was generated {worst_count} times over \
             {RANDOM_TICK_PASSES} random-tick passes; every column must be generated \
             exactly once"
        );
        assert_eq!(
            generated, EXPECTED_TICK_AREA_COLUMNS as u64,
            "expected exactly one generation per column of the tick area \
             ({EXPECTED_TICK_AREA_COLUMNS}); got {generated}. \
             {} would mean every chunk is still regenerated every pass.",
            EXPECTED_TICK_AREA_COLUMNS as u64 * u64::from(RANDOM_TICK_PASSES)
        );
        assert_eq!(
            store.evicted(),
            0,
            "the tick area must fit the default capacity without eviction, or the \
             steady state thrashes"
        );
    }

    /// A zero-capacity store does not opt back into cold generation from the
    /// resident-only tick boundary.
    ///
    /// `ChunkStore::with_capacity(source, 0)` retains nothing. The tick loop
    /// still refuses to cold-generate its bounded area, so a zero-capacity
    /// source remains cold until an explicit blocking caller asks for it.
    #[tokio::test(start_paused = true)]
    async fn without_retention_tick_loop_does_not_cold_generate() {
        let counting = CountingSource::new();
        let calls = Arc::clone(&counting.calls);
        let per_chunk = Arc::clone(&counting.per_chunk);
        let store = Arc::new(ChunkStore::with_capacity(counting, 0));

        drive_tick_loop(Arc::clone(&store), shell_tick_area(), TICKS).await;

        let generated = calls.load(Ordering::Relaxed);
        assert_eq!(generated, 0, "resident-only ticks must not cold-generate");
        assert!(per_chunk.lock().expect("per-chunk map poisoned").is_empty());
        assert_eq!(store.len(), 0, "a zero-capacity store must retain nothing");
    }

    /// The store behavior independent of the tick loop: reading **one
    /// block** must not regenerate a column.
    ///
    /// `crate::server`'s `vitals_tick` does exactly this every 50 ms once the
    /// client has sent a position, on the connection task — the task that
    /// streams chunks. Against a column-regenerating source each probe is a
    /// full column generation,
    /// which is why chunk streaming stops at the first movement packet rather
    /// than at join.
    ///
    /// Negative control in the same body: the unwrapped source, where the same
    /// 40 probes cost 40 generations.
    #[test]
    fn repeated_single_block_probes_generate_one_column_not_forty() {
        const PROBES: u64 = 40;

        // Control: the bare source exposes the explicit column-regenerating
        // form.
        let bare = CountingSource::new();
        for _ in 0..PROBES {
            let _ = bare.block_state_id(5, 8, 5);
        }
        assert_eq!(
            bare.calls(),
            PROBES,
            "control: `CountingSource::block_state` regenerates a whole column per probe \
             (the column-regenerating form that was `ChunkSource`'s default before issue \
             #440). If this is not {PROBES}, the impl changed and the gate below is \
             measuring the wrong thing."
        );

        // Subject: the same probes through the store.
        let counting = CountingSource::new();
        let calls = Arc::clone(&counting.calls);
        let store = ChunkStore::new(counting);
        for _ in 0..PROBES {
            let _ = store.block_state_id(5, 8, 5);
        }
        assert_eq!(
            calls.load(Ordering::Relaxed),
            1,
            "{PROBES} probes of the same block must cost exactly one generation"
        );
        assert_eq!(store.len(), 1, "one column touched, one column resident");
    }

    /// [`ChunkSource::is_column_resident`] reports real residency with no
    /// generation at all, in both directions.
    #[test]
    fn is_column_resident_reports_true_only_for_a_cached_column_and_never_generates() {
        let counting = CountingSource::new();
        let calls = Arc::clone(&counting.calls);
        let store = ChunkStore::new(counting);

        assert!(
            !store.is_column_resident(0, 0),
            "a column nothing has touched must report not-resident"
        );
        assert_eq!(
            calls.load(Ordering::Relaxed),
            0,
            "checking residency must not generate anything — this is the whole point of the \
             primitive: it exists to be asked instead of `block_state`, which would generate \
             on exactly this miss"
        );

        let _ = store.column(0, 0);
        assert!(
            store.is_column_resident(0, 0),
            "a column just generated and cached must report resident"
        );
        assert!(
            !store.is_column_resident(5, 5),
            "a distinct, untouched column must still report not-resident — the true positive \
             above must not be a constant `true`"
        );
        assert_eq!(
            calls.load(Ordering::Relaxed),
            1,
            "the residency checks themselves must not have generated anything beyond the one \
             explicit `column()` call"
        );
    }

    /// A residency check followed by a separate mutation is not an admission
    /// boundary: a cold generation can claim the coordinate between the two
    /// calls and make the tick thread wait on the condition variable. The try
    /// APIs claim the gate first, so they return `Busy` immediately and the
    /// ordinary blocking control remains visibly parked until the lease drops.
    #[test]
    fn try_resident_access_returns_busy_without_waiting_on_generation_lease() {
        let store = Arc::new(ChunkStore::new(CountingSource::new()));
        let _ = store.column(0, 0);
        let lease = store.write_gates.acquire_many(&[(0, 0)], true);

        let started = Arc::new(AtomicBool::new(false));
        let finished = Arc::new(AtomicBool::new(false));
        std::thread::scope(|scope| {
            let blocking_store = Arc::clone(&store);
            let blocking_started = Arc::clone(&started);
            let blocking_finished = Arc::clone(&finished);
            let blocking = scope.spawn(move || {
                blocking_started.store(true, Ordering::Release);
                let _ = blocking_store.column(0, 0);
                blocking_finished.store(true, Ordering::Release);
            });

            while !started.load(Ordering::Acquire) {
                std::thread::yield_now();
            }
            // Give the blocking control a chance to enter `acquire_many`. The
            // held lease makes its final state deterministic even if this loop
            // observes the flag before the waiter reaches the condition
            // variable.
            for _ in 0..128 {
                if finished.load(Ordering::Acquire) {
                    break;
                }
                std::thread::yield_now();
            }
            assert!(
                !finished.load(Ordering::Acquire),
                "the blocking column control must remain parked while the generation lease is held"
            );

            let started_at = lodestone_time::Instant::now();
            assert!(
                matches!(store.as_ref().try_resident_column(0, 0), TryResident::Busy),
                "a resident snapshot must report the held generation gate instead of waiting"
            );
            assert!(
                matches!(
                    store.as_ref().try_resident_block_state_id(0, 0, 0),
                    TryResident::Busy
                ),
                "a resident block read must share the coordinate admission boundary"
            );
            assert_eq!(
                store.as_ref().try_set_block(0, 0, 0, Block::Stone.default_state()),
                TryBlockMutation::Busy,
                "a resident mutation must report the held generation gate instead of waiting"
            );
            let erased: Arc<dyn ChunkSource> = store.clone();
            assert!(matches!(
                erased.try_resident_column(0, 0),
                Some(TryResident::Busy)
            ));
            assert!(matches!(
                erased.try_resident_block_state_id(0, 0, 0),
                Some(TryResident::Busy)
            ));
            assert!(matches!(
                erased.try_set_block(0, 0, 0, Block::Stone.default_state()),
                Some(TryBlockMutation::Busy)
            ));
            assert!(
                started_at.elapsed() < std::time::Duration::from_millis(100),
                "try resident access took {:?} while a generation lease was held",
                started_at.elapsed()
            );

            drop(lease);
            blocking
                .join()
                .expect("the blocking access control must not panic");
            assert!(
                finished.load(Ordering::Acquire),
                "the blocking column must proceed once the generation lease is released"
            );
        });
    }

    /// A cold coordinate is an explicit `Absent`, not an invitation to enter
    /// the blocking miss path. Once the column is resident, the same API can
    /// mutate its cached cell and the snapshot read sees the applied state.
    /// The edit-ledger control below then evicts that cache entry and proves
    /// the applied state is still present when the source reloads it.
    #[test]
    fn try_resident_mutation_is_absent_when_cold_and_applies_when_resident() {
        struct DurableEditSource {
            generated: Arc<AtomicU64>,
            edits: Arc<Mutex<HashMap<(i32, i32), ChunkColumn>>>,
        }

        impl ChunkSource for DurableEditSource {
            fn column(&self, cx: i32, cz: i32) -> ChunkColumn {
                if let Some(column) = self
                    .edits
                    .lock()
                    .expect("durable edit ledger lock poisoned")
                    .get(&(cx, cz))
                    .cloned()
                {
                    return column;
                }
                self.generated.fetch_add(1, Ordering::Relaxed);
                ChunkColumn::new(0, 16)
            }

            fn block_state_id(&self, x: i32, y: i32, z: i32) -> lodestone_data::block_states::StateId {
                self.column(x.div_euclid(16), z.div_euclid(16))
                    .block_state_id(x.rem_euclid(16), y, z.rem_euclid(16))
            }

            fn biome_state_at(&self, x: i32, y: i32, z: i32) -> String {
                self.column(x.div_euclid(16), z.div_euclid(16))
                    .biome_state_at(x.rem_euclid(16), y, z.rem_euclid(16))
                    .to_owned()
            }

            fn set_block(&self, x: i32, y: i32, z: i32, state: lodestone_data::block_states::StateId) {
                let mut edits = self
                    .edits
                    .lock()
                    .expect("durable edit ledger lock poisoned");
                edits
                    .entry((x.div_euclid(16), z.div_euclid(16)))
                    .or_insert_with(|| ChunkColumn::new(0, 16))
                    .set_block_id(x.rem_euclid(16), y, z.rem_euclid(16), state);
            }

            fn try_store_resident_edit(
                &self,
                cx: i32,
                cz: i32,
                column: &ChunkColumn,
            ) -> Option<crate::chunk_store::TryResidentEdit> {
                let mut edits = match self.edits.try_lock() {
                    Ok(edits) => edits,
                    Err(std::sync::TryLockError::WouldBlock) => {
                        return Some(crate::chunk_store::TryResidentEdit::Busy);
                    }
                    Err(std::sync::TryLockError::Poisoned(_)) => {
                        panic!("durable edit ledger lock poisoned")
                    }
                };
                edits.insert((cx, cz), column.clone());
                Some(crate::chunk_store::TryResidentEdit::Applied)
            }
        }

        let generated = Arc::new(AtomicU64::new(0));
        let edits = Arc::new(Mutex::new(HashMap::new()));
        let source = DurableEditSource {
            generated: Arc::clone(&generated),
            edits: Arc::clone(&edits),
        };
        let store = ChunkStore::with_capacity(source, 1);

        assert_eq!(
            store.try_set_block(0, 0, 0, Block::Stone.default_state()),
            TryBlockMutation::Absent,
            "a cold coordinate must not generate just to apply a tick-side mutation"
        );
        assert!(matches!(
            store.try_resident_column(0, 0),
            TryResident::Absent
        ));
        assert!(matches!(
            store.try_resident_block_state_id(0, 0, 0),
            TryResident::Absent
        ));
        assert_eq!(generated.load(Ordering::Relaxed), 0);

        let _ = store.column(0, 0);
        assert_eq!(
            store.try_set_block(0, 0, 0, Block::Stone.default_state()),
            TryBlockMutation::Applied,
            "a resident coordinate should accept the nonblocking mutation"
        );
        assert_eq!(
            store.try_resident_block_state_id(0, 0, 0),
            TryResident::Present(
                lodestone_data::block_states::StateId::new(1)
                    .expect("stone has the stable state id 1")
            )
        );
        assert_eq!(
            store.block_state_id(0, 0, 0),
            Block::Stone.default_state(),
            "the blocking read must preserve the successfully applied resident mutation"
        );
        assert_eq!(generated.load(Ordering::Relaxed), 1);

        let _ = store.column(1, 0);
        assert_eq!(store.evicted(), 1, "capacity one must evict the edited resident");
        assert_eq!(
            store.block_state_id(0, 0, 0),
            Block::Stone.default_state(),
            "an Applied try mutation must survive resident-cache eviction through the edit ledger"
        );
        assert_eq!(
            generated.load(Ordering::Relaxed),
            2,
            "the reload should generate only the untouched column; the edited column must come from the ledger"
        );

        let unsupported = ChunkStore::new(CountingSource::new());
        let _ = unsupported.column(0, 0);
        assert_eq!(
            unsupported.try_set_block(0, 0, 0, Block::Stone.default_state()),
            TryBlockMutation::Unsupported,
            "a source with no nonblocking edit ledger must refuse rather than report cache-local Applied"
        );
    }

    #[test]
    fn resident_block_read_never_generates_and_forwards_through_wrappers() {
        let counting = CountingSource::new();
        let calls = Arc::clone(&counting.calls);
        let store = Arc::new(ChunkStore::new(counting));
        assert_eq!(store.resident_block_state_id(-19, 7, 23), None);
        assert_eq!(calls.load(Ordering::Relaxed), 0);
        let _ = store.column(-2, 1);
        assert_eq!(calls.load(Ordering::Relaxed), 1, "generation control");
        store.set_block(-19, 7, 23, Block::Stone.default_state());
        let wrapped = crate::dimension::DimensionalSource::alone(
            Arc::clone(&store), crate::dimension::Dimension::Overworld,
            crate::portal::PortalIndex::default(),
        );
        let borrowed = &wrapped;
        assert_eq!(ChunkSource::resident_block_state_id(&borrowed, -19, 7, 23).map(|id| id.raw()), Some(1));
        assert_eq!(wrapped.resident_block_state_id(-18, 7, 23).map(|id| id.raw()), Some(0));
        assert_eq!(wrapped.resident_block_state_id(-19, i32::MIN, 23), None);
        assert_eq!(wrapped.resident_block_state_id(-19, i32::MAX, 23), None);
        assert_eq!(wrapped.resident_block_state_id(919, 7, 23), None);
        assert_eq!(calls.load(Ordering::Relaxed), 1, "reads must not load or regenerate");
    }

    /// Edits survive the store, in both directions that can lose them.
    ///
    /// 1. A `set_block` is visible to the very next read (the cache was
    ///    updated in place).
    /// 2. A `set_block` is visible **after eviction** (it was written through
    ///    to the inner source first, so the regeneration carries it).
    ///
    /// Property 2 is the one that licenses bounding the store at all. It is
    /// checked against `OverworldChunkSource`, because that is the only source
    /// in this crate with real retention beneath — a source whose `set_block`
    /// discards the edit (no retention) could not possibly pass, and testing
    /// against one would be a world-species vacuity.
    #[test]
    fn edits_survive_both_a_reread_and_an_eviction() {
        // Capacity 1, so touching a second column evicts the first
        // deterministically — no reliance on how many columns a real view
        // would have pushed through.
        let store = ChunkStore::with_capacity(crate::overworld_chunk_source(42), 1);

        let before = store.block_state_id(0, -50, 0);
        assert_ne!(
            before, Block::DiamondBlock.default_state(),
            "precondition: the generator must not already have placed the block this test \
             writes, or neither property below means anything"
        );

        store.set_block(0, -50, 0, Block::DiamondBlock.default_state());

        // Property 1: visible immediately, from the resident column.
        assert_eq!(
            store.block_state_id(0, -50, 0),
            Block::DiamondBlock.default_state(),
            "an edit must be visible to the next read"
        );
        assert_eq!(
            store.column(0, 0).block_state_id(0, -50, 0),
            Block::DiamondBlock.default_state(),
            "an edit must be visible through `column()` too, not only `block_state()`"
        );

        // Force an eviction of (0, 0) by touching a different column.
        let _ = store.column(7, 7);
        assert!(
            store.evicted() >= 1,
            "precondition: the capacity-1 store must actually have evicted something, or \
             property 2 below is not testing eviction at all"
        );
        assert_eq!(store.len(), 1, "capacity 1 must hold exactly one column");

        // Property 2: still visible after the cached copy is gone, because the
        // regeneration goes back through `OverworldChunkSource::edits`.
        assert_eq!(
            store.block_state_id(0, -50, 0),
            Block::DiamondBlock.default_state(),
            "an edit must survive eviction of its cache entry — this is what makes the \
             store's capacity bound lossless"
        );
    }

    /// The bound is real, and it is the property that stops this design from
    /// trading a starvation bug for an unbounded allocation.
    ///
    /// Also reports the measured per-column clone cost, since that is the one
    /// cost this design deliberately keeps (see the module docs) and a bare
    /// assertion would not record it.
    #[test]
    fn residency_is_bounded_and_the_clone_is_cheap() {
        const CAPACITY: usize = 32;
        const TOUCHED: i32 = 20; // 400 columns, far past the capacity

        let store = ChunkStore::with_capacity(CountingSource::full_height(), CAPACITY);
        for cz in 0..TOUCHED {
            for cx in 0..TOUCHED {
                let _ = store.column(cx, cz);
            }
        }

        assert_eq!(
            store.len(),
            CAPACITY,
            "residency must be pinned at the capacity bound, not grow with what was touched"
        );
        assert_eq!(
            store.evicted(),
            (TOUCHED * TOUCHED) as u64 - CAPACITY as u64,
            "every column past the bound must have been evicted exactly once"
        );
        assert_eq!(store.capacity(), CAPACITY);

        // Not an assertion on wall-clock — a recorded measurement, printed
        // with `--nocapture`. The point of recording it is that it is
        // microseconds against the 909 ms it replaces.
        //
        // `touched_column`, not `ChunkColumn::new`: since packed sections an all-air
        // column's clone allocates *nothing* (every section is `Uniform`), so
        // timing a blank column would report the cost of cloning a `Vec` of 24
        // enum discriminants and call it the store's read cost. That is the
        // fixture-premise trap described in `touched_column`'s own doc.
        let column = touched_column(REAL_MIN_Y, REAL_HEIGHT);
        let started = lodestone_time::Instant::now();
        const CLONES: u32 = 200;
        for _ in 0..CLONES {
            std::hint::black_box(column.clone());
        }
        let per_clone = started.elapsed() / CLONES;
        println!(
            "ChunkColumn clone ({REAL_HEIGHT} rows, ~{} KiB): {per_clone:?} each",
            16 * REAL_HEIGHT * 16 * 2 / 1024
        );
    }

    /// A roaming session must converge back to the configured bound rather
    /// than accumulating one cache high-water mark per centre. This uses the
    /// three user-facing render-distance regimes and a small column fixture so
    /// it remains a normal regression, not a release-only memory tool.
    #[test]
    fn roaming_retention_returns_to_the_configured_bound() {
        for view_radius in [9, 17, 33] {
            let capacity = integrated_capacity_for_view_radius(view_radius);
            let store = ChunkStore::for_integrated_view_radius(CountingSource::new(), view_radius);
            let touch_square = |centre: i32| {
                for cz in -view_radius..=view_radius {
                    for cx in -view_radius..=view_radius {
                        let _ = store.column(centre + cx, centre + cz);
                    }
                }
            };

            touch_square(0);
            touch_square(10_000);
            touch_square(0);

            assert_eq!(
                store.len(),
                capacity,
                "view radius {view_radius} retained {} columns after roaming, not its bound {capacity}",
                store.len()
            );
            assert!(
                store.evicted() > 0,
                "view radius {view_radius} never exercised eviction while roaming"
            );
            println!(
                "roamed view_radius={view_radius} capacity={} resident={} evicted={} packed_block_bytes={}",
                capacity,
                store.len(),
                store.evicted(),
                store.retained_blocks_heap_bytes()
            );
        }
    }

    /// The policy at its **boundaries**, which is the part of it a
    /// behavioural gate cannot reach.
    ///
    /// `tests/view_radius_store_capacity.rs` measures the regimes end to end and
    /// is the gate that matters. What it cannot see is where one regime stops and
    /// the next begins, or what happens to a radius no slider can produce — and
    /// those are the three ways this function breaks silently:
    ///
    /// 1. **the floor's last radius and the derivation's first.** The floor
    ///    applies while `view_columns(r) + 50 <= 512`, i.e. `(2r+1)² <= 462`, i.e.
    ///    `r <= 10`. So radius 10 is the last floored radius and 11 the first
    ///    derived one, and 11 is exactly `render_distance` 10 — the policy's
    ///    boundary notch.
    /// 2. **the cap's first radius.** `FULLY_RESIDENT_VIEW_RADIUS` must be the
    ///    largest radius that is *not* capped, or the constant's name and its
    ///    memory table are both lies.
    /// 3. **arithmetic, not policy.** `view_columns` squares its argument and
    ///    `IntegratedServer::bind` is public, so `i32::MAX` must land on the cap
    ///    rather than wrap to a tiny capacity — a failure that would present as a
    ///    thrashing cache, not as an overflow.
    ///
    /// Every expected value below is computed from `view_columns` and the three
    /// constants rather than written out, so a policy change moves the
    /// expectations with it instead of voiding them.
    #[test]
    fn the_capacity_policy_clamps_at_both_ends_and_cannot_overflow() {
        // (1) the floor/derivation seam. Found by search rather than asserted at
        // a hardcoded radius, so the seam is *located* even if the constants move.
        let first_derived = (0..=64)
            .find(|&r| capacity_for_view_radius(r) > DEFAULT_CAPACITY)
            .expect("some radius must exceed the floor, or the derivation is dead code");
        assert_eq!(
            first_derived, 11,
            "the floor should stop applying at view_radius 11 (render_distance 10), \
             the capacity floor stops applying at {first_derived}"
        );
        assert_eq!(capacity_for_view_radius(first_derived - 1), DEFAULT_CAPACITY);
        assert_eq!(
            capacity_for_view_radius(first_derived),
            view_columns(first_derived) + CONCURRENT_SCAN_COLUMNS
        );

        // (2) the cap's seam, and the claim FULLY_RESIDENT_VIEW_RADIUS's name makes.
        assert_eq!(
            capacity_for_view_radius(FULLY_RESIDENT_VIEW_RADIUS),
            view_columns(FULLY_RESIDENT_VIEW_RADIUS) + CONCURRENT_SCAN_COLUMNS,
            "the largest fully-resident radius must not itself be capped"
        );
        assert_eq!(capacity_for_view_radius(FULLY_RESIDENT_VIEW_RADIUS), MAX_CAPACITY);
        assert_eq!(
            capacity_for_view_radius(FULLY_RESIDENT_VIEW_RADIUS + 1),
            MAX_CAPACITY,
            "one radius past it must be capped, or the cap never engages"
        );

        // Monotonic across the whole slider, so no radius is ever served a
        // *smaller* store than a narrower one. `MAX_RENDER_DISTANCE` is 32, hence
        // a maximum served radius of 33.
        for r in 0..=33 {
            assert!(
                capacity_for_view_radius(r) >= capacity_for_view_radius(r - 1),
                "capacity must not shrink as the radius grows: {} at {} vs {} at {}",
                capacity_for_view_radius(r),
                r,
                capacity_for_view_radius(r - 1),
                r - 1
            );
            assert!(
                capacity_for_view_radius(r) >= view_columns(r).min(MAX_CAPACITY),
                "radius {r} must get either its whole view or the cap"
            );
        }

        // (3) arithmetic. A negative radius is 0 columns (matching
        // `join_view_rings`), and both extremes must land on a clamp rather than
        // on a wrap.
        assert_eq!(view_columns(-1), 0);
        assert_eq!(capacity_for_view_radius(-1), DEFAULT_CAPACITY);
        assert_eq!(capacity_for_view_radius(i32::MIN), DEFAULT_CAPACITY);
        assert_eq!(
            capacity_for_view_radius(i32::MAX),
            MAX_CAPACITY,
            "an absurd radius must saturate onto the cap; a wrap here would present \
             as a thrashing cache rather than as an overflow"
        );
    }

    /// The 256-chunk selectable distance must remain a streaming request, not
    /// an instruction to retain its complete 265,225-column square. The
    /// integrated cache grows through the measured bound and then saturates.
    #[test]
    fn extreme_render_distance_keeps_integrated_retention_bounded() {
        let max_render_view_radius = 257;
        assert_eq!(
            view_columns(max_render_view_radius),
            265_225,
            "256 render chunks plus the mesher ring is a 513x513 stream"
        );
        assert_eq!(
            integrated_capacity_for_view_radius(max_render_view_radius),
            MAX_CAPACITY,
            "the integrated cache must not scale allocation with the full extreme view"
        );
    }

    /// Eviction must be least-recently-used, not arbitrary — otherwise a
    /// capacity that comfortably holds the tick area still thrashes it, because
    /// the streamed view pushes hundreds of one-shot columns through the same
    /// store.
    #[test]
    fn eviction_drops_the_least_recently_used_column() {
        let store = ChunkStore::with_capacity(CountingSource::new(), 2);
        let hot = (0, 0);
        let cold = (1, 0);

        let _ = store.column(hot.0, hot.1);
        let _ = store.column(cold.0, cold.1);
        // Touch `hot` again so `cold` is the least recently used.
        let _ = store.column(hot.0, hot.1);
        // A third column must evict `cold`, not `hot`.
        let _ = store.column(2, 0);

        assert_eq!(store.generated(), 3, "three distinct columns, three generations");
        // Re-reading `hot` must be free; re-reading `cold` must not.
        let before = store.generated();
        let _ = store.column(hot.0, hot.1);
        assert_eq!(
            store.generated(),
            before,
            "the most recently used column was evicted — eviction is not LRU"
        );
        let _ = store.column(cold.0, cold.1);
        assert_eq!(
            store.generated(),
            before + 1,
            "the least recently used column should have been the one evicted"
        );
    }

    /// The store must not change what the world *contains*, only how often it
    /// is computed. Without this, a store that returned blank or stale columns
    /// would pass every count above.
    #[test]
    fn retention_does_not_change_the_blocks() {
        let coords = [(0, 0), (1, -2), (-3, 5)];
        let probes = [(0, -60, 0), (7, 4, 9), (15, 70, 15)];

        // Independently constructed sources per arm, per this crate's own
        // determinism-test reasoning: the generator's memo cache would
        // otherwise make one arm a replay of the other.
        let bare = crate::overworld_chunk_source(7);
        let store = ChunkStore::new(crate::overworld_chunk_source(7));

        for &(cx, cz) in &coords {
            let bare_column = bare.column(cx, cz);
            // Read each column twice through the store: once a miss, once a
            // hit, so a hit that served something different would show up.
            for pass in 0..2 {
                let stored = store.column(cx, cz);
                for &(lx, y, lz) in &probes {
                    assert_eq!(
                        stored.block_state_id(lx, y, lz),
                        bare_column.block_state_id(lx, y, lz),
                        "pass {pass}: retained column ({cx}, {cz}) diverged at ({lx}, {y}, {lz})"
                    );
                }
                assert_eq!(
                    store.block_state_id(cx * 16 + probes[0].0, probes[0].1, cz * 16 + probes[0].2),
                    bare_column.block_state_id(probes[0].0, probes[0].1, probes[0].2),
                    "pass {pass}: the `block_state` override diverged from `column()`"
                );
            }
        }
    }

    /// A full-height column shaped like real terrain, so its **representation
    /// cost** is the one production pays.
    ///
    /// The fixture fills every section with varied terrain states so its packed
    /// representation is governed by palette width and uniform air sections,
    /// rather than by page-fault behavior of a sparse toy column. A two-state
    /// palette would understate the production cost, so the state variety below
    /// is intentional.
    ///
    /// # What it models now
    ///
    /// Terrain below a surface, air above it, with the block-state variety a real
    /// column has — which is what decides both savings: how many sections collapse
    /// to uniform air, and how wide the rest have to pack. The states are real
    /// generator ids from the surface/stone rules rather than
    /// `set_solid`'s stone-or-air pair, because a two-state palette is precisely
    /// the input that would flatter the packing.
    ///
    /// It remains a *model*, but a **calibrated** one rather than a plausible
    /// one. Four generated columns measured
    /// `ChunkColumn::blocks_heap_bytes` at 22,640 / 23,328 / 23,728 / 26,752
    /// bytes (mean **24,112**, against the flat grid's 196,608). This fixture is
    /// shaped to land on that: 12 states plus air is 4 bits, and a surface at
    /// `min_y + height / 2` leaves 12 packed sections of `4096 × 4 / 8` = 2,048
    /// bytes plus 12 uniform air sections, i.e. **24,576 bytes** — within 2% of
    /// the real mean. Getting that agreement is the point; a fixture that
    /// understated it would make every RSS row below optimistic.
    fn touched_column(min_y: i32, height: i32) -> ChunkColumn {
        // Real ids the generator emits, so the palette width the
        // sections pack to is the production one. 12 states + air = 13 => 4 bits
        // for a section drawing from all of them, and the surface band draws
        // from more of them than the deep band, exactly as real terrain does.
        let states = [
            Block::Stone.default_state(),
            Block::Deepslate.default_state(),
            Block::Dirt.default_state(),
            Block::Gravel.default_state(),
            Block::Andesite.default_state(),
            Block::Diorite.default_state(),
            Block::Granite.default_state(),
            Block::GrassBlock.default_state(),
            Block::Water.default_state(),
            Block::CoalOre.default_state(),
            Block::IronOre.default_state(),
            Block::Sand.default_state(),
        ];
        // Surface at the midpoint: 12 packed sections and 12 uniform air ones,
        // which is the split that reproduces the measured real figure (see the
        // doc comment — this constant is calibrated, not chosen).
        let surface = min_y + height / 2;
        let mut column = ChunkColumn::new(min_y, height);
        for y in min_y..surface {
            for z in 0..16 {
                for x in 0..16 {
                    // Deterministic but content-varying, so no section is
                    // accidentally uniform. Cheap: this runs 512 to 1,275 times.
                    let n = (x as usize)
                        .wrapping_mul(31)
                        .wrapping_add((z as usize).wrapping_mul(7))
                        .wrapping_add((y - min_y) as usize);
                    let state = states[n % states.len()];
                    column.set_block_id(x, y, z, state);
                }
            }
        }
        column
    }

    struct TouchedSource;
    impl ChunkSource for TouchedSource {
        fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
            touched_column(REAL_MIN_Y, REAL_HEIGHT)
        }

        fn block_state_id(&self, x: i32, y: i32, z: i32) -> lodestone_data::block_states::StateId {
            // Only `column()` is exercised here (the RSS measurement); this is
            // the plain column-regenerating form, kept for completeness.
            let cx = x.div_euclid(16);
            let cz = z.div_euclid(16);
            let lx = x.rem_euclid(16);
            let lz = z.rem_euclid(16);
            self.column(cx, cz).block_state_id(lx, y, lz)
        }

        fn biome_state_at(&self, x: i32, y: i32, z: i32) -> String {
            // Only `column()` is exercised here (the RSS measurement); this is
            // the plain column-regenerating form, kept for completeness.
            let cx = x.div_euclid(16);
            let cz = z.div_euclid(16);
            let lx = x.rem_euclid(16);
            let lz = z.rem_euclid(16);
            self.column(cx, cz).biome_state_at(lx, y, lz).to_string()
        }

        // A memory-measurement fixture; nothing here writes blocks. Explicitly
        // discards rather than inheriting a silent default.
        fn set_block(&self, _x: i32, _y: i32, _z: i32, _state: lodestone_data::block_states::StateId) {
            // No storage; edits are discarded by design for this fixture.
        }
    }

    /// Reuses one calibrated full-height column while still returning an owned
    /// clone per cache admission. This keeps the roaming RSS measurement about
    /// retained columns rather than repeatedly rebuilding the fixture itself.
    struct SharedTouchedSource(Arc<ChunkColumn>);

    impl ChunkSource for SharedTouchedSource {
        fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
            self.0.as_ref().clone()
        }

        fn block_state_id(&self, x: i32, y: i32, z: i32) -> lodestone_data::block_states::StateId {
            self.0
                .block_state_id(x.rem_euclid(16), y, z.rem_euclid(16))
        }

        fn biome_state_at(&self, x: i32, y: i32, z: i32) -> String {
            self.0
                .biome_state_at(x.rem_euclid(16), y, z.rem_euclid(16))
                .to_owned()
        }

        fn set_block(&self, _x: i32, _y: i32, _z: i32, _state: lodestone_data::block_states::StateId) {}
    }

    /// Fills a store to `capacity` and holds it, so an external
    /// `/usr/bin/time -l` reading attributes the peak RSS to retention.
    fn fill_and_hold(capacity: usize, touch: usize) -> ChunkStore<TouchedSource> {
        let store = ChunkStore::with_capacity(TouchedSource, capacity);
        for i in 0..touch as i32 {
            let _ = store.column(i % 64, i / 64);
        }
        store
    }

    /// **Retained arm** of the RSS measurement. `#[ignore]`d: a measurement
    /// tool, not an assertion, and only meaningful in `--release` under
    /// `/usr/bin/time -l`.
    ///
    /// Run both arms and subtract. Per `docs/plans/chunk-lifecycle.md`'s U2,
    /// **the pair is its own control**: if the delta is ≈0 the measurement is
    /// broken (columns dropped in both arms, or pages not faulted in — see
    /// [`touched_column`]), and the run must be treated as a failure to measure
    /// rather than as "residency is free".
    ///
    /// ```text
    /// cargo test --release -p lodestone-server --lib -- --ignored --nocapture \
    ///     --exact chunk_store::tests::measure_rss_with_retention
    /// ```
    #[test]
    #[ignore = "measurement tool; run in --release under /usr/bin/time -l"]
    fn measure_rss_with_retention() {
        let store = fill_and_hold(DEFAULT_CAPACITY, DEFAULT_CAPACITY);
        assert_eq!(store.len(), DEFAULT_CAPACITY);
        println!(
            "retained {} columns of {} rows (~{} KiB each); arithmetic ceiling {} MiB",
            store.len(),
            REAL_HEIGHT,
            16 * REAL_HEIGHT * 16 * 2 / 1024,
            (DEFAULT_CAPACITY as i32 * 16 * REAL_HEIGHT * 16 * 2) / (1024 * 1024)
        );
        std::hint::black_box(&store);
    }

    /// **Dropped arm** — identical work, retention disabled. The difference
    /// between this arm's peak RSS and the one above is the store's real cost.
    #[test]
    #[ignore = "measurement tool; run in --release under /usr/bin/time -l"]
    fn measure_rss_without_retention() {
        let store = fill_and_hold(0, DEFAULT_CAPACITY);
        assert_eq!(store.len(), 0);
        println!("retained 0 columns after touching {DEFAULT_CAPACITY}");
        std::hint::black_box(&store);
    }

    /// **Retained arm at the cap** — what the ceiling actually costs,
    /// measured rather than extrapolated.
    ///
    /// [`FULLY_RESIDENT_VIEW_RADIUS`]'s memory table is arithmetic off the 31.1
    /// KiB per column the pair above measured at 512, and a 2.5× extrapolation of
    /// a measured rate is still an extrapolation — the map's own growth, its
    /// rehashing and the allocator's fragmentation are all superlinear in
    /// principle. This arm reads the absolute figure at [`MAX_CAPACITY`], the one
    /// number the whole cap decision rests on.
    ///
    /// Its control is `measure_rss_without_retention` exactly as the 512 pair's
    /// is: subtract, and treat a delta near zero as a failure to measure rather
    /// than as free residency.
    ///
    /// ```text
    /// /usr/bin/time -l cargo test --release -p lodestone-server --lib -- --ignored \
    ///     --nocapture --exact chunk_store::tests::measure_rss_at_the_capacity_cap
    /// ```
    #[test]
    #[ignore = "measurement tool; run in --release under /usr/bin/time -l"]
    fn measure_rss_at_the_capacity_cap() {
        let store = fill_and_hold(MAX_CAPACITY, MAX_CAPACITY);
        assert_eq!(store.len(), MAX_CAPACITY);
        println!(
            "retained {} columns of {} rows (~{} KiB each) — the ceiling \
             capacity_for_view_radius saturates at, i.e. view_radius \
             {FULLY_RESIDENT_VIEW_RADIUS} (render_distance {}) and every radius above it; \
             arithmetic ceiling {} MiB",
            store.len(),
            REAL_HEIGHT,
            16 * REAL_HEIGHT * 16 * 2 / 1024,
            FULLY_RESIDENT_VIEW_RADIUS - 1,
            (MAX_CAPACITY as i32 * 16 * REAL_HEIGHT * 16 * 2) / (1024 * 1024)
        );
        std::hint::black_box(&store);
    }

    /// **The owner's own scenario**, measured rather than extrapolated: the
    /// singleplayer store at `render_distance` 32, the slider's maximum.
    ///
    /// This is the row every table in this module extrapolates to, and it was the
    /// only figure in them that had never been read directly — 4,539 columns is a
    /// 3.6× extrapolation of the rate measured at 1,275, and the whole
    /// motivation was the owner asking about RSS at high render distance. It is
    /// affordable precisely *because* of packing: at the unpacked rate this arm
    /// would have needed 867 MiB on a 16 GB box shared with other work.
    ///
    /// ```text
    /// cargo test --release -p lodestone-server --lib -- --ignored --nocapture \
    ///     --exact chunk_store::tests::measure_rss_at_the_singleplayer_slider_maximum
    /// ```
    #[test]
    #[ignore = "measurement tool; run in --release under /usr/bin/time -l"]
    fn measure_rss_at_the_singleplayer_slider_maximum() {
        // Derived from the policy, not restated: `render_distance` 32 is
        // `view_radius` 33 (the shell serves `render_distance + 1`).
        const SLIDER_MAX_VIEW_RADIUS: i32 = 33;
        let capacity = integrated_capacity_for_view_radius(SLIDER_MAX_VIEW_RADIUS);
        let store = fill_and_hold(capacity, capacity);
        assert_eq!(store.len(), capacity);
        println!(
            "retained {} columns of {REAL_HEIGHT} rows — the singleplayer store at \
             render_distance {} (view_radius {SLIDER_MAX_VIEW_RADIUS}, view \
             {} columns); pre-#551 arithmetic for the same set was {} MiB",
            store.len(),
            SLIDER_MAX_VIEW_RADIUS - 1,
            view_columns(SLIDER_MAX_VIEW_RADIUS),
            (capacity as i32 / 1024) * 16 * REAL_HEIGHT * 16 * 2 / 1024,
        );
        std::hint::black_box(&store);
    }

    /// Measures the retained packed block bytes after a view roams to a distant
    /// centre and returns. The three radii are run in one process so
    /// `/usr/bin/time -l` reports the peak RSS while the printed counters prove
    /// that the final resident set is still exactly the configured bound.
    ///
    /// ```text
    /// /usr/bin/time -l cargo test --release -p lodestone-server --lib -- --ignored \
    ///     --nocapture --exact chunk_store::tests::measure_rss_after_roaming
    /// ```
    /// Set `LODESTONE_RETENTION_RADIUS` to one of `9`, `17`, or `33` to run a
    /// single regime in a fresh process and make the external RSS value
    /// comparable without allocator high-water reuse from the other regimes.
    #[test]
    #[ignore = "measurement tool; run in --release under /usr/bin/time -l"]
    fn measure_rss_after_roaming() {
        let source = Arc::new(touched_column(REAL_MIN_Y, REAL_HEIGHT));
        let radii = std::env::var("LODESTONE_RETENTION_RADIUS")
            .ok()
            .and_then(|value| value.parse::<i32>().ok())
            .map_or_else(|| vec![9, 17, 33], |radius| vec![radius]);
        for view_radius in radii {
            let capacity = integrated_capacity_for_view_radius(view_radius);
            let store = ChunkStore::for_integrated_view_radius(
                SharedTouchedSource(Arc::clone(&source)),
                view_radius,
            );
            let touch_square = |centre: i32| {
                for cz in -view_radius..=view_radius {
                    for cx in -view_radius..=view_radius {
                        let _ = store.column(centre + cx, centre + cz);
                    }
                }
            };
            touch_square(0);
            touch_square(10_000);
            touch_square(0);
            assert_eq!(store.len(), capacity);
            println!(
                "rss_roamed view_radius={view_radius} capacity={} resident={} evicted={} packed_block_bytes={}",
                capacity,
                store.len(),
                store.evicted(),
                store.retained_blocks_heap_bytes()
            );
            std::hint::black_box(&store);
        }
    }

    /// The premise everything else rests on: what a **real** composed column
    /// costs in release.
    ///
    /// A fresh, independently constructed source per column, because
    /// The production source's 512-entry memo cache would otherwise turn every
    /// column after the first into a cache hit and report a per-column cost
    /// near zero — the same trap that made `crate::chunk`'s determinism test
    /// vacuous.
    ///
    /// Reports, never asserts: a duration on a shared box is a sample, not a
    /// measurement (a 2.3× spread was measured on an identical release binary
    /// from load alone), so a threshold here would be a flake generator. The
    /// *count* gates above are what protect the bound.
    #[test]
    #[ignore = "measurement tool; run in --release, and only on a quiet machine"]
    fn measure_real_column_generation_cost() {
        const COLUMNS: usize = 4;
        let mut total = std::time::Duration::ZERO;
        for i in 0..COLUMNS as i32 {
            let source = crate::overworld_chunk_source(42);
            let started = lodestone_time::Instant::now();
            let column = source.column(i * 37, i * 53);
            let elapsed = started.elapsed();
            std::hint::black_box(&column);
            println!("column {i}: {elapsed:?}");
            total += elapsed;
        }
        println!(
            "mean over {COLUMNS} cold columns: {:?} — compare the 50 ms tick budget, and \
             multiply by the 49-column tick area",
            total / COLUMNS as u32
        );
    }

    /// A miss must not hold the cache lock, or `generate_columns_parallel`'s
    /// worker-pool batch is serialised behind it and parallel generation is lost.
    ///
    /// # Predicting the value, not the sign
    ///
    /// Eight columns at 60 ms each through the store: if generation runs with
    /// the lock released the burst takes about `8 / workers × 60 ms` — under
    /// 240 ms at any `available_parallelism ≥ 2`. If the lock is held it takes
    /// **≥ 480 ms** (8 × 60 ms, fully serial). The gate asserts under 400 ms,
    /// which sits between the two hypotheses rather than merely below the
    /// serial one.
    ///
    /// This is the one gate here that reads a duration, because "does a lock
    /// serialise this" has no count. It is bracketed to a single burst and the
    /// two hypotheses are 2× apart, so the load spread that makes durations
    /// untrustworthy would have to exceed 2× to flip it. Skipped rather than
    /// failed on a single-core box, where the question is meaningless.
    #[test]
    fn a_miss_does_not_hold_the_lock_across_generation() {
        struct SleepySource {
            per_column: std::time::Duration,
        }
        impl ChunkSource for SleepySource {
            fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
                std::thread::sleep(self.per_column);
                ChunkColumn::new(0, 16)
            }

            fn block_state_id(&self, x: i32, y: i32, z: i32) -> lodestone_data::block_states::StateId {
                // Only `column()` is exercised here (the lock-serialisation
                // gate); the plain column-regenerating form, for completeness.
                let cx = x.div_euclid(16);
                let cz = z.div_euclid(16);
                let lx = x.rem_euclid(16);
                let lz = z.rem_euclid(16);
                self.column(cx, cz).block_state_id(lx, y, lz)
            }

            fn biome_state_at(&self, x: i32, y: i32, z: i32) -> String {
                // Only `column()` is exercised here (the lock-serialisation
                // gate); the plain column-regenerating form, for completeness.
                let cx = x.div_euclid(16);
                let cz = z.div_euclid(16);
                let lx = x.rem_euclid(16);
                let lz = z.rem_euclid(16);
                self.column(cx, cz).biome_state_at(lx, y, lz).to_string()
            }

            // A wall-clock-only fixture; nothing here writes blocks. Explicitly
            // discards rather than inheriting a silent default.
            fn set_block(&self, _x: i32, _y: i32, _z: i32, _state: lodestone_data::block_states::StateId) {
                // No storage; edits are discarded by design for this fixture.
            }
        }

        let workers = std::thread::available_parallelism()
            .map(std::num::NonZero::get)
            .unwrap_or(1);
        if workers < 2 {
            println!("skipping: single-core box, parallelism is not observable");
            return;
        }

        let per_column = std::time::Duration::from_millis(60);
        let store = ChunkStore::new(SleepySource { per_column });
        let coords: Vec<(i32, i32)> = (0..8).map(|i| (i, 0)).collect();

        let started = lodestone_time::Instant::now();
        std::thread::scope(|scope| {
            for chunk in coords.chunks(8 / workers.min(8).max(1)) {
                let store = &store;
                scope.spawn(move || {
                    for &(cx, cz) in chunk {
                        let _ = store.column(cx, cz);
                    }
                });
            }
        });
        let elapsed = started.elapsed();

        assert!(
            elapsed < per_column * 8 * 2 / 3,
            "8 misses at {per_column:?} each took {elapsed:?}; fully serial would be \
             {:?}. The cache lock is being held across `source.column()`, which serialises \
             `generate_columns_parallel`'s fan-out.",
            per_column * 8
        );
        assert_eq!(store.generated(), 8, "each distinct column generated once");
    }

    /// A mutation invalidates snapshots for every relative centre whose light
    /// footprint could have read that column, while leaving unrelated snapshots
    /// available for reuse.
    #[test]
    fn neighbouring_block_mutation_invalidates_only_the_light_footprint() {
        let store = ChunkStore::with_capacity(CountingSource::new(), 32);
        let mut footprint = vec![(0, 0)];
        footprint.extend(
            RETAINED_LIGHT_NEIGHBOUR_OFFSETS
                .iter()
                .map(|&(dx, dz)| (dx, dz)),
        );
        for &(cx, cz) in &footprint {
            let mut column = store.column(cx, cz);
            let mut light = lodestone_world::ColumnLight::new(column.section_count());
            *light.sky_mut(0) = lodestone_world::LightData::Uniform(1);
            column.set_retained_light(light);
            assert!(store.store_resident_column(cx, cz, &column));
        }

        // The edited block is in the relative centre. Every retained centre
        // in the 3x3 footprint must now be recomputed, while a centre two
        // chunks away is outside the dependency radius.
        store.set_block(1, 0, 1, Block::GoldBlock.default_state());
        for &(cx, cz) in &footprint {
            assert_eq!(
                store
                    .resident_column(cx, cz)
                    .expect("the retained footprint remains resident")
                    .retained_light(),
                None,
                "retained light at relative centre ({cx},{cz}) must be invalidated"
            );
        }
        let mut unrelated = store.column(2, 0);
        let mut unrelated_light = lodestone_world::ColumnLight::new(unrelated.section_count());
        *unrelated_light.sky_mut(0) = lodestone_world::LightData::Uniform(9);
        unrelated.set_retained_light(unrelated_light.clone());
        assert!(store.store_resident_column(2, 0, &unrelated));
        store.set_block(1, 0, 1, Block::IronBlock.default_state());
        assert_eq!(
            store
                .resident_column(2, 0)
                .expect("the unrelated centre remains resident")
                .retained_light(),
            Some(&unrelated_light)
        );
    }

    /// The settlement centre is a distinct entry from its dependency
    /// snapshots. The diagnostic iterator must expose only the latter and
    /// preserve their relative offsets and values without cloning them.
    #[test]
    fn settlement_dependency_iteration_excludes_centre() {
        let mut centre = lodestone_world::ColumnLight::new(0);
        *centre.sky_mut(0) = lodestone_world::LightData::Uniform(3);
        let mut west = lodestone_world::ColumnLight::new(0);
        *west.block_mut(0) = lodestone_world::LightData::Uniform(7);
        let mut south = lodestone_world::ColumnLight::new(0);
        *south.sky_mut(1) = lodestone_world::LightData::Uniform(11);

        let settlement = ColumnLightSettlement::with_neighbours(
            centre.clone(),
            [(-1, 0, west.clone()), (0, 1, south.clone())],
        )
        .expect("the two dependency offsets are distinct and in the 3x3 footprint");

        assert_eq!(settlement.centre_light(), &centre);
        let dependencies = settlement.dependency_lights().collect::<Vec<_>>();
        assert_eq!(dependencies.len(), 2);
        assert_eq!(dependencies[0], ((-1, 0), &west));
        assert_eq!(dependencies[1], ((0, 1), &south));
        assert!(dependencies
            .iter()
            .all(|(offset, _)| *offset != (0, 0)));
    }

    /// An allocated-zero dependency is storage, not a settled centre result.
    /// The next admission at that coordinate must still run its solver and may
    /// replace the zero layer with populated light.
    #[test]
    fn allocated_zero_dependency_can_become_populated_on_later_admission() {
        let offsets = vec![
            (-1, -1),
            (0, -1),
            (1, -1),
            (-1, 0),
            (1, 0),
            (-1, 1),
            (0, 1),
            (1, 1),
        ];
        let store = ChunkStore::with_capacity(CountingSource::new(), 32);
        let centre = store.column(0, 0);
        let mut centre_light = lodestone_world::ColumnLight::new(centre.section_count());
        *centre_light.sky_mut(0) = lodestone_world::LightData::Uniform(4);
        let mut zero_dependency = lodestone_world::ColumnLight::new(centre.section_count());
        for section in 0..zero_dependency.light_section_count() {
            *zero_dependency.sky_mut(section) = lodestone_world::LightData::Uniform(0);
            *zero_dependency.block_mut(section) = lodestone_world::LightData::Uniform(0);
        }
        let zero_for_first = zero_dependency.clone();
        let mut first_compute = |_: &ChunkColumn, _: &[(i32, i32, &ChunkColumn)]| {
            ColumnLightSettlement::with_neighbours(
                centre_light.clone(),
                [(1, 0, zero_for_first.clone())],
            )
        };
        store
            .settle_resident_column_lights_with_neighbours(
                0,
                0,
                &centre,
                &offsets,
                false,
                false,
                true,
                &mut first_compute,
            )
            .expect("the first footprint admission must retain its dependency");
        let retained_zero = store
            .resident_column(1, 0)
            .expect("the east dependency remains available")
            .retained_light()
            .cloned()
            .expect("the dependency layer was allocated");
        assert!(!retained_zero.has_nonzero_values());

        let east = store
            .resident_column(1, 0)
            .expect("the allocated dependency is resident");
        let mut populated_compute_calls = 0;
        let mut populated_compute = |column: &ChunkColumn, _: &[(i32, i32, &ChunkColumn)]| {
            populated_compute_calls += 1;
            let mut populated = lodestone_world::ColumnLight::new(column.section_count());
            *populated.sky_mut(0) = lodestone_world::LightData::Uniform(9);
            Some(ColumnLightSettlement::centre(populated))
        };
        let settled = store
            .settle_resident_column_lights_with_neighbours(
                1,
                0,
                &east,
                &offsets,
                false,
                false,
                true,
                &mut populated_compute,
            )
            .expect("the later centre admission must replace allocated zero");
        assert_eq!(populated_compute_calls, 1);
        assert!(settled
            .retained_light()
            .is_some_and(lodestone_world::ColumnLight::has_nonzero_values));
    }

    /// A later footprint can observe a centre as a dependency, but that read
    /// must not replace the centre's settled snapshot with its own dependency
    /// representation. Mutations clear the settled status before recompute;
    /// this control isolates the admission-order case from invalidation.
    #[test]
    fn later_dependency_admission_preserves_settled_centre_snapshot() {
        let store = ChunkStore::with_capacity(CountingSource::new(), 16);
        let target = store.column(0, 0);
        let mut expected = lodestone_world::ColumnLight::new(target.section_count() + 2);
        for section in 1..=4 {
            *expected.sky_mut(section) = lodestone_world::LightData::Uniform(15);
            *expected.block_mut(section) = lodestone_world::LightData::Uniform(0);
        }
        let mut first_compute = |_: &ChunkColumn, _: &[(i32, i32, &ChunkColumn)]| {
            Some(ColumnLightSettlement::centre(expected.clone()))
        };
        store
            .settle_resident_column_lights_with_neighbours(
                0,
                0,
                &target,
                &[(1, 0)],
                false,
                false,
                true,
                &mut first_compute,
            )
            .expect("centre admission");

        let east = store.column(1, 0);
        let empty = lodestone_world::ColumnLight::new(east.section_count());
        let mut second_compute = |_: &ChunkColumn, _: &[(i32, i32, &ChunkColumn)]| {
            ColumnLightSettlement::with_neighbours(empty.clone(), [(-1, 0, empty.clone())])
        };
        store
            .settle_resident_column_lights_with_neighbours(
                1,
                0,
                &east,
                &[(-1, 0)],
                false,
                false,
                true,
                &mut second_compute,
            )
            .expect("dependency admission");

        let retained = store
            .resident_column(0, 0)
            .expect("settled centre remains resident");
        assert_eq!(retained.retained_light(), Some(&expected));
        assert_eq!(
            retained.retained_light_status(),
            Some(crate::chunk::RetainedLightStatus::CentreSettled)
        );
    }

    /// A dependency can already contain populated light and still be only an
    /// initialized holder. It must run its own centre admission before the
    /// fast path becomes eligible; the second call proves the settled status
    /// then skips a redundant computation.
    #[test]
    fn populated_dependency_runs_centre_admission_before_fast_path() {
        let offsets = [(1, 0)];
        let store = ChunkStore::with_capacity(CountingSource::new(), 16);
        let centre = store.column(0, 0);
        let mut dependency_light = lodestone_world::ColumnLight::new(centre.section_count());
        *dependency_light.sky_mut(0) = lodestone_world::LightData::Uniform(11);
        let mut first_compute =
            |_column: &ChunkColumn, _neighbours: &[(i32, i32, &ChunkColumn)]| {
                ColumnLightSettlement::with_neighbours(
                    lodestone_world::ColumnLight::new(centre.section_count()),
                    [(1, 0, dependency_light.clone())],
                )
            };
        store
            .settle_resident_column_lights_with_neighbours(
                0,
                0,
                &centre,
                &offsets,
                false,
                false,
                true,
                &mut first_compute,
            )
            .expect("initial dependency admission");
        let dependency = store
            .resident_column(1, 0)
            .expect("dependency remains resident");
        assert_eq!(
            dependency.retained_light_status(),
            Some(crate::chunk::RetainedLightStatus::DependencyInitialized)
        );
        let mut centre_calls = 0;
        let mut centre_compute =
            |column: &ChunkColumn, _neighbours: &[(i32, i32, &ChunkColumn)]| {
                centre_calls += 1;
                Some(ColumnLightSettlement::centre(
                    lodestone_world::ColumnLight::new(column.section_count()),
                ))
            };
        let settled = store
            .settle_resident_column_lights_with_neighbours(
                1,
                0,
                &dependency,
                &offsets,
                false,
                false,
                true,
                &mut centre_compute,
            )
            .expect("populated dependency must run its centre admission");
        assert_eq!(centre_calls, 1);
        assert_eq!(
            settled.retained_light_status(),
            Some(crate::chunk::RetainedLightStatus::CentreSettled)
        );
        let mut skipped_calls = 0;
        let mut skipped_compute =
            |_column: &ChunkColumn, _neighbours: &[(i32, i32, &ChunkColumn)]| {
                skipped_calls += 1;
                Some(ColumnLightSettlement::centre(
                    lodestone_world::ColumnLight::new(centre.section_count()),
                ))
            };
        store
            .settle_resident_column_lights_with_neighbours(
                1,
                0,
                &settled,
                &offsets,
                false,
                false,
                true,
                &mut skipped_compute,
            )
            .expect("centre-settled snapshot must use the fast path");
        assert_eq!(skipped_calls, 0);
    }

    /// Reusing a centre-settled snapshot as a neighbour must not downgrade its
    /// lifecycle stage. A neighbouring batch may return the retained value for
    /// that coordinate, but the coordinate has already completed its own
    /// centre admission and remains eligible for the fast path.
    #[test]
    fn batch_admission_preserves_settled_dependency_status() {
        let store = ChunkStore::with_capacity(CountingSource::new(), 16);
        let centre = store.column(0, 0);
        let mut first_compute = |column: &ChunkColumn,
                                 _neighbours: &[(i32, i32, &ChunkColumn)]| {
            let mut centre_light = lodestone_world::ColumnLight::new(column.section_count());
            *centre_light.sky_mut(0) = lodestone_world::LightData::Uniform(4);
            let mut dependency_light = lodestone_world::ColumnLight::new(column.section_count());
            *dependency_light.sky_mut(0) = lodestone_world::LightData::Uniform(9);
            ColumnLightSettlement::with_neighbours(
                centre_light,
                [(1, 0, dependency_light)],
            )
        };
        store
            .settle_resident_column_lights_with_neighbours(
                0,
                0,
                &centre,
                &[(1, 0)],
                false,
                false,
                true,
                &mut first_compute,
            )
            .expect("initial batch admission");

        let dependency = store
            .resident_column(1, 0)
            .expect("the dependency remains resident");
        let mut dependency_compute = |column: &ChunkColumn,
                                      _neighbours: &[(i32, i32, &ChunkColumn)]| {
            Some(ColumnLightSettlement::centre(
                lodestone_world::ColumnLight::new(column.section_count()),
            ))
        };
        store
            .settle_resident_column_lights_with_neighbours(
                1,
                0,
                &dependency,
                &[(-1, 0)],
                false,
                false,
                true,
                &mut dependency_compute,
            )
            .expect("dependency centre admission");
        assert_eq!(
            store
                .resident_column(1, 0)
                .expect("settled dependency remains resident")
                .retained_light_status(),
            Some(crate::chunk::RetainedLightStatus::CentreSettled)
        );

        let settled_dependency = store
            .resident_column(1, 0)
            .expect("settled dependency snapshot");
        let dependency_light = settled_dependency
            .retained_light()
            .cloned()
            .expect("settled dependency retains its light");
        let mut neighbouring_compute = |column: &ChunkColumn,
                                        _neighbours: &[(i32, i32, &ChunkColumn)]| {
            ColumnLightSettlement::with_neighbours(
                lodestone_world::ColumnLight::new(column.section_count()),
                [(1, 0, dependency_light.clone())],
            )
        };
        store
            .settle_resident_column_lights_with_neighbours(
                0,
                0,
                &centre,
                &[(1, 0)],
                false,
                true,
                true,
                &mut neighbouring_compute,
            )
            .expect("forced neighbouring batch admission");
        assert_eq!(
            store
                .resident_column(1, 0)
                .expect("reused dependency remains resident")
                .retained_light_status(),
            Some(crate::chunk::RetainedLightStatus::CentreSettled),
            "a reused settled dependency must not be downgraded to initialized-only"
        );
    }

    /// A retained light refresh and a block mutation for one coordinate must
    /// share one ordering across the cache and its wrapped source. The source
    /// deliberately stalls the old refresh after it has entered the source
    /// callback; the newer `set_block` attempts to pass while that callback is
    /// stalled, and therefore proves the per-coordinate gate rather than
    /// relying on scheduler luck.
    #[test]
    fn same_coordinate_light_refresh_cannot_overwrite_a_newer_block_mutation() {
        struct RaceSource {
            persisted: Arc<Mutex<ChunkColumn>>,
            old_started: Arc<AtomicBool>,
            release_old: Arc<(Mutex<bool>, Condvar)>,
            store_calls: Arc<AtomicUsize>,
        }

        impl RaceSource {
            fn new() -> Self {
                Self {
                    persisted: Arc::new(Mutex::new(ChunkColumn::new(0, 16))),
                    old_started: Arc::new(AtomicBool::new(false)),
                    release_old: Arc::new((Mutex::new(false), Condvar::new())),
                    store_calls: Arc::new(AtomicUsize::new(0)),
                }
            }
        }

        impl ChunkSource for RaceSource {
            fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
                self.persisted
                    .lock()
                    .expect("race source column lock poisoned")
                    .clone()
            }

            fn block_state_id(&self, x: i32, y: i32, z: i32) -> lodestone_data::block_states::StateId {
                self.column(x.div_euclid(16), z.div_euclid(16))
                    .block_state_id(x.rem_euclid(16), y, z.rem_euclid(16))
            }

            fn biome_state_at(&self, x: i32, y: i32, z: i32) -> String {
                self.column(x.div_euclid(16), z.div_euclid(16))
                    .biome_state_at(x.rem_euclid(16), y, z.rem_euclid(16))
                    .to_owned()
            }

            fn set_block(&self, _x: i32, _y: i32, _z: i32, _state: lodestone_data::block_states::StateId) {}

            fn store_resident_column(
                &self,
                _cx: i32,
                _cz: i32,
                column: &ChunkColumn,
            ) -> bool {
                let is_old_refresh = column.retained_light().is_some_and(|light| {
                    *light.sky(0) == lodestone_world::LightData::Uniform(1)
                });
                if is_old_refresh {
                    self.old_started.store(true, Ordering::Release);
                    let (released, wake) = &*self.release_old;
                    let mut released = released
                        .lock()
                        .expect("race source release lock poisoned");
                    while !*released {
                        released = wake
                            .wait(released)
                            .expect("race source release lock poisoned");
                    }
                }
                self.store_calls.fetch_add(1, Ordering::AcqRel);
                *self
                    .persisted
                    .lock()
                    .expect("race source persistence lock poisoned") = column.clone();
                true
            }
        }

        let source = RaceSource::new();
        let persisted = Arc::clone(&source.persisted);
        let old_started = Arc::clone(&source.old_started);
        let release_old = Arc::clone(&source.release_old);
        let store_calls = Arc::clone(&source.store_calls);
        let store = Arc::new(ChunkStore::new(source));
        let _ = store.column(0, 0);

        let mut old = store
            .resident_column(0, 0)
            .expect("the old refresh starts from a resident column");
        let mut old_light = lodestone_world::ColumnLight::new(old.section_count());
        *old_light.sky_mut(0) = lodestone_world::LightData::Uniform(1);
        old.set_retained_light(old_light.clone());

        let mutation_attempted = Arc::new(AtomicBool::new(false));
        let mutation_finished = Arc::new(AtomicBool::new(false));
        std::thread::scope(|scope| {
            let old_store = Arc::clone(&store);
            scope.spawn(move || {
                assert!(old_store.store_resident_column(0, 0, &old));
            });

            while !old_started.load(Ordering::Acquire) {
                std::thread::yield_now();
            }

            let mutation_store = Arc::clone(&store);
            let mutation_attempted_for_thread = Arc::clone(&mutation_attempted);
            let mutation_finished_for_thread = Arc::clone(&mutation_finished);
            scope.spawn(move || {
                mutation_attempted_for_thread.store(true, Ordering::Release);
                mutation_store.set_block(1, 0, 1, Block::GoldBlock.default_state());
                mutation_finished_for_thread.store(true, Ordering::Release);
            });

            while !mutation_attempted.load(Ordering::Acquire) {
                std::thread::yield_now();
            }
            assert_eq!(
                store_calls.load(Ordering::Acquire),
                0,
                "the newer mutation must not reach the source while the old refresh owns the gate"
            );
            assert!(
                !mutation_finished.load(Ordering::Acquire),
                "the newer mutation passed the old refresh's coordinate gate"
            );
            assert_eq!(
                store
                    .resident_column(0, 0)
                    .expect("the old snapshot remains in cache while its source write stalls")
                    .retained_light(),
                Some(&old_light)
            );

            let (released, wake) = &*release_old;
            *released.lock().expect("race source release lock poisoned") = true;
            wake.notify_one();
        });

        assert!(mutation_finished.load(Ordering::Acquire));
        let after_mutation = persisted
            .lock()
            .expect("race source persistence lock poisoned")
            .clone();
        assert_eq!(
            after_mutation.block_state_id(1, 0, 1),
            Block::GoldBlock.default_state(),
            "the delayed old refresh must not replace the newer persisted block"
        );
        assert_eq!(
            after_mutation.retained_light(),
            None,
            "the block mutation must invalidate the old retained light"
        );

        // Finish the sequence with the newer light refresh that belongs to the
        // already-mutated column. It must survive in both retention layers.
        let mut newest = store
            .resident_column(0, 0)
            .expect("the mutated column remains resident");
        let mut newest_light = lodestone_world::ColumnLight::new(newest.section_count());
        *newest_light.sky_mut(0) = lodestone_world::LightData::Uniform(9);
        newest.set_retained_light(newest_light.clone());
        assert!(store.store_resident_column(0, 0, &newest));
        let persisted_newest = persisted
            .lock()
            .expect("race source persistence lock poisoned")
            .clone();
        assert_eq!(
            persisted_newest.block_state_id(1, 0, 1),
            Block::GoldBlock.default_state()
        );
        assert_eq!(persisted_newest.retained_light(), Some(&newest_light));
        assert_eq!(
            store
                .resident_column(0, 0)
                .expect("newest column remains resident")
                .retained_light(),
            Some(&newest_light)
        );
        assert_eq!(store_calls.load(Ordering::Acquire), 3);
    }

    /// A light computation may outlive the cache/source read that fed it. The
    /// mutation deliberately completes while that computation is paused; the
    /// stale snapshot must then be rejected at commit, and a later computation
    /// over the new column must still be able to commit.
    #[test]
    fn a_light_snapshot_commit_rejects_a_block_write_after_capture() {
        struct RevisionSource {
            persisted: Arc<Mutex<ChunkColumn>>,
            store_calls: Arc<AtomicUsize>,
        }

        impl ChunkSource for RevisionSource {
            fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
                self.persisted
                    .lock()
                    .expect("revision source column lock poisoned")
                    .clone()
            }

            fn block_state_id(&self, x: i32, y: i32, z: i32) -> lodestone_data::block_states::StateId {
                self.column(x.div_euclid(16), z.div_euclid(16))
                    .block_state_id(x.rem_euclid(16), y, z.rem_euclid(16))
            }

            fn biome_state_at(&self, x: i32, y: i32, z: i32) -> String {
                self.column(x.div_euclid(16), z.div_euclid(16))
                    .biome_state_at(x.rem_euclid(16), y, z.rem_euclid(16))
                    .to_owned()
            }

            fn set_block(&self, _x: i32, _y: i32, _z: i32, _state: lodestone_data::block_states::StateId) {}

            fn store_resident_column(
                &self,
                _cx: i32,
                _cz: i32,
                column: &ChunkColumn,
            ) -> bool {
                self.store_calls.fetch_add(1, Ordering::AcqRel);
                *self
                    .persisted
                    .lock()
                    .expect("revision source persistence lock poisoned") = column.clone();
                true
            }
        }

        let source = RevisionSource {
            persisted: Arc::new(Mutex::new(ChunkColumn::new(0, 16))),
            store_calls: Arc::new(AtomicUsize::new(0)),
        };
        let persisted = Arc::clone(&source.persisted);
        let store_calls = Arc::clone(&source.store_calls);
        let store = Arc::new(ChunkStore::with_capacity(source, 1));
        let _ = store.column(0, 0);

        let captured = Arc::new(AtomicBool::new(false));
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let fallback = store
            .resident_column(0, 0)
            .expect("the transaction starts from a resident column");
        std::thread::scope(|scope| {
            let refresh_store = Arc::clone(&store);
            let refresh_captured = Arc::clone(&captured);
            let refresh_release = Arc::clone(&release);
            let refresh_fallback = fallback.clone();
            let delayed = scope.spawn(move || {
                let mut compute = |_: &ChunkColumn| {
                    refresh_captured.store(true, Ordering::Release);
                    let (released, wake) = &*refresh_release;
                    let mut released = released
                        .lock()
                        .expect("revision source release lock poisoned");
                    while !*released {
                        released = wake
                            .wait(released)
                            .expect("revision source release lock poisoned");
                    }
                    let mut light = lodestone_world::ColumnLight::new(1);
                    *light.sky_mut(0) = lodestone_world::LightData::Uniform(1);
                    Some(light)
                };
                refresh_store.settle_resident_column_light(
                    0,
                    0,
                    &refresh_fallback,
                    true,
                    &mut compute,
                )
            });

            while !captured.load(Ordering::Acquire) {
                std::thread::yield_now();
            }

            // The mutation is not allowed to wait for the light computation;
            // only the short snapshot/commit sections own the coordinate gate.
            store.set_block(1, 0, 1, Block::GoldBlock.default_state());
            let after_mutation = store
                .resident_column(0, 0)
                .expect("the mutation leaves the column resident");
            assert_eq!(
                after_mutation.block_state_id(1, 0, 1),
                Block::GoldBlock.default_state()
            );
            assert_eq!(after_mutation.retained_light(), None);
            assert_eq!(
                persisted
                    .lock()
                    .expect("revision source persistence lock poisoned")
                    .block_state_id(1, 0, 1),
                Block::GoldBlock.default_state()
            );

            let (released, wake) = &*release;
            *released
                .lock()
                .expect("revision source release lock poisoned") = true;
            wake.notify_one();
            assert!(matches!(
                delayed.join().expect("delayed settlement must not panic"),
                Err(ColumnLightSettlementError::Conflict)
            ));
        });

        let current = store
            .resident_column(0, 0)
            .expect("the current column remains resident");
        let mut compute = |column: &ChunkColumn| {
            let mut light = lodestone_world::ColumnLight::new(column.section_count());
            *light.sky_mut(0) = lodestone_world::LightData::Uniform(9);
            Some(light)
        };
        let settled = store
            .settle_resident_column_light(0, 0, &current, true, &mut compute)
            .expect("the refresh over the newer block must commit");
        assert_eq!(settled.block_state_id(1, 0, 1), Block::GoldBlock.default_state());
        assert!(matches!(
            settled.retained_light().map(|light| light.sky(0)),
            Some(lodestone_world::LightData::Uniform(9))
        ));
        let persisted = persisted
            .lock()
            .expect("revision source persistence lock poisoned")
            .clone();
        assert_eq!(persisted.block_state_id(1, 0, 1), Block::GoldBlock.default_state());
        assert_eq!(persisted.retained_light(), settled.retained_light());
        assert_eq!(store_calls.load(Ordering::Acquire), 2);
    }

    /// A light snapshot depends on every column in its footprint, not only the
    /// centre. A neighbour mutation deliberately completes while compute is
    /// paused; the multi-coordinate commit must reject the stale centre light,
    /// after which a fresh computation over the new neighbour can commit.
    #[test]
    fn a_neighbour_snapshot_commit_rejects_a_neighbour_write_after_capture() {
        let offsets = vec![
            (-1, -1),
            (0, -1),
            (1, -1),
            (-1, 0),
            (1, 0),
            (-1, 1),
            (0, 1),
            (1, 1),
        ];
        let store = Arc::new(ChunkStore::with_capacity(CountingSource::new(), 32));
        let mut coordinates = vec![(0, 0)];
        coordinates.extend(offsets.iter().map(|&(dx, dz)| (dx, dz)));
        for (cx, cz) in coordinates {
            let _ = store.column(cx, cz);
        }

        let captured = Arc::new(AtomicBool::new(false));
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let fallback = store
            .resident_column(0, 0)
            .expect("the centre starts resident");
        let result = std::thread::scope(|scope| {
            let refresh_store = Arc::clone(&store);
            let refresh_offsets = offsets.clone();
            let refresh_captured = Arc::clone(&captured);
            let refresh_release = Arc::clone(&release);
            let refresh_fallback = fallback.clone();
            let delayed = scope.spawn(move || {
                let mut compute = |centre: &ChunkColumn,
                                   neighbours: &[(i32, i32, &ChunkColumn)]| {
                    assert_eq!(neighbours.len(), 8);
                    assert_eq!(
                        neighbours
                            .iter()
                            .find(|(dx, dz, _)| (*dx, *dz) == (1, 0))
                            .expect("the east dependency is captured")
                            .2
                            .block_state_id(1, 0, 1),
                        StateId::AIR
                    );
                    refresh_captured.store(true, Ordering::Release);
                    let (released, wake) = &*refresh_release;
                    let mut released = released
                        .lock()
                        .expect("neighbour snapshot release lock poisoned");
                    while !*released {
                        released = wake
                            .wait(released)
                            .expect("neighbour snapshot release lock poisoned");
                    }
                    let mut light = lodestone_world::ColumnLight::new(centre.section_count());
                    *light.sky_mut(0) = lodestone_world::LightData::Uniform(1);
                    Some(light)
                };
                refresh_store.settle_resident_column_light_with_neighbours(
                    0,
                    0,
                    &refresh_fallback,
                    &refresh_offsets,
                    true,
                    true,
                    false,
                    &mut compute,
                )
            });

            while !captured.load(Ordering::Acquire) {
                std::thread::yield_now();
            }
            store.set_block(17, 0, 1, Block::GoldBlock.default_state());
            assert_eq!(
                store
                    .resident_column(1, 0)
                    .expect("the east neighbour remains resident")
                    .block_state_id(1, 0, 1),
                Block::GoldBlock.default_state()
            );
            let (released, wake) = &*release;
            *released
                .lock()
                .expect("neighbour snapshot release lock poisoned") = true;
            wake.notify_one();
            delayed.join().expect("neighbour refresh must not panic")
        });
        assert!(matches!(
            result,
            Err(ColumnLightSettlementError::Conflict)
        ));
        assert_eq!(
            store
                .resident_column(0, 0)
                .expect("the centre remains resident")
                .retained_light(),
            None,
            "a neighbour mutation must reject the stale centre snapshot"
        );

        let current = store
            .resident_column(0, 0)
            .expect("the current centre remains resident");
        let mut compute = |centre: &ChunkColumn,
                           neighbours: &[(i32, i32, &ChunkColumn)]| {
            assert_eq!(neighbours.len(), 8);
            let mut light = lodestone_world::ColumnLight::new(centre.section_count());
            *light.sky_mut(0) = lodestone_world::LightData::Uniform(9);
            Some(light)
        };
        let settled = store
            .settle_resident_column_light_with_neighbours(
                0,
                0,
                &current,
                &offsets,
                true,
                true,
                false,
                &mut compute,
            )
            .expect("the fresh footprint must commit");
        assert_eq!(
            settled.retained_light().map(|light| light.sky(0)),
            Some(&lodestone_world::LightData::Uniform(9))
        );
        assert_eq!(
            store
                .resident_column(1, 0)
                .expect("the east neighbour remains resident")
                .block_state_id(1, 0, 1),
            Block::GoldBlock.default_state()
        );
    }

    /// The exclusive settlement path is the bounded contention fallback. It
    /// holds the sorted footprint gates while computing, so a waiting
    /// neighbour write cannot race the final commit and both operations finish
    /// in a deterministic order.
    #[test]
    fn exclusive_light_settlement_makes_progress_before_a_waiting_neighbour_write() {
        let offsets = vec![
            (-1, -1),
            (0, -1),
            (1, -1),
            (-1, 0),
            (1, 0),
            (-1, 1),
            (0, 1),
            (1, 1),
        ];
        let store = Arc::new(ChunkStore::with_capacity(CountingSource::new(), 32));
        for &(dx, dz) in &offsets {
            let _ = store.column(dx, dz);
        }
        let _ = store.column(0, 0);
        let captured = Arc::new(AtomicBool::new(false));
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let fallback = store
            .resident_column(0, 0)
            .expect("the centre starts resident");
        std::thread::scope(|scope| {
            let refresh_store = Arc::clone(&store);
            let refresh_offsets = offsets.clone();
            let refresh_captured = Arc::clone(&captured);
            let refresh_release = Arc::clone(&release);
            let refresh_fallback = fallback.clone();
            let delayed = scope.spawn(move || {
                let mut compute = |centre: &ChunkColumn,
                                   neighbours: &[(i32, i32, &ChunkColumn)]| {
                    assert_eq!(neighbours.len(), 8);
                    refresh_captured.store(true, Ordering::Release);
                    let (released, wake) = &*refresh_release;
                    let mut released = released
                        .lock()
                        .expect("exclusive release lock poisoned");
                    while !*released {
                        released = wake
                            .wait(released)
                            .expect("exclusive release lock poisoned");
                    }
                    let mut light = lodestone_world::ColumnLight::new(centre.section_count());
                    *light.sky_mut(0) = lodestone_world::LightData::Uniform(3);
                    Some(light)
                };
                refresh_store.settle_resident_column_light_with_neighbours(
                    0,
                    0,
                    &refresh_fallback,
                    &refresh_offsets,
                    true,
                    true,
                    true,
                    &mut compute,
                )
            });
            while !captured.load(Ordering::Acquire) {
                std::thread::yield_now();
            }
            let mutation_started = Arc::new(AtomicBool::new(false));
            let mutation_finished = Arc::new(AtomicBool::new(false));
            let mutation_started_for_thread = Arc::clone(&mutation_started);
            let mutation_finished_for_thread = Arc::clone(&mutation_finished);
            let mutation_store = Arc::clone(&store);
            let mutation = scope.spawn(move || {
                mutation_started_for_thread.store(true, Ordering::Release);
                mutation_store.set_block(17, 0, 1, Block::GoldBlock.default_state());
                mutation_finished_for_thread.store(true, Ordering::Release);
            });
            while !mutation_started.load(Ordering::Acquire) {
                std::thread::yield_now();
            }
            assert!(
                !mutation_finished.load(Ordering::Acquire),
                "the neighbour write must wait for the exclusive final compute"
            );
            let (released, wake) = &*release;
            *released
                .lock()
                .expect("exclusive release lock poisoned") = true;
            wake.notify_one();
            assert!(
                delayed
                    .join()
                    .expect("exclusive settlement must not panic")
                    .is_ok()
            );
            mutation.join().expect("waiting neighbour write must not panic");
        });
        assert_eq!(
            store
                .resident_column(1, 0)
                .expect("the east neighbour remains resident")
                .block_state_id(1, 0, 1),
            Block::GoldBlock.default_state()
        );
        let current = store
            .resident_column(0, 0)
            .expect("the current centre remains resident");
        let mut compute = |centre: &ChunkColumn,
                           neighbours: &[(i32, i32, &ChunkColumn)]| {
            assert_eq!(neighbours.len(), 8);
            let mut light = lodestone_world::ColumnLight::new(centre.section_count());
            *light.sky_mut(0) = lodestone_world::LightData::Uniform(9);
            Some(light)
        };
        let settled = store
            .settle_resident_column_light_with_neighbours(
                0,
                0,
                &current,
                &offsets,
                true,
                true,
                false,
                &mut compute,
            )
            .expect("the post-contention refresh must commit");
        assert_eq!(
            settled.retained_light().map(|light| light.sky(0)),
            Some(&lodestone_world::LightData::Uniform(9))
        );
        assert_eq!(
            settled.block_state_id(1, 0, 1),
            StateId::AIR,
            "the centre has no terrain mutation from the east neighbour write"
        );
    }

    /// A cold generation and an uncached block write share the same
    /// coordinate ordering. The generation pauses before returning its base
    /// column; the write attempts during that pause and must complete only
    /// after insertion, so the cache cannot resurrect stale terrain.
    #[test]
    fn cold_generation_cannot_insert_after_an_uncached_block_write() {
        struct EnsureRaceSource {
            persisted: Arc<Mutex<ChunkColumn>>,
            generated: Arc<Mutex<Option<ChunkColumn>>>,
            generation_started: Arc<AtomicBool>,
            release_generation: Arc<(Mutex<bool>, Condvar)>,
        }

        impl ChunkSource for EnsureRaceSource {
            fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
                self.generation_started.store(true, Ordering::Release);
                let (released, wake) = &*self.release_generation;
                let mut released = released
                    .lock()
                    .expect("ensure race release lock poisoned");
                while !*released {
                    released = wake
                        .wait(released)
                        .expect("ensure race release lock poisoned");
                }
                let generated = self
                    .persisted
                    .lock()
                    .expect("ensure race persistence lock poisoned")
                    .clone();
                *self
                    .generated
                    .lock()
                    .expect("ensure race generated-column lock poisoned") = Some(generated.clone());
                generated
            }

            fn block_state_id(&self, x: i32, y: i32, z: i32) -> lodestone_data::block_states::StateId {
                self.persisted
                    .lock()
                    .expect("ensure race persistence lock poisoned")
                    .block_state_id(x.rem_euclid(16), y, z.rem_euclid(16))
            }

            fn biome_state_at(&self, _x: i32, _y: i32, _z: i32) -> String {
                crate::chunk::DEFAULT_BIOME.to_owned()
            }

            fn set_block(&self, x: i32, y: i32, z: i32, state: lodestone_data::block_states::StateId) {
                self.persisted
                    .lock()
                    .expect("ensure race persistence lock poisoned")
                    .set_block_id(x.rem_euclid(16), y, z.rem_euclid(16), state);
            }
        }

        let source = EnsureRaceSource {
            persisted: Arc::new(Mutex::new(ChunkColumn::new(0, 16))),
            generated: Arc::new(Mutex::new(None)),
            generation_started: Arc::new(AtomicBool::new(false)),
            release_generation: Arc::new((Mutex::new(false), Condvar::new())),
        };
        let persisted = Arc::clone(&source.persisted);
        let generated_source = Arc::clone(&source.generated);
        let generation_started = Arc::clone(&source.generation_started);
        let release_generation = Arc::clone(&source.release_generation);
        let store = Arc::new(ChunkStore::with_capacity(source, 1));
        let mutation_started = Arc::new(AtomicBool::new(false));
        let mutation_finished = Arc::new(AtomicBool::new(false));
        std::thread::scope(|scope| {
            let loading_store = Arc::clone(&store);
            let loading = scope.spawn(move || loading_store.column(0, 0));
            while !generation_started.load(Ordering::Acquire) {
                std::thread::yield_now();
            }

            let mutation_store = Arc::clone(&store);
            let mutation_started_for_thread = Arc::clone(&mutation_started);
            let mutation_finished_for_thread = Arc::clone(&mutation_finished);
            let mutation = scope.spawn(move || {
                mutation_started_for_thread.store(true, Ordering::Release);
                mutation_store.set_block(1, 0, 1, Block::GoldBlock.default_state());
                mutation_finished_for_thread.store(true, Ordering::Release);
            });
            while !mutation_started.load(Ordering::Acquire) {
                std::thread::yield_now();
            }
            let mutation_waited = !mutation_finished.load(Ordering::Acquire);
            let (released, wake) = &*release_generation;
            *released
                .lock()
                .expect("ensure race release lock poisoned") = true;
            wake.notify_one();
            let _generated = loading.join().expect("cold generation must not panic");
            assert!(
                mutation_waited,
                "an uncached block write must wait for the cold generation gate"
            );
            assert_eq!(
                generated_source
                    .lock()
                    .expect("ensure race generated-column lock poisoned")
                    .as_ref()
                    .expect("the cold source must return a generated column")
                    .block_state_id(1, 0, 1),
                StateId::AIR,
                "the in-flight generation observed the pre-write terrain"
            );
            mutation.join().expect("ordered block write must not panic");
        });
        assert!(mutation_finished.load(Ordering::Acquire));
        assert_eq!(
            store.column(0, 0).block_state_id(1, 0, 1),
            Block::GoldBlock.default_state(),
            "the cache must retain the edit that followed generation"
        );
        assert_eq!(
            persisted
                .lock()
                .expect("ensure race persistence lock poisoned")
                .block_state_id(1, 0, 1),
            Block::GoldBlock.default_state()
        );
        assert_eq!(store.generated(), 1);
    }

    /// A generator-backed source's terrain edit ledger must grow only for
    /// actual block mutations. Serving and evicting many exact light snapshots
    /// exercises the cache lifecycle without turning those snapshots into
    /// permanent terrain edits.
    #[test]
    fn light_only_resident_snapshots_do_not_become_inner_terrain_edits() {
        struct EditLedgerSource {
            edits: Arc<Mutex<HashMap<(i32, i32), ChunkColumn>>>,
        }

        impl ChunkSource for EditLedgerSource {
            fn column(&self, cx: i32, cz: i32) -> ChunkColumn {
                self.edits
                    .lock()
                    .expect("edit ledger lock poisoned")
                    .get(&(cx, cz))
                    .cloned()
                    .unwrap_or_else(|| ChunkColumn::new(0, 16))
            }

            fn block_state_id(&self, x: i32, y: i32, z: i32) -> lodestone_data::block_states::StateId {
                self.column(x.div_euclid(16), z.div_euclid(16))
                    .block_state_id(x.rem_euclid(16), y, z.rem_euclid(16))
            }

            fn biome_state_at(&self, x: i32, y: i32, z: i32) -> String {
                self.column(x.div_euclid(16), z.div_euclid(16))
                    .biome_state_at(x.rem_euclid(16), y, z.rem_euclid(16))
                    .to_owned()
            }

            fn set_block(&self, x: i32, y: i32, z: i32, state: lodestone_data::block_states::StateId) {
                let cx = x.div_euclid(16);
                let cz = z.div_euclid(16);
                let mut edits = self.edits.lock().expect("edit ledger lock poisoned");
                edits
                    .entry((cx, cz))
                    .or_insert_with(|| ChunkColumn::new(0, 16))
                    .set_block_id(x.rem_euclid(16), y, z.rem_euclid(16), state);
            }
        }

        let edits = Arc::new(Mutex::new(HashMap::new()));
        let source = EditLedgerSource {
            edits: Arc::clone(&edits),
        };
        let store = ChunkStore::with_capacity(source, 2);
        for cx in 0..16 {
            let mut column = store.column(cx, 0);
            let mut light = lodestone_world::ColumnLight::new(column.section_count());
            *light.sky_mut(0) = lodestone_world::LightData::Uniform((cx % 16) as u8);
            column.set_retained_light(light);
            assert!(store.store_resident_column(cx, 0, &column));
        }

        assert_eq!(store.len(), 2, "the outer cache remains bounded after evictions");
        assert_eq!(
            edits.lock().expect("edit ledger lock poisoned").len(),
            0,
            "light-only snapshots must not become permanent terrain edits"
        );

        store.set_block(15 * 16 + 1, 0, 1, Block::GoldBlock.default_state());
        let edits = edits.lock().expect("edit ledger lock poisoned");
        assert_eq!(edits.len(), 1, "a real block write still enters the edit ledger");
        assert_eq!(
            edits
                .get(&(15, 0))
                .expect("the edited coordinate is retained")
                .block_state_id(1, 0, 1),
            Block::GoldBlock.default_state()
        );
    }

    // ---------------------------------------------------------------------
    // The block-entity lead: is the 20 Hz registry scan a *second*,
    // CPU-side, distance-dependent term? See
    // `docs/block-entity-tick-distance.md` for the full write-up. The counter
    // is `CountingSource::per_chunk` for the *remote* column, and the two
    // competing hypotheses are computed from constants below rather than
    // compared as "more" and "less".
    // ---------------------------------------------------------------------

    /// The chunk a "walked away from" hopper sits in — 1,600 blocks out, the
    /// same stroll length `docs/worldgen-store-distance-leak.md` measured its
    /// memory term over, and far outside [`shell_tick_area`] so the random-tick
    /// pass never touches it. Whether this column is resident is therefore
    /// decided by the block-entity scan alone, which is the whole point.
    const REMOTE_CHUNK: (i32, i32) = (100, 100);

    /// A y inside [`CountingSource::full_height`]'s extent, so
    /// `ChunkColumn::block_state` indexes in range.
    const REMOTE_Y: i32 = 64;

    fn remote_pos(chunk: (i32, i32)) -> BlockPos {
        BlockPos::new(chunk.0 * 16 + 8, REMOTE_Y, chunk.1 * 16 + 8)
    }

    /// A registry holding one hopper at `chunk`, and nothing else.
    ///
    /// A hopper specifically, not any of the other four kinds: it is the **only**
    /// variant whose tick reaches the world at all —
    /// `tick_all_with_hopper_lock`'s `enabled` closure is called for
    /// `BlockEntity::Hopper` and for nothing else, and that closure is the
    /// `world.block_state` call the lead is about.
    fn registry_with_one_hopper(chunk: (i32, i32)) -> BlockEntityHandle {
        let handle = BlockEntityHandle::new();
        handle.with(|registry| {
            registry.insert(
                remote_pos(chunk),
                crate::block_entities::BlockEntity::Hopper(crate::hopper::Hopper::new()),
            );
        });
        handle
    }

    fn generations_for(per_chunk: &PerChunk, chunk: (i32, i32)) -> u64 {
        per_chunk
            .lock()
            .expect("per-chunk map poisoned")
            .get(&chunk)
            .copied()
            .unwrap_or(0)
    }

    /// **The subject, measured with the residency check.** A hopper whose
    /// column is outside the player's loaded area costs **zero** column
    /// generations, not one and not [`TICKS`].
    ///
    /// # Why the remote hopper generates no column
    ///
    /// The block-entity arm calls `world.block_state` per hopper only when its
    /// chunk is resident. `ChunkStore::block_state`
    /// regenerates a whole column on a miss — so a hopper 1,600 blocks out
    /// (never inside [`shell_tick_area`], never in any view) cost exactly
    /// **1** generation when it is probed. The residency check does not make
    /// that read cheaper — it stops the read from happening at all:
    /// `tick_all_with_hopper_lock` takes an
    /// `is_loaded` predicate and skips a hopper's tick (and therefore its
    /// `enabled` closure, and therefore `world.block_state`) entirely when
    /// its chunk is not resident. A never-loaded remote hopper touches
    /// the store **zero** times over its whole life: a block entity outside
    /// every loaded chunk does not tick.
    ///
    /// `a_hopper_inside_the_tick_area_still_transfers_once_the_chunk_is_loaded`
    /// below is the control that `is_loaded` is a real gate and not a
    /// constant `false` that would make this `0` vacuous.
    #[tokio::test(start_paused = true)]
    async fn a_walked_away_hopper_never_reaches_the_store() {
        let counting = CountingSource::full_height();
        let calls = Arc::clone(&counting.calls);
        let per_chunk = Arc::clone(&counting.per_chunk);
        let store = Arc::new(ChunkStore::new(counting));

        preload_tick_area(store.as_ref());
        let clock = drive_tick_loop_with_block_entities(
            Arc::clone(&store),
            shell_tick_area(),
            TICKS,
            registry_with_one_hopper(REMOTE_CHUNK),
        )
        .await;

        assert!(
            clock.tick_count() >= u64::from(TICKS) - 1,
            "precondition: the loop only advanced {} ticks of {TICKS}, so \"zero generations\" \
             would be trivially true",
            clock.tick_count()
        );

        let remote = generations_for(&per_chunk, REMOTE_CHUNK);
        assert_eq!(
            remote, 0,
            "the remote hopper's column {REMOTE_CHUNK:?} was generated {remote} times over \
             {TICKS} ticks. A hopper whose chunk is never loaded must never be probed."
        );
        assert_eq!(
            calls.load(Ordering::Relaxed),
            EXPECTED_TICK_AREA_COLUMNS as u64,
            "the whole session should cost exactly the {EXPECTED_TICK_AREA_COLUMNS}-column \
             tick area and nothing else — the remote hopper must contribute zero, not one"
        );
        assert_eq!(
            store.evicted(),
            0,
            "precondition on the mechanism: the tick area alone is far under \
             {DEFAULT_CAPACITY}, so nothing should be evicted"
        );
    }

    /// **The positive control the subject above needs.** `is_loaded` is a
    /// real gate, not a constant `false` that would make
    /// `a_walked_away_hopper_never_reaches_the_store`'s `0` vacuous: two
    /// hoppers stacked inside [`shell_tick_area`] (chunk `(0, 0)`, which the
    /// random-tick pass visits every tick and therefore keeps resident) must
    /// still transfer an item, exactly as
    /// `crate::block_entities`'s own
    /// `tick_all_moves_two_items_between_a_stacked_hopper_pair_on_the_first_tick`
    /// proves for the unlocked shorthand.
    ///
    /// This positive control proves that loaded hoppers still transfer items;
    /// a constant-false residency predicate would otherwise make the remote
    /// zero-generation result vacuous.
    #[tokio::test(start_paused = true)]
    async fn a_hopper_inside_the_tick_area_still_transfers_once_the_chunk_is_loaded() {
        let below_pos = remote_pos((0, 0));
        let above_pos = BlockPos::new(below_pos.x, below_pos.y + 1, below_pos.z);

        let handle = BlockEntityHandle::new();
        handle.with(|registry| {
            let mut above = crate::hopper::Hopper::new();
            above.set_slot(
                0,
                Some(lodestone_model::ItemStack::new(
                    "minecraft:diamond".parse().expect("valid resource key"),
                    3,
                )),
            );
            registry.insert(below_pos, crate::block_entities::BlockEntity::Hopper(crate::hopper::Hopper::new()));
            registry.insert(above_pos, crate::block_entities::BlockEntity::Hopper(above));
        });

        let counting = CountingSource::full_height();
        let store = Arc::new(ChunkStore::new(counting));

        preload_tick_area(store.as_ref());
        drive_tick_loop_with_block_entities(Arc::clone(&store), shell_tick_area(), TICKS, handle.clone())
            .await;

        assert!(
            store.is_column_resident(0, 0),
            "precondition: chunk (0, 0) must actually be resident by the end of the run, or \
             this control proves nothing about the loaded path"
        );

        let Some(crate::block_entities::BlockEntity::Hopper(below)) =
            handle.with(|registry| registry.get(below_pos).cloned())
        else {
            panic!("the below hopper must still be registered");
        };
        assert!(
            below.slots().iter().any(Option::is_some),
            "a hopper inside a chunk the tick area loads must still receive an item once the \
             chunk becomes resident. If this is empty, `is_loaded` is suppressing every hopper \
             regardless of residency, not just the unloaded ones."
        );
    }

    /// **The curve, in counter form: flat in distance, at zero.**
    ///
    /// The claim under test is that walking away introduces a term that grows
    /// with distance. `is_column_resident`'s cost has no coordinate in it
    /// either — it is the same `HashMap<(i32, i32), _>` lookup the old
    /// `block_state` miss path used, just without the generation on a miss —
    /// so the prediction is the *same* number at every band, including one a
    /// million blocks out, matching `docs/worldgen-store-distance-leak.md`'s
    /// finding that per-column cost is itself flat to 1,048,576 blocks. Without the residency bound,
    /// that flat number was **1** (see `a_walked_away_hopper_never_reaches_the_
    /// store`'s unbounded-scan measurement); with the bound it is **0**, because none of these bands are
    /// ever loaded at all.
    ///
    /// The bands double as the control the walk investigation used (nine runs at
    /// one distance gave a 1.01× spread): here the arms differ *only* in the
    /// hopper's coordinate, so any spread at all is a distance term. A count has
    /// no spread to explain away, which is why this is a counter and not a
    /// timing.
    #[tokio::test(start_paused = true)]
    async fn the_registry_scan_costs_the_same_at_every_distance_from_the_tick_area() {
        // 64 blocks (just outside the 49-column tick area) to 16,000,000.
        const BANDS: [(i32, i32); 4] = [(4, 4), (100, 100), (10_000, 10_000), (1_000_000, 1_000_000)];

        for chunk in BANDS {
            let counting = CountingSource::full_height();
            let calls = Arc::clone(&counting.calls);
            let per_chunk = Arc::clone(&counting.per_chunk);
            let store = Arc::new(ChunkStore::new(counting));

            preload_tick_area(store.as_ref());
            drive_tick_loop_with_block_entities(
                Arc::clone(&store),
                shell_tick_area(),
                TICKS,
                registry_with_one_hopper(chunk),
            )
            .await;

            assert_eq!(
                generations_for(&per_chunk, chunk),
                0,
                "band {chunk:?} ({} blocks out): the remote column must never be generated; \
                 a nonzero, band-dependent count would expose distance-dependent tick work.",
                chunk.0 * 16
            );
            assert_eq!(
                calls.load(Ordering::Relaxed),
                EXPECTED_TICK_AREA_COLUMNS as u64,
                "band {chunk:?}: total generations must be exactly the tick area, identically \
                 at every distance — the remote hopper must contribute nothing"
            );
        }
    }

    /// **With the registry scan bounded, its capacity pressure is gone by construction.** A
    /// remote hopper's column does not compete
    /// for the store's capacity like any other resident column, so pinching
    /// the ceiling down to exactly the tick area's size (`with_capacity(source,
    /// EXPECTED_TICK_AREA_COLUMNS)`) would evict tick-area columns if an
    /// unloaded hopper were admitted to the cache.
    /// With the residency check there is nothing left to evict: `is_loaded` means the remote
    /// hopper's column is **never generated in the first place**, so it never
    /// enters the cache and never competes with the tick area for room, no
    /// matter how tight the ceiling.
    #[tokio::test(start_paused = true)]
    async fn a_tight_capacity_no_longer_makes_the_remote_hopper_cold() {
        let counting = CountingSource::full_height();
        let per_chunk = Arc::clone(&counting.per_chunk);
        let store = Arc::new(ChunkStore::with_capacity(
            counting,
            EXPECTED_TICK_AREA_COLUMNS,
        ));

        preload_tick_area(store.as_ref());
        drive_tick_loop_with_block_entities(
            Arc::clone(&store),
            shell_tick_area(),
            TICKS,
            registry_with_one_hopper(REMOTE_CHUNK),
        )
        .await;

        let remote = generations_for(&per_chunk, REMOTE_CHUNK);
        assert_eq!(
            remote, 0,
            "with the scan bounded by residency, a remote hopper must cost zero generations \
             regardless of how tight the capacity is — pre-#504 this was 12 (see the doc \
             comment's history); a nonzero count here means the fix stopped bounding the scan \
             and capacity pressure is back."
        );
        assert_eq!(
            store.evicted(),
            0,
            "the tick area (exactly {EXPECTED_TICK_AREA_COLUMNS} columns) fits its own \
             capacity with nothing left over to evict, because the remote hopper never enters \
             the cache at all"
        );
    }

    /// **The curve, and the threshold it turns on.** Cold generations per tick
    /// are ~0 while the accumulated block-entity columns fit under
    /// [`DEFAULT_CAPACITY`] alongside the tick area, and **one per entity per
    /// tick** the moment they do not — **that is the unbounded-scan shape this gate excludes.**
    ///
    /// The control uses a `BlockEntityRegistry` with no unload entries and
    /// probes it at 20 Hz against the store's fixed ceiling. The residency
    /// filter rejects remote positions before `world.block_state`; measured
    /// controls produce 400 remote generations for 400 entities and 31,739 for
    /// 600 entities without that filter, matching `600 × TICKS` = 31,200 within
    /// 1.7%.
    ///
    /// **The residency check removes the mechanism entirely, not just its cost.**
    /// None of these entities' chunks are ever loaded (they sit at
    /// `(2_000 + i, 2_000 + i)`, far outside [`shell_tick_area`] and touched by
    /// nothing else in this rig), so `is_loaded` rejects every one of them
    /// before `enabled` — and therefore `world.block_state` — is ever called.
    /// The registry can hold 400, 600, or 400,000 hoppers with identical
    /// result: **zero** remote generations and **zero** evictions, at both
    /// bands, because nothing about an unloaded hopper ever reaches the store.
    /// This assertion tests the residency bound rather than allowing the scan
    /// to depend on capacity.
    #[tokio::test(start_paused = true)]
    async fn the_registry_outgrowing_the_store_no_longer_moves_the_miss_rate() {
        /// Comfortably under the default ceiling: `49 + 400 = 449 <= 512`.
        const UNDER: i32 = 400;
        /// Over the default ceiling: `49 + 600 = 649 > 512`. Both bands are
        /// kept specifically so the
        /// Both bands check that the result is independent of which side of
        /// the ceiling the registry occupies.
        const OVER: i32 = 600;

        const _: () = assert!(EXPECTED_TICK_AREA_COLUMNS + UNDER as usize <= DEFAULT_CAPACITY);
        const _: () = assert!(EXPECTED_TICK_AREA_COLUMNS + OVER as usize > DEFAULT_CAPACITY);

        for entities in [UNDER, OVER] {
            let handle = BlockEntityHandle::new();
            handle.with(|registry| {
                for i in 0..entities {
                    registry.insert(
                        remote_pos((2_000 + i, 2_000 + i)),
                        crate::block_entities::BlockEntity::Hopper(crate::hopper::Hopper::new()),
                    );
                }
            });

            let counting = CountingSource::sized(0, 128);
            let calls = Arc::clone(&counting.calls);
            let store = Arc::new(ChunkStore::new(counting));

            preload_tick_area(store.as_ref());
            drive_tick_loop_with_block_entities(
                Arc::clone(&store),
                shell_tick_area(),
                TICKS,
                handle,
            )
            .await;

            let total = calls.load(Ordering::Relaxed);
            let remote = total - EXPECTED_TICK_AREA_COLUMNS as u64;
            eprintln!(
                "entities {entities:>4}  remote generations {remote:>6}  evictions {:>6}",
                store.evicted()
            );
            assert_eq!(
                remote, 0,
                "{entities} unloaded hoppers must cost zero remote generations regardless of \
                 whether they fit under {DEFAULT_CAPACITY} alongside the tick area. Pre-#504 \
                 this was {entities} under capacity and ~{} over it.",
                u64::from(entities as u32) * u64::from(TICKS)
            );
            assert_eq!(
                store.evicted(),
                0,
                "nothing should be evicted: the tick area alone is far under {DEFAULT_CAPACITY}, \
                 and none of the {entities} hoppers ever enter the cache to compete for room"
            );
        }
    }

    /// The registry is scanned **unfiltered** — and the 1,608-of-1,613
    /// unsimulated kinds round-trip as
    /// [`BlockEntity::Opaque`](crate::block_entities::BlockEntity::Opaque) reach
    /// the world **zero** times regardless.
    ///
    /// `tick_all_with_hopper_lock` does not filter: it assigns *every* key to a
    /// fresh deterministic owner plan and dispatches on the variant, so a whole
    /// block-entity population is walked at 20 Hz. That is a real property and
    /// worth pinning — but the cost of an `Opaque` entry is an owner-plan entry,
    /// two hash probes and an empty `tick_non_hopper` arm. **No coordinate of it
    /// reaches the store**, which is what this gate measures: with 1,608
    /// opaque entities scattered over distinct far-flung
    /// columns, the store still generates only the 49-column tick area.
    ///
    /// The competing hypothesis is `49 + 1608`: if the scan probed the world per
    /// entry — the shape the lead attributes to it — every one of those columns
    /// would be cold.
    #[tokio::test(start_paused = true)]
    async fn sixteen_hundred_opaque_block_entities_never_reach_the_store() {
        /// The measured figure for a captured 26.2 world: 1,608 of its
        /// 1,613 block entities are kinds this crate does not simulate.
        const OPAQUE_ENTITIES: i32 = 1_608;

        let handle = BlockEntityHandle::new();
        handle.with(|registry| {
            for i in 0..OPAQUE_ENTITIES {
                // One per distinct column, all outside the tick area, so a
                // per-entry world probe would be a distinct cold generation and
                // could not be absorbed by the store.
                registry.insert(
                    remote_pos((1_000 + i, 1_000 + i)),
                    crate::block_entities::BlockEntity::Opaque {
                        id: "minecraft:chest".to_owned().into(),
                        nbt: lodestone_core::Nbt::End,
                    },
                );
            }
        });
        assert_eq!(
            handle.with(|registry| registry.len()),
            OPAQUE_ENTITIES as usize,
            "precondition: the registry must really hold all {OPAQUE_ENTITIES} entries, or the \
             count below is right for the wrong reason"
        );

        let counting = CountingSource::full_height();
        let calls = Arc::clone(&counting.calls);
        let store = Arc::new(ChunkStore::new(counting));

        preload_tick_area(store.as_ref());
        drive_tick_loop_with_block_entities(Arc::clone(&store), shell_tick_area(), TICKS, handle)
            .await;

        assert_eq!(
            calls.load(Ordering::Relaxed),
            EXPECTED_TICK_AREA_COLUMNS as u64,
            "the tick area and nothing else. {} would mean the unfiltered scan probes the \
             world per entry.",
            EXPECTED_TICK_AREA_COLUMNS as u64 + OPAQUE_ENTITIES as u64
        );
    }

    // The ticket graph driving this store's residency, above and
    // beyond its own LRU capacity backstop. See `ChunkStore::maybe_tick_tickets`
    // for the design; these gates drive it through the real `ensure()` path
    // (repeated `column()` calls), never by calling ticket-graph internals
    // directly, so a broken wiring between `ChunkStore` and `TicketStoreHandle`
    // would fail here even if `crate::ticket`'s own unit tests stayed green.

    /// Enough `column()` calls to guarantee at least one
    /// `maybe_tick_tickets` check-in, independent of `TICKET_CHECK_PERIOD`'s
    /// exact value.
    fn drive_ticket_check_ins<S: ChunkSource>(store: &ChunkStore<S>, at: (i32, i32), n: u64) {
        for _ in 0..n {
            let _ = store.column(at.0, at.1);
        }
    }

    /// A spawn ticket makes exactly its Chebyshev-radius square `Full`
    /// status and nothing outside it — the status half of the gate, matching
    /// `crate::ticket`'s own hand-derived boundary (distance 3 resident,
    /// distance 4 not).
    #[test]
    fn a_spawn_ticket_makes_its_radius_full_status_and_nothing_outside_it() {
        let store = Arc::new(ChunkStore::new(CountingSource::new()));
        store.set_spawn_ticket((0, 0), 3);
        // The ticket graph must be propagated at least once before status is
        // meaningful — drive real traffic through the store rather than
        // calling `tickets()` directly, so this exercises the wiring.
        drive_ticket_check_ins(&store, (0, 0), ChunkStore::<CountingSource>::TICKET_CHECK_PERIOD + 1);

        assert_eq!(store.ticket_status(3, 0), crate::ticket::ChunkStatus::Full);
        assert_eq!(store.ticket_status(0, -3), crate::ticket::ChunkStatus::Full);
        assert_eq!(store.ticket_status(4, 0), crate::ticket::ChunkStatus::Empty);
        assert_eq!(store.ticket_status(1000, 1000), crate::ticket::ChunkStatus::Empty);
    }

    /// **The discriminating gate**: a chunk reaches `Full` status under a
    /// ticket, and removing the ticket makes it evictable under pressure —
    /// observed at the real [`ChunkSource::unload`] call the source receives.
    #[test]
    fn removing_a_forced_ticket_lets_its_chunk_unload_and_the_source_observes_it() {
        let counting = CountingSource::new();
        let unloaded_log = Arc::clone(&counting.unloaded);
        let store = Arc::new(ChunkStore::with_capacity(counting, 1));

        store.set_forced_ticket(1, (5, 5));
        drive_ticket_check_ins(&store, (5, 5), ChunkStore::<CountingSource>::TICKET_CHECK_PERIOD + 1);
        assert_eq!(store.ticket_status(5, 5), crate::ticket::ChunkStatus::Full);
        assert!(
            store.is_column_resident(5, 5),
            "precondition: the forced-ticket chunk must actually be cached before removal, \
             or the eviction below proves nothing"
        );
        assert!(
            unloaded_log
                .lock()
                .expect("unloaded log poisoned")
                .is_empty(),
            "precondition: nothing has been unloaded yet"
        );

        assert!(store.remove_forced_ticket(1));
        // Drive a miss elsewhere. The removed ticket makes (5,5) eligible, and
        // this miss supplies the capacity pressure that performs the release.
        drive_ticket_check_ins(&store, (500, 500), ChunkStore::<CountingSource>::TICKET_CHECK_PERIOD + 1);

        assert_eq!(
            store.ticket_status(5, 5),
            crate::ticket::ChunkStatus::Empty,
            "the ticket graph itself must show the chunk as no longer wanted"
        );
        assert!(
            !store.is_column_resident(5, 5),
            "the cache entry must actually be gone, not merely ticket-unresident"
        );
        assert_eq!(
            unloaded_log.lock().expect("unloaded log poisoned").as_slice(),
            &[(5, 5)],
            "the source must observe exactly one unload, for exactly the removed ticket's chunk"
        );
    }

    /// The permanent negative control for the gate above: with the ticket
    /// **never removed**, the same amount of driven traffic must leave the
    /// chunk resident and must call `unload` zero times. Without this, the
    /// positive gate could be passing because `maybe_tick_tickets` evicts
    /// unconditionally rather than because it correctly tracks residency.
    #[test]
    fn a_forced_ticket_that_is_never_removed_never_unloads() {
        let counting = CountingSource::new();
        let unloaded_log = Arc::clone(&counting.unloaded);
        let store = Arc::new(ChunkStore::new(counting));

        store.set_forced_ticket(1, (5, 5));
        drive_ticket_check_ins(&store, (5, 5), ChunkStore::<CountingSource>::TICKET_CHECK_PERIOD + 1);
        drive_ticket_check_ins(&store, (500, 500), ChunkStore::<CountingSource>::TICKET_CHECK_PERIOD * 3);

        assert!(
            store.is_column_resident(5, 5),
            "a forced ticket that was never removed must keep its chunk resident indefinitely"
        );
        assert!(
            unloaded_log.lock().expect("unloaded log poisoned").is_empty(),
            "nothing was ever removed, so nothing may be unloaded — a control that fires here \
             means eviction is not actually gated on ticket removal"
        );
    }

    /// LRU pressure must not unload a column that is still covered by a
    /// loading/simulation ticket. The ticket sweep runs periodically, so this
    /// deliberately fills a capacity-one store between sweeps; checking only
    /// `newly_unresident` there would leave a window in which the ordinary LRU
    /// path drops the live ticket's column.
    #[test]
    fn lru_pressure_protects_ticket_resident_columns() {
        let counting = CountingSource::new();
        let unloaded_log = Arc::clone(&counting.unloaded);
        let store = ChunkStore::with_capacity(counting, 1);

        store.set_forced_ticket(1, (0, 0));
        let _ = store.column(0, 0);
        assert_eq!(
            store.ticket_status(0, 0),
            crate::ticket::ChunkStatus::Full,
            "the precondition must establish ticket residency before capacity pressure"
        );

        // The second miss occurs before the next periodic ticket check. The
        // ticketed centre is older, so an unprotected LRU would evict it.
        let _ = store.column(1, 0);

        assert!(
            store.is_column_resident(0, 0),
            "an active ticket must keep its resident column in the cache"
        );
        assert_eq!(
            unloaded_log.lock().expect("unloaded log poisoned").as_slice(),
            &[],
            "capacity pressure must not send an unload for a ticket-resident column"
        );
    }

    /// A ticket the caller never grants leaves ticket status at `Empty`
    /// everywhere and leaves `maybe_tick_tickets` with nothing to do — the
    /// store's ordinary LRU behaviour (already covered elsewhere in this
    /// module) must be completely unaffected by a ticket graph nobody has
    /// used.
    #[test]
    fn an_unused_ticket_graph_never_touches_lru_behaviour() {
        let counting = CountingSource::new();
        let unloaded_log = Arc::clone(&counting.unloaded);
        let store = Arc::new(ChunkStore::with_capacity(counting, 2));

        for cx in 0..5 {
            let _ = store.column(cx, 0);
        }
        // With capacity 2 and five distinct columns touched, LRU eviction must
        // have happened — through the *existing* `evict_down_to_capacity`
        // path, not the ticket path (no ticket was ever granted).
        assert!(store.len() <= 2);
        assert!(
            !unloaded_log.lock().expect("unloaded log poisoned").is_empty(),
            "capacity eviction must still call unload exactly as it did before this module existed"
        );
    }

    #[test]
    fn halo_pins_survive_pressure_and_defer_eviction_until_drop() {
        let source = CountingSource::new();
        let store = ChunkStore::with_capacity(source, 1);
        let halo = store.lease_halo(&[(1, 0), (0, 0)]).unwrap();
        let committed = store
            .commit_generation(
                &halo,
                vec![
                    ((1, 0), ChunkColumn::new(0, 16)),
                    ((0, 0), ChunkColumn::new(0, 16)),
                ],
            )
            .unwrap();
        assert_eq!(committed.coordinates, vec![(0, 0), (1, 0)]);
        assert_eq!(store.len(), 2, "a live halo may exceed the soft cache bound");
        assert!(store.retained_blocks_heap_bytes() > 0);
        drop(halo);
        assert_eq!(store.len(), 1, "deferred eviction runs when the halo is released");
        let retry = store.lease_halo(&[(1, 0)]).unwrap();
        let revision = retry.revision((1, 0)).expect("retry captured its revision");
        store
            .commit_generation(&retry, vec![((1, 0), ChunkColumn::new(0, 16))])
            .unwrap();
        assert_eq!(retry.revision((1, 0)), Some(revision));
        drop(retry);
    }

    #[test]
    fn request_driver_forwarding_runs_inside_store_admission_boundary() {
        use lodestone_worldgen::stage_schedule::{Dimension, GenerationTarget};

        let store = ChunkStore::with_capacity(
            DriverSource {
                driver: RejectingDriver,
            },
            1,
        );
        assert!(<ChunkStore<DriverSource> as ChunkSource>::request_stage_driver(&store).is_some());
        for _ in 0..4 {
            let request = crate::worldgen_session::GenerationRequest::new(
                Dimension::End,
                (0, 0),
                GenerationTarget::Full,
                0,
            );
            let mut session = crate::worldgen_session::GenerationSession::new(request);
            let error = store
                .execute_generation_session(&mut session)
                .expect_err("the test driver rejects before producing a packet");
            assert!(matches!(
                error,
                GenerationSessionExecutionError::Request(
                    crate::worldgen_session::GenerationRequestError::Session(
                        crate::worldgen_session::SessionError::Cancelled
                    )
                )
            ));
            assert_eq!(store.generation_ledger().stats().coordinates, 0);
            assert_eq!(store.len(), 0, "a rejected driver must not insert a packet column");
        }
    }

    #[test]
    fn generation_session_rehydrates_committed_prefix_and_does_not_repeat_it() {
        use lodestone_worldgen::stage_schedule::{Dimension, GenerationTarget};

        let driver = ResumableDriver::new(true);
        let fill_completions = Arc::clone(&driver.fill_completions);
        let calls = Arc::clone(&driver.calls);
        let store = ChunkStore::with_capacity(ResumableSource { driver }, 4);
        let request = crate::worldgen_session::GenerationRequest::new(
            Dimension::End,
            (0, 0),
            GenerationTarget::Full,
            0,
        );
        let mut first = crate::worldgen_session::GenerationSession::new(request);
        let first_error = store
            .execute_generation_session(&mut first)
            .expect_err("the fixture cancels after committing Fill");
        assert!(matches!(
            first_error,
            GenerationSessionExecutionError::Request(
                crate::worldgen_session::GenerationRequestError::Session(
                    SessionError::Cancelled
                )
            )
        ));
        assert_eq!(fill_completions.load(Ordering::Relaxed), 1);
        assert_eq!(store.generation_ledger().stats().products, 1);

        let mut second = crate::worldgen_session::GenerationSession::new(request);
        store
            .execute_generation_session(&mut second)
            .expect("the second request resumes the committed prefix");
        assert_eq!(calls.load(Ordering::Relaxed), 2);
        assert_eq!(
            fill_completions.load(Ordering::Relaxed),
            1,
            "a repeat request must not regenerate the committed Fill prefix"
        );
        let expected_products = second
            .pipeline()
            .schedule()
            .stages_for(second.request().generation_target())
            .iter()
            .filter_map(|stage| second.pipeline().descriptor(*stage))
            .map(|descriptor| descriptor.outputs().len())
            .sum::<usize>();
        assert_eq!(store.generation_ledger().stats().products, expected_products);
    }

    #[test]
    fn concurrent_same_target_execution_runs_one_driver_and_shares_result() {
        use lodestone_worldgen::stage_schedule::{Dimension, GenerationTarget};

        let entered = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let driver = ResumableDriver::new(false);
        let calls = Arc::clone(&driver.calls);
        let store = Arc::new(ChunkStore::with_capacity(
            BlockingSource {
                driver: BlockingDriver {
                    inner: driver,
                    entered: Arc::clone(&entered),
                    release: Arc::clone(&release),
                },
            },
            0,
        ));
        let request = crate::worldgen_session::GenerationRequest::new(
            Dimension::End,
            (0, 0),
            GenerationTarget::Full,
            0,
        );
        std::thread::scope(|scope| {
            let first_store = Arc::clone(&store);
            let first = scope.spawn(move || {
                let mut session = crate::worldgen_session::GenerationSession::new(request);
                first_store.execute_generation_session(&mut session)
            });
            entered.wait();
            let second_store = Arc::clone(&store);
            let second = scope.spawn(move || {
                let mut session = crate::worldgen_session::GenerationSession::new(request);
                second_store.execute_generation_session(&mut session)
            });
            release.wait();
            first.join().expect("leader thread panicked").expect("leader failed");
            second.join().expect("waiter thread panicked").expect("waiter failed");
        });
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn generation_commit_rejects_a_stale_revision_without_partial_insertion() {
        let store = ChunkStore::with_capacity(CountingSource::new(), 4);
        let halo = store.lease_halo(&[(0, 0), (1, 0)]).unwrap();
        store.write_gates.with((0, 0), || {});
        let error = store
            .commit_generation(
                &halo,
                vec![
                    ((0, 0), ChunkColumn::new(0, 16)),
                    ((1, 0), ChunkColumn::new(0, 16)),
                ],
            )
            .unwrap_err();
        assert!(matches!(error, GenerationCommitError::RevisionConflict { coordinate: (0, 0), .. }));
        assert_eq!(store.len(), 0, "a conflicting batch must not partially commit");
    }

    #[test]
    fn cohort_halo_accepts_only_its_own_committed_revision() {
        let store = ChunkStore::with_capacity(CountingSource::new(), 4);
        let mut halo = store.lease_halo(&[(0, 0), (1, 0)]).unwrap();
        let first = store
            .commit_generation(&halo, vec![((0, 0), ChunkColumn::new(0, 16))])
            .unwrap();
        halo.acknowledge(&first);
        assert_eq!(halo.revision((0, 0)), Some(1));
        assert_eq!(halo.revision((1, 0)), Some(0));

        let second = store
            .commit_generation(&halo, vec![((1, 0), ChunkColumn::new(0, 16))])
            .unwrap();
        halo.acknowledge(&second);
        store.write_gates.with((0, 0), || {});
        let error = store
            .commit_generation(&halo, vec![((0, 0), ChunkColumn::new(0, 16))])
            .unwrap_err();
        assert!(matches!(error, GenerationCommitError::RevisionConflict { coordinate: (0, 0), expected: 1, found: 2 }));
    }

    #[test]
    fn cohort_commit_rejects_external_edits_anywhere_in_its_pinned_halo() {
        use lodestone_worldgen::stage_schedule::END_PIPELINE;

        let store = ChunkStore::with_capacity(CountingSource::new(), 8);
        let mut halo = store.lease_halo(&[(0, 0), (1, 0), (2, 0)]).unwrap();
        let first = store
            .commit_generation_with_mutations_and_persistence_destinations_and_receipts_policy(
                &halo,
                vec![((0, 0), ChunkColumn::new(0, 16))],
                &[],
                &BTreeSet::new(),
                &BTreeSet::from([(0, 0)]),
                &[],
                GenerationCommitMode::Cohort,
            )
            .unwrap();
        halo.acknowledge(&first);
        store.write_gates.with((2, 0), || {});

        let mut session = end_shaped_session_at((1, 0), 1, 0);
        complete_end_suffix(&mut session, 2);
        let output = ChunkColumn::new(0, 16);
        let final_outputs = BTreeMap::from([((1, 0), &output)]);
        store
            .generation_ledger()
            .admit(END_PIPELINE, session.admission_order())
            .unwrap();
        let error = store
            .publish_and_commit_generation(
                &[(END_PIPELINE, &session)],
                &final_outputs,
                &halo,
                vec![((1, 0), output.clone())],
                &[],
                &BTreeSet::new(),
                &BTreeSet::from([(1, 0)]),
                &[],
            )
            .unwrap_err();

        assert!(matches!(error, GenerationPublicationCommitError::Commit(
            GenerationCommitError::RevisionConflict {
                coordinate: (2, 0), expected: 0, found: 1,
            }
        )));
        assert!(store.generation_ledger().output_column(END_PIPELINE, (1, 0)).is_none());
        assert!(store.resident_column(1, 0).is_none());
    }

    #[test]
    fn cohort_ledger_publication_rolls_back_when_the_cache_commit_conflicts() {
        use lodestone_worldgen::stage_schedule::END_PIPELINE;

        let mut session = end_shaped_session_at((0, 0), 1, 0);
        complete_end_suffix(&mut session, 2);
        let output = ChunkColumn::new(0, 16);
        let final_outputs = BTreeMap::from([((0, 0), &output)]);
        let mut ledger = GenerationLedger::new();
        ledger.admit(END_PIPELINE, session.admission_order()).unwrap();
        let before = ledger.stats();

        let error = ledger
            .publish_sessions_with_final_outputs_and_commit(
                &[(END_PIPELINE, &session)],
                &final_outputs,
                || {
                    Err(GenerationCommitError::RevisionConflict {
                        coordinate: (0, 0),
                        expected: 0,
                        found: 1,
                    })
                },
            )
            .unwrap_err();

        assert!(matches!(
            error,
            GenerationPublicationCommitError::Commit(
                GenerationCommitError::RevisionConflict { .. }
            )
        ));
        assert_eq!(ledger.stats(), before);
        assert!(ledger.output_column(END_PIPELINE, (0, 0)).is_none());
    }

    #[test]
    fn cohort_cache_coordinates_include_only_mutation_destinations() {
        use lodestone_worldgen::stage_schedule::{ColumnStage, Dimension, StageKey};

        let stage = StageKey::new(Dimension::Overworld, ColumnStage::Features);
        let target = (4, -3);
        let mutations = [
            ProvenanceMutation::test_block_state(
                target,
                target,
                stage,
                0,
                BlockCoordinate::new(65, 8, -47),
                1,
                Block::Stone.default_state(),
            ),
            ProvenanceMutation::test_block_state(
                target,
                (5, -3),
                stage,
                1,
                BlockCoordinate::new(80, 8, -47),
                1,
                Block::Dirt.default_state(),
            ),
            ProvenanceMutation::test_block_state(
                target,
                (4, -2),
                stage,
                2,
                BlockCoordinate::new(65, 8, -32),
                1,
                Block::Dirt.default_state(),
            ),
        ];

        assert_eq!(
            cohort_mutation_destination_coordinates(target, &mutations),
            BTreeSet::from([(4, -2), (5, -3)]),
        );
    }

    #[test]
    fn cohort_final_output_replaces_a_same_stage_interim_neighbour() {
        let store = ChunkStore::with_capacity(CountingSource::new(), 4);
        let coordinate = (0, 0);
        let mut interim = ChunkColumn::new(0, 16);
        interim.set_block_id(1, 4, 1, Block::Stone.default_state());
        let mut halo = store.lease_halo(&[coordinate]).unwrap();
        let report = store
            .commit_generation(&halo, vec![(coordinate, interim)])
            .unwrap();
        halo.acknowledge(&report);
        drop(halo);

        let mut finalized = ChunkColumn::new(0, 16);
        finalized.set_block_id(1, 4, 1, Block::Dirt.default_state());
        let halo = store.lease_halo(&[coordinate]).unwrap();
        store
            .commit_generation_with_mutations_and_persistence_destinations_and_receipts_policy(
                &halo,
                vec![(coordinate, finalized)],
                &[],
                &BTreeSet::new(),
                &BTreeSet::from([coordinate]),
                &[],
                GenerationCommitMode::Cohort,
            )
            .unwrap();

        assert_eq!(
            store.resident_column(0, 0).unwrap().block_state_id(1, 4, 1),
            Block::Dirt.default_state(),
        );
    }

    #[test]
    fn cancellation_after_a_committed_cohort_prefix_does_not_install_the_next_output() {
        use lodestone_worldgen::stage_schedule::{Dimension, END_PIPELINE, GenerationTarget};
        use crate::worldgen_session::{GenerationRequest, GenerationRequestError, GenerationRequestResult, RequestCancellation};

        let store = ChunkStore::with_capacity(CountingSource::new(), 4);
        let mut halo = store.lease_halo(&[(0, 0), (1, 0)]).unwrap();
        let first_session = GenerationSession::new(GenerationRequest::new(
            Dimension::End,
            (0, 0),
            GenerationTarget::Full,
            0,
        ));
        let first_entry = GenerationBatchEntry {
            index: 0,
            pipeline: END_PIPELINE,
            admitted: vec![(0, 0)],
        };
        store
            .commit_cohort_output(
                &mut halo,
                &first_entry,
                &first_session,
                &GenerationRequestResult::Existing(ChunkColumn::new(0, 16)),
            )
            .unwrap();

        let cancellation = RequestCancellation::new();
        cancellation.cancel();
        let cancelled_session = GenerationSession::with_cancellation(
            GenerationRequest::new(Dimension::End, (1, 0), GenerationTarget::Full, 0),
            cancellation,
        );
        let second_entry = GenerationBatchEntry {
            index: 1,
            pipeline: END_PIPELINE,
            admitted: vec![(1, 0)],
        };
        assert!(matches!(
            store.commit_cohort_output(
                &mut halo,
                &second_entry,
                &cancelled_session,
                &GenerationRequestResult::Existing(ChunkColumn::new(0, 16)),
            ),
            Err(GenerationRequestError::Session(SessionError::Cancelled)),
        ));
        assert!(store.resident_column(0, 0).is_some());
        assert!(store.resident_column(1, 0).is_none());
    }

    #[test]
    fn generation_commit_rejects_a_mutation_destination_outside_the_gathered_map() {
        use lodestone_worldgen::stage_schedule::{ColumnStage, Dimension, StageKey};

        let store = ChunkStore::with_capacity(CountingSource::new(), 4);
        let halo = store.lease_halo(&[(0, 0)]).unwrap();
        let mutation = ProvenanceMutation::test_block_state(
            (0, 0),
            (0, 0),
            StageKey::new(Dimension::Overworld, ColumnStage::Features),
            0,
            BlockCoordinate::new(16, 4, 0),
            1,
            Block::Dirt.default_state(),
        );

        let error = store
            .commit_generation_with_mutations(
                &halo,
                vec![((0, 0), ChunkColumn::new(0, 16))],
                std::slice::from_ref(&mutation),
            )
            .unwrap_err();
        assert_eq!(error, GenerationCommitError::MissingMutationDestination((1, 0)));
        assert_eq!(store.len(), 0, "the rejected mutation must not partly commit");
    }

    #[test]
    fn finalized_mutations_preserve_heightmaps_and_reject_a_wrong_output() {
        use lodestone_worldgen::stage_schedule::{ColumnStage, Dimension, StageKey};

        let destination = BlockCoordinate::new(0, 4, 0);
        let expected = Block::Dirt.default_state();
        let mutation = ProvenanceMutation::test_block_state(
            (0, 0),
            (0, 0),
            StageKey::new(Dimension::Overworld, ColumnStage::Features),
            0,
            destination,
            1,
            expected,
        );
        let raw_heightmaps = [[12; 256], [10; 256], [8; 256]];
        let finalized = BTreeSet::from([(0, 0)]);

        let store = ChunkStore::with_capacity(CountingSource::new(), 4);
        let halo = store.lease_halo(&[(0, 0)]).unwrap();
        let mut matching = ChunkColumn::new(0, 16);
        matching.set_block_id(0, 4, 0, expected);
        matching.install_client_heightmaps_raw(raw_heightmaps);
        store
            .commit_generation_with_finalized_mutations(
                &halo,
                vec![((0, 0), matching)],
                std::slice::from_ref(&mutation),
                &finalized,
            )
            .unwrap();
        assert_eq!(
            store.resident_column(0, 0).unwrap().client_heightmaps_raw(),
            Some(raw_heightmaps),
        );

        let rejecting_store = ChunkStore::with_capacity(CountingSource::new(), 4);
        let rejecting_halo = rejecting_store.lease_halo(&[(0, 0)]).unwrap();
        let mut mismatching = ChunkColumn::new(0, 16);
        mismatching.install_client_heightmaps_raw(raw_heightmaps);
        let error = rejecting_store
            .commit_generation_with_finalized_mutations(
                &rejecting_halo,
                vec![((0, 0), mismatching)],
                &[mutation],
                &finalized,
            )
            .unwrap_err();
        assert_eq!(
            error,
            GenerationCommitError::FinalizedMutationMismatch {
                coordinate: (0, 0),
                destination,
                expected,
                actual: Block::Air.default_state(),
            },
        );
        assert_eq!(rejecting_store.len(), 0);
    }

    #[test]
    fn halo_keeps_a_cold_revision_record_until_stale_commit_is_rejected() {
        let store = ChunkStore::with_capacity(CountingSource::new(), 1);
        let halo = store.lease_halo(&[(0, 0)]).unwrap();
        store.set_block(0, 0, 0, Block::Stone.default_state());

        assert_eq!(
            store.write_gates.state.lock().unwrap().len(),
            9,
            "the active cold halo must retain every invalidated revision record"
        );
        let error = store
            .commit_generation(&halo, vec![((0, 0), ChunkColumn::new(0, 16))])
            .unwrap_err();
        assert!(matches!(
            error,
            GenerationCommitError::RevisionConflict {
                coordinate: (0, 0),
                expected: 0,
                found: 1,
            }
        ));
        assert_eq!(store.len(), 0, "a stale cold commit must not create a resident");

        drop(halo);
        assert_eq!(
            store.write_gates.state.lock().unwrap().len(),
            9,
            "nonzero revisions must survive after the halo releases them"
        );
        let next_halo = store.lease_halo(&[(0, 0)]).unwrap();
        assert_eq!(next_halo.revision((0, 0)), Some(1));
    }

    #[test]
    fn absent_light_writes_do_not_retain_gate_records() {
        let store = ChunkStore::with_capacity(CountingSource::new(), 1);
        let fallback = ChunkColumn::new(0, 16);
        let snapshot = store
            .capture_light_snapshot(&[(0, 0)], (0, 0), &fallback, false)
            .unwrap();
        assert_eq!(store.write_gates.state.lock().unwrap().len(), 1);
        drop(snapshot);
        assert_eq!(store.write_gates.state.lock().unwrap().len(), 0);
        for coordinate in 0..64 {
            store.invalidate_retained_light_neighbourhood(coordinate * 3, 0);
        }
        for coordinate in 0..64 {
            store.store_resident_columns(&[(coordinate * 3, 0, ChunkColumn::new(0, 16))]);
        }
        assert_eq!(
            store.write_gates.state.lock().unwrap().len(),
            0,
            "absent multi-coordinate writes must not grow the gate table"
        );
    }

    #[test]
    fn ledger_deduplicates_sources_and_retains_products_after_session_loss() {
        use lodestone_worldgen::stage_schedule::{
            ColumnStage, Dimension, PipelineOptions, END_PIPELINE,
        };

        let mut ledger = GenerationLedger::with_limits(GenerationLedgerLimits {
            pipelines: 1,
            coordinates_per_pipeline: 4,
            source_completions_per_pipeline: 4,
            overlays_per_pipeline: 4,
            products_per_pipeline: 8,
            sidecars_per_pipeline: 4,
        });
        ledger.admit(END_PIPELINE, &[(0, 0)]).unwrap();
        let identity = END_PIPELINE.identity(PipelineOptions::ALL);
        let mut fill_completion = None;
        for &column_stage in END_PIPELINE.stages() {
            if column_stage == ColumnStage::Features {
                break;
            }
            let descriptor = END_PIPELINE
                .descriptor(column_stage)
                .expect("every End stage has a descriptor");
            let completion = ImmutableStageCompletion::new(
                (0, 0),
                StageKey::new(Dimension::End, column_stage),
                [1; 32],
                [2; 32],
                1,
                descriptor
                    .outputs()
                    .iter()
                    .copied()
                    .map(|resource| ImmutableProduct::new(resource, 7_u8))
                    .collect(),
                descriptor
                    .retained_sidecars()
                    .iter()
                    .copied()
                    .map(|sidecar| ImmutableSidecar::new(sidecar, 7_u8))
                    .collect(),
            );
            ledger.commit_immutable(END_PIPELINE, &completion).unwrap();
            if column_stage == ColumnStage::Fill {
                fill_completion = Some(completion);
            }
        }
        let features = StageKey::new(Dimension::End, ColumnStage::Features);
        assert!(ledger
            .complete_source(END_PIPELINE, (0, 0), (0, 0), features, 0)
            .unwrap());
        assert!(!ledger
            .complete_source(END_PIPELINE, (0, 0), (0, 0), features, 0)
            .unwrap());

        let pure_stages = END_PIPELINE
            .stages()
            .iter()
            .position(|stage| *stage == ColumnStage::Features)
            .expect("the End pipeline has a Features boundary");
        let expected_products = END_PIPELINE.stages()[..pure_stages]
            .iter()
            .map(|stage| END_PIPELINE.descriptor(*stage).unwrap().outputs().len())
            .sum::<usize>();
        let completion = fill_completion.expect("the End pipeline starts with Fill");
        let descriptor = END_PIPELINE
            .descriptor(ColumnStage::Fill)
            .expect("Fill has a descriptor");
        assert!(ledger
            .product(identity, (0, 0), completion.stage(), descriptor.outputs()[0])
            .unwrap()
            .is_some());
        let stats = ledger.stats();
        assert_eq!(stats.sources, 1);
        assert_eq!(stats.products, expected_products);
        assert!(stats.retained_bytes > 0);
        assert_eq!(ledger.frontier(identity, (0, 0)).unwrap().records().len(), pure_stages);
    }

    #[test]
    fn ledger_admission_evicts_a_closed_coordinate_before_extending() {
        use lodestone_worldgen::stage_schedule::END_PIPELINE;

        let mut ledger = GenerationLedger::with_limits(GenerationLedgerLimits {
            pipelines: 1,
            coordinates_per_pipeline: 2,
            source_completions_per_pipeline: 1,
            overlays_per_pipeline: 1,
            products_per_pipeline: 1,
            sidecars_per_pipeline: 1,
        });
        ledger.admit(END_PIPELINE, &[(0, 0)]).unwrap();
        let identity = END_PIPELINE.identity(PipelineOptions::ALL);
        assert_eq!(ledger.admit(END_PIPELINE, &[(1, 0), (2, 0)]), Ok(vec![(1, 0), (2, 0)]));
        assert!(matches!(
            ledger.frontier(identity, (0, 0)),
            Err(GenerationLedgerError::UnknownCoordinate((0, 0)))
        ));
        assert_eq!(ledger.stats().coordinates, 2);
    }

    fn end_shaped_session(
        fingerprint: u8,
    ) -> crate::worldgen_session::GenerationSession {
        end_shaped_session_with_radius(fingerprint, 0)
    }

    fn end_shaped_session_with_radius(
        fingerprint: u8,
        dependency_radius: u8,
    ) -> crate::worldgen_session::GenerationSession {
        end_shaped_session_at((0, 0), fingerprint, dependency_radius)
    }

    fn end_shaped_session_at(
        target: (i32, i32),
        fingerprint: u8,
        dependency_radius: u8,
    ) -> crate::worldgen_session::GenerationSession {
        use lodestone_worldgen::stage_schedule::Dimension;

        let request = crate::worldgen_session::GenerationRequest::new(
            Dimension::End,
            target,
            GenerationTarget::Full,
            dependency_radius,
        );
        let mut session = GenerationSession::new(request);
        import_end_shaped_prefix(&mut session, target, fingerprint);
        session
    }

    fn import_end_shaped_prefix(
        session: &mut GenerationSession,
        target: (i32, i32),
        fingerprint: u8,
    ) {
        use lodestone_worldgen::stage_schedule::END_PIPELINE;

        let boundary = END_PIPELINE
            .schedule()
            .target_stage(GenerationTarget::Shaped);
        session
            .import_aggregate_prefix(
                target,
                boundary,
                ImmutableProduct::new(ResourceKey::MaterializedWorld, ChunkColumn::new(0, 16)),
                [],
                [fingerprint; 32],
                [fingerprint; 32],
                1,
            )
            .expect("End shaped prefix fixture is valid");
    }

    #[test]
    fn ledger_accepts_a_compact_generated_shaped_prefix() {
        use lodestone_worldgen::stage_schedule::{Dimension, END_PIPELINE};

        let generated = crate::overworld_generator(42).column_shaped(0, 0);
        let request = crate::worldgen_session::GenerationRequest::new(
            Dimension::End,
            (0, 0),
            GenerationTarget::Full,
            0,
        );
        let boundary = END_PIPELINE
            .schedule()
            .target_stage(GenerationTarget::Shaped);
        let mut session = GenerationSession::new(request);
        session
            .import_aggregate_prefix(
                (0, 0),
                boundary,
                ImmutableProduct::new(ResourceKey::MaterializedWorld, generated),
                [],
                [1; 32],
                [1; 32],
                1,
            )
            .expect("compact shaped prefix is valid session state");

        let mut ledger = GenerationLedger::new();
        ledger.admit(END_PIPELINE, &[(0, 0)]).unwrap();
        ledger
            .publish_session(END_PIPELINE, &session)
            .expect("ledger accepts the compact shaped prefix used by production");
    }

    fn complete_end_suffix(
        session: &mut GenerationSession,
        fingerprint: u8,
    ) {
        complete_end_suffix_inner(session, fingerprint, None);
    }

    fn complete_end_suffix_with_mutation(
        session: &mut GenerationSession,
        fingerprint: u8,
        destination: BlockCoordinate,
    ) {
        complete_end_suffix_inner_with_state(
            session,
            fingerprint,
            Some(destination),
            Block::Stone.default_state(),
        );
    }

    fn complete_end_suffix_with_mutation_state(
        session: &mut GenerationSession,
        fingerprint: u8,
        destination: BlockCoordinate,
        state: StateId,
    ) {
        complete_end_suffix_inner_with_state(session, fingerprint, Some(destination), state);
    }

    fn complete_end_suffix_inner(
        session: &mut GenerationSession,
        fingerprint: u8,
        mutation_destination: Option<BlockCoordinate>,
    ) {
        complete_end_suffix_inner_with_state(
            session,
            fingerprint,
            mutation_destination,
            Block::Stone.default_state(),
        );
    }

    fn complete_end_suffix_inner_with_state(
        session: &mut GenerationSession,
        fingerprint: u8,
        mutation_destination: Option<BlockCoordinate>,
        mutation_state: StateId,
    ) {
        complete_end_suffix_inner_with_state_and_output(
            session,
            fingerprint,
            mutation_destination,
            mutation_state,
            None,
        );
    }

    fn complete_end_suffix_inner_with_state_and_output(
        session: &mut GenerationSession,
        fingerprint: u8,
        mutation_destination: Option<BlockCoordinate>,
        mutation_state: StateId,
        output_column: Option<ChunkColumn>,
    ) {
        use lodestone_worldgen::stage_schedule::{ColumnStage, Dimension};

        let target = session.request().target();
        let features = StageKey::new(Dimension::End, ColumnStage::Features);
        session
            .declare_mutable_sources(features, [(0, target)])
            .expect("End features source plan is valid");
        let mut transaction = session
            .begin_mutable_source(target, features, 0)
            .expect("End feature source is valid");
        if let Some(destination) = mutation_destination {
            transaction
                .push(0, destination, mutation_state)
                .expect("End feature spill is valid");
        }
        session
            .complete_mutable_source(transaction)
            .expect("End feature source commits");
        let descriptor = session
            .pipeline()
            .descriptor(ColumnStage::Features)
            .expect("End features descriptor exists");
        let products = descriptor
            .outputs()
            .iter()
            .copied()
            .map(|resource| ImmutableProduct::new(resource, fingerprint))
            .collect();
        let sidecars = descriptor
            .retained_sidecars()
            .iter()
            .copied()
            .map(|sidecar| ImmutableSidecar::new(sidecar, fingerprint))
            .collect();
        session
            .commit_mutable_stage(
                features,
                [fingerprint; 32],
                [fingerprint; 32],
                1,
                products,
                sidecars,
            )
            .expect("End features commit");

        let output = StageKey::new(Dimension::End, ColumnStage::Output);
        let descriptor = session
            .pipeline()
            .descriptor(ColumnStage::Output)
            .expect("End output descriptor exists");
        let products = descriptor
            .outputs()
            .iter()
            .copied()
            .map(|resource| {
                if resource == ResourceKey::OutputColumn {
                    ImmutableProduct::new(
                        resource,
                        output_column
                            .clone()
                            .unwrap_or_else(|| ChunkColumn::new(0, 16)),
                    )
                } else {
                    ImmutableProduct::new(resource, fingerprint)
                }
            })
            .collect();
        let sidecars = descriptor
            .retained_sidecars()
            .iter()
            .copied()
            .map(|sidecar| ImmutableSidecar::new(sidecar, fingerprint))
            .collect();
        session
            .complete_immutable(ImmutableStageCompletion::new(
                target,
                output,
                [fingerprint; 32],
                [fingerprint; 32],
                1,
                products,
                sidecars,
            ))
            .expect("End output completion");
        session
            .advance_ready_immutable()
            .expect("End output commits in order");
    }

    fn complete_end_features_with_settlement(
        session: &mut GenerationSession,
        target: (i32, i32),
    ) {
        use lodestone_worldgen::stage_schedule::{ColumnStage, Dimension};

        let features = StageKey::new(Dimension::End, ColumnStage::Features);
        session
            .declare_mutable_sources(features, [(0, target)])
            .expect("End feature source plan is valid");
        let transaction = session
            .begin_mutable_source(target, features, 0)
            .expect("End feature source is valid");
        session
            .complete_mutable_source(transaction)
            .expect("End feature source commits");
        let descriptor = session
            .pipeline()
            .descriptor(ColumnStage::Features)
            .expect("End features descriptor exists");
        session
            .commit_mutable_stage(
                features,
                [2; 32],
                [2; 32],
                1,
                descriptor
                    .outputs()
                    .iter()
                    .copied()
                    .map(|resource| ImmutableProduct::new(resource, 2_u8))
                    .collect(),
                descriptor
                    .retained_sidecars()
                    .iter()
                    .copied()
                    .map(|sidecar| ImmutableSidecar::new(sidecar, 2_u8))
                    .collect(),
            )
            .expect("End features commit");
        let owners = (-1..=1)
            .flat_map(|dx| (-1..=1).map(move |dz| (target.0 + dx, target.1 + dz)))
            .collect::<Vec<_>>();
        session
            .commit_target_feature_settlement(
                FeatureSettlementProof::square(target, 1),
                &owners,
                &[],
                &[],
                0,
            )
            .expect("settlement proof is tied to the committed feature stage");
    }

    fn complete_end_output(session: &mut GenerationSession, output_column: ChunkColumn) {
        use lodestone_worldgen::stage_schedule::{ColumnStage, Dimension};

        let output = StageKey::new(Dimension::End, ColumnStage::Output);
        let descriptor = session
            .pipeline()
            .descriptor(ColumnStage::Output)
            .expect("End output descriptor exists");
        session
            .complete_immutable(ImmutableStageCompletion::new(
                session.request().target(),
                output,
                [3; 32],
                [3; 32],
                1,
                descriptor
                    .outputs()
                    .iter()
                    .copied()
                    .map(|resource| {
                        let product = if resource == ResourceKey::OutputColumn {
                            ImmutableProduct::new(resource, output_column.clone())
                        } else {
                            ImmutableProduct::new(resource, 3_u8)
                        };
                        product
                    })
                    .collect(),
                descriptor
                    .retained_sidecars()
                    .iter()
                    .copied()
                    .map(|sidecar| ImmutableSidecar::new(sidecar, 3_u8))
                    .collect(),
            ))
            .expect("End output completion is valid");
        session
            .advance_ready_immutable()
            .expect("End output commits in order");
    }

    #[test]
    fn pending_feature_settlement_survives_ledger_checkpoint_until_output() {
        use lodestone_worldgen::stage_schedule::{ColumnStage, Dimension, END_PIPELINE};

        let target = (0, 0);
        let mut session = end_shaped_session_at(target, 1, 1);
        complete_end_features_with_settlement(&mut session, target);

        let mut ledger = GenerationLedger::new();
        ledger
            .admit(END_PIPELINE, session.admission_order())
            .expect("the feature proof halo is admitted");
        ledger
            .publish_session(END_PIPELINE, &session)
            .expect("a committed feature proof remains publishable before Output");

        let identity = END_PIPELINE.identity(PipelineOptions::ALL);
        let checkpoint = ledger
            .checkpoint(END_PIPELINE, session.request())
            .expect("pending feature receipt is included in the checkpoint");
        assert_eq!(checkpoint.feature_settlement(), session.feature_settlement());
        let mut restored = GenerationSession::from_checkpoint(checkpoint)
            .expect("FEATURES and its pending receipt restore without Output");
        let destination = BlockCoordinate::new(0, 4, 0);
        let replay = ProvenanceMutation::test_block_state(
            (1, 0),
            (1, 0),
            StageKey::new(Dimension::End, ColumnStage::Features),
            0,
            destination,
            1,
            Block::Dirt.default_state(),
        );
        assert!(!ledger
            .pipeline_ref(identity)
            .unwrap()
            .accepts_settled_feature_replay(target, &replay));

        let mut output = ChunkColumn::new(0, 16);
        output.set_block_id(0, 4, 0, Block::Stone.default_state());
        complete_end_output(&mut restored, output.clone());
        ledger
            .publish_sessions_with_final_outputs(
                &[(END_PIPELINE, &restored)],
                &BTreeMap::from([(target, &output)]),
            )
            .expect("the final output settles the pending feature receipt");
        let revision = ledger.revision(identity, target).unwrap();
        assert_eq!(
            ledger.commit_overlay(END_PIPELINE, target, revision, replay),
            Ok(revision),
            "only the matching finalized output may accept this replay",
        );
    }

    #[test]
    fn ledger_accepts_a_stale_shaped_publish_after_full_publication() {
        use lodestone_worldgen::stage_schedule::END_PIPELINE;

        let mut ledger = GenerationLedger::new();
        ledger.admit(END_PIPELINE, &[(0, 0)]).unwrap();
        let mut shaped = end_shaped_session(1);
        ledger.publish_session(END_PIPELINE, &shaped).unwrap();

        let checkpoint = ledger
            .checkpoint(END_PIPELINE, shaped.request())
            .expect("published shaped checkpoint");
        let mut full = GenerationSession::from_checkpoint(checkpoint).unwrap();
        complete_end_suffix(&mut full, 2);
        ledger.publish_session(END_PIPELINE, &full).unwrap();

        let identity = END_PIPELINE.identity(PipelineOptions::ALL);
        let full_len = END_PIPELINE.stages().len();
        assert_eq!(ledger.frontier(identity, (0, 0)).unwrap().records().len(), full_len);
        let revision = ledger.revision(identity, (0, 0)).unwrap();

        ledger
            .publish_session(END_PIPELINE, &shaped)
            .expect("compatible stale shaped prefix is an idempotent publication");
        assert_eq!(ledger.frontier(identity, (0, 0)).unwrap().records().len(), full_len);
        assert_eq!(ledger.revision(identity, (0, 0)).unwrap(), revision);
        shaped.cancel();
    }

    #[test]
    fn ledger_retains_cross_column_mutation_after_target_output_publication() {
        use lodestone_worldgen::stage_schedule::END_PIPELINE;

        let target = (0, 0);
        let destination = BlockCoordinate::new(16, 70, 0);
        let session_request = crate::worldgen_session::GenerationRequest::new(
            Dimension::End,
            target,
            GenerationTarget::Full,
            1,
        );
        let mut session = GenerationSession::new(session_request);
        let boundary = END_PIPELINE
            .schedule()
            .target_stage(GenerationTarget::Shaped);
        session
            .import_aggregate_prefix(
                target,
                boundary,
                ImmutableProduct::new(ResourceKey::MaterializedWorld, ChunkColumn::new(0, 16)),
                [],
                [1; 32],
                [1; 32],
                1,
            )
            .expect("End shaped prefix fixture is valid");
        complete_end_suffix_with_mutation(&mut session, 2, destination);

        let mut ledger = GenerationLedger::new();
        ledger
            .admit(END_PIPELINE, session.admission_order())
            .expect("the mutation destination is admitted in the request halo");
        ledger
            .publish_session(END_PIPELINE, &session)
            .expect("cross-column mutation remains publishable after target output");

        let identity = END_PIPELINE.identity(PipelineOptions::ALL);
        assert_eq!(ledger.stats().overlays, 1);
        let checkpoint = ledger
            .checkpoint(END_PIPELINE, session.request())
            .expect("cross-column overlay is checkpointed for the halo");
        assert_eq!(checkpoint.committed_mutations().len(), 1);
        assert_eq!(
            checkpoint.committed_mutations()[0].provenance().destination(),
            destination
        );
        let restored = GenerationSession::from_checkpoint(checkpoint)
            .expect("cross-column overlay remains valid during session restore");
        assert_eq!(restored.committed_mutations().count(), 1);
        ledger
            .publish_session(END_PIPELINE, &restored)
            .expect("restored cross-column overlay remains idempotent");
        assert_eq!(ledger.stats().overlays, 1);
        assert!(ledger.revision(identity, (1, 0)).is_ok());

        let adjacent_request = crate::worldgen_session::GenerationRequest::new(
            Dimension::End,
            (2, 0),
            GenerationTarget::Full,
            1,
        );
        let adjacent = GenerationSession::new(adjacent_request);
        ledger
            .admit(END_PIPELINE, adjacent.admission_order())
            .expect("the adjacent request halo is admitted");
        let adjacent_checkpoint = ledger
            .checkpoint(END_PIPELINE, adjacent.request())
            .expect("the edge overlay is checkpointed by destination");
        assert_eq!(adjacent_checkpoint.committed_mutations().len(), 1);
        GenerationSession::from_checkpoint(adjacent_checkpoint)
            .expect("a retained foreign source may sit outside the new request halo");
    }

    #[test]
    fn ledger_settles_completed_destination_and_rejects_late_spills() {
        use lodestone_worldgen::stage_schedule::END_PIPELINE;

        let destination = (1, 0);
        let destination_block = BlockCoordinate::new(16, 4, 0);
        let mut ledger = GenerationLedger::with_limits(GenerationLedgerLimits {
            pipelines: 1,
            coordinates_per_pipeline: 32,
            source_completions_per_pipeline: 32,
            overlays_per_pipeline: 1,
            products_per_pipeline: 32,
            sidecars_per_pipeline: 32,
        });

        let mut source = end_shaped_session_at((0, 0), 1, 1);
        complete_end_suffix_with_mutation(&mut source, 2, destination_block);
        ledger
            .admit(END_PIPELINE, source.admission_order())
            .expect("the source target and its destination are admitted");
        ledger
            .publish_session(END_PIPELINE, &source)
            .expect("the leading-edge spill is retained before its destination output");
        assert_eq!(ledger.stats().overlays, 1);
        assert_eq!(
            ledger.settle_mutations(END_PIPELINE, &[], false, &[destination]),
            0,
            "a destination without an output product cannot retire its spill"
        );
        assert_eq!(ledger.stats().overlays, 1);

        let mut destination_session = end_shaped_session_at(destination, 3, 1);
        let mut destination_output = ChunkColumn::new(0, 16);
        destination_output.set_block_id(
            0,
            4,
            0,
            Block::Stone.default_state(),
        );
        complete_end_suffix_inner_with_state_and_output(
            &mut destination_session,
            4,
            None,
            StateId::AIR,
            Some(destination_output),
        );
        ledger
            .admit(END_PIPELINE, destination_session.admission_order())
            .expect("the completed destination request extends the line by one halo");
        ledger
            .publish_session(END_PIPELINE, &destination_session)
            .expect("the destination output is published");
        assert_eq!(
            ledger.settle_mutations(END_PIPELINE, &[], false, &[destination]),
            1,
            "the destination output verifies and retires its canonical spill"
        );
        assert_eq!(ledger.stats().overlays, 0);
        let output = ledger
            .output_column(END_PIPELINE, destination)
            .expect("the completed destination remains available after settlement");
        assert_eq!(output.block_state_id(0, 4, 0), Block::Stone.default_state());

        let identity = END_PIPELINE.identity(PipelineOptions::ALL);
        let mut duplicate = end_shaped_session_at((0, 0), 1, 1);
        complete_end_suffix_with_mutation(&mut duplicate, 2, destination_block);
        ledger
            .publish_session(END_PIPELINE, &duplicate)
            .expect("the same output state is an idempotent no-op");
        assert_eq!(ledger.stats().overlays, 0);

        let later_target = (2, 0);
        let later_block = BlockCoordinate::new(16, 5, 0);
        let mut conflicting = end_shaped_session_at(later_target, 8, 1);
        complete_end_suffix_with_mutation_state(
            &mut conflicting,
            9,
            destination_block,
            Block::Dirt.default_state(),
        );
        ledger
            .admit(END_PIPELINE, conflicting.admission_order())
            .expect("the conflicting source target is admitted");
        assert_eq!(
            ledger.publish_session(END_PIPELINE, &conflicting),
            Err(GenerationLedgerError::CheckpointMismatch)
        );
        assert_eq!(ledger.stats().overlays, 0);

        let mut later_same_state = end_shaped_session_at(later_target, 6, 1);
        complete_end_suffix_with_mutation(
            &mut later_same_state,
            7,
            destination_block,
        );
        ledger
            .admit(END_PIPELINE, later_same_state.admission_order())
            .expect("the later source target is admitted");
        ledger
            .publish_session(END_PIPELINE, &later_same_state)
            .expect("a higher-provenance identical write is a no-op");
        assert_eq!(ledger.stats().overlays, 0);

        let mut later_spill = end_shaped_session_at(later_target, 6, 1);
        complete_end_suffix_with_mutation(&mut later_spill, 7, later_block);
        assert_eq!(
            ledger.publish_session(END_PIPELINE, &later_spill),
            Err(GenerationLedgerError::CheckpointMismatch)
        );
        assert_eq!(ledger.stats().overlays, 0);
        assert_eq!(
            ledger
                .output_column(END_PIPELINE, destination)
                .expect("settled output remains authoritative")
                .block_state_id(0, 4, 0),
            Block::Stone.default_state()
        );
        assert!(ledger.revision(identity, destination).unwrap() > 0);
    }

    #[test]
    fn ledger_accepts_only_replays_covered_by_reverse_settlement() {
        use lodestone_worldgen::stage_schedule::{Dimension, OVERWORLD_PIPELINE};

        let destination = (0, 0);
        let destination_block = BlockCoordinate::new(15, 4, 0);
        let mut output = ChunkColumn::new(0, 16);
        output.set_block_id(15, 4, 0, Block::Stone.default_state());

        let mut ledger = GenerationLedger::new();
        ledger
            .admit(OVERWORLD_PIPELINE, &[destination, (1, 0), (2, 0)])
            .expect("the settlement fixture is admitted");
        let identity = OVERWORLD_PIPELINE.identity(PipelineOptions::ALL);
        let state = ledger
            .pipelines
            .get_mut(&identity)
            .expect("the Overworld pipeline is admitted");
        let covered = ProvenanceMutation::test_block_state(
            (1, 0),
            (1, 0),
            StageKey::new(Dimension::Overworld, ColumnStage::Features),
            0,
            destination_block,
            1,
            Block::Dirt.default_state(),
        );
        state.feature_settlements.insert(
            destination,
            FeatureSettlementProof::square(destination, 1),
        );
        assert!(!state.accepts_settled_feature_replay(destination, &covered));
        state.products.insert(
            crate::worldgen_session::ProductKey::new(
                destination,
                StageKey::new(Dimension::Overworld, ColumnStage::Output),
                ResourceKey::OutputColumn,
            ),
            ImmutableProduct::new(ResourceKey::OutputColumn, output),
        );
        assert!(state.accepts_settled_feature_replay(destination, &covered));

        let revision = ledger.revision(identity, destination).unwrap();
        assert_eq!(
            ledger.commit_overlay(OVERWORLD_PIPELINE, destination, revision, covered),
            Ok(revision)
        );

        let wrong_source = ProvenanceMutation::test_block_state(
            (1, 0),
            destination,
            StageKey::new(Dimension::Overworld, ColumnStage::Features),
            0,
            destination_block,
            1,
            Block::Dirt.default_state(),
        );
        assert_eq!(
            ledger.commit_overlay(OVERWORLD_PIPELINE, destination, revision, wrong_source),
            Err(GenerationLedgerError::CheckpointMismatch)
        );

        let outside = ProvenanceMutation::test_block_state(
            (2, 0),
            (2, 0),
            StageKey::new(Dimension::Overworld, ColumnStage::Features),
            0,
            destination_block,
            1,
            Block::Dirt.default_state(),
        );
        assert_eq!(
            ledger.commit_overlay(OVERWORLD_PIPELINE, destination, revision, outside),
            Err(GenerationLedgerError::CheckpointMismatch)
        );
        assert_eq!(ledger.stats().overlays, 0);
        assert_eq!(
            ledger
                .output_column(OVERWORLD_PIPELINE, destination)
                .unwrap()
                .block_state_id(15, 4, 0),
            Block::Stone.default_state()
        );
    }

    #[test]
    fn ledger_batch_publication_folds_multi_direction_spills_before_a_future_request() {
        use lodestone_worldgen::stage_schedule::END_PIPELINE;

        let west_block = BlockCoordinate::new(0, 4, 0);
        let east_block = BlockCoordinate::new(16, 4, 0);
        let mut west_output = ChunkColumn::new(0, 16);
        west_output.set_block_id(0, 4, 0, Block::Stone.default_state());
        let mut east_output = ChunkColumn::new(0, 16);
        east_output.set_block_id(0, 4, 0, Block::Stone.default_state());

        let mut west = end_shaped_session_at((0, 0), 1, 1);
        complete_end_suffix_inner_with_state_and_output(
            &mut west,
            2,
            Some(east_block),
            Block::Stone.default_state(),
            Some(west_output.clone()),
        );
        let mut east = end_shaped_session_at((1, 0), 3, 1);
        complete_end_suffix_inner_with_state_and_output(
            &mut east,
            4,
            Some(west_block),
            Block::Stone.default_state(),
            Some(east_output.clone()),
        );

        let mut ledger = GenerationLedger::new();
        let mut admissions = west.admission_order().to_vec();
        admissions.extend(east.admission_order());
        ledger.admit(END_PIPELINE, &admissions).unwrap();
        let final_outputs = BTreeMap::from([
            ((0, 0), &west_output),
            ((1, 0), &east_output),
        ]);
        ledger
            .publish_sessions_with_final_outputs(
                &[(END_PIPELINE, &west), (END_PIPELINE, &east)],
                &final_outputs,
            )
            .expect("both target outputs authenticate folded reverse spills");
        assert_eq!(ledger.stats().overlays, 0);
        assert_eq!(
            ledger
                .output_column(END_PIPELINE, (0, 0))
                .unwrap()
                .block_state_id(0, 4, 0),
            Block::Stone.default_state()
        );
        assert_eq!(
            ledger
                .output_column(END_PIPELINE, (1, 0))
                .unwrap()
                .block_state_id(0, 4, 0),
            Block::Stone.default_state()
        );

        let future_block = BlockCoordinate::new(48, 5, 0);
        let mut future = end_shaped_session_at((2, 0), 5, 1);
        complete_end_suffix_with_mutation(&mut future, 6, future_block);
        ledger
            .admit(END_PIPELINE, future.admission_order())
            .expect("the future target can retain an outside-output spill");
        ledger
            .publish_session(END_PIPELINE, &future)
            .expect("future non-finalized mutation keeps its provenance");
        assert_eq!(ledger.stats().overlays, 1);
        assert_eq!(
            ledger
                .checkpoint(END_PIPELINE, future.request())
                .unwrap()
                .committed_mutations()
                .len(),
            1
        );
    }

    #[test]
    fn overworld_small_batches_resume_overlapping_frontiers() {
        let store = ChunkStore::new(crate::overworld_chunk_source(42));
        let mut first_row = (0..16)
            .map(|x| {
                GenerationSession::new(crate::worldgen_session::GenerationRequest::new(
                    Dimension::Overworld,
                    (20_000 + x, -20_000),
                    GenerationTarget::Full,
                    1,
                ))
            })
            .collect::<Vec<_>>();

        let first_results = ChunkSource::request_generation_batch(&store, &mut first_row);
        assert_eq!(first_results.len(), first_row.len());
        for (index, result) in first_results.into_iter().enumerate() {
            assert!(
                result.as_ref().is_ok_and(Option::is_some),
                "first-row target {:?} failed: {result:?}",
                first_row[index].request().target(),
            );
        }

        let target = (20_000, -19_999);
        let destination = BlockCoordinate::new(320_013, 50, -319_984);
        let prior_spill = first_row[1]
            .committed_mutations()
            .find(|mutation| mutation.provenance().destination() == destination)
            .expect("the east target must have committed the boundary spill");
        assert_eq!(
            (
                prior_spill.provenance().target(),
                prior_spill.provenance().source(),
                prior_spill.provenance().ordinal(),
            ),
            ((20_001, -20_000), (20_001, -20_000), 973),
        );
        let prior_state = *prior_spill
            .get::<StateId>()
            .expect("feature spill is a block-state mutation");
        let direct = crate::overworld_chunk_source(42).column(target.0, target.1);
        let direct_state = direct.block_state_id(13, 50, 0);
        assert_ne!(
            prior_state, direct_state,
            "the independent cold scalar output must supersede the earlier target's spill"
        );
        let mut next_row = vec![GenerationSession::new(
            crate::worldgen_session::GenerationRequest::new(
                Dimension::Overworld,
                target,
                GenerationTarget::Full,
                1,
            ),
        )];
        let results = ChunkSource::request_generation_batch(&store, &mut next_row);
        assert_eq!(results.len(), 1);
        assert!(
            results[0].as_ref().is_ok_and(Option::is_some),
            "overlapping target {target:?} failed: {:?}",
            results[0],
        );
        let receipt = next_row[0]
            .feature_winner_receipts()
            .copied()
            .find(|receipt| receipt.destination() == destination)
            .expect("the target-owned winner is retained through output commit");
        assert_eq!(receipt.owner(), target);
        assert_eq!(receipt.state(), direct_state);
        let output_state = match results[0].as_ref().unwrap().as_ref().unwrap() {
            crate::worldgen_session::GenerationRequestResult::Existing(column) => {
                column.block_state_id(13, 50, 0)
            }
            crate::worldgen_session::GenerationRequestResult::Generated(snapshot) => {
                snapshot.column().block_state_id(13, 50, 0)
            }
        };
        assert_eq!(output_state, receipt.state());
        let checkpoint = store
            .generation_ledger()
            .checkpoint(
                lodestone_worldgen::stage_schedule::OVERWORLD_PIPELINE,
                crate::worldgen_session::GenerationRequest::new(
                    Dimension::Overworld,
                    target,
                    GenerationTarget::Full,
                    1,
                ),
            )
            .expect("the finalized target remains checkpointable");
        assert!(checkpoint
            .feature_winner_receipts()
            .iter()
            .all(|receipt| receipt.destination() != destination));
        assert!(checkpoint
            .committed_mutations()
            .iter()
            .all(|mutation| mutation.provenance().destination() != destination));
    }

    #[test]
    fn ledger_batch_publication_rejects_a_wrong_final_output_before_partial_publish() {
        use lodestone_worldgen::stage_schedule::END_PIPELINE;

        let destination = BlockCoordinate::new(0, 4, 0);
        let mut source = end_shaped_session_at((1, 0), 1, 1);
        complete_end_suffix_with_mutation(&mut source, 2, destination);
        let mut wrong_output = ChunkColumn::new(0, 16);
        wrong_output.set_block_id(0, 4, 0, Block::Dirt.default_state());

        let mut ledger = GenerationLedger::new();
        ledger.admit(END_PIPELINE, source.admission_order()).unwrap();
        let before = ledger.stats();
        let final_outputs = BTreeMap::from([((0, 0), &wrong_output)]);
        let result = ledger.publish_sessions_with_final_outputs(
            &[(END_PIPELINE, &source)],
            &final_outputs,
        );
        assert_eq!(result, Err(GenerationLedgerError::CheckpointMismatch));
        assert_eq!(ledger.stats(), before);
        assert!(ledger
            .output_column(END_PIPELINE, (1, 0))
            .is_none());
        assert_eq!(ledger.stats().overlays, 0);
    }

    #[test]
    fn feature_settlement_receipt_cannot_override_a_higher_priority_mutation() {
        use lodestone_worldgen::stage_schedule::{ColumnStage, Dimension, END_PIPELINE};

        let destination = BlockCoordinate::new(16, 4, 0);
        let stage = StageKey::new(Dimension::End, ColumnStage::Features);
        let mutation = ProvenanceMutation::test_block_state(
            (0, 0),
            (0, 0),
            stage,
            0,
            destination,
            1,
            Block::Stone.default_state(),
        );
        let receipt = TargetFeatureWrite::new(
            (1, 0),
            (1, 0),
            0,
            destination,
            Block::Dirt.default_state(),
        );
        let audit = FinalizedMutationAudit {
            winners: BTreeMap::from([(destination, (END_PIPELINE, &mutation))]),
            settled_winners: BTreeMap::from([(destination, (END_PIPELINE, receipt))]),
            conflicting_receipts: false,
        };

        assert_eq!(
            validate_finalized_mutation_audit(&audit, |coordinate, candidate| {
                assert_eq!(coordinate, (1, 0));
                assert_eq!(candidate, destination);
                Some(Block::Dirt.default_state())
            }),
            Err(GenerationCommitError::FinalizedMutationMismatch {
                coordinate: (1, 0),
                destination,
                expected: Block::Stone.default_state(),
                actual: Block::Dirt.default_state(),
            }),
        );
        assert_eq!(
            ChunkStore::<BatchStoreSource>::validate_finalized_mutation_winners_with_receipts(
                &ChunkStore::<BatchStoreSource>::finalized_mutation_winners(
                    std::slice::from_ref(&mutation),
                    &BTreeSet::from([(1, 0)]),
                ),
                &[receipt],
                |_, _| Some(Block::Dirt.default_state()),
            ),
            Err(GenerationCommitError::FinalizedMutationMismatch {
                coordinate: (1, 0),
                destination,
                expected: Block::Stone.default_state(),
                actual: Block::Dirt.default_state(),
            }),
        );
    }

    #[test]
    fn ledger_evicts_closed_coordinate_bundles_and_revisits_regenerate() {
        use lodestone_worldgen::stage_schedule::{Dimension, END_PIPELINE};

        let mut ledger = GenerationLedger::with_limits(GenerationLedgerLimits {
            pipelines: 1,
            coordinates_per_pipeline: 12,
            source_completions_per_pipeline: 32,
            overlays_per_pipeline: 4,
            products_per_pipeline: 64,
            sidecars_per_pipeline: 64,
        });
        let mut maximum_mutation_records = 0;
        for x in 0..12 {
            let target = (x, 0);
            let request = crate::worldgen_session::GenerationRequest::new(
                Dimension::End,
                target,
                GenerationTarget::Full,
                1,
            );
            let admission = GenerationSession::new(request);
            ledger
                .admit(END_PIPELINE, admission.admission_order())
                .expect("the next target and its halo are admitted");
            let checkpoint = ledger
                .checkpoint(END_PIPELINE, request)
                .expect("incoming destination mutations are checkpointed");
            let mut session = GenerationSession::from_checkpoint(checkpoint)
                .expect("the target resumes from its retained incoming mutations");
            import_end_shaped_prefix(&mut session, target, 1);
            let incoming_mutations = session.committed_mutations().cloned().collect::<Vec<_>>();
            let mut output = ChunkColumn::new(0, 16);
            for mutation in &incoming_mutations {
                let destination = mutation.provenance().destination();
                if (destination.x().div_euclid(16), destination.z().div_euclid(16)) == target {
                    output.set_block_id(
                        destination.x().rem_euclid(16),
                        destination.y(),
                        destination.z().rem_euclid(16),
                        *mutation
                            .get::<StateId>()
                            .expect("block mutation contains a StateId"),
                    );
                }
            }
            complete_end_suffix_inner_with_state_and_output(
                &mut session,
                2,
                Some(BlockCoordinate::new((x + 1) * 16, 4, 0)),
                Block::Stone.default_state(),
                Some(output),
            );
            ledger
                .publish_session(END_PIPELINE, &session)
                .expect("the line target publishes");
            ledger.settle_mutations(END_PIPELINE, &incoming_mutations, false, &[target]);
            let stats = ledger.stats();
            maximum_mutation_records = maximum_mutation_records.max(stats.overlays);
            assert!(stats.coordinates <= 12);
            assert!(stats.products <= 64);
            assert!(stats.overlays <= 4);
        }
        assert!(maximum_mutation_records > 0);
        assert!(ledger.output_column(END_PIPELINE, (0, 0)).is_none());

        let source = ResumableSource {
            driver: ResumableDriver::new(false),
        };
        let calls = Arc::clone(&source.driver.calls);
        let store = ChunkStore::with_capacity(source, 1);
        {
            let mut ledger = store.generation_ledger();
            ledger.limits = GenerationLedgerLimits {
                pipelines: 1,
                coordinates_per_pipeline: 2,
                source_completions_per_pipeline: 16,
                overlays_per_pipeline: 16,
                products_per_pipeline: 64,
                sidecars_per_pipeline: 64,
            };
        }

        for x in 0..8 {
            let request = crate::worldgen_session::GenerationRequest::new(
                Dimension::End,
                (x, 0),
                GenerationTarget::Full,
                0,
            );
            let mut session = GenerationSession::new(request);
            let result = store
                .execute_generation_session(&mut session)
                .expect("bounded ledger generation succeeds");
            assert!(matches!(
                result,
                crate::worldgen_session::GenerationRequestResult::Generated(_)
            ));
            let stats = store.generation_ledger().stats();
            assert!(stats.coordinates <= 2);
            assert!(stats.products <= 64);
            assert!(stats.overlays <= 16);
        }

        let stats = store.generation_ledger().stats();
        assert_eq!(stats.coordinates, 2);
        assert!(store.resident_column(7, 0).is_some());
        assert!(store
            .generation_ledger()
            .output_column(END_PIPELINE, (0, 0))
            .is_none());

        let before_cache_hit = calls.load(Ordering::Relaxed);
        let cache_request = crate::worldgen_session::GenerationRequest::new(
            Dimension::End,
            (7, 0),
            GenerationTarget::Full,
            0,
        );
        let mut cache_session = GenerationSession::new(cache_request);
        assert!(matches!(
            store
                .execute_generation_session(&mut cache_session)
                .expect("the newest resident column remains authoritative"),
            crate::worldgen_session::GenerationRequestResult::Existing(_)
        ));
        assert_eq!(calls.load(Ordering::Relaxed), before_cache_hit);

        let before_revisit = calls.load(Ordering::Relaxed);
        let request = crate::worldgen_session::GenerationRequest::new(
            Dimension::End,
            (0, 0),
            GenerationTarget::Full,
            0,
        );
        let mut session = GenerationSession::new(request);
        let result = store
            .execute_generation_session(&mut session)
            .expect("an evicted coordinate can be regenerated");
        assert!(matches!(
            result,
            crate::worldgen_session::GenerationRequestResult::Generated(_)
        ));
        assert_eq!(calls.load(Ordering::Relaxed), before_revisit + 1);
        assert_eq!(store.generation_ledger().stats().coordinates, 2);
    }

    #[test]
    fn ledger_rejects_a_divergent_suffix_without_mutating_advanced_state() {
        use lodestone_worldgen::stage_schedule::{PipelineOptions, END_PIPELINE};

        let mut ledger = GenerationLedger::new();
        ledger.admit(END_PIPELINE, &[(0, 0)]).unwrap();
        let shaped = end_shaped_session(1);
        ledger.publish_session(END_PIPELINE, &shaped).unwrap();
        let checkpoint = ledger
            .checkpoint(END_PIPELINE, shaped.request())
            .expect("published shaped checkpoint");
        let mut full = GenerationSession::from_checkpoint(checkpoint.clone()).unwrap();
        complete_end_suffix(&mut full, 2);
        ledger.publish_session(END_PIPELINE, &full).unwrap();

        let identity = END_PIPELINE.identity(PipelineOptions::ALL);
        let before = ledger.frontier(identity, (0, 0)).unwrap().records().to_vec();
        let before_revision = ledger.revision(identity, (0, 0)).unwrap();
        let before_stats = ledger.stats();
        let divergent_aggregate = end_shaped_session(8);
        assert_eq!(
            ledger.publish_session(END_PIPELINE, &divergent_aggregate),
            Err(GenerationLedgerError::CheckpointMismatch)
        );
        assert_eq!(ledger.frontier(identity, (0, 0)).unwrap().records(), before);
        assert_eq!(ledger.revision(identity, (0, 0)).unwrap(), before_revision);
        assert_eq!(ledger.stats(), before_stats);

        let mut divergent = GenerationSession::from_checkpoint(checkpoint).unwrap();
        complete_end_suffix(&mut divergent, 9);
        assert_eq!(
            ledger.publish_session(END_PIPELINE, &divergent),
            Err(GenerationLedgerError::CheckpointMismatch)
        );
        assert_eq!(ledger.frontier(identity, (0, 0)).unwrap().records(), before);
        assert_eq!(ledger.revision(identity, (0, 0)).unwrap(), before_revision);
        assert_eq!(ledger.stats(), before_stats);
    }

    #[test]
    fn ledger_capacity_rejection_rolls_back_every_touched_entry() {
        use lodestone_worldgen::stage_schedule::{ColumnStage, PipelineOptions, END_PIPELINE};

        let features_outputs = END_PIPELINE
            .descriptor(ColumnStage::Features)
            .expect("End features descriptor exists")
            .outputs()
            .len();
        let mut ledger = GenerationLedger::with_limits(GenerationLedgerLimits {
            pipelines: 1,
            coordinates_per_pipeline: 1,
            source_completions_per_pipeline: 1,
            overlays_per_pipeline: 1,
            products_per_pipeline: 1 + features_outputs,
            sidecars_per_pipeline: 16,
        });
        ledger.admit(END_PIPELINE, &[(0, 0)]).unwrap();
        let identity = END_PIPELINE.identity(PipelineOptions::ALL);
        let before_stats = ledger.stats();
        let before_last_used = ledger.pipelines[&identity].last_used;
        let mut full = end_shaped_session(1);
        complete_end_suffix(&mut full, 2);
        let before_checkpoint = ledger
            .checkpoint(END_PIPELINE, full.request())
            .expect("admitted empty checkpoint");

        assert_eq!(
            ledger.publish_session(END_PIPELINE, &full),
            Err(GenerationLedgerError::ProductCapacity)
        );
        assert_eq!(ledger.stats(), before_stats);
        assert_eq!(ledger.pipelines[&identity].last_used, before_last_used);
        let after_checkpoint = ledger
            .checkpoint(END_PIPELINE, full.request())
            .expect("checkpoint survives rejected publication");
        assert_eq!(after_checkpoint.frontiers(), before_checkpoint.frontiers());
        assert_eq!(
            after_checkpoint
                .products()
                .iter()
                .map(|(key, product)| (*key, product.resource(), product.retained_bytes()))
                .collect::<Vec<_>>(),
            before_checkpoint
                .products()
                .iter()
                .map(|(key, product)| (*key, product.resource(), product.retained_bytes()))
                .collect::<Vec<_>>()
        );
        assert_eq!(
            after_checkpoint
                .sidecars()
                .iter()
                .map(|(key, sidecar)| (*key, sidecar.sidecar(), sidecar.retained_bytes()))
                .collect::<Vec<_>>(),
            before_checkpoint
                .sidecars()
                .iter()
                .map(|(key, sidecar)| (*key, sidecar.sidecar(), sidecar.retained_bytes()))
                .collect::<Vec<_>>()
        );
        assert_eq!(
            after_checkpoint
                .aggregates()
                .iter()
                .map(|(coordinate, aggregate)| {
                    (
                        *coordinate,
                        aggregate.boundary(),
                        aggregate.product().resource(),
                        aggregate.product().retained_bytes(),
                        aggregate.input_fingerprint(),
                        aggregate.output_fingerprint(),
                        aggregate.executor_version(),
                    )
                })
                .collect::<Vec<_>>(),
            before_checkpoint
                .aggregates()
                .iter()
                .map(|(coordinate, aggregate)| {
                    (
                        *coordinate,
                        aggregate.boundary(),
                        aggregate.product().resource(),
                        aggregate.product().retained_bytes(),
                        aggregate.input_fingerprint(),
                        aggregate.output_fingerprint(),
                        aggregate.executor_version(),
                    )
                })
                .collect::<Vec<_>>()
        );
        assert_eq!(after_checkpoint.source_completions(), before_checkpoint.source_completions());
        assert_eq!(after_checkpoint.committed_mutation_order(), before_checkpoint.committed_mutation_order());
        assert_eq!(after_checkpoint.current_revision(), before_checkpoint.current_revision());
        assert_eq!(ledger.frontier(identity, (0, 0)).unwrap().records(), before_checkpoint.frontiers()[0].1);
    }

    #[test]
    fn generation_regions_serialize_overlapping_requests() {
        let coordinator = Arc::new(GenerationRegionCoordinator::default());
        let cancellation = crate::worldgen_session::RequestCancellation::new();
        let first = coordinator.acquire(&[(0, 0)], &cancellation).unwrap();
        let (queued, queued_rx) = std::sync::mpsc::channel();
        let (finished, finished_rx) = std::sync::mpsc::channel();
        let waiter = Arc::clone(&coordinator);
        let thread = std::thread::spawn(move || {
            queued.send(()).expect("test receiver is alive");
            let _second = waiter
                .acquire(&[(0, 0)], &crate::worldgen_session::RequestCancellation::new())
                .unwrap();
            finished.send(()).expect("test receiver is alive");
        });
        queued_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("overlapping request was submitted");
        assert!(finished_rx.try_recv().is_err());
        drop(first);
        finished_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("overlapping request was released in FIFO order");
        thread.join().expect("generation region waiter did not panic");
    }

    #[test]
    fn generation_regions_keep_disjoint_requests_parallel() {
        let coordinator = Arc::new(GenerationRegionCoordinator::default());
        let cancellation = crate::worldgen_session::RequestCancellation::new();
        let first = coordinator.acquire(&[(0, 0)], &cancellation).unwrap();
        let (finished, finished_rx) = std::sync::mpsc::channel();
        let waiter = Arc::clone(&coordinator);
        let thread = std::thread::spawn(move || {
            let _second = waiter
                .acquire(&[(4, 0)], &crate::worldgen_session::RequestCancellation::new())
                .unwrap();
            finished.send(()).expect("test receiver is alive");
        });
        finished_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("disjoint request must not wait for the first region");
        drop(first);
        thread.join().expect("generation region waiter did not panic");
    }

    #[test]
    fn cancelled_overlapping_request_leaves_the_region_queue() {
        let coordinator = Arc::new(GenerationRegionCoordinator::default());
        let first_cancel = crate::worldgen_session::RequestCancellation::new();
        let first = coordinator.acquire(&[(0, 0)], &first_cancel).unwrap();
        let cancellation = crate::worldgen_session::RequestCancellation::new();
        let waiter = Arc::clone(&coordinator);
        let queued_cancel = cancellation.clone();
        let thread = std::thread::spawn(move || {
            waiter.acquire(&[(0, 0)], &queued_cancel).is_err()
        });
        std::thread::yield_now();
        cancellation.cancel();
        assert!(thread.join().expect("cancelled waiter did not panic"));
        drop(first);
        let next = coordinator.acquire(&[(0, 0)], &first_cancel);
        assert!(next.is_ok());
    }

    #[test]
    fn checkpoint_keeps_destination_owned_spills_for_a_neighbor_request() {
        let target = (0, 0);
        let destination = (1, 0);
        let halo = ChunkRequest::single(destination.0, destination.1, 1).admission_order();
        let mut ledger = GenerationLedger::new();
        ledger.admit(lodestone_worldgen::stage_schedule::END_PIPELINE, &halo).unwrap();

        let request = crate::worldgen_session::GenerationRequest::new(
            Dimension::End,
            target,
            GenerationTarget::Full,
            1,
        );
        let mut source_session = GenerationSession::new(request);
        for &stage in lodestone_worldgen::stage_schedule::END_PIPELINE.stages() {
            if stage == lodestone_worldgen::stage_schedule::ColumnStage::Features {
                break;
            }
            let descriptor = lodestone_worldgen::stage_schedule::END_PIPELINE
                .descriptor(stage)
                .expect("End stage descriptor");
            let products = descriptor
                .outputs()
                .iter()
                .copied()
                .map(|resource| ImmutableProduct::new(resource, stage as u8))
                .collect();
            let sidecars = descriptor
                .retained_sidecars()
                .iter()
                .copied()
                .map(|sidecar| ImmutableSidecar::new(sidecar, stage as u8))
                .collect();
            source_session
                .complete_immutable(ImmutableStageCompletion::new(
                    target,
                    StageKey::new(Dimension::End, stage),
                    [stage as u8; 32],
                    [stage as u8; 32],
                    1,
                    products,
                    sidecars,
                ))
                .unwrap();
            source_session.advance_ready_immutable().unwrap();
        }
        let features = StageKey::new(Dimension::End, lodestone_worldgen::stage_schedule::ColumnStage::Features);
        source_session
            .declare_mutable_sources(features, [(0, target)])
            .unwrap();
        let mut transaction = source_session.begin_mutable_source(target, features, 0).unwrap();
        transaction
            .push(0, BlockCoordinate::new(16, 70, 0), 4_u8)
            .unwrap();
        source_session.complete_mutable_source(transaction).unwrap();
        let mutation = source_session
            .committed_mutations()
            .next()
            .expect("source completion produced a spill")
            .clone();
        let identity = lodestone_worldgen::stage_schedule::END_PIPELINE
            .identity(PipelineOptions::ALL);
        let expected = ledger.revision(identity, destination).unwrap();
        ledger
            .commit_overlay(
                lodestone_worldgen::stage_schedule::END_PIPELINE,
                destination,
                expected,
                mutation,
            )
            .unwrap();

        let checkpoint = ledger
            .checkpoint(
                lodestone_worldgen::stage_schedule::END_PIPELINE,
                crate::worldgen_session::GenerationRequest::new(
                    Dimension::End,
                    destination,
                    GenerationTarget::Full,
                    1,
                ),
            )
            .unwrap();
        let restored = GenerationSession::from_checkpoint(checkpoint)
            .expect("foreign target spill is valid when its destination is in the halo");
        assert_eq!(restored.committed_mutations().count(), 1);
    }
}
