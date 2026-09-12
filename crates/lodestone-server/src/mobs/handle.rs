//! Shared production handles and snapshot sources for the mob simulation.
//!
//! The simulation itself stays in [`super::MobSim`]. These wrappers provide
//! the lockable mutation seam used by connection and tick tasks, plus the
//! read-only source used for entity streaming.

use super::*;

// NOTE: this module owns `ChunkWorld` + `MobSim`; the acceptance gate lives in
// `tests/mob_sim.rs` so it drives them through the crate's *public* API — the
// same discipline the rest of the project uses (a consumer that is only a
// `#[cfg(test)]` fake proves nothing about the public seam).

/// A live [`EntitySource`] fed by a background-ticked [`MobSim`] (the live mob tick).
/// [`IntegratedServer::open_in_memory_with_mobs`](crate::IntegratedServer::open_in_memory_with_mobs)
/// constructs one alongside [`crate::tick::run_tick_loop`] (the shared tick loop; this
/// used to be [`run_mob_tick_loop`] before the mob and block-entity tick
/// loops were unified into one), the task that owns the sim and republishes
/// its snapshots here every tick.
///
/// Deliberately the same shape as `entity_streaming_live.rs`'s own test-only
/// `SharedSnapshotSource` (an `Arc<Mutex<Vec<EntitySnapshot>>>` behind
/// [`EntitySource`]) — that test already proved the read side of this shape
/// reaches a real client; this type is the production version, now fed by a
/// real simulation instead of a hand-mutated `Vec`.
#[derive(Debug, Clone, Default)]
pub struct LiveMobSource(
    Arc<Mutex<Vec<EntitySnapshot>>>,
    /// The dragon fight's boss bars, published the same way `.0` is — see
    /// [`publish_boss_bars`](Self::publish_boss_bars). A second field rather
    /// than folding into `.0` because a boss bar is not an entity and has no
    /// `EntitySnapshot` shape to borrow.
    Arc<Mutex<Vec<crate::protocol::BossBarSnapshot>>>,
);

impl EntitySource for LiveMobSource {
    fn snapshots(&self) -> Vec<EntitySnapshot> {
        self.0
            .lock()
            .expect("live mob snapshot lock poisoned")
            .clone()
    }

    fn boss_bars(&self) -> Vec<crate::protocol::BossBarSnapshot> {
        self.1
            .lock()
            .expect("live mob boss-bar lock poisoned")
            .clone()
    }
}

impl LiveMobSource {
    /// Replaces the published snapshot set. Called once per tick — in
    /// production by [`crate::tick::run_tick_loop`], and directly by the
    /// tick-source test. The
    /// next `snapshots()` call from any connection (there may be several,
    /// e.g. open-to-LAN) sees the new set. `pub(crate)`, not private: the
    /// unified loop lives in a sibling module (`tick.rs`) and needs to call
    /// this directly rather than through a second wrapper.
    pub(crate) fn publish(&self, snapshots: Vec<EntitySnapshot>) {
        *self.0.lock().expect("live mob snapshot lock poisoned") = snapshots;
    }

    /// Replaces the published boss-bar set — the [`boss_bars`](EntitySource::boss_bars)
    /// twin of [`publish`](Self::publish), called from the same tick-loop
    /// call site right after it (see `crate::tick::run_tick_loop`).
    pub(crate) fn publish_boss_bars(&self, bars: Vec<crate::protocol::BossBarSnapshot>) {
        *self.1.lock().expect("live mob boss-bar lock poisoned") = bars;
    }
}

/// A shared, mutation-capable handle onto one live [`MobSim`] — the
/// counterpart [`crate::BlockEntityHandle`] already established for block
/// entities, and the exact piece the combat census named as
/// missing: *"there is no way to reach a live mob's health from a
/// connection's own task... `MobSim` is ticked entirely inside its own
/// background task and is never wrapped in a shared, lockable handle."*
/// [`LiveMobSource`] is deliberately read-only (a snapshot cache for
/// streaming, fed *by* the tick loop); this is the mutation-capable sibling a
/// connection needs to actually damage/knock back a mob a player attacked —
/// see `crate::server::apply_attack`, its one production caller.
///
/// # Why `MobSim<'static>`, and the leak that produces it
///
/// [`MobSim`] borrows its [`ChunkWorld`] (`MobSim<'w>`), but a handle shared
/// with a separately-`tokio::spawn`ed connection task must be `'static` (that
/// is what `tokio::spawn` requires of everything it captures). [`new`](Self::new)
/// resolves this with [`Box::leak`]: the `ChunkWorld` a caller hands in is
/// leaked once, for the process's remaining lifetime, rather than borrowed
/// for one task's own stack frame the way [`run_mob_tick_loop`]'s previous
/// (pre-handle) implementation did.
///
/// This is a **deliberate, bounded** leak, not an oversight.
/// `run_mob_tick_loop`'s own doc comment already discloses that its
/// `ChunkWorld` snapshot is static for the sim's whole lifetime — a fixed
/// area around the mob-spawn center, never widened after the initial load.
/// Leaking it only changes *whose* lifetime "static" is measured against:
/// "static for this one task" becomes "static for the process" — the same
/// bytes, held slightly longer, for the one [`MobSim`] a running
/// [`crate::IntegratedServer`] ever constructs per call to
/// [`open_in_memory_with_mobs`](crate::IntegratedServer::open_in_memory_with_mobs).
/// A caller that constructs many short-lived handles (e.g. one per test) does
/// leak once per handle — acceptable for a bounded terrain snapshot in a
/// process that exits shortly after, the same trade-off `MobSim`'s own
/// `assert_send::<MobSim<'static>>()` const-check already anticipated by
/// name.
#[derive(Debug, Clone)]
pub struct MobHandle(Arc<Mutex<MobSim<'static>>>);

impl Default for MobHandle {
    /// A handle over an empty, mobless sim backed by a tiny leaked
    /// [`ChunkWorld`] — the "nothing ticks it, but it is real and safe to
    /// attack against" default [`crate::BlockEntityHandle::default`] already
    /// establishes for connections built without a live mob population
    /// (`IntegratedServer::open_in_memory`/`open_in_memory_with_entities`/`bind`).
    /// An `Attack` packet against any entity id here simply finds no mob
    /// ([`MobSim::attack`] returns `None`) — a harmless no-op, never a panic.
    fn default() -> Self {
        Self::new(ChunkWorld::new(-64, 384))
    }
}

impl MobHandle {
    /// Builds a handle over a fresh, empty [`MobSim`] backed by a leaked copy
    /// of `world` — see the struct's own doc comment for why leaking is the
    /// deliberate choice here.
    #[must_use]
    pub fn new(world: ChunkWorld) -> Self {
        let world: &'static ChunkWorld = Box::leak(Box::new(world));
        Self(Arc::new(Mutex::new(MobSim::new(world))))
    }

    /// Builds a handle already seeded with [`seed_demo_mobs`]'s baseline
    /// population, snapshotting `world_source` the same way the previous
    /// (pre-handle) `run_mob_tick_loop` did at the top of its own future —
    /// see that function's doc comment for the `cx_range`/`cz_range`/
    /// `mob_center` scope notes, unchanged by this refactor.
    #[must_use]
    pub fn seeded<S: ChunkSource>(
        world_source: &S,
        cx_range: std::ops::RangeInclusive<i32>,
        cz_range: std::ops::RangeInclusive<i32>,
        center_x: i32,
        center_z: i32,
        mob_count: usize,
    ) -> Self {
        let handle = Self::default();
        handle.reseed(
            ChunkWorld::from_source(world_source, cx_range, cz_range),
            center_x,
            center_z,
            mob_count,
        );
        handle
    }

    /// Replaces this handle's terrain snapshot **and** its population with a
    /// fresh [`MobSim`] over `world`, seeded exactly as
    /// [`seeded`](Self::seeded) would have.
    ///
    /// # Why this exists
    ///
    /// `seeded` did the whole job inside
    /// [`crate::IntegratedServer::open_in_memory_with_mobs`]'s body, *before any
    /// task spawned* — so the 49-column `ChunkWorld::from_source` snapshot it
    /// needs was on the critical path of opening a world, at ~909 ms per
    /// composed column. Vanilla does not block world-open on mob population, and
    /// neither does this crate any more: the constructor now builds a
    /// [`Default`] handle (empty, mobless, safe to attack against — see that
    /// impl's own doc comment) and a background task calls this once the terrain
    /// it needs has been fetched off-thread.
    ///
    /// # What is deliberately thrown away
    ///
    /// Everything: the old `MobSim` is dropped, not merged. That is correct for
    /// the one caller — a handle that has only ever been `Default` has no
    /// population to lose, and `set_next_id(1000)` must be re-applied to the new
    /// sim anyway. It is **not** a general "load more terrain" primitive; a mob
    /// spawned in the window before the first reseed would vanish. Widening the
    /// snapshot as the player walks (this module's long-standing documented
    /// scope cut) needs a sim that can *extend* its world, not replace it.
    ///
    /// Takes `&self`, like every other accessor here, because the sim lives
    /// behind the handle's own `Mutex` — so this is safe to call from a
    /// background task while the connection task holds a clone.
    pub fn reseed(&self, mut world: ChunkWorld, center_x: i32, center_z: i32, mob_count: usize) {
        // Drain pending generation spawns while `world` is still an owned local,
        // before it leaks to `'static` below. The list is non-empty only while
        // these chunks are ever generated — see `ChunkWorld`'s own field doc
        // (`pending_generation_spawns`) for why that is what keeps a fresh
        // world's `SPAWN`-stage animals from duplicating across a restart: a
        // reload of an existing world loads these same chunks from disk, which
        // never populates this list.
        let pending_generation_spawns = world.take_pending_generation_spawns();
        // Leaked for the same reason `new` leaks: `MobSim` borrows its world for
        // `'static`. See the struct's own doc comment — one bounded snapshot per
        // reseed, and production reseeds exactly once per world.
        let world: &'static ChunkWorld = Box::leak(Box::new(world));
        self.with(|sim| {
            *sim = MobSim::new(world);
            // See `MobSim::set_next_id`'s own doc comment: id `1` collides
            // with `LOCAL_PLAYER_ENTITY_ID` on the wire.
            sim.set_next_id(1000);
            // Exactly `mob_count`, including zero — see [`seed_demo_mobs`].
            seed_demo_mobs(sim, center_x, center_z, mob_count);
            // Place the `SPAWN` stage's proposed animals as real mobs,
            // re-validated against the per-species placement rule
            // and this world's own light through the exact gate the
            // tick-driven cycle uses — see
            // `NaturalSpawner::validate_generation_spawns`'s doc for why this
            // reuses rather than re-implements it.
            if !pending_generation_spawns.is_empty() {
                let mut spawner = crate::natural_spawn::NaturalSpawner::new(
                    crate::worldgen_data::bundled_biome_spawners().clone(),
                    0,
                )
                .with_world_seed(crate::worldgen_data::active_world_seed());
                spawner.begin_cycle(std::sync::Arc::new(world.clone()), 0, Vec::new());
                for candidate in spawner.validate_generation_spawns(pending_generation_spawns) {
                    let mob = sim.spawn_species(candidate.entity_type, candidate.pos);
                    mob.set_category(MobCategory::Creature)
                        .set_persistent(MobCategory::Creature.is_persistent());
                }
            }
        });
    }

    /// Runs `f` against the locked sim, returning its result — the same
    /// funnel-every-access shape [`crate::BlockEntityHandle::with`]
    /// established, for the identical "no caller can forget to handle a
    /// poisoned lock inconsistently" reason.
    pub fn with<R>(&self, f: impl FnOnce(&mut MobSim<'static>) -> R) -> R {
        let _order = crate::lock_order::acquire(crate::lock_order::LockClass::Mobs);
        let mut guard = self.0.lock().expect("mob sim lock poisoned");
        f(&mut guard)
    }
}

impl EntitySource for MobHandle {
    /// A `MobHandle` is a legitimate [`EntitySource`] all on its own — no
    /// separate [`LiveMobSource`] cache required — for any caller that mutates
    /// the sim directly and does not also need a background tick loop
    /// ([`crate::tick::run_tick_loop`]) republishing it on a
    /// timer. Production (`IntegratedServer::open_in_memory_with_mobs`) still layers
    /// [`LiveMobSource`] on top so the tick loop's own AI motion reaches the
    /// wire on its own cadence; a test that only cares about a hand-placed,
    /// unticked mob (e.g. an attack test) can use the handle directly instead.
    fn snapshots(&self) -> Vec<EntitySnapshot> {
        self.with(|sim| sim.snapshots())
    }

    fn boss_bars(&self) -> Vec<crate::protocol::BossBarSnapshot> {
        self.with(|sim| sim.boss_bars())
    }
}

