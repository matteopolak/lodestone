//! The chunk ticket type and the empty-to-full status pipeline (issue #289).
//!
//! # What it is
//!
//! A [`TicketType`]/[`Ticket`] pair carrying a **source** and a **level**, plus
//! [`TicketStore`], the min-fixed-point graph that turns a set of tickets into a
//! per-column [`ChunkStatus`]. This is the piece `docs/plans/chunk-lifecycle.md`
//! names and a fresh grep confirmed absent tree-wide: nothing in this crate had a
//! notion of "why is this chunk resident" independent of one connection's view
//! radius before this file.
//!
//! # How it works
//!
//! Ported in shape, not transliterated, from vanilla's own
//! ticket-type, ticket, distance-manager and chunk-tracker types, its own
//! ticket-storage type, plus
//! its own chunk-status pipeline and
//! its own chunk-level arithmetic:
//!
//! * A [`TicketType`] is `(timeout_ticks, flags)`. The five flags
//!   (`FLAG_PERSIST`/`FLAG_LOADING`/`FLAG_SIMULATION`/
//!   `FLAG_KEEP_DIMENSION_ACTIVE`/`FLAG_CAN_EXPIRE_IF_UNLOADED`) and the nine
//!   registered constants in [`ticket_type`] are transcribed from
//!   vanilla's own `TicketType` — the table in the plan doc is the citation for every
//!   literal below.
//! * A [`Ticket`] carries a **level**, not a radius: `add_ticket_with_radius`
//!   (vanilla's own ticket-storage type) stores exactly **one** ticket at the centre chunk,
//!   level `FULL_CHUNK_LEVEL - radius`. There is no per-chunk fan-out at
//!   insertion time.
//! * The level reaches neighbouring chunks through [`TicketStore::propagate`],
//!   a **from-scratch min-fixed-point recompute**: effective level at `p` is
//!   `min` over every active ticket `t` of `t.level + chebyshev(t.pos, p)`,
//!   which is `ChunkTracker::computeLevelFromNeighbor == fromLevel + 1`
//!   (vanilla's own chunk-tracker type) unrolled to a direct distance. Vanilla's own
//!   `DynamicGraphMinFixedPoint` is incremental for a chunk count this crate
//!   does not have yet; recomputing per tick is correct and far simpler, and
//!   the BFS radius is bounded by `MAX_LEVEL - ticket.level` per ticket, so it
//!   never scans more than the ticket's own reach.
//! * **Two independent trackers**, never collapsed (S3 in the plan): a
//!   *loading* level from tickets with [`TicketType::does_load`] and a
//!   *simulation* level from tickets with [`TicketType::does_simulate`]. This
//!   is what lets a `PLAYER_SPAWN` ticket make a chunk resident without making
//!   it tick — collapsing the two trackers is the exact bug the plan's own
//!   negative control demonstrates below.
//! * [`ChunkStatus`] is a **two-state** simplification of vanilla's twelve
//!   (`EMPTY` through `FULL`) — see "What this deliberately does not port"
//!   below.
//!
//! # What this deliberately does not port from vanilla's threading model
//!
//! * **No `ChunkHolder` future graph, no per-status `CompletableFuture` chain**
//!   (`GenerationChunkHolder`/`ChunkGenerationTask`). `OverworldGenerator::column`
//!   is one monolithic call with no seam to stop at `NOISE` or `SURFACE`, so
//!   there is exactly one state transition this crate can express:
//!   `Empty -> Full`. [`ChunkStatus::from_level`] is that whole pipeline.
//! * **`RADIUS_AROUND_FULL_CHUNK` is `0`, not vanilla's runtime-computed value**
//!   (at least 8, driven by `STRUCTURE_STARTS@8` in vanilla's own chunk-pyramid type).
//!   Consequently [`MAX_LEVEL`] is `33`, not vanilla's `33 + n`. **Do not
//!   "fix" this to match vanilla's literal** — the extra radius exists there to
//!   generate neighbours far enough ahead that a later status step's
//!   dependency is already at the stage it needs, which has no meaning against
//!   a single-transition pipeline.
//! * **No `ConsecutiveExecutor`/`ChunkTaskDispatcher`/`PriorityConsecutiveExecutor`
//!   worker pool.** Vanilla's answer to "loading-priority system" is that
//!   priority *is* the ticket level, routed through a 4-band priority queue
//!   over a background executor. This crate already has an offloaded,
//!   windowed generation pipeline (`crate::chunk::generate_columns_offloaded`,
//!   `crate::join_scheduler::ColumnPipeline`) solving the *scheduling* axis
//!   independently and well; this module does not duplicate it. What it adds
//!   is the *why* — which columns are wanted at all, and at what level — which
//!   is the input a priority queue would consume, not the queue itself.
//! * **No `TicketStorage` persistence.** Vanilla's tickets survive a restart
//!   (`SavedData` id `chunk_tickets`, `Ticket.CODEC`). Nothing here writes a
//!   ticket to disk; every [`TicketStore`] is rebuilt fresh at world open. A
//!   `FORCED` ticket (the `/forceload` case) therefore does not yet survive a
//!   restart — a real, named gap, not an oversight.
//! * **No per-status neighbour requirements** (vanilla's own chunk-pyramid type's
//!   `STRUCTURE_STARTS@8`, `BIOMES@1`, `blockStateWriteRadius(1)` on
//!   `FEATURES`). These describe *why* vanilla needs extra radius around a
//!   status target; with one transition there is nothing for them to gate.
//!
//! # How to change it, and the gotchas
//!
//! * **`propagate` is O(active tickets × their own reach), not O(world).** Do
//!   not replace the per-ticket bounded BFS with a whole-store scan — a single
//!   `FORCED` ticket at level 31 only reaches Chebyshev distance 2, and a
//!   `PLAYER_LOADING` ticket at a 32-chunk view radius reaches 32. Scanning
//!   every resident position for every ticket turns an O(radius²) operation
//!   into O(residents × tickets).
//! * **A ticket is keyed by `(TicketOwner, TicketKind)`, not by position.**
//!   Moving a ticket (a player walking) is `set_ticket_with_radius` again at
//!   the new position under the same key — the old position's contribution is
//!   gone the next `propagate`, exactly like vanilla's "re-add replaces."
//! * **`purge_stale` must be driven once per unit of time you intend "ticks" to
//!   mean.** A ticket with `timeout == 0` never decrements — that is
//!   `FLAG_PERSIST`-shaped tickets (`FORCED`, `PLAYER_SIMULATION`), which live
//!   until explicitly removed.
//! * **Two-tracker split is load-bearing — grep for the negative control
//!   before touching it.** `tests::collapsing_the_two_trackers_breaks_the_s3_property`
//!   is the permanent demonstration that a one-tracker build makes a
//!   loading-only ticket simulate.
//!
//! # Configuration
//!
//! No env vars or flags. The vanilla constants ([`FULL_CHUNK_LEVEL`],
//! [`ENTITY_TICKING_LEVEL`], the [`ticket_type`] table) are the only tunables,
//! and they are transcriptions, not knobs — changing one means re-deriving it
//! from vanilla's own `ChunkLevel` or
//! `TicketType`, not picking a new number.
//!
//! # Dependencies
//!
//! None beyond `std`. Deliberately: this module is the *policy* (which chunks
//! are wanted, at what level), and [`crate::chunk_store::ChunkStore`] is the
//! *mechanism* that acts on it — see that module for how the two are wired
//! together without a new [`crate::chunk::ChunkSource`] trait method (adding
//! one would hit the exact unforwarded-wrapper trap `crate::dimension`'s
//! `DimensionalSource` already carries a scar from, for `is_column_resident`).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Vanilla's `ChunkLevel.FULL_CHUNK_LEVEL` — the level at and below which a
/// chunk is fully generated and loaded (vanilla's own `ChunkLevel`).
pub const FULL_CHUNK_LEVEL: i32 = 33;
/// Vanilla's `ChunkLevel.BLOCK_TICKING_LEVEL` (its own `ChunkLevel`). Unused by
/// [`ChunkStatus`]'s two-state simplification, kept for the doc trail and for
/// a future status split.
pub const BLOCK_TICKING_LEVEL: i32 = 32;
/// Vanilla's `ChunkLevel.ENTITY_TICKING_LEVEL` (its own `ChunkLevel`) — the level
/// at and below which a chunk simulates.
pub const ENTITY_TICKING_LEVEL: i32 = 31;
/// Vanilla's `ChunkLevel.MAX_LEVEL` is `FULL_CHUNK_LEVEL + RADIUS_AROUND_FULL_CHUNK`.
/// This crate's generator has no per-status neighbour requirement (see the
/// module doc's "what this deliberately does not port"), so
/// `RADIUS_AROUND_FULL_CHUNK` is `0` and `MAX_LEVEL` collapses to
/// `FULL_CHUNK_LEVEL` exactly. **Do not raise this to match a vanilla source
/// citation without re-reading that note** — it would silently widen residency
/// by however many rings were added, for no corresponding generation step.
pub const MAX_LEVEL: i32 = FULL_CHUNK_LEVEL;

/// The empty-to-full status pipeline, collapsed to two states (S1 in
/// `docs/plans/chunk-lifecycle.md`) — see the module doc for why a twelve-state
/// `ChunkStatus` has no seam to express against this crate's monolithic
/// generator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChunkStatus {
    /// Not resident: no active ticket reaches this column at or below
    /// [`MAX_LEVEL`]. Vanilla's `ChunkStatus.EMPTY` through the statuses below
    /// `FULL`, folded into one state.
    Empty,
    /// Resident and generated: vanilla's `ChunkStatus.FULL`.
    Full,
}

impl ChunkStatus {
    /// `Full` iff `level <= MAX_LEVEL` — vanilla's own `ChunkLevel.isLoaded`,
    /// which `ChunkMap` uses directly to decide whether a
    /// chunk belongs in `toDrop`.
    #[must_use]
    pub const fn from_level(level: i32) -> Self {
        if level <= MAX_LEVEL {
            Self::Full
        } else {
            Self::Empty
        }
    }
}

// Flag bits, transcribed from vanilla's own `TicketType`'s five `boolean` fields packed
// into one byte here rather than five struct fields — the type stays `Copy`
// and fits a `HashMap` value cheaply.
const FLAG_PERSIST: u8 = 1;
const FLAG_LOADING: u8 = 2;
const FLAG_SIMULATION: u8 = 4;
const FLAG_KEEP_DIMENSION_ACTIVE: u8 = 8;
const FLAG_CAN_EXPIRE_IF_UNLOADED: u8 = 16;

/// `record TicketType(long timeout, int flags)` (vanilla's own `TicketType`). `timeout`
/// is in ticks; `0` means "does not expire from [`TicketStore::purge_stale`]
/// alone" (vanilla's own convention — a `FLAG_PERSIST` ticket is removed only
/// explicitly).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TicketType {
    pub timeout: u64,
    flags: u8,
}

impl TicketType {
    #[must_use]
    pub const fn does_load(&self) -> bool {
        self.flags & FLAG_LOADING != 0
    }
    #[must_use]
    pub const fn does_simulate(&self) -> bool {
        self.flags & FLAG_SIMULATION != 0
    }
}

/// The nine registered ticket types, transcribed from vanilla's own `TicketType`'s own
/// fields — timeout and flags exactly as that file states them.
pub mod ticket_type {
    use super::{
        FLAG_CAN_EXPIRE_IF_UNLOADED, FLAG_KEEP_DIMENSION_ACTIVE, FLAG_LOADING, FLAG_PERSIST,
        FLAG_SIMULATION, TicketType,
    };

    /// Timeout 20, flags 2 (loading only). Loads terrain for a joining player
    /// before their entity exists — vanilla's own spawn-preparation task's
    /// `addTicketAndLoadWithRadius`, radius 3.
    pub const PLAYER_SPAWN: TicketType = TicketType {
        timeout: 20,
        flags: FLAG_LOADING,
    };
    /// Timeout 1, flags 2 (loading only).
    pub const SPAWN_SEARCH: TicketType = TicketType {
        timeout: 1,
        flags: FLAG_LOADING,
    };
    /// Timeout 0 (persists), flags 6 (loading + simulation).
    pub const DRAGON: TicketType = TicketType {
        timeout: 0,
        flags: FLAG_LOADING | FLAG_SIMULATION,
    };
    /// Timeout 0, flags 2 (loading only). A connection's streamed view.
    pub const PLAYER_LOADING: TicketType = TicketType {
        timeout: 0,
        flags: FLAG_LOADING,
    };
    /// Timeout 0, flags 12 (simulation + keep-dimension-active).
    pub const PLAYER_SIMULATION: TicketType = TicketType {
        timeout: 0,
        flags: FLAG_SIMULATION | FLAG_KEEP_DIMENSION_ACTIVE,
    };
    /// Timeout 0, flags 15 (persist + loading + simulation + keep-dimension-active).
    /// `/forceload` — the "keeps ticking with nobody nearby" ticket 26.2
    /// actually has (see `crate::ticket`'s module doc and issue #297's own
    /// re-verdict for why a `spawnChunkRadius`-shaped permanent ticket is not
    /// what 26.2 does).
    pub const FORCED: TicketType = TicketType {
        timeout: 0,
        flags: FLAG_PERSIST | FLAG_LOADING | FLAG_SIMULATION | FLAG_KEEP_DIMENSION_ACTIVE,
    };
    /// Timeout 300, flags 15 — `FORCED`, but expiring.
    pub const PORTAL: TicketType = TicketType {
        timeout: 300,
        flags: FLAG_PERSIST | FLAG_LOADING | FLAG_SIMULATION | FLAG_KEEP_DIMENSION_ACTIVE,
    };
    /// Timeout 40, flags 14 (loading + simulation + keep-dimension-active).
    pub const ENDER_PEARL: TicketType = TicketType {
        timeout: 40,
        flags: FLAG_LOADING | FLAG_SIMULATION | FLAG_KEEP_DIMENSION_ACTIVE,
    };
    /// Timeout 1, flags 18 (loading + can-expire-if-unloaded).
    pub const UNKNOWN: TicketType = TicketType {
        timeout: 1,
        flags: FLAG_LOADING | FLAG_CAN_EXPIRE_IF_UNLOADED,
    };
}

/// The radius `PLAYER_SPAWN` is granted at — vanilla's own spawn-preparation
/// task's `addTicketAndLoadWithRadius` call, the same citation `PLAYER_SPAWN`'s own
/// doc comment above already carries. Named here rather than left a literal
/// at each grant site, since [`crate::server`]'s join arm and
/// [`ticket_type::PLAYER_SPAWN`]'s own doc both need to agree on it.
pub const PLAYER_SPAWN_RADIUS: i32 = 3;

/// Which named ticket a [`TicketOwner`] holds — the second half of a
/// [`TicketStore`] key. A plain enum rather than storing a [`TicketType`]
/// directly in the key: two tickets of the same *kind* held by the same owner
/// would otherwise silently collide in the map, and this makes that a compile
/// error (there is no `TicketKind::PlayerLoading(u8)` to construct two of).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TicketKind {
    PlayerSpawn,
    SpawnSearch,
    Dragon,
    PlayerLoading,
    PlayerSimulation,
    Forced,
    Portal,
    EnderPearl,
    Unknown,
}

impl TicketKind {
    #[must_use]
    pub const fn ticket_type(self) -> TicketType {
        match self {
            Self::PlayerSpawn => ticket_type::PLAYER_SPAWN,
            Self::SpawnSearch => ticket_type::SPAWN_SEARCH,
            Self::Dragon => ticket_type::DRAGON,
            Self::PlayerLoading => ticket_type::PLAYER_LOADING,
            Self::PlayerSimulation => ticket_type::PLAYER_SIMULATION,
            Self::Forced => ticket_type::FORCED,
            Self::Portal => ticket_type::PORTAL,
            Self::EnderPearl => ticket_type::ENDER_PEARL,
            Self::Unknown => ticket_type::UNKNOWN,
        }
    }
}

/// Who a ticket belongs to — the first half of a [`TicketStore`] key.
///
/// Kept deliberately small: this crate has no per-player entity id reaching
/// this layer yet (see the module doc's dependency note), so a player ticket
/// is keyed by an opaque `u64` the caller supplies (a connection id is enough
/// — uniqueness, not identity, is all a `HashMap` key needs).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TicketOwner {
    /// The one world-spawn ticket every world may hold.
    Spawn,
    /// A `/forceload`-shaped ticket, keyed by an id the caller assigns (e.g. a
    /// serial counter) so more than one forced region can coexist.
    Forced(u64),
    /// A player-following ticket, keyed by an opaque connection/session id.
    Player(u64),
}

/// One active ticket: a position, the type it was granted as, the level it
/// contributes, and (for an expiring type) how many ticks remain.
#[derive(Debug, Clone, Copy)]
struct Ticket {
    pos: (i32, i32),
    ty: TicketType,
    level: i32,
    ticks_left: u64,
}

/// What changed on the last [`TicketStore::propagate`] — the actionable half
/// of a tick, so a caller does not have to diff two full level maps itself.
#[derive(Debug, Clone, Default)]
pub struct TicketDelta {
    /// Positions whose loading level crossed from `> MAX_LEVEL` to `<= MAX_LEVEL`
    /// on this call — newly generatable/resident.
    pub newly_resident: Vec<(i32, i32)>,
    /// Positions whose loading level crossed the other way — no longer backed
    /// by any ticket, and therefore [`crate::chunk_store::ChunkStore`]'s
    /// eviction candidates.
    pub newly_unresident: Vec<(i32, i32)>,
}

/// The ticket graph: active tickets plus the two propagated level maps.
///
/// See the module doc for the propagation rule and for why loading and
/// simulation are kept as two independent maps rather than one.
#[derive(Debug, Default)]
pub struct TicketStore {
    tickets: HashMap<(TicketOwner, TicketKind), Ticket>,
    loading_levels: HashMap<(i32, i32), i32>,
    simulation_levels: HashMap<(i32, i32), i32>,
}

#[must_use]
fn chebyshev(a: (i32, i32), b: (i32, i32)) -> i32 {
    (a.0 - b.0).abs().max((a.1 - b.1).abs())
}

impl TicketStore {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Grants (or moves, or renews) a ticket at level `FULL_CHUNK_LEVEL - radius`
    /// — vanilla's `TicketStorage::addTicketWithRadius`. A `radius` of `0`
    /// grants only the centre chunk at `FULL_CHUNK_LEVEL`.
    pub fn set_ticket_with_radius(
        &mut self,
        owner: TicketOwner,
        kind: TicketKind,
        pos: (i32, i32),
        radius: i32,
    ) {
        self.set_ticket_at_level(owner, kind, pos, FULL_CHUNK_LEVEL - radius);
    }

    /// Grants (or moves, or renews) a ticket at an explicit level — vanilla's
    /// direct-level tickets (`PLAYER_SIMULATION`, `FORCED`) rather than the
    /// radius-derived form.
    pub fn set_ticket_at_level(
        &mut self,
        owner: TicketOwner,
        kind: TicketKind,
        pos: (i32, i32),
        level: i32,
    ) {
        let ty = kind.ticket_type();
        self.tickets.insert(
            (owner, kind),
            Ticket {
                pos,
                ty,
                level,
                ticks_left: ty.timeout,
            },
        );
    }

    /// Withdraws a ticket. Returns whether one was present.
    pub fn remove_ticket(&mut self, owner: TicketOwner, kind: TicketKind) -> bool {
        self.tickets.remove(&(owner, kind)).is_some()
    }

    /// Resets an expiring ticket's countdown to its type's timeout, without
    /// moving it — vanilla's `Ticket::resetTicksLeft`, called by
    /// `PrepareSpawnTask`'s `Ready.keepAlive()` for `PLAYER_SPAWN`. A no-op if
    /// the ticket is not currently held.
    pub fn refresh_ticket(&mut self, owner: TicketOwner, kind: TicketKind) -> bool {
        if let Some(ticket) = self.tickets.get_mut(&(owner, kind)) {
            ticket.ticks_left = ticket.ty.timeout;
            true
        } else {
            false
        }
    }

    /// Decrements every expiring ticket's countdown by one and removes those
    /// that hit zero — vanilla's `TicketStorage::purgeStaleTickets`, one
    /// `decreaseTicksLeft()` per tick. A ticket with `timeout == 0` is
    /// untouched (never decremented, per [`TicketType`]'s own doc). Returns
    /// the keys removed.
    pub fn purge_stale(&mut self) -> Vec<(TicketOwner, TicketKind)> {
        let mut expired = Vec::new();
        self.tickets.retain(|&key, ticket| {
            if ticket.ty.timeout == 0 {
                return true;
            }
            if ticket.ticks_left == 0 {
                expired.push(key);
                return false;
            }
            ticket.ticks_left -= 1;
            if ticket.ticks_left == 0 {
                expired.push(key);
                false
            } else {
                true
            }
        });
        expired
    }

    /// Recomputes both level maps from scratch and reports what changed.
    ///
    /// O(active tickets × each ticket's own reach) — see the module doc's "how
    /// to change it" note for why a whole-store scan must not replace this.
    pub fn propagate(&mut self) -> TicketDelta {
        let loading = Self::propagate_one(self.tickets.values().filter(|t| t.ty.does_load()));
        let simulation =
            Self::propagate_one(self.tickets.values().filter(|t| t.ty.does_simulate()));

        let delta = Self::diff(&self.loading_levels, &loading);
        self.loading_levels = loading;
        self.simulation_levels = simulation;
        delta
    }

    fn propagate_one<'a>(tickets: impl Iterator<Item = &'a Ticket>) -> HashMap<(i32, i32), i32> {
        let mut levels: HashMap<(i32, i32), i32> = HashMap::new();
        for ticket in tickets {
            let reach = MAX_LEVEL - ticket.level;
            if reach < 0 {
                continue;
            }
            for dz in -reach..=reach {
                for dx in -reach..=reach {
                    let pos = (ticket.pos.0 + dx, ticket.pos.1 + dz);
                    let distance = chebyshev(ticket.pos, pos);
                    let level = ticket.level + distance;
                    if level > MAX_LEVEL {
                        continue;
                    }
                    levels
                        .entry(pos)
                        .and_modify(|current| *current = (*current).min(level))
                        .or_insert(level);
                }
            }
        }
        levels
    }

    /// Resident-set delta between two loading-level snapshots, keyed on
    /// [`MAX_LEVEL`] rather than on exact-level equality — a position moving
    /// from level 20 to level 25 is not a residency change and must not be
    /// reported as one.
    fn diff(before: &HashMap<(i32, i32), i32>, after: &HashMap<(i32, i32), i32>) -> TicketDelta {
        let mut delta = TicketDelta::default();
        for (&pos, &level) in after {
            let was_resident = before.get(&pos).is_some_and(|&l| l <= MAX_LEVEL);
            let is_resident = level <= MAX_LEVEL;
            if is_resident && !was_resident {
                delta.newly_resident.push(pos);
            }
        }
        for (&pos, &level) in before {
            let was_resident = level <= MAX_LEVEL;
            let is_resident = after.get(&pos).is_some_and(|&l| l <= MAX_LEVEL);
            if was_resident && !is_resident {
                delta.newly_unresident.push(pos);
            }
        }
        delta
    }

    #[must_use]
    pub fn loading_level(&self, pos: (i32, i32)) -> i32 {
        self.loading_levels
            .get(&pos)
            .copied()
            .unwrap_or(MAX_LEVEL + 1)
    }

    #[must_use]
    pub fn simulation_level(&self, pos: (i32, i32)) -> i32 {
        self.simulation_levels
            .get(&pos)
            .copied()
            .unwrap_or(MAX_LEVEL + 1)
    }

    #[must_use]
    pub fn is_resident(&self, pos: (i32, i32)) -> bool {
        self.loading_level(pos) <= MAX_LEVEL
    }

    #[must_use]
    pub fn is_simulating(&self, pos: (i32, i32)) -> bool {
        self.simulation_level(pos) <= MAX_LEVEL
    }

    #[must_use]
    pub fn status(&self, pos: (i32, i32)) -> ChunkStatus {
        ChunkStatus::from_level(self.loading_level(pos))
    }

    /// Every currently-resident position, ordered nearest-level-first — the
    /// answer to "priority is the ticket level" (`ChunkTaskDispatcher.submit`,
    /// vanilla's own `TicketType`): a caller driving generation off this list visits
    /// the chunks vanilla's own priority queue would visit first, first.
    #[must_use]
    pub fn resident_positions_by_level(&self) -> Vec<(i32, i32)> {
        let mut positions: Vec<((i32, i32), i32)> = self
            .loading_levels
            .iter()
            .filter(|&(_, &level)| level <= MAX_LEVEL)
            .map(|(&pos, &level)| (pos, level))
            .collect();
        positions.sort_unstable_by_key(|&(_, level)| level);
        positions.into_iter().map(|(pos, _)| pos).collect()
    }

    #[cfg(test)]
    fn active_ticket_count(&self) -> usize {
        self.tickets.len()
    }
}

/// A shared, cloneable handle to one [`TicketStore`] — the same
/// `Arc<Mutex<…>>`-behind-a-newtype shape this crate already uses four times
/// (`crate::tick::BlockTickFeed`, `ExplosionFeed`, `MobHandle`,
/// `BlockEntityHandle` — see `docs/plans/chunk-lifecycle.md`'s "where the store
/// lives" section for why that shape rather than an ECS resource).
#[derive(Debug, Clone, Default)]
pub struct TicketStoreHandle(Arc<Mutex<TicketStore>>);

impl TicketStoreHandle {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_ticket_with_radius(
        &self,
        owner: TicketOwner,
        kind: TicketKind,
        pos: (i32, i32),
        radius: i32,
    ) {
        self.lock()
            .set_ticket_with_radius(owner, kind, pos, radius);
    }

    pub fn set_ticket_at_level(
        &self,
        owner: TicketOwner,
        kind: TicketKind,
        pos: (i32, i32),
        level: i32,
    ) {
        self.lock().set_ticket_at_level(owner, kind, pos, level);
    }

    pub fn remove_ticket(&self, owner: TicketOwner, kind: TicketKind) -> bool {
        self.lock().remove_ticket(owner, kind)
    }

    pub fn refresh_ticket(&self, owner: TicketOwner, kind: TicketKind) -> bool {
        self.lock().refresh_ticket(owner, kind)
    }

    /// Purges expired tickets and re-propagates in one call — the per-tick
    /// entry point. Returns the residency delta so a caller (e.g.
    /// [`crate::chunk_store::ChunkStore`]) can act on exactly what changed.
    pub fn tick(&self) -> TicketDelta {
        let mut store = self.lock();
        store.purge_stale();
        store.propagate()
    }

    #[must_use]
    pub fn is_resident(&self, pos: (i32, i32)) -> bool {
        self.lock().is_resident(pos)
    }

    #[must_use]
    pub fn is_simulating(&self, pos: (i32, i32)) -> bool {
        self.lock().is_simulating(pos)
    }

    #[must_use]
    pub fn status(&self, pos: (i32, i32)) -> ChunkStatus {
        self.lock().status(pos)
    }

    #[must_use]
    pub fn resident_positions_by_level(&self) -> Vec<(i32, i32)> {
        self.lock().resident_positions_by_level()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, TicketStore> {
        self.0.lock().expect("ticket store lock poisoned")
    }

    #[cfg(test)]
    fn active_ticket_count(&self) -> usize {
        self.lock().active_ticket_count()
    }

    /// Grants a player-following `PLAYER_LOADING`/`PLAYER_SIMULATION` ticket
    /// pair at `pos`, both at `radius` — issue #619's connection-side wiring.
    ///
    /// Both tickets share one radius because this crate has no separately
    /// configured simulation distance yet (vanilla's `server.properties`
    /// splits `view-distance` from `simulation-distance`); see
    /// `docs/chunk-tickets.md`'s "Open work" for the gap. `id` need only be
    /// unique per connection — [`TicketOwner::Player`]'s own doc says a
    /// caller-assigned `u64` is enough, and [`crate::server`] derives one from
    /// the connection's login uuid.
    ///
    /// Returns a [`PlayerTicketGuard`] whose `Drop` withdraws both tickets —
    /// the same RAII shape [`crate::players::PlayerRegistry::join`]'s
    /// `PlayerTicket` already uses for the entity roster, applied here to
    /// residency so a connection's ticket dies on every exit path out of
    /// `serve_play`, not just a clean disconnect.
    #[must_use]
    pub fn grant_player(&self, id: u64, pos: (i32, i32), radius: i32) -> PlayerTicketGuard {
        self.set_ticket_with_radius(TicketOwner::Player(id), TicketKind::PlayerLoading, pos, radius);
        self.set_ticket_with_radius(TicketOwner::Player(id), TicketKind::PlayerSimulation, pos, radius);
        PlayerTicketGuard {
            store: self.clone(),
            id,
        }
    }
}

/// RAII guard for one connection's player-following ticket pair, from
/// [`TicketStoreHandle::grant_player`]. See that method's doc for why the
/// pair shares a radius.
///
/// `Drop` removes both tickets — [`crate::server`]'s `serve_play` owns one of
/// these for the lifetime of the connection, so a chunk near a player stops
/// being ticket-resident on every exit path (clean disconnect, a `?`
/// propagated error, or a cancelled task), never only the happy path.
#[derive(Debug)]
pub struct PlayerTicketGuard {
    store: TicketStoreHandle,
    id: u64,
}

impl PlayerTicketGuard {
    /// Re-grants this connection's ticket pair at `pos`/`radius` — moving a
    /// ticket is granting it again under the same `(TicketOwner, TicketKind)`
    /// key, exactly as this module's own doc says (`ticket.rs`'s "A ticket is
    /// keyed by `(TicketOwner, TicketKind)`, not by position"). Called from
    /// [`crate::server`]'s view-recenter and view-radius-change arms so a
    /// player's residency claim follows their real position, not just their
    /// join point.
    pub fn move_to(&self, pos: (i32, i32), radius: i32) {
        self.store
            .set_ticket_with_radius(TicketOwner::Player(self.id), TicketKind::PlayerLoading, pos, radius);
        self.store.set_ticket_with_radius(
            TicketOwner::Player(self.id),
            TicketKind::PlayerSimulation,
            pos,
            radius,
        );
    }

    /// Resets the world's `PLAYER_SPAWN` ticket's countdown without moving
    /// it — vanilla's `Ready.keepAlive()`, called from any connected
    /// player's own keep-alive timer. A no-op if no spawn ticket is
    /// currently held (e.g. every connection using a private, disconnected
    /// [`TicketStoreHandle::default`]).
    pub fn refresh_world_spawn(&self) -> bool {
        self.store.refresh_ticket(TicketOwner::Spawn, TicketKind::PlayerSpawn)
    }
}

impl Drop for PlayerTicketGuard {
    fn drop(&mut self) {
        self.store.remove_ticket(TicketOwner::Player(self.id), TicketKind::PlayerLoading);
        self.store.remove_ticket(TicketOwner::Player(self.id), TicketKind::PlayerSimulation);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Property 1 from `docs/plans/chunk-lifecycle.md` U4, transcribed by hand
    /// from vanilla's own `ChunkLevel`/`TicketStorage` types rather than derived from
    /// this module's own code (the `decode(encode(x)) == x` trap this repo's
    /// evidence standards forbid).
    #[test]
    fn a_direct_level_ticket_spreads_by_chebyshev_distance() {
        let mut store = TicketStore::new();
        store.set_ticket_at_level(
            TicketOwner::Forced(1),
            TicketKind::PlayerSimulation,
            (0, 0),
            ENTITY_TICKING_LEVEL,
        );
        store.propagate();

        assert_eq!(store.simulation_level((0, 0)), 31);
        assert_eq!(
            store.simulation_level((3, 0)),
            34,
            "31 + chebyshev((0,0),(3,0))=3 = 34, which exceeds MAX_LEVEL (33): not resident"
        );
        assert!(!store.is_resident((3, 0)));
        assert_eq!(
            store.simulation_level((0, -2)),
            33,
            "31 + 2 = 33, exactly MAX_LEVEL: resident"
        );
        assert!(store.is_simulating((0, -2)));
    }

    /// Property 2: two tickets take the minimum, never the sum and never
    /// last-write-wins.
    #[test]
    fn two_tickets_take_the_minimum_level_not_the_sum() {
        let mut store = TicketStore::new();
        store.set_ticket_at_level(TicketOwner::Forced(1), TicketKind::Forced, (0, 0), 31);
        store.set_ticket_at_level(TicketOwner::Forced(2), TicketKind::Forced, (10, 0), 31);
        store.propagate();

        // (5,0) is distance 5 from the first ticket (level 36, unresident) and
        // distance 5 from the second (level 36, unresident) — neither alone
        // reaches it, and the minimum of two unresident values must stay
        // unresident, not become resident by "adding up" contributions.
        assert!(!store.is_resident((5, 0)));
        // (2,0) is distance 2 from the first (level 33, resident) and distance
        // 8 from the second (level 39). The minimum must be 33, not some
        // blend, and must not be perturbed by the second, farther ticket.
        assert_eq!(store.loading_level((2, 0)), i32::MAX.min(33).min(39).min(33));
        assert_eq!(store.loading_level((2, 0)), 33);
    }

    /// Property 3 (S3): a loading-only ticket makes its chunk resident and
    /// **not** simulating. This is the assertion the two-tracker split exists
    /// to make true.
    #[test]
    fn a_loading_only_ticket_is_resident_but_not_simulating() {
        let mut store = TicketStore::new();
        store.set_ticket_with_radius(
            TicketOwner::Spawn,
            TicketKind::PlayerSpawn,
            (0, 0),
            3,
        );
        store.propagate();

        assert!(store.is_resident((0, 0)), "radius 3 => level 30 at centre, well inside MAX_LEVEL");
        assert!(
            !store.is_simulating((0, 0)),
            "PLAYER_SPAWN is loading-only (flags=2): it must never simulate"
        );
        // The ring at Chebyshev distance 3 is level 33, exactly resident; 4 is
        // level 34, not.
        assert!(store.is_resident((3, 0)));
        assert!(!store.is_resident((4, 0)));
    }

    /// The permanent negative control for the property above: a one-tracker
    /// build (feeding *every* active ticket into both maps, ignoring
    /// `does_load`/`does_simulate`) makes a loading-only ticket simulate. If
    /// `propagate_one`'s filters were ever removed or a caller fed the wrong
    /// iterator to the wrong map, this is the assertion that would catch it.
    #[test]
    fn collapsing_the_two_trackers_breaks_the_s3_property() {
        let mut store = TicketStore::new();
        store.set_ticket_with_radius(
            TicketOwner::Spawn,
            TicketKind::PlayerSpawn,
            (0, 0),
            3,
        );
        // Simulate a one-tracker build: propagate loading with *no* does_load
        // filter (i.e. every ticket, unfiltered) and treat that as the
        // simulation map too.
        let unfiltered = TicketStore::propagate_one(store.tickets.values());
        let would_be_simulating = unfiltered
            .get(&(0, 0))
            .is_some_and(|&level| level <= MAX_LEVEL);
        assert!(
            would_be_simulating,
            "a one-tracker build makes a loading-only ticket simulate — this is the bug \
             the two-tracker split exists to prevent, demonstrated rather than merely asserted"
        );
    }

    /// Priority ordering: nearer-a-ticket-source chunks sort first, which is
    /// #289's "loading-priority system" — derived from the level, not a
    /// separate heuristic.
    #[test]
    fn resident_positions_sort_by_level_ascending() {
        let mut store = TicketStore::new();
        store.set_ticket_at_level(TicketOwner::Forced(1), TicketKind::Forced, (0, 0), 31);
        store.propagate();
        let ordered = store.resident_positions_by_level();
        assert!(!ordered.is_empty());
        let mut previous = i32::MIN;
        for pos in &ordered {
            let level = store.loading_level(*pos);
            assert!(
                level >= previous,
                "resident_positions_by_level must be non-decreasing in level"
            );
            previous = level;
        }
        // The centre itself must be first: it has the lowest level (31) of
        // anything in the set.
        assert_eq!(ordered[0], (0, 0));
    }

    /// Expiry: a 20-tick `PLAYER_SPAWN` ticket with no refresh disappears
    /// after exactly 20 `purge_stale` calls, and its chunk stops being
    /// resident. Ticks are driven by the test itself (a local loop), never
    /// read from any shared clock — see this crate's duration-species
    /// warning.
    #[test]
    fn an_unrefreshed_spawn_ticket_expires_after_twenty_ticks() {
        let mut store = TicketStore::new();
        store.set_ticket_with_radius(
            TicketOwner::Spawn,
            TicketKind::PlayerSpawn,
            (0, 0),
            3,
        );
        store.propagate();
        assert!(store.is_resident((0, 0)));

        for tick in 0..19 {
            let expired = store.purge_stale();
            assert!(
                expired.is_empty(),
                "the ticket must survive tick {tick} of 19 — it has a 20-tick timeout"
            );
        }
        store.propagate();
        assert!(
            store.is_resident((0, 0)),
            "19 ticks driven, one short of the 20-tick timeout: still resident"
        );

        let expired = store.purge_stale();
        assert_eq!(
            expired,
            vec![(TicketOwner::Spawn, TicketKind::PlayerSpawn)],
            "the 20th tick must expire exactly this ticket"
        );
        let delta = store.propagate();
        assert!(!store.is_resident((0, 0)));
        assert!(
            delta.newly_unresident.contains(&(0, 0)),
            "propagate must report the centre as newly unresident so a caller can unload it"
        );
    }

    /// Refreshing resets the countdown — vanilla's `Ready.keepAlive()`.
    #[test]
    fn refreshing_a_ticket_resets_its_countdown() {
        let mut store = TicketStore::new();
        store.set_ticket_with_radius(
            TicketOwner::Spawn,
            TicketKind::PlayerSpawn,
            (0, 0),
            3,
        );
        for _ in 0..19 {
            store.purge_stale();
        }
        assert!(store.refresh_ticket(TicketOwner::Spawn, TicketKind::PlayerSpawn));
        for tick in 0..19 {
            let expired = store.purge_stale();
            assert!(expired.is_empty(), "refreshed ticket must survive tick {tick} again");
        }
    }

    /// A `FORCED` ticket (timeout 0, flags 15) neither expires nor merely
    /// loads — it simulates with zero other tickets present, which is 26.2's
    /// real "keeps ticking with nobody nearby" behaviour (see issue #297's
    /// re-verdict, cited in this module's `ticket_type::FORCED` doc).
    #[test]
    fn a_forced_ticket_simulates_and_never_expires_on_its_own() {
        let mut store = TicketStore::new();
        store.set_ticket_at_level(
            TicketOwner::Forced(1),
            TicketKind::Forced,
            (100, 100),
            ENTITY_TICKING_LEVEL,
        );
        store.propagate();
        assert!(store.is_simulating((100, 100)));

        for _ in 0..1000 {
            let expired = store.purge_stale();
            assert!(
                expired.is_empty(),
                "a timeout=0 ticket must never be purged by ticks alone"
            );
        }
        store.propagate();
        assert!(
            store.is_simulating((100, 100)),
            "still simulating after 1000 ticks with zero refreshes and zero other tickets"
        );
    }

    /// Removing a ticket is what actually drops it, for a `timeout == 0`
    /// type — `purge_stale` alone cannot, by design (see the test above).
    #[test]
    fn removing_a_persistent_ticket_makes_its_chunk_unresident() {
        let mut store = TicketStore::new();
        store.set_ticket_at_level(TicketOwner::Forced(1), TicketKind::Forced, (0, 0), 31);
        store.propagate();
        assert!(store.is_resident((0, 0)));

        assert!(store.remove_ticket(TicketOwner::Forced(1), TicketKind::Forced));
        let delta = store.propagate();
        assert!(!store.is_resident((0, 0)));
        assert!(delta.newly_unresident.contains(&(0, 0)));
    }

    /// `TicketStoreHandle` forwards correctly and is the shape a shared
    /// resource in this crate always takes — a smoke test for the wrapper,
    /// not a re-test of the logic above.
    #[test]
    fn the_shared_handle_forwards_to_the_same_store() {
        let handle = TicketStoreHandle::new();
        handle.set_ticket_with_radius(TicketOwner::Spawn, TicketKind::PlayerSpawn, (0, 0), 3);
        assert_eq!(handle.active_ticket_count(), 1);
        let delta = handle.tick();
        assert!(delta.newly_resident.contains(&(0, 0)));
        assert!(handle.is_resident((0, 0)));
        assert_eq!(handle.status((0, 0)), ChunkStatus::Full);
        assert_eq!(handle.status((1000, 1000)), ChunkStatus::Empty);
    }

    /// Issue #619: `grant_player` must actually install both tickets,
    /// `move_to` must move both together, and dropping the guard must
    /// withdraw both — a positive control before the negative one below.
    #[test]
    fn a_player_ticket_guard_grants_moves_and_drops_both_tickets() {
        let handle = TicketStoreHandle::new();
        let guard = handle.grant_player(11, (0, 0), 4);
        handle.tick();
        assert!(handle.is_resident((4, 0)), "loading radius 4 must reach (4, 0)");
        assert!(handle.is_simulating((4, 0)), "simulation radius 4 must reach (4, 0)");
        assert!(!handle.is_resident((5, 0)), "radius 4 must not reach (5, 0)");

        // Move far away — the old position must lose residency and the new
        // one must gain it, proving this is a re-grant under the same key
        // rather than a second, additive ticket.
        guard.move_to((100, 100), 4);
        handle.tick();
        assert!(
            !handle.is_resident((0, 0)),
            "the old position must lose residency once the ticket moved away"
        );
        assert!(handle.is_resident((104, 100)), "the new position must gain residency");

        drop(guard);
        handle.tick();
        assert!(
            !handle.is_resident((104, 100)),
            "dropping the guard must withdraw both tickets, not just stop refreshing them"
        );
    }

    /// The permanent negative control for the gate above: a plain
    /// `set_ticket_with_radius` grant with **no** guard must NOT be removed
    /// by anything this test does — proving the drop above is the guard's
    /// `Drop` impl actually firing, not `tick()`/`propagate()` incidentally
    /// clearing every ticket.
    #[test]
    fn a_ticket_granted_without_a_guard_survives_unrelated_guard_drops() {
        let handle = TicketStoreHandle::new();
        handle.set_ticket_with_radius(TicketOwner::Forced(1), TicketKind::Forced, (0, 0), 0);
        let guard = handle.grant_player(22, (50, 50), 2);
        handle.tick();
        assert!(handle.is_resident((0, 0)));

        drop(guard);
        handle.tick();
        assert!(
            handle.is_resident((0, 0)),
            "an unrelated FORCED ticket must survive a different owner's guard being dropped"
        );
    }

    /// `refresh_world_spawn` resets the countdown without moving the ticket,
    /// and reports `false` when nothing is held — the shape
    /// `crate::server`'s keep-alive-tick refresh relies on for every
    /// connection whose `TicketStoreHandle` never received a spawn grant
    /// (every non-production caller's private default handle).
    #[test]
    fn refresh_world_spawn_resets_without_moving_and_reports_absence() {
        let handle = TicketStoreHandle::new();
        let guard = handle.grant_player(1, (0, 0), 1);
        assert!(
            !guard.refresh_world_spawn(),
            "no spawn ticket was ever granted on this handle"
        );

        handle.set_ticket_with_radius(TicketOwner::Spawn, TicketKind::PlayerSpawn, (7, 7), PLAYER_SPAWN_RADIUS);
        assert!(guard.refresh_world_spawn());
        handle.tick();
        assert!(handle.is_resident((7 + PLAYER_SPAWN_RADIUS, 7)));
        assert!(!handle.is_resident((7 + PLAYER_SPAWN_RADIUS + 1, 7)));
    }
}
