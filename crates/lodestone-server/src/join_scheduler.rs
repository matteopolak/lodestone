//! The join burst's generation scheduler: a **primed sliding window** over the
//! wire order, with a single-column first emission and bounded concurrency.
//!
//! # Why the pipeline uses a bounded window
//!
//! Join generation queues columns by priority and keeps only a bounded number
//! in flight. The window is derived from `available_parallelism`, not the view
//! radius ([`generation_window`]), so a large visible area cannot create an
//! unbounded blocking-pool fan-out.
//!
//! Each staged world-generation cache computes a column once even when several
//! requests race. The scheduler handles a separate concern: limiting
//! concurrent generation and preserving deterministic wire order while columns
//! complete at different speeds.
//!
//! # The dependency edges it schedules on
//!
//! The dependency model for a column `C` is:
//!
//! * fill / surface / carve depend on the seed alone — embarrassingly parallel;
//! * `ore(C)` reads `pre_ore(3×3(C))`;
//! * `veg(C)` reads `post_ore(3×3(C))`, which closes over `pre_ore(5×5(C))`;
//! * `top_layer(C)` depends on `veg(C)` alone.
//!
//! So two columns at Chebyshev distance ≥ 5 share **no** store entry and are
//! wholly independent; adjacent columns share 20 of their 25 pre-ore entries.
//! Those shared entries are the dependency edges, and each per-entry `OnceLock`
//! honours them: a second worker arriving mid-computation *waits for the value*
//! instead of computing its own copy. The synchronisation is per-edge rather
//! than per-ring — two workers block only when they need the same chunk's same
//! stage at the same instant.
//!
//! Because the window is a **contiguous** window over the outward ring order, the
//! in-flight set is always spatially local, so those shared edges are hits rather
//! than independent cold computations. Nothing here needs to know that; it falls
//! out of scheduling in wire order.
//!
//! # Why "primed"
//!
//! The player's own column reaches the client after **one** column of
//! generation, not after the whole view. A plain sliding
//! window would break it: the window is filled *before* the head is awaited, so on
//! a fast source the entire window completes before the first emit and
//! "columns generated before the first chunk was encoded" jumps from 1 to `window`.
//!
//! So the window is **1 for the first column and `window` thereafter**
//! ([`ColumnPipeline::next`]). The head column is generated alone, which is
//! exactly the one-column serialisation documented as a
//! deliberate trade; every column after it runs with the window fully open. The
//! barrier is absent for rings 1..=r and retained, deliberately, for the single
//! column of ring 0.
//!
//! This is a **counter**, not a timing: `join_scheduler_gates.rs` asserts
//! `columns_completed_before_first_emit == 1` on both arms, while the window
//! keeps the remaining columns in flight behind that first emission.
//!
//! # Only the `SourceRef::Shared` arm is scheduled, and that is not an oversight
//!
//! `SourceRef::Borrowed` (the transport tests) holds a source that is not
//! `'static`, so it cannot be spawned at all: every batch on that arm is a
//! `generate_columns_parallel` call that blocks until the whole batch finishes.
//! A window's entire payoff is overlapping generation with the *encode* of an
//! already-finished column, and a blocking source has nothing to overlap. Worse,
//! measured while building `join_scheduler_gates.rs`: the rings' cumulative sizes
//! are `1 + 4r(r + 1)`, which is always ≡ 1 (mod 8), so at a window of 8 no
//! window-sized batch even *straddles* a ring boundary — the split would replace
//! ring 8's single 64-column batch with eight serial ones and add barriers rather
//! than remove them. So that arm keeps the rings, and what is held identical
//! across the two arms is the **wire order**, which is what the client sees and
//! what both `805a1fb` gates assert.
//!
//! # The wire order
//!
//! The pending stream uses two properties while preserving the wire order:
//!
//! * the join emits its innermost rings inline, and the rest becomes a
//!   [`JoinChunkStream`] the play loop
//!   drains, so the *pending* set outlives the moment its order was chosen;
//! * a pending set that outlives that moment can be re-keyed, which is what
//!   "generate where the player is looking, and re-sort when they move" needs
//!   ([`ColumnQueue`], [`priority_key`]).
//!
//! The order is `(Chebyshev distance, in-frustum bonus, ring-walk index)`.
//! With **no rotation known** — every client that has not yet sent a movement
//! packet, which includes every ordering gate in this crate — that key reduces to
//! the ring walk exactly, because distance *is* the ring index and the tie-break
//! *is* the given order. Distance stays primary so the frustum bonus can only
//! reorder within a ring: a near column behind the player always beats a far one
//! in front, which is what stops a slowly spinning player starving what is behind
//! them.
//!
//! Emitting in *completion* order would still be the natural mistake, and it is
//! still what `tests::control_completion_order_is_not_input_order` exists to
//! reject: the pipeline chooses priority at **spawn** time and emits in spawn
//! order, so the emitted sequence is a function of the queue rather than of which
//! worker happened to finish first.

use std::collections::VecDeque;
use std::sync::Arc;

use crate::chunk::{ChunkColumn, ChunkGenerationStage, ChunkSource};
use crate::protocol::{ChunkEncodeError, ChunkEncoder, ServerDirective};
use crate::server::SourceRef;

/// What one pipeline slot hands back: either the wire bytes, already encoded on
/// the worker that generated the column, or the column itself for a caller with
/// no off-task encoder.
///
/// # Why this is an enum and not just `ServerDirective`
///
/// [`ChunkEncoder`] is optional — [`crate::protocol::ServerProtocol::chunk_encoder`]
/// defaults to `None`, so every test protocol in this workspace and every
/// legacy family keeps encoding on the connection task exactly as before. The
/// fallback arm is that path, not a degenerate case: `crate::server`'s
/// `encode_column` turns either arm into one directive, so the wire is identical
/// whichever arm a caller is on. That identity is what makes the encoder
/// adoptable one family at a time.
#[derive(Debug)]
pub enum ColumnPayload {
    /// Encoded on the blocking worker, off the connection task. The win.
    Encoded(ServerDirective),
    /// No off-task encoder: the caller encodes this itself, on its own task.
    Column(ChunkColumn),
}

impl ColumnPayload {
    /// The column, for a caller that wants the terrain rather than the bytes —
    /// `None` once it has been encoded and dropped.
    ///
    /// Only the gates in `tests/join_parallel_efficiency.rs` need this (they read
    /// `solid_count()` to keep the generator's work from being optimised away);
    /// production only ever writes the directive.
    #[must_use]
    pub fn column(&self) -> Option<&ChunkColumn> {
        match self {
            Self::Column(column) => Some(column),
            Self::Encoded(_) => None,
        }
    }
}

/// Half-angle, in degrees, of the horizontal cone counted as "the player is
/// looking at this column" by [`ColumnQueue`]'s frustum bonus.
///
/// Generous on purpose. Vanilla's default 70° *vertical* FOV is roughly 106°
/// horizontal at 16:9, so a 60° half-angle (120° total) is the real view plus a
/// margin — a column that is about to rotate into view should already have been
/// generated. Over-including costs ordering precision within one distance band
/// and nothing else; under-including shows the player a hole in the direction
/// they are actually facing, which is the whole complaint this exists to answer.
const FRUSTUM_HALF_ANGLE_DEGREES: f32 = 60.0;

/// Radius around a player that receives complete terrain generation when the
/// streaming path enables progressive generation.
///
/// Eight strictly contains the ticked/mob/interaction area (radius three) and
/// leaves a four-chunk margin for movement and packet latency. It is not a
/// cache or allocation limit: callers still bound their streamed view and its
/// retained columns independently.
pub const DEFAULT_FULL_GENERATION_RADIUS: i32 = 8;

/// How finely a yaw is quantised before it counts as "the player turned".
///
/// 16 sectors of 22.5°. This is a *re-sort trigger*, not part of the ordering:
/// the frustum test itself uses the raw yaw. Quantising means a player panning
/// smoothly re-sorts the pending set ~16 times per revolution rather than once
/// per movement packet, which is the cheap half of "re-prioritisation must be
/// cheap" (the other half is that a sort of ≤ 1,089 `(i32, u8, u32)` keys is
/// microseconds).
const YAW_SECTORS: f32 = 16.0;

/// Chebyshev (chess-king) ring index of `coord` around `centre` — **the same
/// distance `crate::server`'s `join_view_rings` orders on**, which is what makes
/// a distance-ordered queue with an unknown facing byte-identical to the fixed
/// ring walk it replaced.
#[must_use]
fn ring_distance(centre: (i32, i32), coord: (i32, i32)) -> i32 {
    (coord.0 - centre.0).abs().max((coord.1 - centre.1).abs())
}

/// This module's own generation priority, expressed as a `crate::ticket`
/// **level** rather than as a bare ring index — "priority is the
/// ticket level" unified with this module's pre-existing, independently-built
/// distance/frustum queue.
///
/// # Why a formula, and not a shared [`crate::ticket::TicketStore`]
///
/// This module's [`ColumnQueue`] answers "in what order should *this
/// connection's* still-owed columns be generated" — a per-connection wire-order
/// question with its own frustum bonus and re-prioritisation-on-turn, already
/// solved (see the module doc's "The wire order" section). `crate::ticket`
/// answers a different question: "does any chunk anywhere want to exist at
/// all, independent of one connection's view." Coupling the two stores would
/// make a join's *wire order* depend on residency bookkeeping that has nothing
/// to do with it, and — the reason this crate does not do it this session —
/// every call site that would grant the ticket lives in `crate::server`'s
/// generic, `S: ChunkSource`-parameterised connection code, which cannot reach
/// a concrete `crate::chunk_store::ChunkStore`'s ticket handle without either a
/// new `ChunkSource` trait method (the exact unforwarded-wrapper trap
/// `crate::dimension::DimensionalSource` already carries a scar from) or a
/// signature change to a public, cross-crate entry point. See
/// `docs/chunk-tickets.md` for the fuller account.
///
/// What *is* shared, safely, is the **arithmetic**: `crate::ticket::TicketStore`'s
/// propagator computes `ticket.level + chebyshev(ticket.pos, target)` for
/// every position within the ticket's reach (`crate::ticket::TicketStore`'s
/// own `propagate_one`), and `base_level + ring` here is exactly that
/// expression read backwards — "how urgent" from "how far," off the same
/// centre-level convention. `ring` is [`ring_distance`]'s output (or any
/// Chebyshev distance from a priority centre); `base_level` is the level a
/// ticket at the centre would carry (`crate::ticket::FULL_CHUNK_LEVEL - radius`
/// for a radius-derived grant, per that module).
///
/// **Only meaningful within the granting ticket's own reach**
/// (`crate::ticket::MAX_LEVEL - base_level`) — beyond it a real
/// [`crate::ticket::TicketStore`] does not report a level at all (the position
/// is simply unresident), so a level computed past that point describes
/// nothing a real ticket would ever produce and must not be compared to one.
#[must_use]
pub(crate) const fn ticket_level_for_ring(base_level: i32, ring: i32) -> i32 {
    base_level + ring
}

/// Whether `coord` lies inside the horizontal cone a player at `centre` facing
/// `yaw_degrees` can see, in Minecraft's yaw convention (0 = +Z, 90 = −X).
///
/// The player's own column and its eight neighbours are always "in view": the
/// direction vector to them is degenerate or dominated by the player's own
/// position within the column, and they are the ground under the player's feet
/// either way.
#[must_use]
fn in_frustum(centre: (i32, i32), yaw_degrees: f32, coord: (i32, i32)) -> bool {
    if ring_distance(centre, coord) <= 1 {
        return true;
    }
    let yaw = yaw_degrees.to_radians();
    // Minecraft's yaw: 0 looks towards +Z, 90 towards −X.
    let (fx, fz) = (-yaw.sin(), yaw.cos());
    let (dx, dz) = (
        (coord.0 - centre.0) as f32,
        (coord.1 - centre.1) as f32,
    );
    let len = (dx * dx + dz * dz).sqrt();
    if len == 0.0 {
        return true;
    }
    let cosine = (fx * dx + fz * dz) / len;
    cosine >= FRUSTUM_HALF_ANGLE_DEGREES.to_radians().cos()
}

/// The pending-column ordering: **distance first, in-frustum bonus second**.
///
/// Returned as a sort key rather than a comparator so the ordering is a total,
/// deterministic function of integers — `(ring, penalty, given_index)`:
///
/// * `ring` — Chebyshev distance from the current view centre. Primary, and that
///   is the anti-starvation property: a column at distance `d` *behind* the
///   player (`(d, 1, _)`) still sorts before every column at distance `d + 1`,
///   in view or not (`(d + 1, 0, _)`). Pure frustum-first would let a slowly
///   spinning player starve the columns behind them for minutes, and then show a
///   hole when they turn round.
/// * `penalty` — `0` in the facing cone, `1` outside it. This is the whole of
///   what "generate where the user is looking" means here: it reorders *within* a
///   ring and can never promote a far column over a near one.
/// * `given_index` — the column's position in the order the queue was handed,
///   i.e. the fixed outward ring walk. A deterministic tie-break, and the reason
///   a queue with **no** known facing emits exactly the ring order: with
///   `penalty` constant, this key is `(ring, 0, ring_walk_index)`, which is the
///   ring walk.
#[must_use]
fn priority_key(
    centre: (i32, i32),
    facing: Option<f32>,
    coord: (i32, i32),
    given_index: u32,
) -> (i32, u8, u32) {
    let (ring, penalty) = distance_and_penalty(centre, facing, coord);
    (ring, penalty, given_index)
}

/// [`priority_key`]'s first two components, which are the ordering proper — the
/// third is only a tie-break, and a caller with no "given order" needs its own.
#[must_use]
fn distance_and_penalty(centre: (i32, i32), facing: Option<f32>, coord: (i32, i32)) -> (i32, u8) {
    let penalty = match facing {
        Some(yaw) if in_frustum(centre, yaw, coord) => 0,
        Some(_) => 1,
        None => 0,
    };
    (ring_distance(centre, coord), penalty)
}

/// [`priority_key`] for a caller ordering a *set* rather than draining a queue —
/// `crate::server`'s `ViewTracker::build_batch`, which streams the columns that
/// became visible when the player moved.
///
/// Same two leading components, so a move is ordered exactly like a join; the
/// tie-break is the coordinate itself, because there is no prior order to inherit
/// and the wire order still has to be a deterministic function of the pose rather
/// than of `HashSet` iteration.
#[must_use]
pub(crate) fn view_order_key(
    centre: (i32, i32),
    facing: Option<f32>,
    coord: (i32, i32),
) -> (i32, u8, i32, i32) {
    let (ring, penalty) = distance_and_penalty(centre, facing, coord);
    (ring, penalty, coord.0, coord.1)
}

/// How a [`ColumnQueue`] decides what to hand out next.
#[derive(Debug, Clone, Copy)]
enum QueueOrder {
    /// Exactly the order the coordinates were given. Used by the pre-play-loop
    /// burst and by this module's own gates, where the input order *is* the
    /// assertion.
    AsGiven,
    /// [`priority_key`] around a centre that moves and a facing that turns.
    Priority {
        centre: (i32, i32),
        facing: Option<f32>,
        /// The quantised yaw the current ordering was computed for — see
        /// [`YAW_SECTORS`]. `None` means "no rotation known yet", which is a
        /// distinct state from "any particular sector".
        sector: Option<i32>,
    },
}

/// The set of columns a join still owes the client, in the order it intends to
/// generate them — and **re-orderable**, which is the property a plain `Vec`
/// walk could not offer.
///
/// Pops from the back of `pending`, so `pending` is always stored worst-first.
#[derive(Debug)]
pub(crate) struct ColumnQueue {
    /// `(coord, given_index)`, worst priority first: [`pop`](Self::pop) takes the
    /// last element.
    pending: Vec<((i32, i32), u32)>,
    order: QueueOrder,
}

impl ColumnQueue {
    /// A queue that hands `coords` back in exactly the order given.
    #[must_use]
    pub(crate) fn as_given(coords: Vec<(i32, i32)>) -> Self {
        let mut pending: Vec<((i32, i32), u32)> = coords
            .into_iter()
            .enumerate()
            .map(|(i, c)| (c, u32::try_from(i).unwrap_or(u32::MAX)))
            .collect();
        pending.reverse();
        Self {
            pending,
            order: QueueOrder::AsGiven,
        }
    }

    /// A queue ordered by [`priority_key`] around `centre`, with `facing` in
    /// degrees of yaw where the player's rotation is known.
    ///
    /// `coords` should be given in the fixed outward ring order: it becomes the
    /// tie-break, so a queue built this way with `facing: None` emits the ring
    /// order unchanged.
    #[must_use]
    pub(crate) fn prioritised(
        coords: Vec<(i32, i32)>,
        centre: (i32, i32),
        facing: Option<f32>,
    ) -> Self {
        let mut queue = Self::as_given(coords);
        queue.order = QueueOrder::Priority {
            centre,
            facing,
            sector: facing.map(yaw_sector),
        };
        queue.sort();
        queue
    }

    /// Re-keys the pending set for a player who has moved to `centre` or turned
    /// to `facing`, returning whether anything was actually re-ordered.
    ///
    /// A no-op — and specifically **not** a sort — when neither the centre chunk
    /// nor the quantised yaw changed, which is the common case on a movement
    /// packet arriving every few ticks. Also a no-op on an [`AsGiven`
    /// queue](QueueOrder::AsGiven): the pre-play-loop burst and the gates that
    /// assert a fixed order must not be re-ordered under them.
    pub(crate) fn reprioritise(&mut self, centre: (i32, i32), facing: Option<f32>) -> bool {
        let QueueOrder::Priority {
            centre: current_centre,
            facing: current_facing,
            sector: current_sector,
        } = self.order
        else {
            return false;
        };
        let sector = facing.map(yaw_sector);
        if current_centre == centre && current_sector == sector {
            // Keep the *old* yaw rather than storing the new one: the stored yaw
            // is what the ordering was computed from, and overwriting it with a
            // sub-sector nudge would make a later comparison lie.
            let _ = current_facing;
            return false;
        }
        self.order = QueueOrder::Priority {
            centre,
            facing,
            sector,
        };
        self.sort();
        true
    }

    /// Adds columns the caller did not know about when the queue was built —
    /// the newly-visible strip a player walking across a chunk boundary reveals.
    ///
    /// They join the **back** of the pop order, so nothing already queued is
    /// displaced by arrival alone; under a
    /// [`Priority`](QueueOrder::Priority) order the subsequent re-key is what
    /// decides where they actually land, which is the point — a column the player
    /// just walked towards should out-rank one behind them regardless of which was
    /// enqueued first.
    ///
    /// Under [`AsGiven`](QueueOrder::AsGiven) there is no re-key, so "back of the
    /// pop order" is the whole behaviour and the gates that assert a fixed sequence
    /// see appended columns strictly after the originals.
    pub(crate) fn extend(&mut self, coords: Vec<(i32, i32)>) {
        if coords.is_empty() {
            return;
        }
        let mut index = self
            .pending
            .iter()
            .map(|&(_, i)| i)
            .max()
            .map_or(0, |max| max.saturating_add(1));
        let mut appended: Vec<((i32, i32), u32)> = Vec::with_capacity(coords.len());
        for coord in coords {
            appended.push((coord, index));
            index = index.saturating_add(1);
        }
        // `pending` is stored worst-first and `pop` takes the last element, so
        // "behind everything already queued" is the *front* of the vector, and the
        // appended run itself has to be reversed to keep its own given order.
        appended.reverse();
        self.pending.splice(0..0, appended);
        self.sort();
    }

    /// Drops every still-pending column in `dropped`, returning how many went.
    ///
    /// The order of what survives is untouched — this is a filter, not a re-key —
    /// so a cancellation cannot reshuffle the wire.
    pub(crate) fn cancel(&mut self, dropped: &std::collections::HashSet<(i32, i32)>) -> usize {
        if dropped.is_empty() {
            return 0;
        }
        let before = self.pending.len();
        self.pending.retain(|&(coord, _)| !dropped.contains(&coord));
        before - self.pending.len()
    }

    /// The next column to generate, or `None` when the queue is empty.
    pub(crate) fn pop(&mut self) -> Option<(i32, i32)> {
        self.pending.pop().map(|(coord, _)| coord)
    }

    /// How many columns have not been handed out yet.
    #[must_use]
    pub(crate) fn len(&self) -> usize {
        self.pending.len()
    }

    fn sort(&mut self) {
        let QueueOrder::Priority { centre, facing, .. } = self.order else {
            return;
        };
        // Worst first, so `pop` takes the best. `sort_unstable_by_key` on the
        // reversed key would need a negation that `u32` cannot express, so the
        // comparison is reversed instead.
        self.pending.sort_unstable_by(|a, b| {
            priority_key(centre, facing, b.0, b.1).cmp(&priority_key(centre, facing, a.0, a.1))
        });
    }
}

/// The quantised yaw sector a rotation falls in — see [`YAW_SECTORS`].
#[must_use]
fn yaw_sector(yaw_degrees: f32) -> i32 {
    if !yaw_degrees.is_finite() {
        return 0;
    }
    let wrapped = yaw_degrees.rem_euclid(360.0);
    (wrapped / (360.0 / YAW_SECTORS)).floor() as i32
}

/// How many columns the join burst keeps in flight once primed, derived from the
/// machine rather than from the view.
///
/// `available_parallelism`, floored at 2 — **one in-flight column per hardware
/// thread**.
///
/// * **the floor of 2** is what makes a window a window. At 1 this degenerates to
///   the fully serial shape, and the ring-overlap detector in
///   `join_scheduler_gates.rs` would be vacuous on a machine reporting
///   `available_parallelism() == 1`.
/// * **there is no ceiling, and that is the point.** `5104adf`'s in-flight count
///   was `(2r + 1)²` — it grew with the *view radius*, which is why an 8-core
///   machine ran 289 concurrent generator calls. This one grows with cores, so
///   the pathological case is unreachable by construction at any view radius.
///
/// # It was `2 × available_parallelism` until §12.132, and the factor of 2 cost
/// a third of the burst's throughput
///
/// The doubling was for encode overlap — the caller awaits one column, then writes
/// it to the socket, and a window of exactly `parallelism` leaves the pool idle for
/// that write. Reasonable, unmeasured, and **wrong by 1.5×**. A window sweep over
/// the real 289-column burst on the 10-core reference machine, instructions retired
/// held flat to 1.4% across every arm so the comparison is of scheduling and not of
/// work:
///
/// | window | wall | speedup over serial | cycles/column vs serial | IPC |
/// |---|---|---|---|---|
/// | 4 | 4.78 s | 2.00× | 1.01× | 5.27 |
/// | 6 | 4.19 s | 2.28× | 1.05× | 5.09 |
/// | **8** | **3.67 s** | **2.60×** | 1.26× | 4.25 |
/// | 10 (`P`) | 4.28 s | 2.23× | 1.96× | 2.73 |
/// | 12 | 4.87 s | 1.96× | 2.54× | 2.11 |
/// | 20 (`2P`, the old value) | 6.43 s | **1.49×** | **4.39×** | **1.23** |
///
/// So the curve is a U with its floor at 8–10 and a **steep** right-hand side, and
/// `2 × P` sat well past it. The mechanism is **not a lock**, and the shape of the
/// curve is what says so: a lock contended by 20 workers is contended by 4, so cycle
/// inflation would be a constant tax rather than the threshold it measures —
/// 1.01–1.15× at a window of 4 over five runs, against 2.6–4.4× at 20, growing
/// super-linearly in between. It is **cache capacity**: each in-flight column carries
/// a multi-megabyte working set, and past roughly the core count they stop fitting
/// together. `join_parallel_efficiency.rs`'s
/// `a_small_window_shows_no_lock_on_the_shared_generator` is that assertion.
///
/// The coefficient is 1 rather than a fitted 0.8 because it is a
/// *machine-derived proxy* for a cache bound this code cannot query, it lands
/// inside the measured floor, and the encode overlap the 2 was buying is worth far
/// less than the capacity it spent. `join_parallel_efficiency.rs`'s
/// `the_production_window_sits_at_the_measured_optimum` is the gate that fails if a
/// different machine's floor moves; re-run the sweep rather than adjusting this by
/// feel.
///
/// # The store interaction, stated rather than left to be discovered
///
/// Each in-flight `column()` call pins its own pre-ore neighbourhood in the staged
/// store — `COLUMN_CLOSURE_RADIUS + REFS_RADIUS = 10`, so 21×21 = 441 entries per
/// column under the 2,048-entry retention ceiling derived from
/// the 289-column burst's own 37×37 closure. At `P` in flight nothing is evicted
/// for the duration of the burst, which is what licenses reading the stage counters
/// as one-per-chunk; halving the window can only make that more true.
#[must_use]
pub fn generation_window() -> usize {
    generation_window_for(
        std::thread::available_parallelism()
            .map(std::num::NonZero::get)
            .unwrap_or(4),
    )
}

/// [`generation_window`]'s arithmetic, split out so it is testable without
/// depending on the host's core count.
#[must_use]
pub fn generation_window_for(parallelism: usize) -> usize {
    parallelism.max(2)
}

/// A primed sliding window over a fixed coordinate order.
///
/// [`next`](Self::next) tops the in-flight set up to the window, then awaits and
/// emits the **oldest** one — so emission order is exactly the order the
/// coordinates were handed in, independent of which column finished first. See the
/// module doc for why the first top-up is to 1 rather than to `window`.
///
/// # wasm32
///
/// There is no blocking pool, so the window is forced to 1 and columns are
/// generated inline — the unchanged behaviour of a target that never had a second
/// thread. Same as `crate::chunk::generate_columns_offloaded`'s `cfg`.
pub struct ColumnPipeline<S> {
    source: Arc<S>,
    /// Protocol encode, moved **into** the worker that generates the column —
    /// [`ChunkEncoder`] carries the measurement. `None` restores the pre-existing
    /// shape, where the caller encodes on its own task; see [`ColumnPayload`].
    encoder: Option<Arc<dyn ChunkEncoder>>,
    /// What to generate next, and in what order — see [`ColumnQueue`]. A queue
    /// rather than the `Vec` + cursor this held before, because the *pending*
    /// half of a join must be re-orderable when the player moves or turns while
    /// it is still draining.
    queue: ColumnQueue,
    /// The centre and inclusive near radius that receive complete generation.
    /// `None` keeps the historic all-full behaviour for every existing caller.
    generation_band: Option<((i32, i32), i32)>,
    /// How many columns this pipeline was built with, so
    /// [`remaining`](Self::remaining) can be answered without the queue and the
    /// in-flight set having to agree about who owns a column mid-flight.
    total: usize,
    emitted: usize,
    window: usize,
    /// Set once the head column has been emitted. Until then the window is 1.
    primed: bool,
    /// Each entry is the coordinate paired with the worker generating it, so
    /// **emission order is the order columns were handed to the pool**, not the
    /// order they finish in. Pairing them here (rather than indexing a `coords`
    /// vector) is what lets the spawn order itself be dynamic.
    #[cfg(not(target_arch = "wasm32"))]
    inflight: VecDeque<((i32, i32), tokio::task::JoinHandle<Result<ColumnPayload, ChunkEncodeError>>)>,
    #[cfg(target_arch = "wasm32")]
    inflight: VecDeque<((i32, i32), ColumnPayload)>,
}

impl<S> std::fmt::Debug for ColumnPipeline<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ColumnPipeline")
            .field("columns", &self.total)
            .field("window", &self.window)
            .field("pending", &self.queue.len())
            .field("emitted", &self.emitted)
            .field("generation_band", &self.generation_band)
            .field("inflight", &self.inflight.len())
            .finish_non_exhaustive()
    }
}

impl<S: ChunkSource + 'static> ColumnPipeline<S> {
    /// A pipeline over `coords` with the machine-derived [`generation_window`].
    #[must_use]
    pub fn new(source: Arc<S>, coords: Vec<(i32, i32)>) -> Self {
        Self::with_window(source, coords, generation_window())
    }

    /// A pipeline with an explicit window, for gates that must not vary with the
    /// host's core count.
    #[must_use]
    pub fn with_window(source: Arc<S>, coords: Vec<(i32, i32)>, window: usize) -> Self {
        let total = coords.len();
        Self::over(source, ColumnQueue::as_given(coords), total, window)
    }

    /// A pipeline whose *pending* columns are ordered by distance from `centre`
    /// with an in-frustum bonus, and can be re-ordered later via
    /// [`reprioritise`](Self::reprioritise).
    ///
    /// With `facing: None` this emits exactly the order `coords` was given in
    /// (see [`priority_key`]), so handing it the fixed outward ring walk makes it
    /// a drop-in for [`with_window`](Self::with_window) until a rotation is
    /// known.
    #[must_use]
    pub(crate) fn prioritised(
        source: Arc<S>,
        coords: Vec<(i32, i32)>,
        window: usize,
        centre: (i32, i32),
        facing: Option<f32>,
    ) -> Self {
        let total = coords.len();
        Self::over(
            source,
            ColumnQueue::prioritised(coords, centre, facing),
            total,
            window,
        )
    }

    fn over(source: Arc<S>, queue: ColumnQueue, total: usize, window: usize) -> Self {
        Self {
            source,
            encoder: None,
            queue,
            generation_band: None,
            total,
            emitted: 0,
            window: window.max(1),
            primed: false,
            inflight: VecDeque::new(),
        }
    }

    /// Moves protocol encode into this pipeline's workers, so
    /// [`next`](Self::next) yields wire bytes rather than terrain and the
    /// connection task only writes frames. See [`ChunkEncoder`].
    ///
    /// A builder rather than a constructor parameter because the encoder is
    /// optional at every call site — `crate::server` passes
    /// `proto.chunk_encoder()` straight through, and a protocol that has not
    /// implemented one hands back `None`, restoring the pre-existing shape with
    /// no branch at the call site.
    #[must_use]
    pub fn encoding_with(mut self, encoder: Option<Arc<dyn ChunkEncoder>>) -> Self {
        self.encoder = encoder;
        self
    }

    /// Requests full generation through the inclusive Chebyshev `radius` around
    /// `centre`, and shaped generation for the rest of this pipeline's view.
    ///
    /// This is a streaming concern, not a terrain-source policy: gameplay
    /// reads continue to call [`ChunkSource::column`] and therefore always ask
    /// for a full column. A negative radius is useful to tests as an all-shaped
    /// control and is deliberately not clamped into a misleading one-column
    /// full band.
    #[must_use]
    pub(crate) fn with_generation_band(mut self, centre: (i32, i32), radius: i32) -> Self {
        self.generation_band = Some((centre, radius));
        self
    }

    fn generation_stage_for(&self, coord: (i32, i32)) -> ChunkGenerationStage {
        match self.generation_band {
            Some((centre, radius)) if ring_distance(centre, coord) > radius => {
                ChunkGenerationStage::Shaped
            }
            _ => ChunkGenerationStage::Full,
        }
    }

    /// Re-keys the columns not yet handed to the pool for a player who has moved
    /// or turned, returning whether the order actually changed.
    ///
    /// The in-flight set is deliberately **not** re-ordered: those columns are
    /// already being generated, there are at most [`generation_window`] of them,
    /// and they were the highest-priority columns at the moment they were
    /// spawned. So the effective granularity of a re-prioritisation is one
    /// window, not one column — which is also what keeps the emitted order a
    /// deterministic function of the queue rather than of who finished first.
    pub(crate) fn reprioritise(&mut self, centre: (i32, i32), facing: Option<f32>) -> bool {
        if let Some((band_centre, _)) = &mut self.generation_band {
            *band_centre = centre;
        }
        self.queue.reprioritise(centre, facing)
    }

    /// Adds columns this pipeline was not built with, so it keeps streaming past
    /// the view it started as.
    ///
    /// # Why a join pipeline is the right home for a *move*
    ///
    /// A newly visible strip contains `2r + 1` columns (`33` at
    /// `view_radius = 16`). Enqueuing it here lets the connection task keep
    /// reading and writing while the blocking pool generates the strip; one
    /// await covers only the first strip segment.
    ///
    /// Enqueueing into the live pipeline gives the strip the same primed window
    /// as the initial join, re-keys it through
    /// [`reprioritise`](Self::reprioritise) as the player keeps moving, and there is
    /// no second ordering rule to drift.
    ///
    /// `total` grows, so [`remaining`](Self::remaining) becomes non-zero again and
    /// the `select!` branch gated on it re-enables. `primed` is deliberately left
    /// alone: the one-column priming exists for time-to-first-chunk at join, and a
    /// pipeline that has already emitted anything should top up to the full window
    /// immediately.
    pub(crate) fn enqueue(&mut self, coords: Vec<(i32, i32)>) {
        if coords.is_empty() {
            return;
        }
        self.total += coords.len();
        self.queue.extend(coords);
    }

    /// Withdraws still-pending columns the client has been told to forget,
    /// returning how many were withdrawn.
    ///
    /// # Why this is needed, and why it is not new
    ///
    /// `ViewTracker` records a column as `loaded` the moment it decides to send it,
    /// so its `loaded` set means *owed* rather than *delivered* — which is what lets
    /// the join seed the whole square up front. A player who steps across a boundary
    /// and straight back therefore forgets a column that is still sitting in this
    /// queue, and without this it would be sent afterwards: the client loads a column
    /// outside its own view and never forgets it again, and the next step re-adds the
    /// same coordinate so it goes out twice. Vanilla's `PlayerChunkSender` drops
    /// pending sends for exactly this reason.
    ///
    /// The bug predates the steady-state path — the join stream has always been able
    /// to outlive a forget — and fixing it here fixes both.
    ///
    /// **In-flight columns are not cancelled.** They have already been spawned, there
    /// are at most [`window`](Self::window) of them, and reaching into the pool to
    /// abandon a `JoinHandle` would lose the column for a player who walks back into
    /// it. So the guarantee is "a forgotten column is not *newly started*", not "no
    /// forgotten column is ever sent".
    pub(crate) fn cancel(&mut self, dropped: &std::collections::HashSet<(i32, i32)>) -> usize {
        let removed = self.queue.cancel(dropped);
        // `remaining()` is `total - emitted`, and the `select!` branch is gated on
        // it: leaving `total` alone would keep the branch enabled with nothing to
        // hand back, and `next` would spin returning `None`.
        self.total -= removed;
        removed
    }

    /// The window this pipeline was built with.
    #[must_use]
    pub fn window(&self) -> usize {
        self.window
    }

    /// How many columns have yet to be emitted.
    #[must_use]
    pub fn remaining(&self) -> usize {
        self.total - self.emitted
    }

    /// The next column in queue order, or `None` once the view is drained.
    ///
    /// Ordering is load-bearing for the wire and is *not* a property of the pool:
    /// the front of `inflight` carries its own coordinate, so a column that
    /// finishes early simply sits in the queue behind the one that was spawned
    /// before it.
    ///
    /// # Cancel safety
    ///
    /// **This is now a `select!` branch** (`crate::server`'s `serve_play` races it
    /// against the socket read), so being dropped mid-`await` has to be free. It
    /// is: the front entry is awaited *by reference* and only popped once its
    /// column is in hand, so a cancelled `next` leaves the pipeline exactly as it
    /// found it and the next call re-awaits the same worker. Popping first — as
    /// this did while it was only ever driven to completion — would have dropped
    /// the `JoinHandle` on cancellation and silently lost that column from the
    /// wire.
    #[cfg(not(target_arch = "wasm32"))]
    pub async fn next(&mut self) -> Result<Option<((i32, i32), ColumnPayload)>, ChunkEncodeError> {
        if self.remaining() == 0 {
            return Ok(None);
        }
        // The first top-up is to 1, not to `window`: this is the
        // time-to-first-chunk fix. See the module doc.
        let target = if self.primed { self.window } else { 1 };
        while self.inflight.len() < target {
            let Some((cx, cz)) = self.queue.pop() else {
                break;
            };
            let source = Arc::clone(&self.source);
            let stage = self.generation_stage_for((cx, cz));
            // **Protocol encode happens here, on the worker, not on the caller's
            // task** — that is the whole point of `encoder`. The column is dropped
            // inside the closure, so the connection task never even sees the
            // terrain: it receives ~40 KiB of finished frame instead of a
            // multi-hundred-KiB column plus 62 M instructions of work to do to it.
            let encoder = self.encoder.clone();
            self.inflight.push_back((
                (cx, cz),
                tokio::task::spawn_blocking(move || {
                    let column = source.column_at(cx, cz, stage);
                    match encoder {
                        Some(encoder) => encoder
                            .try_encode_chunk(cx, cz, &column)
                            .map(ColumnPayload::Encoded),
                        None => Ok(ColumnPayload::Column(column)),
                    }
                }),
            ));
        }
        let (pos, handle) = self
            .inflight
            .front_mut()
            .expect("the top-up above spawns at least one column while any remain");
        let pos = *pos;
        let payload = handle.await.expect("worldgen join burst panicked")?;
        self.inflight.pop_front();
        self.emitted += 1;
        self.primed = true;
        Ok(Some((pos, payload)))
    }

    /// wasm32: no blocking pool, so this is the serial path. See the struct doc.
    #[cfg(target_arch = "wasm32")]
    pub async fn next(&mut self) -> Result<Option<((i32, i32), ColumnPayload)>, ChunkEncodeError> {
        if self.remaining() == 0 {
            return Ok(None);
        }
        let Some((cx, cz)) = self.queue.pop() else {
            return Ok(None);
        };
        let column = self.source.column_at(cx, cz, self.generation_stage_for((cx, cz)));
        self.emitted += 1;
        self.primed = true;
        let _ = &self.inflight;
        // There is no worker to move the encode to, so the arm is the same one a
        // protocol without a `ChunkEncoder` takes: the caller encodes it. Using
        // `self.encoder` here would be a lie about where the work happened.
        Ok(Some(((cx, cz), ColumnPayload::Column(column))))
    }
}

/// The part of a join view that has **not** been sent by the time the play loop
/// starts, plus how to finish producing it.
///
/// This is the seam that stops the join burst standing in front of the play loop:
/// `crate::server`'s `serve_connection_inner` streams the innermost rings inline
/// (so the player has ground under their feet before they can act), builds one of
/// these for the rest, and `serve_play` drains it from a `tokio::select!` branch
/// alongside the socket read. Vanilla's shape — `PlayerChunkSender` feeding a
/// player who is already in the level — rather than "generate the whole view,
/// then let the player exist".
///
/// The two variants are the two [`SourceRef`] arms, and they exist for the reason
/// the module doc already gives: a borrowed source is not `'static`, so it cannot
/// be spawned and has nothing for a window to overlap. What they hold identical
/// is the **order**, not the concurrency.
#[derive(Debug)]
pub(crate) enum JoinChunkStream<S> {
    /// Nothing deferred (a view small enough to have gone out inline), or a
    /// stream that has since drained.
    Drained,
    /// [`SourceRef::Shared`]: the same primed window the inline burst used,
    /// handed on with its remaining columns and re-orderable while it drains.
    Windowed(ColumnPipeline<S>),
    /// [`SourceRef::Borrowed`]: whole rings, generated one ring at a time by the
    /// caller's own blocking source and emitted one column at a time.
    Ringed {
        rings: VecDeque<Vec<(i32, i32)>>,
        ready: VecDeque<((i32, i32), ChunkColumn)>,
        remaining: usize,
    },
}

impl<S: ChunkSource + 'static> JoinChunkStream<S> {
    /// The deferred half of a [`SourceRef::Shared`] join: whatever the inline
    /// burst left in `pipeline`.
    /// **A pipeline with nothing left is still `Windowed`, not `Drained`.** It has
    /// to be: `Drained` carries no source, no window and no encoder, so a stream
    /// that collapsed into it could never be re-fed, and
    /// [`enqueue`](Self::enqueue) is what makes the steady-state view share this
    /// machinery. `is_done` already reports emptiness from
    /// [`remaining`](Self::remaining), so the `select!` branch is disabled either
    /// way and nothing spins.
    #[must_use]
    pub(crate) fn windowed(pipeline: ColumnPipeline<S>) -> Self {
        Self::Windowed(pipeline)
    }

    /// Whether [`enqueue`](Self::enqueue) would take columns.
    ///
    /// **Asked before handing anything over, never discovered afterwards.** The
    /// caller's fallback needs those coordinates, and a refusal that had already
    /// consumed them would leave nothing to fall back with — a hole in the world
    /// with a clean test suite, this crate's dominant defect shape.
    ///
    /// `false` on both other arms:
    ///
    /// * [`Ringed`](Self::Ringed) is the [`SourceRef::Borrowed`] arm — no `'static`
    ///   source, so nothing to spawn and no window to overlap. Protocol tests only.
    /// * [`Drained`](Self::Drained) is a stream nothing is polling. Only the
    ///   `Ringed` arm ever resolves to it; a `Windowed` one stays `Windowed`
    ///   precisely so that it can be re-fed (see [`windowed`](Self::windowed)).
    #[must_use]
    pub(crate) fn accepts_enqueue(&self) -> bool {
        matches!(self, Self::Windowed(_))
    }

    /// Hands `coords` to the streaming pipeline. A no-op on the two arms
    /// [`accepts_enqueue`](Self::accepts_enqueue) reports `false` for — ask first.
    pub(crate) fn enqueue(&mut self, coords: Vec<(i32, i32)>) {
        if let Self::Windowed(pipeline) = self {
            pipeline.enqueue(coords);
        }
    }

    /// Withdraws still-pending columns the client has been told to forget — see
    /// [`ColumnPipeline::cancel`], which owns the reasoning and the in-flight caveat.
    ///
    /// A no-op on the [`Ringed`](Self::Ringed) arm, whose unit of work is a whole
    /// ring: withdrawing one column would split a batch that arm exists to keep
    /// intact, and it serves the `&S`-shaped tests where nothing moves.
    pub(crate) fn cancel(&mut self, dropped: &std::collections::HashSet<(i32, i32)>) -> usize {
        match self {
            Self::Windowed(pipeline) => pipeline.cancel(dropped),
            Self::Drained | Self::Ringed { .. } => 0,
        }
    }

    /// The deferred half of a [`SourceRef::Borrowed`] join: the rings the inline
    /// burst did not reach.
    #[must_use]
    pub(crate) fn ringed(rings: Vec<Vec<(i32, i32)>>) -> Self {
        let remaining: usize = rings.iter().map(Vec::len).sum();
        if remaining == 0 {
            return Self::Drained;
        }
        Self::Ringed {
            rings: rings.into(),
            ready: VecDeque::new(),
            remaining,
        }
    }

    /// How many columns this stream still owes the client.
    #[must_use]
    pub(crate) fn remaining(&self) -> usize {
        match self {
            Self::Drained => 0,
            Self::Windowed(pipeline) => pipeline.remaining(),
            Self::Ringed { remaining, .. } => *remaining,
        }
    }

    /// Whether the client has everything this stream was built to send.
    ///
    /// The `select!` branch driving [`next`](Self::next) is disabled on this, so
    /// it must go `true` exactly when the last column has been *emitted* — a
    /// stream that reported done early would silently truncate the view, and one
    /// that reported done late would spin the play loop on a branch that
    /// immediately returns `None`.
    #[must_use]
    pub(crate) fn is_done(&self) -> bool {
        self.remaining() == 0
    }

    /// Re-keys the pending columns for a player who has moved to chunk `centre`
    /// or turned to `facing` (degrees of yaw), returning whether anything moved.
    ///
    /// A no-op on the [`Ringed`](Self::Ringed) arm, which generates whole rings
    /// by construction: its unit of work is a ring, so there is no per-column
    /// order to change without splitting the batches that arm exists to keep.
    /// That arm serves the `&S`-shaped tests, and with a stationary player both
    /// arms emit the identical sequence either way (see [`priority_key`]).
    pub(crate) fn reprioritise(&mut self, centre: (i32, i32), facing: Option<f32>) -> bool {
        match self {
            Self::Windowed(pipeline) => pipeline.reprioritise(centre, facing),
            Self::Drained | Self::Ringed { .. } => false,
        }
    }

    /// The next column, or `None` once drained.
    ///
    /// `source` is only read on the [`Ringed`](Self::Ringed) arm — the
    /// [`Windowed`](Self::Windowed) arm owns its own `Arc`. Passing it per call
    /// rather than storing it is what keeps this type free of the borrowed
    /// source's lifetime, so it can live in `serve_play`'s frame.
    ///
    /// # Cancel safety
    ///
    /// Both arms are safe to drop mid-`await`, which they must be to sit in a
    /// `select!`: [`ColumnPipeline::next`] documents its own, and the `Ringed`
    /// arm's `generate` is `generate_columns_parallel` — synchronous work inside
    /// an `async fn`, so it has no suspension point to be cancelled at, and the
    /// ring is only popped once its columns are buffered.
    pub(crate) async fn next(
        &mut self,
        source: SourceRef<'_, S>,
    ) -> Result<Option<((i32, i32), ColumnPayload)>, ChunkEncodeError> {
        match self {
            Self::Drained => Ok(None),
            // **Not collapsed to `Drained` on exhaustion** — see
            // [`windowed`](Self::windowed). The pipeline has to survive so a later
            // [`enqueue`](Self::enqueue) can refill it.
            Self::Windowed(pipeline) => pipeline.next().await,
            Self::Ringed {
                rings,
                ready,
                remaining,
            } => {
                while ready.is_empty() {
                    // An exhausted ring list with a non-zero `remaining` cannot
                    // happen (the count is the rings' own total), but resolving it
                    // to `Drained` rather than to a bare `None` matters: the
                    // `select!` branch is disabled on `is_done`, so a stream that
                    // reported work it could not produce would spin the play loop.
                    let Some(ring) = rings.front().cloned() else {
                        *remaining = 0;
                        *self = Self::Drained;
                        return Ok(None);
                    };
                    let columns = source.generate(ring.clone()).await;
                    for (coord, column) in ring.into_iter().zip(columns) {
                        ready.push_back((coord, column));
                    }
                    rings.pop_front();
                }
                let Some((coord, column)) = ready.pop_front() else {
                    return Ok(None);
                };
                *remaining = remaining.saturating_sub(1);
                if *remaining == 0 {
                    *self = Self::Drained;
                }
                // This arm's source is not `'static` (see the type doc), so there
                // is no worker to encode on and never was: the caller encodes it,
                // exactly as it did before `ColumnPayload` existed.
                Ok(Some((coord, ColumnPayload::Column(column))))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;
    use std::time::Duration;

    /// A source whose column cost is a function of its position in a known list,
    /// so completion order is a *chosen* permutation rather than whatever the pool
    /// happened to do. `delays[i]` is applied to `coords[i]`.
    struct SkewedSource {
        coords: Vec<(i32, i32)>,
        delays: Vec<Duration>,
        completed: Arc<AtomicUsize>,
    }

    /// A deliberately non-progressive terrain body with a progressive request
    /// log. The stage comes from the scheduler, so this detects an accidental
    /// return to `ChunkSource::column` even though the placeholder terrain is
    /// identical at both stages.
    struct StageRecordingSource {
        requests: Mutex<Vec<((i32, i32), ChunkGenerationStage)>>,
    }

    impl ChunkSource for StageRecordingSource {
        fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
            ChunkColumn::new(0, 16)
        }

        fn column_at(&self, cx: i32, cz: i32, stage: ChunkGenerationStage) -> ChunkColumn {
            self.requests
                .lock()
                .expect("stage request log lock poisoned")
                .push(((cx, cz), stage));
            ChunkColumn::new(0, 16)
        }

        fn block_state(&self, _x: i32, _y: i32, _z: i32) -> String {
            crate::chunk::AIR.to_string()
        }

        fn biome_state_at(&self, _x: i32, _y: i32, _z: i32) -> String {
            crate::chunk::DEFAULT_BIOME.to_string()
        }

        fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {}
    }

    impl ChunkSource for SkewedSource {
        fn column(&self, cx: i32, cz: i32) -> ChunkColumn {
            let idx = self
                .coords
                .iter()
                .position(|&c| c == (cx, cz))
                .expect("the gate only asks for coordinates it declared");
            std::thread::sleep(self.delays[idx]);
            self.completed.fetch_add(1, Ordering::SeqCst);
            // A 16-block-tall column: this gate measures ordering and in-flight
            // counts, and a full -64..320 column would allocate 196 KiB per call
            // for no assertion.
            ChunkColumn::new(0, 16)
        }

        fn block_state(&self, _x: i32, _y: i32, _z: i32) -> String {
            "minecraft:air".to_string()
        }

        fn biome_state_at(&self, _x: i32, _y: i32, _z: i32) -> String {
            crate::chunk::DEFAULT_BIOME.to_string()
        }

        fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {}
    }

    /// Twelve columns whose costs *decrease* with index, so the pool finishes them
    /// in exactly reverse order. That makes "completion order" a concrete,
    /// deterministic sequence the control below can produce.
    fn inverted_cost_view(n: usize) -> (Vec<(i32, i32)>, Vec<Duration>) {
        let coords: Vec<(i32, i32)> = (0..n as i32).map(|i| (i, 0)).collect();
        let delays = (0..n)
            .map(|i| Duration::from_millis(((n - i) * 4) as u64))
            .collect();
        (coords, delays)
    }

    /// The fixed outward ring walk, restated here so the queue gates can be read
    /// without `crate::server` — and identical to `join_view_rings` flattened,
    /// which is what makes "no facing emits the ring order" a real claim.
    fn ring_walk(radius: i32) -> Vec<(i32, i32)> {
        let mut coords = Vec::new();
        for r in 0..=radius {
            for dz in -r..=r {
                for dx in -r..=r {
                    if dx.abs().max(dz.abs()) == r {
                        coords.push((dx, dz));
                    }
                }
            }
        }
        coords
    }

    fn drain(mut queue: ColumnQueue) -> Vec<(i32, i32)> {
        let mut out = Vec::new();
        while let Some(coord) = queue.pop() {
            out.push(coord);
        }
        out
    }

    /// With no rotation known, a prioritised queue is byte-identical to the ring
    /// walk it was handed. This is what keeps the join's wire order unchanged for
    /// every client that has not sent a movement packet yet, and it is why the
    /// frustum bonus could be added without changing what a fresh join looks like.
    #[test]
    fn an_unknown_facing_emits_the_ring_order_unchanged() {
        let coords = ring_walk(4);
        let queue = ColumnQueue::prioritised(coords.clone(), (0, 0), None);
        assert_eq!(drain(queue), coords);
    }

    /// A column enqueued after the queue was built joins the **back** of the pop
    /// order and is then re-keyed with everything else — so a strip the player just
    /// walked towards out-ranks one behind them regardless of arrival order.
    ///
    /// Two arms because the two `QueueOrder`s answer differently and only one of
    /// them is production: under `AsGiven` there is no re-key, so "back of the pop
    /// order" is the whole behaviour and the fixed-sequence gates above stay valid.
    #[test]
    fn an_enqueued_column_is_ordered_by_priority_not_by_arrival() {
        // `AsGiven`: strictly appended.
        let mut plain = ColumnQueue::as_given(vec![(0, 0), (1, 0)]);
        plain.extend(vec![(2, 0), (3, 0)]);
        assert_eq!(drain(plain), vec![(0, 0), (1, 0), (2, 0), (3, 0)]);

        // `Priority`: the late arrival is nearer the centre than what was already
        // queued, so it must come out first. Under an append-only queue it would be
        // last, which is the ordering this discriminates against.
        let mut prioritised = ColumnQueue::prioritised(vec![(5, 0), (6, 0)], (0, 0), None);
        prioritised.extend(vec![(1, 0)]);
        assert_eq!(
            drain(prioritised),
            vec![(1, 0), (5, 0), (6, 0)],
            "a column enqueued late but close must be re-keyed ahead of far columns \
             already pending, or a player walking into new terrain waits on the strip \
             they walked away from"
        );
    }

    /// Cancelling withdraws exactly the named pending columns, leaves the order of
    /// the survivors alone, and — the part that would wedge the play loop — keeps
    /// `remaining()` consistent.
    ///
    /// `serve_play`'s `select!` branch is gated on `is_done()`, i.e. on
    /// `remaining()`. A cancel that removed entries from the queue without
    /// decrementing `total` would leave the branch enabled over an empty queue, and
    /// `next` would spin returning `None` forever — a busy loop, not a wrong answer,
    /// which is why the count is asserted here and not left to a wire gate.
    #[test]
    fn cancelling_withdraws_the_named_columns_and_keeps_remaining_consistent() {
        let coords = vec![(0, 0), (1, 0), (2, 0), (3, 0)];
        // Nothing is generated here — this is queue arithmetic — so the source only
        // has to exist.
        let source = Arc::new(SkewedSource {
            coords: coords.clone(),
            delays: vec![Duration::ZERO; coords.len()],
            completed: Arc::new(AtomicUsize::new(0)),
        });
        let mut pipeline = ColumnPipeline::with_window(source, coords, 2);
        assert_eq!(pipeline.remaining(), 4);

        let dropped: std::collections::HashSet<(i32, i32)> =
            [(1, 0), (2, 0)].into_iter().collect();
        assert_eq!(pipeline.cancel(&dropped), 2);
        assert_eq!(
            pipeline.remaining(),
            2,
            "remaining() gates serve_play's select! branch: a stale count spins the loop"
        );

        // Cancelling something that was never queued is free and moves nothing.
        let absent: std::collections::HashSet<(i32, i32)> = [(9, 9)].into_iter().collect();
        assert_eq!(pipeline.cancel(&absent), 0);
        assert_eq!(pipeline.remaining(), 2);
    }

    /// **Distance is the primary key**, so no amount of looking one way can
    /// starve the other. Asserted as the property rather than as a fixed
    /// sequence: every popped column is at least as far as the one before it, and
    /// the ring bands are therefore contiguous.
    #[test]
    fn distance_is_the_primary_key_so_a_spinning_player_cannot_starve_a_ring() {
        let radius = 5;
        // Yaw 0 is due +Z in Minecraft's convention, so this player is looking at
        // the columns with positive `dz`.
        let order = drain(ColumnQueue::prioritised(ring_walk(radius), (0, 0), Some(0.0)));
        assert_eq!(order.len(), ((2 * radius + 1) * (2 * radius + 1)) as usize);

        let mut previous = 0;
        for &coord in &order {
            let distance = ring_distance((0, 0), coord);
            assert!(
                distance >= previous,
                "{coord:?} at distance {distance} follows distance {previous}: an in-frustum \
                 bonus must never promote a far column over a near one, or a player who turns \
                 round finds a hole that was deprioritised for minutes"
            );
            previous = distance;
        }

        // The concrete form of the same claim, and the one that fails under a
        // pure frustum-first scheduler: the *worst* column in ring 3 (directly
        // behind the player) still precedes the *best* column in ring 4
        // (directly in front).
        let behind = order
            .iter()
            .position(|&c| c == (0, -3))
            .expect("the column directly behind the player at distance 3 is in the view");
        let ahead = order
            .iter()
            .position(|&c| c == (0, 4))
            .expect("the column directly in front of the player at distance 4 is in the view");
        assert!(
            behind < ahead,
            "a near column behind the player must beat a far column in front of them"
        );
    }

    /// …and *within* a ring the facing cone really does win, or the whole feature
    /// is inert. The control for the assertion above: if this failed, the
    /// distance-monotonicity gate would be satisfied by an ordering that ignores
    /// the player's rotation entirely.
    #[test]
    fn the_facing_cone_orders_within_a_ring() {
        let order = drain(ColumnQueue::prioritised(ring_walk(5), (0, 0), Some(0.0)));
        let ring: Vec<(i32, i32)> = order
            .into_iter()
            .filter(|&c| ring_distance((0, 0), c) == 5)
            .collect();
        let front = ring
            .iter()
            .position(|&c| c == (0, 5))
            .expect("directly in front is in ring 5");
        let back = ring
            .iter()
            .position(|&c| c == (0, -5))
            .expect("directly behind is in ring 5");
        assert!(
            front < back,
            "within one ring, the column the player is looking at must be generated before the \
             one behind them; got in-front at {front} and behind at {back}"
        );

        // The whole in-frustum half of the ring precedes the whole out-of-frustum
        // half — a single-column comparison could be satisfied by a tie-break
        // accident.
        let split = ring
            .iter()
            .position(|&c| !in_frustum((0, 0), 0.0, c))
            .expect("a 120° cone cannot contain a whole ring");
        assert!(
            ring[..split].iter().all(|&c| in_frustum((0, 0), 0.0, c)),
            "the in-frustum columns of a ring must form its prefix"
        );
    }

    /// Re-prioritisation is meant to be called on every movement packet, so the
    /// common case must not sort: it re-sorts when the player crosses a chunk
    /// boundary or turns into a new yaw sector, and does nothing otherwise.
    #[test]
    fn reprioritisation_only_fires_when_the_centre_or_the_sector_moves() {
        let mut queue = ColumnQueue::prioritised(ring_walk(3), (0, 0), Some(0.0));
        assert!(
            !queue.reprioritise((0, 0), Some(0.0)),
            "an identical centre and yaw must not re-sort"
        );
        assert!(
            !queue.reprioritise((0, 0), Some(10.0)),
            "a sub-sector nudge (10° of 22.5°) must not re-sort"
        );
        assert!(
            queue.reprioritise((0, 0), Some(90.0)),
            "a quarter turn is a new sector and must re-sort"
        );
        assert!(
            queue.reprioritise((1, 0), Some(90.0)),
            "crossing a chunk boundary must re-sort"
        );
        // And the new centre is what the order is keyed on afterwards.
        let order = drain(queue);
        let mut previous = 0;
        for &coord in &order {
            let distance = ring_distance((1, 0), coord);
            assert!(distance >= previous, "{coord:?} is out of order about (1, 0)");
            previous = distance;
        }

        // An `as_given` queue is never re-ordered, whatever it is told: the
        // pre-play-loop burst and this module's own ordering gates run on one.
        let mut fixed = ColumnQueue::as_given(ring_walk(2));
        assert!(!fixed.reprioritise((9, 9), Some(180.0)));
        assert_eq!(drain(fixed), ring_walk(2));
    }

    #[test]
    fn the_window_is_derived_from_cores_and_never_below_two() {
        assert_eq!(generation_window_for(0), 2, "a bogus 0 must still window");
        assert_eq!(generation_window_for(1), 2);
        // One in-flight column per hardware thread since §12.132 — `2 × P` measured
        // 1.49× against window 8's 2.60× on the 289-column burst.
        assert_eq!(generation_window_for(8), 8);
        assert_eq!(generation_window_for(64), 64);
        assert!(
            generation_window() >= 2,
            "the host-derived window must window on any machine"
        );
    }

    /// The window must not scale with the view, which is the whole defect
    /// `4307b59` reverted. 289 columns must not mean 289 in flight.
    #[test]
    fn the_window_does_not_scale_with_the_view() {
        let window = generation_window();
        assert!(
            window < 289,
            "a window of {window} would reproduce 5104adf's 289 concurrent generator calls \
             on this machine — the in-flight count must derive from cores, not the view"
        );
    }

    /// Emission order is the input order even when every column finishes in
    /// exactly the opposite order.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn the_pipeline_emits_input_order_under_inverted_costs() {
        let (coords, delays) = inverted_cost_view(12);
        let completed = Arc::new(AtomicUsize::new(0));
        let source = Arc::new(SkewedSource {
            coords: coords.clone(),
            delays,
            completed: Arc::clone(&completed),
        });

        let mut pipeline = ColumnPipeline::with_window(source, coords.clone(), 8);
        let mut emitted = Vec::new();
        while let Some((pos, _column)) = pipeline
            .next()
            .await
            .expect("a source without a fallible encoder cannot fail")
        {
            emitted.push(pos);
        }
        assert_eq!(
            emitted, coords,
            "the pipeline must emit in coordinate order regardless of completion order"
        );
        assert_eq!(completed.load(Ordering::SeqCst), coords.len());
    }

    /// **The control for the assertion above, and it must fail it.**
    ///
    /// A scheduler that emitted whichever column finished first would, on this
    /// cost profile, emit exactly the reverse of the input — the delays are
    /// monotonically decreasing, so completion order *is* reverse index order.
    /// Producing that sequence and requiring the equality above to reject it is
    /// what stops `the_pipeline_emits_input_order_under_inverted_costs` from being
    /// satisfied by a source that happens to finish in order anyway.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn control_completion_order_is_not_input_order() {
        let (coords, delays) = inverted_cost_view(12);
        let completed = Arc::new(AtomicUsize::new(0));
        let source = Arc::new(SkewedSource {
            coords: coords.clone(),
            delays,
            completed: Arc::clone(&completed),
        });

        // Spawn the whole view, then drain in completion order. For this cost
        // profile that is reverse index order, so it can be produced without a
        // `select!` over 12 futures.
        let handles: Vec<_> = coords
            .iter()
            .map(|&(cx, cz)| {
                let source = Arc::clone(&source);
                tokio::task::spawn_blocking(move || source.column(cx, cz))
            })
            .collect();
        let mut emitted = Vec::new();
        for (idx, handle) in handles.into_iter().enumerate().rev() {
            handle.await.expect("no worker may panic");
            emitted.push(coords[idx]);
        }

        assert_ne!(
            emitted, coords,
            "if completion order equals input order on this cost profile, the ordering \
             assertion beside this control is vacuous — the source is not actually skewed"
        );
        assert_eq!(
            emitted.first().copied(),
            coords.last().copied(),
            "the cheapest column is last in the view, so completion order starts there"
        );
    }

    /// As a counter, exactly **one** column has been generated at the
    /// moment the first one is emitted. This is what "primed" buys, and it is the
    /// property a plain sliding window would lose.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn exactly_one_column_is_generated_before_the_first_emit() {
        let (coords, delays) = inverted_cost_view(16);
        let completed = Arc::new(AtomicUsize::new(0));
        let source = Arc::new(SkewedSource {
            coords: coords.clone(),
            delays,
            completed: Arc::clone(&completed),
        });

        let mut pipeline = ColumnPipeline::with_window(source, coords.clone(), 8);
        let (first, _column) = pipeline
            .next()
            .await
            .expect("a source without a fallible encoder cannot fail")
            .expect("a non-empty view emits");
        let at_first = completed.load(Ordering::SeqCst);
        assert_eq!(first, coords[0]);
        assert_eq!(
            at_first, 1,
            "{at_first} columns had been generated when the first was emitted; #453 requires \
             the player's own column to reach the wire after one column of generation"
        );

        // …and the window really does open afterwards, or the line above would be
        // satisfied by a fully serial pipeline.
        let _ = pipeline.next().await.expect("encoding cannot fail");
        let _ = pipeline.next().await.expect("encoding cannot fail");
        assert!(
            completed.load(Ordering::SeqCst) > 3,
            "after priming, more columns must be in flight than have been emitted — \
             otherwise this is the serial shape and the barrier was not removed"
        );
    }

    /// With a [`ChunkEncoder`] attached the pipeline yields **bytes**, not
    /// terrain — and the column is dropped on the worker rather than travelling
    /// to the caller. The counter that says the encode really ran off the calling
    /// task (rather than merely later) is
    /// `tests/serve_play.rs`'s `every_join_column_is_encoded_on_its_generating_thread`,
    /// which has a live negative control; this only fixes the shape.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_attached_encoder_makes_the_pipeline_emit_bytes() {
        /// Encodes the coordinate pair and nothing else, so the assertion can
        /// name an exact payload rather than "something non-empty".
        struct CoordEncoder;
        impl ChunkEncoder for CoordEncoder {
            fn encode_chunk(&self, cx: i32, cz: i32, _column: &ChunkColumn) -> ServerDirective {
                ServerDirective::Send {
                    packet_id: 7,
                    payload: vec![cx as u8, cz as u8],
                }
            }
        }

        let coords = vec![(1, 0), (2, 0)];
        let source = Arc::new(SkewedSource {
            coords: coords.clone(),
            delays: vec![Duration::from_millis(0); 2],
            completed: Arc::new(AtomicUsize::new(0)),
        });
        let mut pipeline = ColumnPipeline::with_window(source, coords, 2)
            .encoding_with(Some(Arc::new(CoordEncoder)));

        let (pos, payload) = pipeline
            .next()
            .await
            .expect("the coordinate encoder cannot fail")
            .expect("a non-empty view emits");
        assert_eq!(pos, (1, 0));
        assert!(
            payload.column().is_none(),
            "an encoded payload must not also carry the column — the point is that the \
             connection task never receives the terrain"
        );
        match payload {
            ColumnPayload::Encoded(ServerDirective::Send { packet_id, payload }) => {
                assert_eq!((packet_id, payload), (7, vec![1, 0]));
            }
            other => panic!("an attached encoder must yield encoded bytes, got {other:?}"),
        }
    }

    /// A worker-side encoding error belongs to the next coordinate in wire order;
    /// a later ready worker must not pass it and create a hole in the stream.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_encoder_failure_stops_at_its_ordered_coordinate() {
        struct RejectSecond;

        impl ChunkEncoder for RejectSecond {
            fn encode_chunk(&self, cx: i32, cz: i32, _column: &ChunkColumn) -> ServerDirective {
                ServerDirective::Send {
                    packet_id: 8,
                    payload: vec![cx as u8, cz as u8],
                }
            }

            fn try_encode_chunk(
                &self,
                cx: i32,
                cz: i32,
                column: &ChunkColumn,
            ) -> Result<ServerDirective, ChunkEncodeError> {
                if (cx, cz) == (2, 0) {
                    Err(ChunkEncodeError::new("second coordinate rejected"))
                } else {
                    Ok(self.encode_chunk(cx, cz, column))
                }
            }
        }

        let coords = vec![(1, 0), (2, 0), (3, 0)];
        let source = Arc::new(SkewedSource {
            coords: coords.clone(),
            delays: vec![Duration::ZERO; coords.len()],
            completed: Arc::new(AtomicUsize::new(0)),
        });
        let mut pipeline = ColumnPipeline::with_window(source, coords, 2)
            .encoding_with(Some(Arc::new(RejectSecond)));

        let first = pipeline
            .next()
            .await
            .expect("the first coordinate must encode")
            .expect("the first coordinate must be emitted");
        assert_eq!(first.0, (1, 0));
        let error = pipeline
            .next()
            .await
            .expect_err("the second coordinate must stop the ordered stream");
        assert_eq!(
            error,
            ChunkEncodeError::new("second coordinate rejected"),
            "the error must be reported before any later coordinate can be emitted"
        );
    }

    /// A one-column view still works, and a zero-column view emits nothing rather
    /// than panicking on the `pop_front` above.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn degenerate_views_are_handled() {
        let completed = Arc::new(AtomicUsize::new(0));
        let empty: Vec<(i32, i32)> = Vec::new();
        let source = Arc::new(SkewedSource {
            coords: empty.clone(),
            delays: Vec::new(),
            completed: Arc::clone(&completed),
        });
        let mut pipeline = ColumnPipeline::with_window(source, empty, 8);
        assert!(pipeline
            .next()
            .await
            .expect("a source without an encoder cannot fail")
            .is_none());
        assert_eq!(pipeline.remaining(), 0);

        let one = vec![(3, 4)];
        let source = Arc::new(SkewedSource {
            coords: one.clone(),
            delays: vec![Duration::from_millis(0)],
            completed: Arc::clone(&completed),
        });
        let mut pipeline = ColumnPipeline::with_window(source, one, 8);
        assert_eq!(
            pipeline
                .next()
                .await
                .expect("a source without an encoder cannot fail")
                .map(|(pos, _)| pos),
            Some((3, 4)),
            "a single-column view emits it"
        );
        assert!(pipeline
            .next()
            .await
            .expect("a source without an encoder cannot fail")
            .is_none());
    }

    /// A stream wider than its complete-generation band must request the
    /// reduced prefix only for the far columns. The request log is the detector:
    /// the returned fixture columns are intentionally indistinguishable, so a
    /// test that looked at pixels or payloads here would let a full-generation
    /// regression pass.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn far_columns_request_shaped_generation_and_near_columns_remain_full() {
        let coords = vec![(0, 0), (1, 1), (2, 0), (-3, 3)];
        let source = Arc::new(StageRecordingSource {
            requests: Mutex::new(Vec::new()),
        });
        let mut pipeline = ColumnPipeline::with_window(Arc::clone(&source), coords.clone(), 1)
            .with_generation_band((0, 0), 1);
        while pipeline
            .next()
            .await
            .expect("stage-recording source cannot fail")
            .is_some()
        {}
        assert_eq!(
            *source.requests.lock().expect("stage request log lock poisoned"),
            vec![
                ((0, 0), ChunkGenerationStage::Full),
                ((1, 1), ChunkGenerationStage::Full),
                ((2, 0), ChunkGenerationStage::Shaped),
                ((-3, 3), ChunkGenerationStage::Shaped),
            ],
            "a far request recorded as Full means the expensive suffix is still wired into the stream"
        );

        // Detector control: a negative radius is an all-shaped view. If a
        // future refactor ignores `generation_band`, this control reports the
        // first full request rather than merely observing a green all-full run.
        let control = Arc::new(StageRecordingSource {
            requests: Mutex::new(Vec::new()),
        });
        let mut pipeline = ColumnPipeline::with_window(Arc::clone(&control), coords, 1)
            .with_generation_band((0, 0), -1);
        while pipeline
            .next()
            .await
            .expect("stage-recording source cannot fail")
            .is_some()
        {}
        assert!(
            control
                .requests
                .lock()
                .expect("stage request log lock poisoned")
                .iter()
                .all(|(_, stage)| *stage == ChunkGenerationStage::Shaped),
            "control: radius -1 must request no full columns; a Full request proves the stage detector is inert"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn moving_recentres_the_full_generation_band_for_new_columns() {
        let source = Arc::new(StageRecordingSource {
            requests: Mutex::new(Vec::new()),
        });
        let mut pipeline = ColumnPipeline::prioritised(
            Arc::clone(&source),
            Vec::new(),
            1,
            (0, 0),
            None,
        )
        .with_generation_band((0, 0), 1);

        assert!(pipeline.reprioritise((100, 100), None));
        pipeline.enqueue(vec![(0, 0), (101, 100)]);
        while pipeline
            .next()
            .await
            .expect("stage-recording source cannot fail")
            .is_some()
        {}

        assert_eq!(
            *source.requests.lock().expect("stage request log lock poisoned"),
            vec![
                ((101, 100), ChunkGenerationStage::Full),
                ((0, 0), ChunkGenerationStage::Shaped),
            ],
            "the near band must follow the current player chunk instead of the join chunk"
        );
    }

    /// [`ticket_level_for_ring`] must describe the same quantity a real
    /// [`crate::ticket::TicketStore`] computes, within the granting ticket's
    /// reach, or the two "priority" notions this module's own doc comment
    /// says are unified in arithmetic only would silently drift apart. The
    /// expected values are read from the real propagator, not hand-derived
    /// twice — this is a **parity** gate between two independent
    /// implementations of the same physical rule, not a restatement of
    /// either one. `base_level` is chosen with a generous reach (`MAX_LEVEL -
    /// base_level = 33`) so every tested ring is well inside it — see
    /// [`ticket_level_for_ring`]'s own doc for why a ring past the reach is
    /// not a valid input to compare.
    #[test]
    fn ticket_level_for_ring_matches_a_real_ticket_stores_propagation() {
        use crate::ticket::{TicketKind, TicketOwner, TicketStore};

        const BASE_LEVEL: i32 = 0;
        let mut store = TicketStore::new();
        store.set_ticket_at_level(TicketOwner::Forced(0), TicketKind::Forced, (0, 0), BASE_LEVEL);
        store.propagate();

        for ring in 0..5 {
            let expected = store.loading_level((ring, 0));
            assert_eq!(
                ticket_level_for_ring(BASE_LEVEL, ring),
                expected,
                "ring {ring}: this module's own priority arithmetic must match the real \
                 ticket propagator's level at the same Chebyshev distance"
            );
        }
    }
}
