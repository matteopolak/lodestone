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
//! Retention turns a column's representation into real resident memory, which is
//! `docs/plans/chunk-lifecycle.md`'s top risk and the reason this type is
//! bounded rather than a plain `HashMap`.
//!
//! **Measured, not assumed** (the plan's U2 question, answered here because
//! this is the unit that creates the cost). `/usr/bin/time -l` on the release
//! lib-test binary, one arm per configuration, `measure_rss_without_retention`
//! the shared control.
//!
//! ## Before issue #551: a dense `Vec<u16>` over full world height
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
//! ## After issue #551: bit-packed per section (`crate::chunk_blocks`)
//!
//! Same three arms, same command, same box:
//!
//! | arm | peak RSS | delta | per column |
//! |---|---|---|---|
//! | retention off (the shared control) | 8.4 MiB | — | — |
//! | 512 retained | 24.0 MiB | 15.5 MiB | **31.1 KiB** |
//! | **1,275 retained** ([`MAX_CAPACITY`]) | **47.3 MiB** | **38.9 MiB** | **31.2 KiB** |
//!
//! **195.5 KiB → 31.1 KiB per retained column, a 6.3× cut**, and the rate is
//! again flat across the 2.5× range (31.1 vs 31.2), so residency stays linear in
//! the retained count. Of the 31.1 KiB, `ChunkColumn::blocks_heap_bytes` accounts
//! for ~24 KiB and the rest is the palette `String`s, the 3-D biome grid (~3 KiB,
//! issue #512 — now the *second* largest term and the next thing to look at) and
//! the map entry.
//!
//! ## Comparing the two tables is sound, and it is worth saying why
//!
//! `touched_column` had to change: its old 48-writes-per-column shape faulted
//! every page of a contiguous 192 KiB allocation but packs to ~12 KiB under the
//! new representation, which would have reported a saving no real column gets (see
//! that function's own doc — it is a worked example of CLAUDE.md's *world* species
//! of vacuous test). The two tables are nonetheless directly comparable **because
//! the old representation's cost was independent of a column's content**: 192 KiB
//! was `vec![0u16; 16 * 16 * height]` whatever went in it, so the 194.8 KiB row is
//! valid for the new fixture too. The new fixture is calibrated against four
//! *real* generated columns (mean 24,112 packed bytes), so the after-table is not
//! flattered by its input either.
//!
//! The two arms of each table are also each other's control: a delta near zero
//! would mean the columns were dropped in both arms, or that the pages were never
//! faulted in, and the run would be a failure to measure rather than evidence that
//! residency is free.
//!
//! [`capacity_for_view_radius`] is the knob, and 15.5 MiB is what it buys at the
//! default render distance. Lowering it to 128 (~4 MiB) **still fixes the
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
//! store at 4,539 columns, i.e. **139.2 MiB — measured directly** by
//! `measure_rss_at_the_singleplayer_slider_maximum` rather than extrapolated (it
//! was 867 MiB before issue #551) — the memory of the person who moved the
//! slider, and
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
//! **Reducing the cost rather than the count is done** — unit **U8** of the plan,
//! issue #551, `crate::chunk_blocks`, and the after-table above is the measurement
//! it was gated on. What U8 still leaves on the table is `Arc<ChunkSection>`
//! copy-on-write sharing between the store and the wire encoder; the remaining
//! per-column terms are now the biome grid (~3 KiB) and the palette `String`s
//! rather than the block grid.
//!
//! # The clone this keeps, deliberately
//!
//! [`ChunkSource::column`] returns a `ChunkColumn` **by value**, so a store
//! read is a deep copy (measured in the gate below, tens of microseconds) rather
//! than a refcount bump. Since issue #551 it copies ~24 KiB of packed sections
//! instead of a flat 192 KiB, so this trade got cheaper by the same factor the
//! residency did — but it is now `sections.len()` separate `Vec` allocations
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

use std::collections::HashMap;
use std::collections::hash_map::Entry as MapEntry;
use std::sync::Mutex;

use crate::chunk::{ChunkColumn, ChunkSource};
use crate::ticket::{TicketDelta, TicketKind, TicketOwner, TicketStoreHandle};

/// The floor under [`capacity_for_view_radius`], and the capacity a radius-less
/// `ChunkStore::new` store retains before evicting the least-recently-used one.
///
/// 512 packed full-height columns measured **15.5 MiB** of resident memory (see
/// this module's memory section for the paired `/usr/bin/time -l` arms — that is
/// a measurement, not arithmetic; it was 97.6 MiB before issue #551 packed the
/// grid per section). It holds `run_tick_loop`'s
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
/// byte-for-byte the 512-column configuration it has always been (15.5 MiB since
/// issue #551, 97.6 MiB before it).
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
/// **This section's original reason is now historical, and the replacement reason
/// is weaker but still real.** It used to read: *"`mob_area` is centred on world
/// spawn and never moves, so once the player has walked away it is 49 columns
/// outside the streamed view that are still touched at 20 Hz"* — which was true,
/// and was the bug `crate::tick_area` fixes. The tick area now follows the players,
/// so in the steady state those 49 columns are a **subset** of the streamed view
/// rather than a disjoint square, and the union has collapsed.
///
/// The headroom is kept because the collapse is not instantaneous. The area moves
/// the tick a player's movement packet lands, which is before that strip has
/// finished streaming; and `crate::tick::run_tick_loop`'s
/// [`crate::tick::INITIAL_RANDOM_TICK_DEFERRAL_TICKS`] deferral, a teleport, and the
/// playerless fallback square all put the tick area transiently outside the view.
/// The working set is still the **union** of the concurrent scans, not the largest
/// of them; the union is simply much smaller than it was.
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
/// than arithmetic — `measure_rss_without_retention` (8.4 MiB) is the control
/// both are differenced against. **Re-measured for issue #551**, which packed the
/// block grid per section and cut the per-column rate 6.3×; the pre-#551 column
/// is kept because the argument for the number was made against it:
///
/// | `render_distance` | `view_radius` | view columns | capacity | resident | pre-#551 |
/// |---|---|---|---|---|---|
/// | 8 (default) | 9 | 361 | 512 (floor) | **15.5 MiB, measured** | 97.4 MiB |
/// | 10 | 11 | 529 | 579 | 17.6 MiB | 110 MiB |
/// | 12 (vanilla default) | 13 | 729 | 779 | 23.7 MiB | 148 MiB |
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
/// **139.2 MiB, measured,** of resident chunk cache (867 MiB pre-#551) inside a process that
/// also holds meshes, textures and a GPU allocator, on a machine whose whole
/// budget this repo's own operational notes put at 16 GB shared with everything
/// else.
///
/// **The cap is a much weaker call than it was.** It was defending against 867
/// MiB; it now defends against 139 MiB, which is within what a hosted server can
/// reasonably spend. Nothing here has been changed on that basis — moving a policy
/// constant is a separate decision from changing a representation — but a future
/// reader asking "why is this 17 and not 33?" should know the answer is now
/// "history", not "867 MiB".
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
/// * the **ceiling** stops a CPU cliff being traded for a memory one — 139 MiB at
///   `render_distance` 32 since issue #551, and 866 MiB before it, which is why
///   the ceiling's own doc now argues from a much smaller number than it used to.
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
/// This store costs a measured **31.1 KiB per retained column** since issue #551
/// packed the block grid per section (the module docs' before/after tables,
/// `/usr/bin/time -l` on the release lib-test binary). The streamed set is
/// `(2 × (rd + 1) + 1)²`, and capacity is that plus [`CONCURRENT_SCAN_COLUMNS`]:
///
/// | `render_distance` | view columns | capacity | resident | pre-#551 |
/// |---|---|---|---|---|
/// | 8 (our default) | 361 | 512 (floor) | 15.5 MiB, measured | 97.4 MiB |
/// | 12 (vanilla default) | 729 | 779 | 23.7 MiB | 148 MiB |
/// | 16 | 1,225 | 1,275 | 38.9 MiB, measured | 242.0 MiB |
/// | 24 | 2,601 | 2,651 | 80.5 MiB | 506 MiB |
/// | **32 (slider max)** | **4,489** | **4,539** | **139.2 MiB, measured** | 867 MiB |
///
/// Residency measured flat at 31.1–31.4 KiB per column across a **8.9×** range —
/// the last row used to be a 3.6× extrapolation and is now
/// `measure_rss_at_the_singleplayer_slider_maximum`, an arm that only became
/// affordable to run *because* of issue #551. It landed within 1% of the
/// extrapolation, which is the retrospective justification for reading the
/// interpolated rows.
///
/// **This table is the whole reason issue #551 was worth doing**, and it is worth
/// saying what changed about the *argument* rather than only about the numbers.
/// The uncapped policy used to be a genuine trade — 867 MiB is a real cost to a
/// client process that also holds meshes, textures and a GPU allocator, on a
/// machine this repo's operational notes budget at 16 GB, and "the user's call"
/// was doing load-bearing work in justifying it. At 139 MiB it is barely a trade
/// at all. The per-column rate, not the policy, was the thing worth fixing.
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

/// Which derivation a store re-applies when a connection changes its view radius
/// mid-session ([`ChunkStore::set_retention_radius`], issue #551).
///
/// The store has to *remember* which of the two policies built it, because the two
/// differ (`MAX_CAPACITY` ceiling or not) and "whose memory is this" does not
/// change when the slider moves. A third arm is needed for the explicit-capacity
/// constructor, which must never resize at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CapacityPolicy {
    /// [`capacity_for_view_radius`] — the hosted (open-to-LAN) ceiling applies.
    Hosted,
    /// [`integrated_capacity_for_view_radius`] — singleplayer, uncapped.
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
    /// The eviction bound. **Inside the mutex since issue #551**, because a live
    /// render-distance change re-derives it — see
    /// [`ChunkStore::set_retention_radius`]. It was a plain `usize` field behind
    /// the `Arc`, i.e. immutable for the life of the store, which is exactly why
    /// raising render distance mid-session over-subscribed the cache.
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

impl Cache {
    fn next_stamp(&mut self) -> u64 {
        self.stamp += 1;
        self.stamp
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
    fn evict_down_to_capacity(&mut self) -> Vec<(i32, i32)> {
        let capacity = self.capacity;
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
    /// Which derivation [`set_retention_radius`](Self::set_retention_radius)
    /// re-applies. Immutable for the life of the store, unlike the capacity it
    /// produces — the *policy* is a question about whose memory this is, and that
    /// does not change when the slider moves.
    policy: CapacityPolicy,
    cache: Mutex<Cache>,
    /// The ticket graph this store's residency answers to — issue #289. See
    /// [`maybe_tick_tickets`](Self::maybe_tick_tickets) for how it is driven and
    /// this module's own doc section on the ticket/status pipeline for the
    /// design (why this is a plain field rather than a new
    /// [`ChunkSource`] trait method).
    tickets: TicketStoreHandle,
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
                capacity,
                stamp: 0,
                generated: 0,
                evicted: 0,
                next_ticket_check: 0,
            }),
            tickets: TicketStoreHandle::new(),
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

    /// The store's **current** eviction bound. No longer "what it was built
    /// with": [`set_retention_radius`](Self::set_retention_radius) moves it.
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
        f.debug_struct("ChunkStore")
            .field("resident", &cache.columns.len())
            .field("capacity", &cache.capacity)
            .field("policy", &self.policy)
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
        // Issue #289: a cheap, rate-limited check-in with the ticket graph on
        // every real op through this store — see `maybe_tick_tickets`'s own
        // doc for why this piggybacks on read traffic instead of a new
        // `run_tick_loop` parameter.
        self.maybe_tick_tickets();
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
        if cache.capacity == 0 {
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
        let evicted = cache.evict_down_to_capacity();
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
    pub(crate) fn set_spawn_ticket(&self, pos: (i32, i32), radius: i32) {
        self.tickets.set_ticket_with_radius(
            TicketOwner::Spawn,
            TicketKind::PlayerSpawn,
            pos,
            radius,
        );
    }

    /// Refreshes the spawn ticket's countdown without moving it — vanilla's
    /// `Ready.keepAlive()`. A caller that never refreshes lets it expire
    /// naturally after 20 ticks, which is correct for "just enough terrain to
    /// join into," not a bug to route around.
    pub(crate) fn refresh_spawn_ticket(&self) -> bool {
        self.tickets
            .refresh_ticket(TicketOwner::Spawn, TicketKind::PlayerSpawn)
    }

    /// Grants a persistent, simulating `FORCED` ticket at `pos` — vanilla's
    /// `/forceload`, `TicketLevel = ChunkLevel.byStatus(ENTITY_TICKING) = 31`.
    /// `id` distinguishes more than one forced region; the caller owns
    /// uniqueness (a serial counter is enough).
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
    /// # Safety of calling `unload` from here
    ///
    /// Locks are never held across it: the ticket-graph lock and the cache
    /// lock are each acquired and released in their own scope before
    /// `self.source.unload` is called, matching the existing
    /// `evict_down_to_capacity` pattern in [`ensure`](Self::ensure) — see
    /// [`ChunkSource::unload`]'s own doc for why it must do no I/O and cannot
    /// call back into this store.
    fn maybe_tick_tickets(&self) {
        let due = {
            let mut guard = self.lock();
            let stamp = guard.stamp;
            if stamp < guard.next_ticket_check {
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
        let evicted: Vec<(i32, i32)> = {
            let mut guard = self.lock();
            let cache = &mut *guard;
            let mut out = Vec::new();
            for pos in &delta.newly_unresident {
                if cache.columns.remove(pos).is_some() {
                    cache.evicted += 1;
                    out.push(*pos);
                }
            }
            out
        };
        // Outside the lock, deliberately — same reasoning as
        // `evict_down_to_capacity`'s call site in `ensure`.
        for (vx, vz) in evicted {
            self.source.unload(vx, vz);
        }
    }
}

impl<S: ChunkSource> ChunkSource for ChunkStore<S> {
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
        if let Some(fresh) = self.ensure(cx, cz) {
            return find(&fresh);
        }
        self.read(cx, cz, find)
            .unwrap_or_else(|| self.source.block_entity(x, y, z))
    }

    /// The one real override of [`ChunkSource::is_column_resident`] (issue
    /// #504) — a plain map lookup, no generation, and no `last_used` bump.
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

    /// Re-derives the capacity for `view_radius` under this store's
    /// [`CapacityPolicy`], evicting down to it if it shrank — issue #551's
    /// second half.
    ///
    /// # Why this is the fix and not a nice-to-have
    ///
    /// The whole of [`integrated_capacity_for_view_radius`]'s "why the ceiling
    /// existed" section applies to an *over-subscribed* store just as it does to a
    /// capped one, because they are the same condition: capacity below the streamed
    /// view. `crate::server`'s `join_view_rings` streams outward, so the
    /// least-recently-used entry is the **innermost** ring — the column the player
    /// is standing in, which `vitals_tick` probes every 50 ms and `run_tick_loop`
    /// random-ticks. Raising render distance mid-session therefore produced exactly
    /// the LRU collapse the fixed literal produced before issue #505: the horizon
    /// arrived and the ground underfoot was regenerated at ~909 ms a column.
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
    /// * The memory it would reclaim is now small. Before issue #551 the ceiling
    ///   was 867 MiB and handing it back mattered; the same ceiling is 139 MiB
    ///   packed, and it is only reached by a player who *did* ask for
    ///   `render_distance` 32 at some point in the session.
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
        /// Every `(cx, cz)` this source's `unload` was called with, in call
        /// order — issue #289's ticket-driven eviction gate needs to observe
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

        fn unloaded(&self) -> Vec<(i32, i32)> {
            self.unloaded.lock().expect("unloaded log poisoned").clone()
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

        fn unload(&self, cx: i32, cz: i32) {
            self.unloaded
                .lock()
                .expect("unloaded log poisoned")
                .push((cx, cz));
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

    /// [`ChunkSource::is_column_resident`] (issue #504): reports real
    /// residency with no generation at all, in both directions.
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
        // with `--nocapture`. The point of recording it is that it is
        // microseconds against the 909 ms it replaces.
        //
        // `touched_column`, not `ChunkColumn::new`: since issue #551 an all-air
        // column's clone allocates *nothing* (every section is `Uniform`), so
        // timing a blank column would report the cost of cloning a `Vec` of 24
        // enum discriminants and call it the store's read cost. That is the
        // fixture-premise trap `touched_column`'s own doc describes, in a second
        // place.
        let column = touched_column(REAL_MIN_Y, REAL_HEIGHT);
        let started = web_time::Instant::now();
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

    /// A full-height column shaped like real terrain, so its **representation
    /// cost** is the one production pays.
    ///
    /// # This fixture's premise was falsified once, and reading it is the lesson
    ///
    /// It used to write **one cell per 8 y-rows** — 48 writes in a 98,304-cell
    /// column — and that was exactly right for what it was built against: a flat
    /// `vec![0u16; 98304]` is `alloc_zeroed`, which macOS can serve from
    /// lazily-zeroed pages the process never faults in, so an *untouched* column
    /// understated RSS. 48 scattered writes faulted every page of a contiguous
    /// 192 KiB allocation, and the column's cost was independent of its content.
    ///
    /// Issue #551 made the cost a **function of the content** (see
    /// `crate::chunk_blocks`), and that premise died silently and in the
    /// *safe*-looking direction: the fixture still runs, still faults its pages,
    /// still produces a plausible non-zero delta — but 48 writes over 24 sections
    /// leaves every section holding two distinct ids, which packs to **1 bit**,
    /// about 12 KiB a column. It would have reported a spectacular saving that no
    /// real column gets. That is CLAUDE.md's *world* species of vacuity: the flaw
    /// is in the input data, not in any assertion, and nothing about the code
    /// looks wrong.
    ///
    /// # What it models now
    ///
    /// Terrain below a surface, air above it, with the block-state variety a real
    /// column has — which is what decides both savings: how many sections collapse
    /// to uniform air, and how wide the rest have to pack. The states are real
    /// vanilla ids from the generator's own surface/stone rules rather than
    /// `set_solid`'s stone-or-air pair, because a two-state palette is precisely
    /// the input that would flatter the packing.
    ///
    /// It remains a *model*, but a **calibrated** one rather than a plausible
    /// one. Four real generated columns measured
    /// `ChunkColumn::blocks_heap_bytes` at 22,640 / 23,328 / 23,728 / 26,752
    /// bytes (mean **24,112**, against the flat grid's 196,608). This fixture is
    /// shaped to land on that: 12 states plus air is 4 bits, and a surface at
    /// `min_y + height / 2` leaves 12 packed sections of `4096 × 4 / 8` = 2,048
    /// bytes plus 12 uniform air sections, i.e. **24,576 bytes** — within 2% of
    /// the real mean. Getting that agreement is the point; a fixture that
    /// understated it would make every RSS row below optimistic.
    fn touched_column(min_y: i32, height: i32) -> ChunkColumn {
        // Real ids the overworld generator emits, so the palette width the
        // sections pack to is the production one. 12 states + air = 13 => 4 bits
        // for a section drawing from all of them, and the surface band draws
        // from more of them than the deep band, exactly as real terrain does.
        const STATES: [&str; 12] = [
            "minecraft:stone",
            "minecraft:deepslate",
            "minecraft:dirt",
            "minecraft:gravel",
            "minecraft:andesite",
            "minecraft:diorite",
            "minecraft:granite",
            "minecraft:grass_block[snowy=false]",
            "minecraft:water[level=0]",
            "minecraft:coal_ore",
            "minecraft:iron_ore",
            "minecraft:sand",
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
                    column.set_block(x, y, z, STATES[n % STATES.len()]);
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
    /// 3.6× extrapolation of the rate measured at 1,275, and issue #551's whole
    /// motivation was the owner asking about RSS at high render distance. It is
    /// affordable now precisely *because* of #551: at the pre-#551 rate this arm
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
            let started = web_time::Instant::now();
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

        let started = web_time::Instant::now();
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

    /// **The subject, re-measured after issue #504's fix.** A hopper whose
    /// column the player has walked away from now costs **zero** column
    /// generations, not one and not [`TICKS`].
    ///
    /// # What changed from the pre-fix measurement
    ///
    /// Before #504, `tick.rs`'s block-entity arm called `world.block_state`
    /// per hopper every tick unconditionally, and `ChunkStore::block_state`
    /// regenerates a whole column on a miss — so a hopper 1,600 blocks out
    /// (never inside [`shell_tick_area`], never in any view) cost exactly
    /// **1** generation (the store's own pinning-once-resident behaviour;
    /// see this test's history for the two-hypotheses argument that used to
    /// live here). The fix does not make that read cheaper — it stops the
    /// read from happening at all: `tick_all_with_hopper_lock` now takes an
    /// `is_loaded` predicate and skips a hopper's tick (and therefore its
    /// `enabled` closure, and therefore `world.block_state`) entirely when
    /// its chunk is not resident. A never-loaded remote hopper now touches
    /// the store **zero** times over its whole life, matching vanilla: a
    /// block entity outside every loaded chunk does not tick.
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
             {TICKS} ticks. Pre-#504 this was 1 (the store pinning a column it had already \
             probed); {TICKS} was issue #503's lead (one cold column per tick, no pinning at \
             all). 0 is the fix: a hopper whose chunk is never loaded is never probed."
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
    /// Without this arm the fix could be "skip everything" and every gate in
    /// this module would still be green — the same *assertion* species of
    /// vacuity the pre-fix test module already guarded against, now aimed at
    /// the new gate instead of the old absence of one.
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
    /// finding that per-column cost is itself flat to 1,048,576 blocks. Pre-#504
    /// that flat number was **1** (see `a_walked_away_hopper_never_reaches_the_
    /// store`'s history); post-fix it is **0**, because none of these bands are
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
                "band {chunk:?} ({} blocks out): the remote column must never be generated at \
                 all — a nonzero, band-dependent count here would be the distance-dependent CPU \
                 term issue #503's lead predicted, now supposedly fixed.",
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

    /// **Re-measured after #504: capacity pressure from the registry scan is
    /// gone, by construction.** Pre-fix, a remote hopper's column competed
    /// for the store's capacity like any other resident column, so pinching
    /// the ceiling down to exactly the tick area's size (`with_capacity(source,
    /// EXPECTED_TICK_AREA_COLUMNS)`) forced an eviction and turned the "self
    /// heals to 1" result into "cold once per random-tick pass" (measured 12
    /// at the time — see the git history on this test for the full argument).
    /// Post-fix there is nothing left to evict: `is_loaded` means the remote
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
    /// tick** the moment they do not — **that was the pre-#504 shape.**
    ///
    /// This is the exact scenario issue #503's lead was really about: a
    /// `BlockEntityRegistry` that only ever grows (no unload path) walked at
    /// 20 Hz by an unfiltered scan, past the store's fixed ceiling. Pre-fix,
    /// each probed position held one column resident, so exploration alone
    /// turned the registry into a cyclic scan through an LRU smaller than it
    /// — LRU's worst case, where the miss rate goes from 0 to 1 over a narrow
    /// band (measured at the time: 400 entities → 400 remote generations
    /// total; 600 entities → 31,739, matching the predicted `600 × TICKS` =
    /// 31,200 to 1.7%).
    ///
    /// **Issue #504's fix removes the mechanism entirely, not just its cost.**
    /// None of these entities' chunks are ever loaded (they sit at
    /// `(2_000 + i, 2_000 + i)`, far outside [`shell_tick_area`] and touched by
    /// nothing else in this rig), so `is_loaded` rejects every one of them
    /// before `enabled` — and therefore `world.block_state` — is ever called.
    /// The registry can hold 400, 600, or 400,000 hoppers with identical
    /// result: **zero** remote generations and **zero** evictions, at both
    /// bands, because nothing about an unloaded hopper ever reaches the store.
    /// This is the arm this module's own history said would go red "by
    /// design" once the fix landed — it did, and this is the rewrite against
    /// the new bound, not a relaxation of the old assertion.
    #[tokio::test(start_paused = true)]
    async fn the_registry_outgrowing_the_store_no_longer_moves_the_miss_rate() {
        /// Comfortably under the pre-fix ceiling: `49 + 400 = 449 <= 512`.
        const UNDER: i32 = 400;
        /// Over the pre-fix ceiling: `49 + 600 = 649 > 512`. Both bands are
        /// kept from the pre-#504 version of this test specifically so the
        /// "no longer depends on which side of the ceiling you're on" claim
        /// is checked at both of the values that used to disagree.
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

    // Issue #289: the ticket graph driving this store's residency, above and
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
    /// ticket, and removing the ticket lets it unload — observed at the real
    /// [`ChunkSource::unload`] call the source receives, not merely at the
    /// cache entry disappearing (which would also happen for an unrelated
    /// reason, e.g. LRU pressure).
    #[test]
    fn removing_a_forced_ticket_lets_its_chunk_unload_and_the_source_observes_it() {
        let counting = CountingSource::new();
        let unloaded_log = Arc::clone(&counting.unloaded);
        let store = Arc::new(ChunkStore::new(counting));

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
        // Drive enough further traffic (elsewhere, so it does not touch (5,5)
        // and re-cache it) for the next check-in to observe the removal.
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
}
