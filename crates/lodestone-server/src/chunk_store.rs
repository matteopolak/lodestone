//! A bounded cache of generated chunk columns (`docs/plans/chunk-lifecycle.md`
//! unit **U3**, issue #289 part 1).
//!
//! # What it is
//!
//! [`ChunkStore`] wraps any [`ChunkSource`] and *is* a [`ChunkSource`], so it
//! drops in wherever a source is constructed with no call-site changes
//! anywhere. It retains the columns it has been asked for, evicting the
//! least-recently-used one past a capacity bound, so a column is generated
//! **once** and thereafter read.
//!
//! # Why it exists: this was a correctness bug, not a performance gap
//!
//! [`crate::chunk::OverworldChunkSource`] retains **only edited** columns —
//! its own doc comment says so, and says regenerating an unedited column on
//! every request is deliberate because the generator is deterministic, making
//! "regenerate" and "cache forever" observationally identical. That reasoning
//! was sound and is now false, because it was arithmetic about a *cheap*
//! generator. [`crate::tick::run_tick_loop`]'s doc comment drew the explicit
//! conclusion — regeneration every tick is *"a real, documented performance
//! gap … not a correctness one"*. Both went stale the same way, and this is a
//! textbook instance of CLAUDE.md's rule 2: the claim was true and evidenced
//! when written.
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
//! Two independent consumers were paying that per-column cost on a repeating
//! timer, and they starve two *different* tasks — which is why the owner's
//! report had four symptoms and not one:
//!
//! | site | cadence | columns per firing | task starved |
//! |---|---|---|---|
//! | [`crate::tick::run_tick_loop`]'s random-tick loop | every tick (50 ms) | the whole `tick_area` — **49** at the shell's `mob_radius.clamp(1, 3)` | the world tick |
//! | `crate::server`'s `vitals_tick` submersion probe | every 50 ms, once `player_pos` is `Some` | 1, to read a **single block** | the *connection*, i.e. chunk streaming |
//!
//! At 909 ms per column the world tick was therefore spending ~44.5 s of
//! generation per 50 ms of budget — about **0.022 TPS** — and the connection
//! task ~909 ms per 50 ms. That single number explains all four of the symptoms
//! reported against singleplayer: the server barely ticks, so chunk streaming
//! starves ("takes forever to load"); the connection task is saturated from the
//! first movement packet onward ("stops generating chunks after the first
//! load"); the view never recenters ("chunks not close to me"); and the client
//! freezes the player rather than falling into an unloaded column ("stuck in
//! the air" — `lodestone-shell/src/sim/collide.rs`'s
//! `is_chunk_loaded` early return, via `PlayerCollision::Pending`).
//!
//! The second one is the more surprising of the pair and it is not a call-site
//! mistake: until issue #440 made it a **required** method, `block_state`'s
//! default implementation was `self.column(cx, cz).block_state(..)`, so
//! reading one block regenerated a whole 16×384×16 column. It fires only once
//! the client has sent a position, which lines up exactly with *"it seems to
//! stop generating chunks after the first load"* — the connection task streams
//! chunks fine during the join burst and is saturated from the first movement
//! packet onward.
//!
//! [`ChunkStore`] therefore overrides `block_state` as well as `column`; the
//! override reads one cell out of the retained column and clones nothing, and
//! it is the model for the post-#440 required method: a source that retains
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
//!   [`crate::chunk::generate_columns_parallel`]'s whole scoped fan-out and
//!   undo issue #414.
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
//!
//! # The memory this costs, and how to change it
//!
//! `ChunkColumn` is a dense `Vec<u16>` over full world height
//! (`crate::chunk`), i.e. `16 × 384 × 16 × 2 B` ≈ **192 KiB** per column —
//! free today precisely *because* nothing retained it. Retention turns that
//! into real resident memory, which is
//! `docs/plans/chunk-lifecycle.md`'s top risk and the reason this type is
//! bounded rather than a plain `HashMap`.
//!
//! **Measured, not assumed** (the plan's U2 question, answered here because
//! this is the unit that creates the cost). `/usr/bin/time -l` on the release
//! test binary, one arm per configuration:
//!
//! | arm | peak RSS |
//! |---|---|
//! | 512 columns retained | 105.4 MiB |
//! | same 512 touched, retention off | 7.8 MiB |
//! | **delta** | **97.6 MiB**, i.e. 195.5 KiB per column |
//!
//! Re-read while sizing issue #505's cap, with a third arm at [`MAX_CAPACITY`]
//! (`measure_rss_at_the_capacity_cap`) so the ceiling rests on a measurement
//! rather than on a 2.5× extrapolation of the rate above:
//!
//! | arm | peak RSS | delta | per column |
//! |---|---|---|---|
//! | retention off (the shared control) | 8.1 MiB | — | — |
//! | 512 retained | 105.4 MiB | 97.4 MiB | 194.8 KiB |
//! | **1,275 retained** ([`MAX_CAPACITY`]) | **250.1 MiB** | **242.0 MiB** | 194.4 KiB |
//!
//! The 512 row reproduces the original to 0.2 MiB, and the rate is flat across
//! the 2.5× range, so residency is linear in the retained count.
//!
//! The delta lands within 2% of the 192 KiB arithmetic, the remainder being the
//! palette and biome `String`s and the map itself. The two arms are also each
//! other's control: a delta near zero would mean the columns were dropped in
//! both arms, or that the pages were never faulted in, and the run would be a
//! failure to measure rather than evidence that residency is free. See
//! `touched_column`, which exists because `alloc_zeroed` pages that are never
//! written do not show up in RSS.
//!
//! [`capacity_for_view_radius`] is the knob, and 97.6 MiB is what it buys at the
//! default render distance. Lowering it to 128 (~24 MiB) **still fixes the
//! originally reported bug completely** — the starvation fix needs only the
//! 49-column `tick_area` resident, and everything beyond that is avoided
//! *re*-generation as a player walks back over ground they have seen. 512 was
//! chosen to also cover the default streamed view (`render_distance` 8 ⇒
//! `view_radius` 9 ⇒ 361 columns) so that walking in a circle does not pay 909 ms
//! per column again.
//!
//! # Why the capacity is a function of the view radius (issue #505)
//!
//! That last sentence is the whole of issue #505: 512 covered *the default*
//! streamed view, as a bare literal, in a different file from the
//! `render_distance` it was chosen for. The shell serves
//! `view_radius = render_distance + 1` (`crates/lodestone-shell/src/app/session.rs`
//! — the `+ 1` is vanilla's `ChunkTrackingView` buffer ring and is correct), so
//! the streamed square is `(2 × (rd + 1) + 1)²`: 361 columns at `rd = 8`, 441 at
//! 9, **529 at 10**, **729 at vanilla's own default of 12**, 4,489 at the
//! slider's maximum of 32. One notch of the render-distance slider past 9 and the
//! literal was under the set it was chosen to hold.
//!
//! [`capacity_for_view_radius`] derives it instead, floored at
//! [`DEFAULT_CAPACITY`] and capped at [`MAX_CAPACITY`]; both of those constants
//! carry their own argument. The gate is
//! `tests/view_radius_store_capacity.rs`, whose subject is `view_radius = 13`
//! (`render_distance` 12) precisely because a gate at the default radius is under
//! the old ceiling on *both* arms and can see nothing.
//!
//! # Two policies, because the ceiling is a question about whose memory it is
//!
//! The ceiling is **not** applied to singleplayer. `render_distance` 32 sizes the
//! store at 4,539 columns, i.e. **867 MiB** at the 195.5 KiB per column measured
//! above — large, but it is the memory of the person who moved the slider, and
//! truncating the cache under a view they are already being streamed only buys
//! them re-generation of the ground under their feet (see
//! [`integrated_capacity_for_view_radius`] for why *innermost* rings are what a
//! short capacity drops). A hosted server spends an operator's memory on behalf
//! of players who did not choose the setting, so it keeps the cap.
//!
//! | path | constructor | policy |
//! |---|---|---|
//! | singleplayer (`open_in_memory*`) | `ChunkStore::for_integrated_view_radius` | uncapped, floored at [`DEFAULT_CAPACITY`] |
//! | open-to-LAN (`IntegratedServer::bind`) | `ChunkStore::for_view_radius` | capped at [`MAX_CAPACITY`] |
//!
//! To reduce the cost rather than the count, the prior art is
//! `lodestone-world`'s `PalettedContainer` over `PackedArray` plus
//! `Arc<ChunkSection>` copy-on-write sections — that is unit **U8** of the
//! plan, deliberately gated on a *measurement* rather than on the arithmetic
//! above.
//!
//! # The clone this keeps, deliberately
//!
//! [`ChunkSource::column`] returns a `ChunkColumn` **by value**, so a store
//! read is a ~192 KiB `memcpy` (measured in the gate below, tens of
//! microseconds) rather than a refcount bump. Handing back
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

use std::collections::HashMap;
use std::collections::hash_map::Entry as MapEntry;
use std::sync::Mutex;

use crate::chunk::{ChunkColumn, ChunkSource};

/// The floor under [`capacity_for_view_radius`], and the capacity a radius-less
/// `ChunkStore::new` store retains before evicting the least-recently-used one.
///
/// 512 dense full-height columns measured **97.6 MiB** of resident memory (see
/// this module's memory section for the paired `/usr/bin/time -l` arms — that is
/// a measurement, not the 96 MiB arithmetic). It holds `run_tick_loop`'s
/// 49-column `tick_area` plus the default streamed view (`render_distance` 8 ⇒
/// `view_radius` 9 ⇒ 361 columns) with room to spare.
///
/// **It is no longer the capacity a served connection gets** — issue #505.
/// A bare literal is not a function of the radius the connection serves, and at
/// `render_distance` 10 the streamed view alone (529 columns) passes it. See
/// [`capacity_for_view_radius`], which every constructor in
/// [`crate::integrated`] now goes through. This constant survives as that
/// function's **floor**, so the derivation can only ever move capacity *up*
/// from what was measured here, never down: at the default radius the store is
/// byte-for-byte the 512-column, 97.6 MiB configuration it has always been.
pub const DEFAULT_CAPACITY: usize = 512;

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
/// `mob_area` is centred on world spawn and never moves, so once the player has
/// walked away it is 49 columns *outside* the streamed view that are still
/// touched at 20 Hz. The working set is the **union** of the concurrent scans,
/// not the largest of them.
///
/// And the union is what matters rather than the frequency, which is the
/// counter-intuitive half. Issue #504's investigation measured a column polled
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
/// the set it probes only grows with exploration — that is issue #503/§12.110 and
/// it is unbounded by construction, so no constant here can size for it. Hopper
/// chunks past this headroom evict *view* columns rather than each other (the
/// view is touched once per column, the scan every 50 ms, so LRU's minimum stamp
/// always falls on a view column), which means they erode the "the whole view is
/// resident" property that `tests/view_radius_store_capacity.rs` gates with an
/// empty registry. Bounding the scan by the loaded view, as vanilla does, is the
/// fix; adding to this constant is not.
pub const CONCURRENT_SCAN_COLUMNS: usize = view_columns(CONCURRENT_TICK_RADIUS) + 1;

/// The largest view radius whose whole square this store promises to hold
/// resident, and therefore where [`capacity_for_view_radius`] stops growing.
///
/// 17 is `render_distance` 16 (the shell serves `render_distance + 1`) — twice
/// our default of 8, and the midpoint of vanilla's own `IntRange(2, 32)` slider
/// (`crates/lodestone-shell/src/config.rs`'s `MAX_RENDER_DISTANCE`). **The cap
/// is a memory decision and the number is the whole argument.** Both ends of the
/// table are `/usr/bin/time -l` readings on the release lib-test binary rather
/// than arithmetic — `measure_rss_without_retention` (8.1 MiB) is the control
/// both are differenced against:
///
/// | `render_distance` | `view_radius` | view columns | capacity | resident |
/// |---|---|---|---|---|
/// | 8 (default) | 9 | 361 | 512 (floor) | **97.4 MiB, measured** |
/// | 10 | 11 | 529 | 579 | 110 MiB |
/// | 12 (vanilla default) | 13 | 729 | 779 | 148 MiB |
/// | 16 | 17 | 1225 | 1275 | **242.0 MiB, measured** |
/// | 24 | 25 | 2601 | 1275 (capped) | 242.0 MiB |
/// | 32 (slider max) | 33 | 4489 | 1275 (capped) | 242.0 MiB |
///
/// The two measured rows are 194.8 and 194.4 KiB per column, so residency is
/// linear in the count across a 2.5× range and the interpolated rows are safe to
/// read. That linearity is not a given and is why the cap row was measured
/// instead of extrapolated: a `HashMap` growing through several rehash thresholds
/// with 192 KiB values in it could plausibly have been superlinear.
///
/// An *un*capped derivation costs 4,539 columns at `render_distance` 32, i.e.
/// **867 MiB** of resident chunk cache inside a process that also holds meshes,
/// textures and a GPU allocator, on a machine whose whole budget this repo's own
/// operational notes put at 16 GB shared with everything else.
///
/// **That is now the singleplayer policy** — see
/// [`integrated_capacity_for_view_radius`], which is that same derivation with
/// this ceiling removed, because there the memory belongs to the person who moved
/// the slider. This constant governs the *hosted* path only
/// (`IntegratedServer::bind`), where the setting is an operator's and the players
/// paying for it did not choose it.
///
/// # What degrades above it, precisely
///
/// The store holds 1,275 of the view's columns and no more, so the columns
/// outside that set cost a regeneration when something asks for them again —
/// a `block_state` probe from redstone, a fluid tick, mob pathing, or the same
/// column re-entering the view after the player walked back over it. It is not
/// a per-access cost on the whole view (the view is *diffed* as the player
/// moves, never rescanned — see `ViewTracker::recenter`), and the 20 Hz scans
/// are covered by [`CONCURRENT_SCAN_COLUMNS`] at every radius. So the
/// degradation is bounded and localised rather than the LRU-worst-case
/// collapse that `render_distance` 10 hits today, and
/// `tests/view_radius_store_capacity.rs` measures it as this module's permanent
/// negative control.
///
/// To raise it, raise this constant — but re-run
/// `measure_rss_with_retention`/`measure_rss_without_retention` first and put
/// the new pair of numbers in the table above. Reducing the *cost* per column
/// instead is unit U8 of `docs/plans/chunk-lifecycle.md`.
pub const FULLY_RESIDENT_VIEW_RADIUS: i32 = 17;

/// The ceiling [`capacity_for_view_radius`] saturates at — see
/// [`FULLY_RESIDENT_VIEW_RADIUS`] for the memory argument behind the number.
pub const MAX_CAPACITY: usize =
    view_columns(FULLY_RESIDENT_VIEW_RADIUS) + CONCURRENT_SCAN_COLUMNS;

/// The capacity a **hosted** store serving `view_radius` is built with —
/// issue #505's fix, and what `IntegratedServer::bind` (open-to-LAN) calls.
///
/// Singleplayer uses [`integrated_capacity_for_view_radius`] instead, which is
/// this derivation without the ceiling; read that function for why the fork
/// exists.
///
/// `view_columns(view_radius) + CONCURRENT_SCAN_COLUMNS`, clamped to
/// `DEFAULT_CAPACITY ..= MAX_CAPACITY`. Each of those three terms is load-bearing
/// and documented on its own constant; in short:
///
/// * the **view** term is the bug: a literal 512 covers `render_distance` 9 and
///   nothing above it, and 10 is one notch of a slider away;
/// * the **scan** term is the union with `run_tick_loop`'s tick area, which is
///   not a subset of the view;
/// * the **floor** keeps every existing measurement in this module valid, since
///   the derivation can then only move capacity up;
/// * the **ceiling** stops a CPU cliff being traded for an 866 MiB memory one.
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

/// [`capacity_for_view_radius`] **without the [`MAX_CAPACITY`] ceiling** — the
/// integrated (singleplayer) policy.
///
/// # Why the ceiling does not apply here
///
/// A hosted server's render distance is the *operator's* budget spent on behalf
/// of players who did not choose it, so capping it is right. Singleplayer is the
/// opposite: the person paying for the memory is the person who moved the slider,
/// and the slider goes to 32. Refusing to hold the view they asked for buys them
/// nothing — the columns are streamed and meshed either way; only the *cache*
/// under them is truncated, so the cost of the cap is re-generation of ground
/// they are currently looking at.
///
/// # The number, so the choice is informed
///
/// This store costs a measured **195.5 KiB per retained column** (the module
/// docs' table, `/usr/bin/time -l` on the release lib-test binary). The streamed
/// set is `(2 × (rd + 1) + 1)²`, and capacity is that plus
/// [`CONCURRENT_SCAN_COLUMNS`]:
///
/// | `render_distance` | view columns | capacity | resident |
/// |---|---|---|---|
/// | 8 (our default) | 361 | 512 (floor) | 97.4 MiB, measured |
/// | 12 (vanilla default) | 729 | 779 | 148 MiB |
/// | 16 | 1,225 | 1,275 | 242.0 MiB, measured |
/// | 24 | 2,601 | 2,651 | 506 MiB |
/// | **32 (slider max)** | **4,489** | **4,539** | **867 MiB** |
///
/// Residency measured flat at 194.4–194.8 KiB per column across a 2.5× range, so
/// the interpolated rows are safe to read; the last row is a 3.6× extrapolation
/// of that rate and is the one to re-measure if it ever matters. 867 MiB inside a
/// client process that also holds meshes, textures and a GPU allocator is large,
/// on a machine this repo's operational notes budget at 16 GB — **large, and the
/// user's call**, which is the whole difference from the hosted policy.
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
    if want < DEFAULT_CAPACITY { DEFAULT_CAPACITY } else { want }
}

/// One retained column plus the stamp that orders eviction.
struct Entry {
    column: ChunkColumn,
    /// Value of `Cache::stamp` at this entry's most recent read or write.
    /// Smallest wins eviction.
    last_used: u64,
}

struct Cache {
    columns: HashMap<(i32, i32), Entry>,
    /// Monotonic counter handed out by [`Cache::next_stamp`]. Not a tick count
    /// and not comparable to anything outside this struct.
    stamp: u64,
    /// Cumulative count of calls that reached `source.column()`. This is a
    /// store-lifetime accumulator, so a gate must read it as a **delta** or
    /// against a freshly constructed store — see [`ChunkStore::generated`].
    generated: u64,
    /// Cumulative count of evictions, same accumulator caveat.
    evicted: u64,
}

impl Cache {
    fn next_stamp(&mut self) -> u64 {
        self.stamp += 1;
        self.stamp
    }

    /// Drops least-recently-used entries until `len() <= capacity`.
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
    fn evict_down_to(&mut self, capacity: usize) -> Vec<(i32, i32)> {
        let mut evicted = Vec::new();
        while self.columns.len() > capacity {
            let Some(victim) = self
                .columns
                .iter()
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

/// A [`ChunkSource`] that retains what it generates. See the module docs.
pub(crate) struct ChunkStore<S> {
    source: S,
    capacity: usize,
    cache: Mutex<Cache>,
}

impl<S> ChunkStore<S> {
    /// Wraps `source`, retaining up to [`DEFAULT_CAPACITY`] columns.
    ///
    /// **`#[cfg(test)]` since issue #505, and that is the fix stated as a type
    /// signature.** Every production caller is in [`crate::integrated`] and every
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
    /// second copy of the derivation, and the reason issue #505 existed at all is
    /// that the number and the radius it was chosen for lived in two different
    /// files.
    pub(crate) fn for_view_radius(source: S, view_radius: i32) -> Self {
        Self::with_capacity(source, capacity_for_view_radius(view_radius))
    }

    /// [`for_view_radius`](Self::for_view_radius) for the **integrated** server:
    /// the same derivation with no [`MAX_CAPACITY`] ceiling.
    ///
    /// The two constructors are the two halves of one decision, and which one a
    /// call site picks is a question about *whose* memory is being spent — see
    /// [`integrated_capacity_for_view_radius`] for the numbers. Singleplayer is
    /// this one; open-to-LAN (`IntegratedServer::bind`) is the capped one.
    pub(crate) fn for_integrated_view_radius(source: S, view_radius: i32) -> Self {
        Self::with_capacity(source, integrated_capacity_for_view_radius(view_radius))
    }

    /// Wraps `source` with an explicit capacity. A capacity of 0 disables
    /// retention entirely, which is the pre-store behaviour and is what the
    /// gate below uses as its negative control.
    pub(crate) fn with_capacity(source: S, capacity: usize) -> Self {
        Self {
            source,
            capacity,
            cache: Mutex::new(Cache {
                columns: HashMap::new(),
                stamp: 0,
                generated: 0,
                evicted: 0,
            }),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Cache> {
        self.cache.lock().expect("chunk store lock poisoned")
    }

    // The four accessors below are `#[cfg(test)]` rather than
    // `#[allow(dead_code)]`: nothing in production reads them, and pretending
    // otherwise is how dead code accumulates. Production observability goes
    // through the `Debug` impl, which reports all four. Units U6 (unloading)
    // and U8 (sectioned storage) of `docs/plans/chunk-lifecycle.md` are the
    // ones that will want them for real; drop the `cfg` then.

    /// Columns currently retained. Never exceeds [`capacity`](Self::capacity).
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.lock().columns.len()
    }

    /// The eviction bound this store was built with.
    #[cfg(test)]
    pub(crate) fn capacity(&self) -> usize {
        self.capacity
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
        f.debug_struct("ChunkStore")
            .field("resident", &cache.columns.len())
            .field("capacity", &self.capacity)
            .field("generated", &cache.generated)
            .field("evicted", &cache.evicted)
            .finish_non_exhaustive()
    }
}

impl<S: ChunkSource> ChunkStore<S> {
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
    fn ensure(&self, cx: i32, cz: i32) -> Option<ChunkColumn> {
        {
            let mut guard = self.lock();
            let cache = &mut *guard;
            let stamp = cache.next_stamp();
            if let Some(entry) = cache.columns.get_mut(&(cx, cz)) {
                entry.last_used = stamp;
                return None;
            }
        }

        // Lock released: a ~909 ms generation must not serialise
        // `generate_columns_parallel`'s scoped fan-out.
        let fresh = self.source.column(cx, cz);

        let mut guard = self.lock();
        let cache = &mut *guard;
        cache.generated += 1;
        let stamp = cache.next_stamp();
        if self.capacity == 0 {
            return Some(fresh);
        }
        match cache.columns.entry((cx, cz)) {
            // Another thread won the race while this one generated. Keep
            // theirs: it may carry a `set_block` edit this column predates.
            MapEntry::Occupied(mut occupied) => occupied.get_mut().last_used = stamp,
            MapEntry::Vacant(vacant) => {
                vacant.insert(Entry {
                    column: fresh,
                    last_used: stamp,
                });
            }
        }
        let evicted = cache.evict_down_to(self.capacity);
        drop(guard);
        // Outside the lock, deliberately: see `evict_down_to`. This is what
        // lets the layer beneath release a column it has already written, so
        // the edit map is no longer the process's real memory bound for a
        // heavily-built world.
        for (vx, vz) in evicted {
            self.source.unload(vx, vz);
        }
        None
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
}

impl<S: ChunkSource> ChunkSource for ChunkStore<S> {
    fn column(&self, cx: i32, cz: i32) -> ChunkColumn {
        // `Some` means retention is off (the negative-control configuration) —
        // the column was just generated and there is nothing to read it from.
        if let Some(fresh) = self.ensure(cx, cz) {
            return fresh;
        }
        // The fallback below is reachable only if another thread evicted this
        // entry in the window since `ensure` inserted it, which needs a
        // capacity-worth of concurrent misses. Correct rather than dead, and it
        // costs a regeneration, never a wrong block.
        self.read(cx, cz, ChunkColumn::clone)
            .unwrap_or_else(|| self.source.column(cx, cz))
    }

    /// One block, without regenerating or cloning a column.
    ///
    /// Overriding this is half the fix, not an optimisation: the
    /// column-regenerating form (`self.column(cx, cz).block_state(..)`, once
    /// `block_state`'s default and now each non-retaining implementor's
    /// explicit choice) regenerates a whole column per probe, and
    /// `crate::server`'s `vitals_tick` calls this every 50 ms on the
    /// connection task. See the module docs.
    fn block_state(&self, x: i32, y: i32, z: i32) -> String {
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        if let Some(fresh) = self.ensure(cx, cz) {
            return fresh.block_state(lx, y, lz).to_string();
        }
        self.read(cx, cz, |column| column.block_state(lx, y, lz).to_string())
            .unwrap_or_else(|| self.source.block_state(x, y, z))
    }

    /// Writes through to the inner source **first**, then to the retained
    /// column if one is resident.
    ///
    /// That order is what makes eviction lossless — see the module docs. If no
    /// entry is resident this deliberately does not create one: the next read
    /// regenerates through the inner source, which for
    /// [`crate::chunk::OverworldChunkSource`] consults its `edits` map and so
    /// returns the edited column.
    fn set_block(&self, x: i32, y: i32, z: i32, name: &str) {
        self.source.set_block(x, y, z, name);

        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        let mut guard = self.lock();
        let cache = &mut *guard;
        let stamp = cache.next_stamp();
        if let Some(entry) = cache.columns.get_mut(&(cx, cz)) {
            // A `y` outside the column's vertical extent is a no-op rather than
            // an index panic. `ChunkColumn::set_block` indexes unguarded, and
            // the inner source's own `set_block` may have accepted the edit (or
            // rejected it its own way) without this retained column being able
            // to hold it — so the store guards its own update rather than
            // relying on the source to reject out-of-range `y`.
            if y >= entry.column.min_y && y < entry.column.min_y + entry.column.height {
                entry.column.set_block(lx, y, lz, name);
                entry.last_used = stamp;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::ops::RangeInclusive;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

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
    /// **Hand-written on purpose, and this is the anti-vacuity property of
    /// every count below.** The real `OverworldGenerator` carries a per-instance
    /// 512-entry memo cache keyed on exact `(cx, cz)`, so a generation-count
    /// gate built on `crate::overworld_chunk_source` passes *even with a
    /// completely broken store* — the memo absorbs the second call. That exact
    /// vacuity was found and fixed once already in `crate::chunk`'s
    /// `parallel_generation_is_deterministic_and_matches_serial`. This source
    /// has no cache of any kind, so every call it is asked for, it counts.
    struct CountingSource {
        calls: Arc<AtomicU64>,
        /// Recorded per coordinate too, so a failure can say *which* chunk was
        /// regenerated rather than only that the total was wrong — per
        /// CLAUDE.md's "make failure output say *where*". Shared by `Arc` so a
        /// gate can keep reading it after the source is moved into the store.
        per_chunk: PerChunk,
        min_y: i32,
        height: i32,
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
            }
        }

        fn calls(&self) -> u64 {
            self.calls.load(Ordering::Relaxed)
        }
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
        // on one probe costing exactly one generation. This is the explicit,
        // column-regenerating form that used to be `block_state`'s default.
        fn block_state(&self, x: i32, y: i32, z: i32) -> String {
            let cx = x.div_euclid(16);
            let cz = z.div_euclid(16);
            let lx = x.rem_euclid(16);
            let lz = z.rem_euclid(16);
            self.column(cx, cz).block_state(lx, y, lz).to_string()
        }

        // `run_tick_loop` forwards random-tick and grazing mutations through
        // the store to the inner source, so this must not panic; but the
        // source has no storage (its `column()` is a fresh blank column plus a
        // counter), so the edit is deliberately discarded. Explicit rather than
        // inherited — the point of issue #440.
        fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {
            // No storage; edits are discarded by design for this counting stub.
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

    const EXPECTED_TICK_AREA_COLUMNS: usize =
        ((2 * SHELL_TICK_RADIUS + 1) * (2 * SHELL_TICK_RADIUS + 1)) as usize;

    /// How many random-tick *passes* the gates below observe — **not** how many
    /// ticks they drive.
    ///
    /// [`crate::tick::run_tick_loop`] is the only thing that calls
    /// `world.column()` here, and since issue #481 it skips its random-tick pass
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
            // Issue #468: this gate measures column generation, not persistence,
            // so a fresh handle -- behaviourally the locals this replaced.
            crate::region_source::ScheduledTickHandle::default(),
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
    /// store, `RANDOM_TICK_PASSES × 49` visits produce **49** generations;
    /// without one they produce **`RANDOM_TICK_PASSES × 49`**. Those are not
    /// "more" and "less", they are two exact numbers a factor of
    /// [`RANDOM_TICK_PASSES`] apart, and the negative control below lands on the
    /// second.
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

    /// **The negative control, and it must fail the assertion above.**
    ///
    /// `ChunkStore::with_capacity(source, 0)` retains nothing, so every read
    /// falls through to `source.column()` — bit-for-bit the pre-store
    /// behaviour, reproduced as a real *configuration* of the shipped type
    /// rather than as a temporary neuter, so the control is permanent.
    ///
    /// Observed when this landed: **588** generations for 49 columns over 12
    /// random-tick passes, i.e. exactly `49 × 12`, against 49 with the store. At
    /// the measured 909 ms per real column that is 44.5 s of generation per
    /// 50 ms tick budget.
    #[tokio::test(start_paused = true)]
    async fn without_retention_every_chunk_is_regenerated_every_tick() {
        let counting = CountingSource::new();
        let calls = Arc::clone(&counting.calls);
        let per_chunk = Arc::clone(&counting.per_chunk);
        let store = Arc::new(ChunkStore::with_capacity(counting, 0));

        drive_tick_loop(Arc::clone(&store), shell_tick_area(), TICKS).await;

        let generated = calls.load(Ordering::Relaxed);
        // The per-chunk view of the same failure: without retention *every*
        // column is regenerated on *every* tick, so the worst chunk's count is
        // the tick count itself, not 1.
        let (worst_coord, worst_count) = worst_chunk(&per_chunk);
        assert_eq!(
            worst_count,
            u64::from(RANDOM_TICK_PASSES),
            "control: chunk {worst_coord:?} should have been regenerated once per random-tick \
             pass ({RANDOM_TICK_PASSES}), got {worst_count}"
        );
        assert_eq!(
            generated,
            EXPECTED_TICK_AREA_COLUMNS as u64 * u64::from(RANDOM_TICK_PASSES),
            "the zero-capacity control must reproduce the pre-store behaviour exactly: \
             {EXPECTED_TICK_AREA_COLUMNS} columns × {RANDOM_TICK_PASSES} passes. If this ever reports \
             {EXPECTED_TICK_AREA_COLUMNS} instead, retention has leaked into the control \
             and the positive gate above is no longer measuring anything."
        );
        assert_eq!(store.len(), 0, "a zero-capacity store must retain nothing");
    }

    /// The half of the fix that is not about the tick loop: reading **one
    /// block** must not regenerate a column.
    ///
    /// `crate::server`'s `vitals_tick` does exactly this every 50 ms once the
    /// client has sent a position, on the connection task — the task that
    /// streams chunks. Against the column-regenerating form (once
    /// `ChunkSource::block_state`'s default, now each non-retaining source's
    /// explicit choice — issue #440) each probe is a full column generation,
    /// which is why chunk streaming stops at the first movement packet rather
    /// than at join.
    ///
    /// Negative control in the same body: the unwrapped source, where the same
    /// 40 probes cost 40 generations.
    #[test]
    fn repeated_single_block_probes_generate_one_column_not_forty() {
        const PROBES: u64 = 40;

        // Control: the bare source, whose `block_state` is the explicit
        // column-regenerating form (what used to be the trait default).
        let bare = CountingSource::new();
        for _ in 0..PROBES {
            let _ = bare.block_state(5, 8, 5);
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
            let _ = store.block_state(5, 8, 5);
        }
        assert_eq!(
            calls.load(Ordering::Relaxed),
            1,
            "{PROBES} probes of the same block must cost exactly one generation"
        );
        assert_eq!(store.len(), 1, "one column touched, one column resident");
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

        let before = store.block_state(0, -50, 0);
        assert_ne!(
            before, "minecraft:diamond_block",
            "precondition: the generator must not already have placed the block this test \
             writes, or neither property below means anything"
        );

        store.set_block(0, -50, 0, "minecraft:diamond_block");

        // Property 1: visible immediately, from the resident column.
        assert_eq!(
            store.block_state(0, -50, 0),
            "minecraft:diamond_block",
            "an edit must be visible to the next read"
        );
        assert_eq!(
            store.column(0, 0).block_state(0, -50, 0),
            "minecraft:diamond_block",
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
            store.block_state(0, -50, 0),
            "minecraft:diamond_block",
            "an edit must survive eviction of its cache entry — this is what makes the \
             store's capacity bound lossless"
        );
    }

    /// The bound is real, and it is the property that stops this fix from
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
        // with `--nocapture`. A dense full-height column is ~192 KiB, so the
        // clone `column()` returns is a memcpy of that; the point of recording
        // it is that it is microseconds against the 909 ms it replaces.
        let column = ChunkColumn::new(REAL_MIN_Y, REAL_HEIGHT);
        let started = std::time::Instant::now();
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

    /// Issue #505's policy at its **boundaries**, which is the part of it a
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
    ///    derived one, and 11 is exactly `render_distance` 10 — the notch issue
    ///    #505 reports.
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
             which is the notch issue #505 reports; it stops at {first_derived}"
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
                        stored.block_state(lx, y, lz),
                        bare_column.block_state(lx, y, lz),
                        "pass {pass}: retained column ({cx}, {cz}) diverged at ({lx}, {y}, {lz})"
                    );
                }
                assert_eq!(
                    store.block_state(cx * 16 + probes[0].0, probes[0].1, cz * 16 + probes[0].2),
                    bare_column.block_state(probes[0].0, probes[0].1, probes[0].2),
                    "pass {pass}: the `block_state` override diverged from `column()`"
                );
            }
        }
    }

    /// A full-height column with every memory page actually **written**.
    ///
    /// `ChunkColumn::new` allocates through `vec![0u16; …]`, i.e. `alloc_zeroed`,
    /// and at 192 KiB that can be served by lazily-zeroed pages the process
    /// never faults in — so a store full of *untouched* columns would understate
    /// resident memory and the RSS measurement below would be a
    /// world-species vacuity (measuring an allocation pattern production never
    /// has, since a generated column is fully written). One write per 8 y-rows
    /// is enough to touch every page at any plausible page size.
    fn touched_column(min_y: i32, height: i32) -> ChunkColumn {
        let mut column = ChunkColumn::new(min_y, height);
        let mut y = min_y;
        while y < min_y + height {
            column.set_solid(y.rem_euclid(16), y, 0, true);
            y += 8;
        }
        column
    }

    struct TouchedSource;
    impl ChunkSource for TouchedSource {
        fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
            touched_column(REAL_MIN_Y, REAL_HEIGHT)
        }

        fn block_state(&self, x: i32, y: i32, z: i32) -> String {
            // Only `column()` is exercised here (the RSS measurement); this is
            // the plain column-regenerating form, kept for completeness.
            let cx = x.div_euclid(16);
            let cz = z.div_euclid(16);
            let lx = x.rem_euclid(16);
            let lz = z.rem_euclid(16);
            self.column(cx, cz).block_state(lx, y, lz).to_string()
        }

        // A memory-measurement fixture; nothing here writes blocks. Explicitly
        // discards rather than inheriting a silent default (issue #440).
        fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {
            // No storage; edits are discarded by design for this fixture.
        }
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

    /// **Retained arm at the cap** — what issue #505's ceiling actually costs,
    /// measured rather than extrapolated.
    ///
    /// [`FULLY_RESIDENT_VIEW_RADIUS`]'s memory table is arithmetic off the 195.5
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

    /// The premise everything else rests on: what a **real** composed column
    /// costs in release.
    ///
    /// A fresh, independently constructed source per column, because
    /// `OverworldGenerator`'s 512-entry memo cache would otherwise turn every
    /// column after the first into a cache hit and report a per-column cost
    /// near zero — the same trap that made `crate::chunk`'s determinism test
    /// vacuous.
    ///
    /// Reports, never asserts: a duration on a shared box is a sample, not a
    /// measurement (a 2.3× spread was measured on an identical release binary
    /// from load alone), so a threshold here would be a flake generator. The
    /// *count* gates above are what protect the fix.
    #[test]
    #[ignore = "measurement tool; run in --release, and only on a quiet machine"]
    fn measure_real_column_generation_cost() {
        const COLUMNS: usize = 4;
        let mut total = std::time::Duration::ZERO;
        for i in 0..COLUMNS as i32 {
            let source = crate::overworld_chunk_source(42);
            let started = std::time::Instant::now();
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
    /// scoped fan-out is serialised behind it and issue #414 is undone.
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

            fn block_state(&self, x: i32, y: i32, z: i32) -> String {
                // Only `column()` is exercised here (the lock-serialisation
                // gate); the plain column-regenerating form, for completeness.
                let cx = x.div_euclid(16);
                let cz = z.div_euclid(16);
                let lx = x.rem_euclid(16);
                let lz = z.rem_euclid(16);
                self.column(cx, cz).block_state(lx, y, lz).to_string()
            }

            // A wall-clock-only fixture; nothing here writes blocks. Explicitly
            // discards rather than inheriting a silent default (issue #440).
            fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {
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

        let started = std::time::Instant::now();
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

    // ---------------------------------------------------------------------
    // Issue #503's block-entity lead: is the 20 Hz registry scan a *second*,
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

    /// **The subject.** A hopper whose column the player has walked away from
    /// costs **one** column generation in total, not one per tick.
    ///
    /// # The two hypotheses, both computed outside the measurement
    ///
    /// `tick.rs`'s block-entity arm really does call `world.block_state` per
    /// hopper, every tick, and `ChunkStore::block_state` really does regenerate
    /// a whole column on a miss (`ensure` → `source.column`). Issue #503's lead
    /// reads those two facts together and predicts **[`TICKS`]** generations of
    /// the remote column — one per tick, ~50 ms each against a 50 ms budget.
    ///
    /// The competing prediction is **1**, and the reason is the line the lead
    /// does not account for: *the miss is self-healing*. `ensure` inserts with
    /// the newest stamp and `read` refreshes `last_used` on every hit, so a
    /// position polled at 20 Hz is permanently among the most-recently-used
    /// entries. `evict_down_to` takes the **minimum** stamp, so evicting this
    /// column would require [`DEFAULT_CAPACITY`] *other* distinct columns to be
    /// touched inside one 50 ms tick period. The 20 Hz scan does not merely fail
    /// to evict the column — it **pins** it.
    ///
    /// So the two hypotheses are 1 and 52, a factor of [`TICKS`] apart, and this
    /// gate lands on one of them. `without_retention_a_remote_hopper_is_a_cold_
    /// column_every_single_tick` below is the same rig landing on the other, so
    /// the instrument is known to be able to report 52.
    #[tokio::test(start_paused = true)]
    async fn a_walked_away_hopper_costs_one_column_generation_not_one_per_tick() {
        let counting = CountingSource::full_height();
        let calls = Arc::clone(&counting.calls);
        let per_chunk = Arc::clone(&counting.per_chunk);
        let store = Arc::new(ChunkStore::new(counting));

        let clock = drive_tick_loop_with_block_entities(
            Arc::clone(&store),
            shell_tick_area(),
            TICKS,
            registry_with_one_hopper(REMOTE_CHUNK),
        )
        .await;

        assert!(
            clock.tick_count() >= u64::from(TICKS) - 1,
            "precondition: the loop only advanced {} ticks of {TICKS}, so \"one generation\" \
             would be trivially true",
            clock.tick_count()
        );

        let remote = generations_for(&per_chunk, REMOTE_CHUNK);
        assert_eq!(
            remote, 1,
            "the remote hopper's column {REMOTE_CHUNK:?} was generated {remote} times over \
             {TICKS} ticks. 0 would mean the block-entity scan never reached the world at all \
             and this gate measures nothing; {TICKS} is issue #503's lead — one cold column per \
             tick; 1 is the store pinning the column it just polled."
        );
        assert_eq!(
            calls.load(Ordering::Relaxed),
            EXPECTED_TICK_AREA_COLUMNS as u64 + 1,
            "the whole session should cost the {EXPECTED_TICK_AREA_COLUMNS}-column tick area \
             plus exactly one remote column"
        );
        assert_eq!(
            store.evicted(),
            0,
            "precondition on the mechanism: 49 tick-area columns + 1 remote is far under \
             {DEFAULT_CAPACITY}, so nothing should be evicted. If this is ever non-zero the \
             pinning argument above needs re-deriving, not the assertion relaxing."
        );
    }

    /// **The negative control, and it lands on the lead's own number.**
    ///
    /// `with_capacity(source, 0)` retains nothing, so every poll of the remote
    /// hopper is a cold column — exactly the failure issue #503's lead
    /// describes, reproduced as a real *configuration* of the shipped type
    /// rather than a temporary neuter. Predicted **[`TICKS`]** (the
    /// block-entity scan runs on every tick, unlike the random-tick pass, which
    /// `INITIAL_RANDOM_TICK_DEFERRAL_TICKS` holds off), and that is what makes
    /// the subject's `1` a measurement rather than an absence.
    ///
    /// Without this arm the subject is the *assertion* species of vacuity: a
    /// registry that was never scanned, a hopper that was never ticked and a
    /// closure that was never called would all also report a low number.
    #[tokio::test(start_paused = true)]
    async fn without_retention_a_remote_hopper_is_a_cold_column_every_single_tick() {
        let counting = CountingSource::full_height();
        let per_chunk = Arc::clone(&counting.per_chunk);
        let store = Arc::new(ChunkStore::with_capacity(counting, 0));

        drive_tick_loop_with_block_entities(
            Arc::clone(&store),
            shell_tick_area(),
            TICKS,
            registry_with_one_hopper(REMOTE_CHUNK),
        )
        .await;

        let remote = generations_for(&per_chunk, REMOTE_CHUNK);
        assert_eq!(
            remote,
            u64::from(TICKS),
            "control: with retention off the remote hopper's column must be regenerated on \
             every one of {TICKS} ticks, got {remote}. If this ever reports 1, retention has \
             leaked into the control and the subject above is no longer measuring anything."
        );
    }

    /// **The curve, in counter form: flat in distance.**
    ///
    /// The claim under test is that walking away introduces a term that grows
    /// with distance. `world.block_state`'s cost has no coordinate in it — the
    /// store is a `HashMap<(i32, i32), _>` and the LRU stamp is a counter — so
    /// the prediction is the *same* number at every band, including one a
    /// million blocks out, matching `docs/worldgen-store-distance-leak.md`'s
    /// finding that per-column cost is itself flat to 1,048,576 blocks.
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

            drive_tick_loop_with_block_entities(
                Arc::clone(&store),
                shell_tick_area(),
                TICKS,
                registry_with_one_hopper(chunk),
            )
            .await;

            assert_eq!(
                generations_for(&per_chunk, chunk),
                1,
                "band {chunk:?} ({} blocks out): the remote column must be generated exactly \
                 once, as at every other band. A band-dependent count here is the \
                 distance-dependent CPU term issue #503's lead predicts.",
                chunk.0 * 16
            );
            assert_eq!(
                calls.load(Ordering::Relaxed),
                EXPECTED_TICK_AREA_COLUMNS as u64 + 1,
                "band {chunk:?}: total generations must be the tick area plus one, identically \
                 at every distance"
            );
        }
    }

    /// **The arm that isolates the mechanism, and the one that says the lead is
    /// real.** Once the store is over its ceiling, the 20 Hz-polled remote
    /// column becomes a cold generation **once per random-tick pass** — on the
    /// tick thread, against a 50 ms budget.
    ///
    /// # Why this arm exists
    ///
    /// Without it the subject above has a second, much weaker explanation: at
    /// the default render distance the working set (361-column view + 49-column
    /// tick area = 410) never reaches [`DEFAULT_CAPACITY`] at all, so "no
    /// eviction" could be pure headroom rather than any property of the polling.
    /// (This read **289** until issue #505. 289 is `(2 × 8 + 1)²`, i.e. the view
    /// for a *view radius* of 8 — but 8 is the `render_distance`, and the shell
    /// serves `render_distance + 1`. The conclusion held with room to spare either
    /// way, which is exactly why the arithmetic slipped through twice in one
    /// file.)
    /// `with_capacity(source, EXPECTED_TICK_AREA_COLUMNS)` removes the headroom:
    /// 49 tick-area columns plus one remote is 50 against a ceiling of 49.
    ///
    /// # A wrong prediction, kept
    ///
    /// This arm was written predicting **1**, on the argument that
    /// `ChunkStore::read` refreshes `last_used` on every hit and `ensure`
    /// inserts with the newest stamp, so a position polled at 20 Hz is
    /// permanently among the most-recently-used entries and `evict_down_to`
    /// (which takes the **minimum**) would always prefer a stale tick-area
    /// column. It measured **12** and the argument is wrong, for a reason worth
    /// keeping: the block-entity scan runs *once* per tick and the random-tick
    /// pass then touches 49 columns *after* it, so by the end of a pass the
    /// remote column's stamp is the oldest in the map, not the newest. Being
    /// polled at 20 Hz does not pin a column when something else touches 49
    /// columns in the same 50 ms. The pin only ever came from headroom.
    ///
    /// # The prediction, derived
    ///
    /// One cold generation on the first poll, then one per random-tick pass
    /// thereafter — except the final pass's eviction has no following poll
    /// inside the window, so `1 + (RANDOM_TICK_PASSES - 1)` =
    /// [`RANDOM_TICK_PASSES`]. The three candidate values are therefore 1 (the
    /// headroom regime the subject measures), 12 (this regime) and [`TICKS`]
    /// (no retention at all), and this arm lands on the middle one.
    #[tokio::test(start_paused = true)]
    async fn an_over_capacity_store_makes_the_polled_column_cold_every_pass() {
        let counting = CountingSource::full_height();
        let per_chunk = Arc::clone(&counting.per_chunk);
        let store = Arc::new(ChunkStore::with_capacity(
            counting,
            EXPECTED_TICK_AREA_COLUMNS,
        ));

        drive_tick_loop_with_block_entities(
            Arc::clone(&store),
            shell_tick_area(),
            TICKS,
            registry_with_one_hopper(REMOTE_CHUNK),
        )
        .await;

        assert!(
            store.evicted() > 0,
            "precondition: a capacity of {EXPECTED_TICK_AREA_COLUMNS} against a \
             {EXPECTED_TICK_AREA_COLUMNS}-column tick area plus one remote column must actually \
             evict, or this arm is the same measurement as the subject and proves nothing"
        );
        let remote = generations_for(&per_chunk, REMOTE_CHUNK);
        assert_eq!(
            remote,
            u64::from(RANDOM_TICK_PASSES),
            "over capacity, the remote hopper's column must be cold once per random-tick pass \
             ({RANDOM_TICK_PASSES}), got {remote}. 1 would mean the polling pins it (it does \
             not — see this test's doc comment); {TICKS} would mean retention had stopped \
             working entirely."
        );
    }

    /// **The curve, and the threshold it turns on.** Cold generations per tick
    /// are ~0 while the accumulated block-entity columns fit under
    /// [`DEFAULT_CAPACITY`] alongside the tick area, and **one per entity per
    /// tick** the moment they do not.
    ///
    /// This is the term issue #503's lead is really about, once the arms above
    /// have located it. `BlockEntityRegistry` has no unload path, so the set of
    /// positions the 20 Hz scan probes only ever grows with exploration. Each
    /// probed position holds one column resident. So exploration does not make
    /// any single call slower — it walks the *working set* across the store's
    /// fixed ceiling, and the miss rate goes from 0 to 1 over a narrow band.
    ///
    /// # Both hypotheses computed from constants, and the boundary is exact
    ///
    /// The rig's resident set is `EXPECTED_TICK_AREA_COLUMNS + entities`. Below
    /// the ceiling every column is generated exactly **once** for the whole
    /// session, so remote generations total `entities`. Above it the access
    /// pattern is a **cyclic** scan of `entities` positions through an LRU of
    /// [`DEFAULT_CAPACITY`], which is LRU's worst case — by the time the scan
    /// returns to a position it has touched every other one, so **every** probe
    /// misses and the total is `entities × TICKS`.
    ///
    /// At `entities = UNDER` those are 400 and 20,800; at `entities = OVER` they
    /// are 600 and 31,200. Four numbers, no fitting, and the two regimes are a
    /// factor of [`TICKS`] apart rather than "more" and "less".
    ///
    /// Production's own threshold is lower than this rig's, because production
    /// also holds a streamed view — at the default `render_distance` 8 that is
    /// `view_radius` 9 and so **361** columns, leaving `512 - 361 - 49` = **102**
    /// distinct hopper-bearing *chunks*, since only `BlockEntity::Hopper` probes
    /// the world at all (see
    /// `sixteen_hundred_opaque_block_entities_never_reach_the_store`) and hoppers
    /// sharing a chunk share one column.
    ///
    /// This read `512 - 289 - 49 = 174` until issue #505; 289 is the square of a
    /// *view radius* of 8, and 8 is the `render_distance`, which the shell serves
    /// plus one. **Treat 102 as a conservative floor rather than the threshold**:
    /// it assumes the view competes with the 20 Hz scans for residency, and in
    /// steady state it does not — the view is touched once per column while the
    /// tick area and the registry are touched every 50 ms, so LRU's minimum stamp
    /// always falls on a view column. What the view really costs is that the store
    /// sits permanently at its ceiling, permanently evicting, once
    /// `view_columns(view_radius)` alone exceeds it. That is issue #505, and
    /// `tests/view_radius_store_capacity.rs` is where the view side is measured
    /// rather than derived.
    ///
    /// # This arm characterises a defect, it does not bless it
    ///
    /// The over-capacity band is **not** desired behaviour — it is issue #503's
    /// block-entity lead, measured. If a fix lands (vanilla ticks block entities
    /// per *loaded chunk*, so the scan should be bounded by the view rather than
    /// by everything the registry ever saw) this band goes red, and the right
    /// response is to rewrite it against the new bound, not to relax it. See
    /// `docs/block-entity-tick-distance.md`.
    #[tokio::test(start_paused = true)]
    async fn the_miss_rate_crosses_from_zero_to_one_when_the_registry_outgrows_the_store() {
        /// Comfortably under the ceiling: `49 + 400 = 449 <= 512`.
        const UNDER: i32 = 400;
        /// Over it: `49 + 600 = 649 > 512`.
        const OVER: i32 = 600;

        const _: () = assert!(EXPECTED_TICK_AREA_COLUMNS + UNDER as usize <= DEFAULT_CAPACITY);
        const _: () = assert!(EXPECTED_TICK_AREA_COLUMNS + OVER as usize > DEFAULT_CAPACITY);

        for (entities, over_capacity) in [(UNDER, false), (OVER, true)] {
            let handle = BlockEntityHandle::new();
            handle.with(|registry| {
                for i in 0..entities {
                    registry.insert(
                        remote_pos((2_000 + i, 2_000 + i)),
                        crate::block_entities::BlockEntity::Hopper(crate::hopper::Hopper::new()),
                    );
                }
            });

            // A shorter column than `full_height` for this arm alone: the
            // over-capacity band generates ~31,200 columns, and at 192 KiB each
            // that is allocation churn the measurement does not need. The count
            // is what is under test, not the column's size.
            let counting = CountingSource::sized(0, 128);
            let calls = Arc::clone(&counting.calls);
            let store = Arc::new(ChunkStore::new(counting));

            drive_tick_loop_with_block_entities(
                Arc::clone(&store),
                shell_tick_area(),
                TICKS,
                handle,
            )
            .await;

            let total = calls.load(Ordering::Relaxed);
            let remote = total - EXPECTED_TICK_AREA_COLUMNS as u64;
            // Printed as well as asserted: the doc quotes these, and a curve is
            // more use to the next reader than a bare pass.
            eprintln!(
                "entities {entities:>4}  resident-set {:>4}  ceiling {DEFAULT_CAPACITY}  \
                 remote generations {remote:>6}  evictions {:>6}  per tick {:>6.1}",
                EXPECTED_TICK_AREA_COLUMNS + entities as usize,
                store.evicted(),
                remote as f64 / f64::from(TICKS)
            );
            if over_capacity {
                let predicted = u64::from(entities as u32) * u64::from(TICKS);
                assert!(
                    remote >= predicted * 9 / 10,
                    "over capacity ({entities} entities + \
                     {EXPECTED_TICK_AREA_COLUMNS} tick area > {DEFAULT_CAPACITY}): a cyclic scan \
                     through a smaller LRU must miss on essentially every probe, so remote \
                     generations should approach {entities} × {TICKS} = {predicted}; got \
                     {remote}. {entities} would mean the store was still absorbing them."
                );
                assert!(
                    store.evicted() > 0,
                    "precondition: the over-capacity band must actually evict"
                );
            } else {
                assert_eq!(
                    remote,
                    u64::from(entities as u32),
                    "under capacity ({entities} entities + {EXPECTED_TICK_AREA_COLUMNS} tick \
                     area <= {DEFAULT_CAPACITY}): each column must be generated exactly once for \
                     the whole session, so {entities} total. {} would be one per tick.",
                    u64::from(entities as u32) * u64::from(TICKS)
                );
                assert_eq!(
                    store.evicted(),
                    0,
                    "precondition: the under-capacity band must not evict, or it is not the \
                     regime it claims to be"
                );
            }
        }
    }

    /// The registry is scanned **unfiltered** — and the 1,608-of-1,613
    /// unsimulated kinds issue #477 round-trips as
    /// [`BlockEntity::Opaque`](crate::block_entities::BlockEntity::Opaque) reach
    /// the world **zero** times regardless.
    ///
    /// `tick_all_with_hopper_lock` does not filter: it collects *every* key into
    /// a fresh `Vec` and dispatches on the variant, so a vanilla world's whole
    /// block-entity population is walked at 20 Hz. That is a real property and
    /// worth pinning — but the cost of an `Opaque` entry is a `Vec` push, two
    /// hash probes and an empty `tick_non_hopper` arm. **No coordinate of it
    /// reaches the store**, which is what this gate measures: with #477's own
    /// figure of 1,608 opaque entities scattered over distinct far-flung
    /// columns, the store still generates only the 49-column tick area.
    ///
    /// The competing hypothesis is `49 + 1608`: if the scan probed the world per
    /// entry — the shape the lead attributes to it — every one of those columns
    /// would be cold.
    #[tokio::test(start_paused = true)]
    async fn sixteen_hundred_opaque_block_entities_never_reach_the_store() {
        /// Issue #477's measured figure for a real vanilla world: 1,608 of its
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
                        id: "minecraft:chest".to_owned(),
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
}
