//! World persistence: a [`ChunkSource`] backed by Anvil region files on disk
//! with the restart and registry invariants documented below.
//!
//! # What it is
//!
//! This source lets a singleplayer world survive quitting. It reads and writes
//! Anvil region files while preserving block entities, tickets, and registry
//! data for the next load.
//!
//! # Where this intercepts, and why exactly there
//!
//! The chunk path is three layers deep, and persistence goes in the middle:
//!
//! ```text
//! ChunkStore            bounded LRU cache, 512 columns, lossless eviction
//!   └─ RegionChunkSource   <-- here: disk load, edit retention, dirty set
//!        └─ OverworldChunkSource -> the generator
//! ```
//!
//! **Below [`crate::chunk_store::ChunkStore`]**, because the store's whole
//! bound depends on eviction being lossless, and eviction is lossless only
//! because the layer *underneath* it retains every edit permanently (see that
//! module's "eviction is lossless" note). Persistence above the store would
//! make dropping a cache entry drop a block. Persistence *is* now that
//! retaining layer.
//!
//! **Above [`crate::chunk::OverworldChunkSource`]**, because a loaded column
//! must win over a generated one. This is the trap in the layering and it is
//! worth stating plainly: this type deliberately **does not forward
//! `set_block` to its inner source**. Forwarding looks obviously right — it is
//! what `ChunkStore` does — but `OverworldChunkSource::set_block` seeds its
//! edit map by *generating* the column first, so editing a chunk that exists
//! on disk would silently resurrect fresh worldgen terrain underneath the
//! edit and discard everything the player built. The edit map lives here
//! instead, seeded from [`Self::column`], which consults disk first.
//!
//! # What gets saved
//!
//! The dirty set, and only the dirty set. That is not merely an optimisation:
//! `docs/plans/chunk-lifecycle.md`'s U3 store holds up to 512 columns, and a
//! save proportional to *residency* rather than to *mutation* would write
//! ~100 MiB every autosave for a player standing still. A tick that mutates
//! nothing writes nothing, and that is asserted as a **count**, not a
//! duration, in `tests/world_persistence_round_trip.rs`.
//!
//! Note the world is now genuinely mutated by more than the player: random
//! ticks, and since `ChunkWorld::block_cues`/`pending_grazes` the mob sim too.
//! Everything that mutates does so through [`ChunkSource::set_block`], which
//! is the single choke point this module hooks — so those mutations are
//! carried without any of `tick.rs`, `mobs.rs` or `server.rs` being touched.
//!
//! # Not blocking the tick
//!
//! The world-open stall (10.86 s → 75.6 ms, `docs/world-open-latency.md`) was
//! the last large performance defect here, and a synchronous full-region write
//! on the tick thread would be the same class of bug. So:
//!
//! - [`WorldSaveHandle`] is a cheap `Arc` clone that carries **no** reference
//!   to the generator or the store, so a save can run entirely off-thread.
//! - [`crate::integrated`] drives it from `tokio::task::spawn_blocking`, not
//!   from `tick::run_tick_loop`. That also sidesteps `MobSim` holding
//!   `world: &'w ChunkWorld` immutably — the save path never needs `&mut`
//!   anything the tick owns, because it reads its own retained columns.
//! - Marking a chunk dirty, which *is* on the mutation path, is a `HashSet`
//!   insert behind a `Mutex` and nothing else. No I/O, no encoding.
//! - An unload is only a bounded hand-off into a durable-save ledger. The
//!   ledger's typed token is acknowledged after a successful region rewrite;
//!   merely queueing the unload cannot release the authoritative edit.
//!
//! # How to change it, and the gotchas
//!
//! - **A region file is rewritten whole.** `lodestone_anvil::region` builds a
//!   complete `.mca` in one pass and has no incremental single-chunk update.
//!   [`WorldSaveHandle::save`] therefore reads
//!   the existing file back and re-emits untouched chunks **as their original
//!   compressed bytes**, without decoding them — so the cost of saving one
//!   chunk in a full region is a sector copy, not 1,024 NBT round trips.
//! - **Oversized (`.mcc`-externalised) chunks are the exception** to that
//!   pass-through: their bytes live in a sibling file, so they are resolved,
//!   recompressed and allowed to re-externalise. Rare by construction (256
//!   sectors, 1 MiB compressed).
//! - **The write is atomic per region**: a temp file in the same directory,
//!   then `rename`. A half-written `.mca` is indistinguishable from a corrupt
//!   one and would cost the player the whole region.
//! - **`min_y`/`height` must match the world the columns came from.** They are
//!   passed in rather than read from `yPos` for the reason `chunk_nbt`
//!   documents: vanilla writes light-only sections past both ends.
//!
//! # Configuration
//!
//! The world directory and the [`Dimension`] being persisted, both passed to
//! [`RegionChunkSource::new`]. Region files land in
//! `<world>/dimensions/minecraft/<dimension>/region/r.<rx>.<rz>.mca` —
//! `<dimension>` from [`Dimension::dir_name`] — which is 26.2's real layout
//! (verified against `.cache/mc/survival/world`, **not** the pre-1.21
//! `<world>/region/`; that snapshot has a real `dimensions/minecraft/overworld/`
//! directory too, so the overworld is not a special case here). Chunks are
//! written with [`CompressionScheme::Zlib`], the default region compression.
//!
//! # Dependencies
//!
//! `lodestone-anvil` for the container, [`crate::chunk_nbt`] for the schema,
//! and `std::fs`. Target-gated to non-wasm by `lib.rs` — a browser world has
//! no filesystem.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use lodestone_anvil::CompressionScheme;
use lodestone_anvil::region::{ChunkToWrite, RegionFile, build_region, region_and_local};
use lodestone_core::{Reader, Writer, read_named_nbt, write_named_nbt};
use lodestone_model::BlockPos;

use crate::block_entities::BlockEntityHandle;
use crate::chunk::{ChunkColumn, ChunkSource};
use crate::chunk_nbt::{self, ChunkExtras};
use crate::dimension::Dimension;

/// Zlib is the compression scheme used by every captured region file this
/// repository reads.
const SCHEME: CompressionScheme = CompressionScheme::Zlib;

/// The most independent physical region rewrites one save worker may execute
/// at once.
///
/// This is deliberately a small fixed bound instead of a per-save fan-out:
/// autosave already runs on Tokio's blocking pool, and an unbounded dirty set
/// must not turn one world into an unbounded source of native threads. Region
/// assignments are still selected and their results consumed in canonical
/// order; this limit affects only how many independent files may be in flight.
const MAX_CONCURRENT_REGION_SAVES: usize = 2;

/// What can go wrong saving or loading a world.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A filesystem operation failed.
    #[error("world io error at {path}: {source}")]
    Io {
        /// The path being operated on.
        path: PathBuf,
        /// The underlying error.
        #[source]
        source: std::io::Error,
    },
    /// The region container rejected the bytes, or could not build them.
    #[error("region container error: {0}")]
    Anvil(#[source] lodestone_anvil::Error),
    /// The NBT codec failed.
    #[error("nbt error: {0}")]
    Nbt(#[source] lodestone_core::Error),
    /// The chunk schema rejected a tree.
    #[error("chunk schema error: {0}")]
    Schema(#[source] chunk_nbt::Error),
}

fn io(path: &Path) -> impl FnOnce(std::io::Error) -> Error + '_ {
    move |source| Error::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// What [`resolve_world_seed`] found: the seed the world will actually
/// generate with, and whether this call created the world's metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedSeed {
    /// The seed to hand [`crate::overworld_chunk_source`]. For an existing
    /// world this is the **stored** seed, not the requested one.
    pub seed: i64,
    /// `true` when no settings file existed and one was written with the
    /// requested seed — i.e. this open created the world.
    pub created: bool,
}

/// The seed `world_dir` must generate with, writing the world's metadata on
/// first open.
///
/// # Why this exists, and why it is not optional
///
/// The persistence behavior makes
/// blocks survive a restart but left the seed unstored, and the two together
/// are worse than neither: chunks the player *had* visited come back from disk
/// while chunks they had not are regenerated from a **different** seed, so the
/// world is discontinuous exactly at the edge of where they explored. A
/// blocks-only gate structurally cannot see it, because every block such a gate
/// checks is one that was saved. The stored generator identity must therefore
/// be validated before loading any chunks.
///
/// # The stored seed always wins
///
/// `requested` is a **creation** parameter, not an open parameter. Once a world
/// exists, its own seed is authoritative and `requested` is ignored — the only
/// rule under which an existing world stays the world it was. Callers that want
/// to know whether their seed was honoured read
/// [`ResolvedSeed::created`].
///
/// # Where the seed lives (it is *not* `level.dat`)
///
/// See [`lodestone_anvil::world_gen_settings`]: 26.2 moved world-gen settings
/// out of `level.dat` into `<world>/data/minecraft/world_gen_settings.dat`, and
/// a 26.2 `level.dat` contains no seed field at all. Vanilla's own behaviour
/// when that file is missing or unreadable is to fall back to
/// `WorldOptions.defaultWithRandomSeed()` — precisely the bug above — which is
/// why an unreadable-but-present file here is an **error** rather than a
/// silent re-roll.
///
/// # Errors
///
/// [`Error::Anvil`] if a settings file exists but cannot be decoded or carries
/// no seed, or if a new one cannot be written. A *missing* file is not an
/// error: that is every world's first open.
pub fn resolve_world_seed(world_dir: &Path, requested: i64) -> Result<ResolvedSeed, Error> {
    let path = lodestone_anvil::world_gen_settings::path_in(world_dir);
    if path.exists() {
        let settings =
            lodestone_anvil::world_gen_settings::read_from_file(&path).map_err(Error::Anvil)?;
        let seed = settings.seed().map_err(Error::Anvil)?;
        return Ok(ResolvedSeed {
            seed,
            created: false,
        });
    }
    let settings = lodestone_anvil::world_gen_settings::WorldGenSettings::from_seed(requested);
    lodestone_anvil::world_gen_settings::write_to_file(&settings, &path).map_err(Error::Anvil)?;
    Ok(ResolvedSeed {
        seed: requested,
        created: true,
    })
}

/// The world's `level.dat`, opened once per session and stamped on every save.
///
/// # Why a world needs one at all
///
/// Region files alone are not a world. **Vanilla will not open a directory
/// with no `level.dat`** — `LevelStorageSource` reads it before anything else
/// and a missing one is not a world it can list, let alone load. Until this
/// existed, a Lodestone save was a folder of `.mca` files that only Lodestone
/// could make sense of, which is a strange thing for a format whose entire
/// value is that it is *the* format.
///
/// # What it stores, and the two things it does not
///
/// See [`lodestone_anvil::level_dat`] for the measured 14-field schema. The
/// two traps, both of which are easy to miss:
///
/// - **The seed is not here.** It lives in `world_gen_settings.dat`, resolved
///   separately by [`resolve_world_seed`]. Do not add it.
/// - **`Time` is the world's total age, not the time of day.** The sky clock
///   is `world_clocks.dat`, a file 26.2 keeps beside this one and nothing here
///   writes yet.
///
/// # Why the tick base lives here rather than in `TickClock`
///
/// [`crate::tick::TickClock`] counts **this session's** ticks and is right to:
/// its `tick_count` is what `mspt` and overrun accounting are measured
/// against, and a clock pre-loaded with a previous session's total would make
/// every one of those numbers meaningless. So the persisted total is
/// `base_ticks + clock.tick_count()`, with the base captured here at open.
/// That also keeps the whole feature inside this module — no other file's
/// notion of a tick changes.
#[derive(Debug)]
pub struct LevelDatHandle {
    path: PathBuf,
    level: Mutex<lodestone_anvil::level_dat::LevelDat>,
    /// Ticks this world had already run before the current session opened.
    base_ticks: i64,
    /// `true` when this open created the file — i.e. a brand-new world.
    created: bool,
    writes: AtomicU64,
}

impl LevelDatHandle {
    /// Reads `<world_dir>/level.dat`, or writes a fresh one if the world is
    /// new.
    ///
    /// The world's name is taken from the directory's own file name, so a
    /// caller that already chose `saves/<name>` does not have to say it twice.
    ///
    /// # Errors
    ///
    /// [`Error::Anvil`] if a `level.dat` exists but cannot be decoded, or if a
    /// new one cannot be written. An existing-but-unreadable file is an error
    /// rather than a silent overwrite for the same reason
    /// [`resolve_world_seed`] treats one that way: quietly replacing a world's
    /// metadata is how a world stops being the world it was.
    pub fn open_or_create(
        world_dir: &Path,
        spawn: &lodestone_anvil::level_dat::Spawn,
        game_type: i32,
    ) -> Result<Self, Error> {
        let path = lodestone_anvil::level_dat::path_in(world_dir);
        if path.exists() {
            let level = lodestone_anvil::level_dat::read_from_file(&path).map_err(Error::Anvil)?;
            let base_ticks = level.time().unwrap_or(0);
            return Ok(Self {
                path,
                level: Mutex::new(level),
                base_ticks,
                created: false,
                writes: AtomicU64::new(0),
            });
        }
        let name = world_dir
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("world");
        let level = lodestone_anvil::level_dat::LevelDat::for_new_world(name, spawn, game_type);
        lodestone_anvil::level_dat::write_to_file(&level, &path).map_err(Error::Anvil)?;
        Ok(Self {
            path,
            level: Mutex::new(level),
            base_ticks: 0,
            created: true,
            writes: AtomicU64::new(1),
        })
    }

    /// Ticks this world had run before the current session opened.
    #[must_use]
    pub fn base_ticks(&self) -> i64 {
        self.base_ticks
    }

    /// `true` when this open created the world.
    #[must_use]
    pub fn created(&self) -> bool {
        self.created
    }

    /// How many times this handle has written the file, including the write
    /// that created it. A **count**, for the same reason
    /// [`PersistenceStats`] are counts.
    #[must_use]
    pub fn writes(&self) -> u64 {
        self.writes.load(Ordering::Relaxed)
    }

    /// The world's `Data` compound as read at open — what
    /// [`crate::world_state::WorldStateHandle::load_level_data`] reads its rules,
    /// difficulty and clock back out of.
    #[must_use]
    pub fn data(&self) -> Option<lodestone_core::Nbt> {
        self.level
            .lock()
            .expect("level.dat lock poisoned")
            .data()
            .cloned()
    }

    /// Stamps `Time` and `LastPlayed`, merges `extra` into the `Data` compound, and
    /// writes the file back.
    ///
    /// `extra` is [`crate::world_state::WorldStateHandle::level_data_fields`] —
    /// game rules, difficulty and the day clock, under vanilla's own field names,
    /// so a world this server writes is readable by a real 26.2 server.
    ///
    /// `session_ticks` is [`crate::tick::TickClock::tick_count`] — this
    /// session's own ticks, which this adds to the base captured at open.
    ///
    /// Blocking, like [`WorldSaveHandle::save`]: it is called from the same
    /// `spawn_blocking` the region write uses, never from the tick loop.
    ///
    /// # Errors
    ///
    /// [`Error::Anvil`] if the file cannot be encoded or written.
    pub fn write(
        &self,
        session_ticks: u64,
        extra: &[(String, lodestone_core::Nbt)],
    ) -> Result<(), Error> {
        let mut level = self.level.lock().expect("level.dat lock poisoned");
        for (field, value) in extra {
            level.set_data_field(field, value.clone()).map_err(Error::Anvil)?;
        }
        let total = self
            .base_ticks
            .saturating_add(i64::try_from(session_ticks).unwrap_or(i64::MAX));
        level.set_time(total).map_err(Error::Anvil)?;
        level
            .set_last_played(now_millis())
            .map_err(Error::Anvil)?;
        lodestone_anvil::level_dat::write_to_file(&level, &self.path).map_err(Error::Anvil)?;
        self.writes.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}

/// Wall-clock milliseconds since the epoch, saturating rather than panicking
/// on a system clock set before 1970.
fn now_millis() -> i64 {
    i64::try_from(lodestone_time::epoch_duration().as_millis()).unwrap_or(i64::MAX)
}

/// Counters for what the save/load path actually did.
///
/// Deliberately counters and not timings: a duration measured while five other
/// agents are building is attributed to the wrong cause, and two sequential
/// durations are not protected by being expressed as a ratio. These are
/// store-lifetime accumulators, so read them as deltas.
#[derive(Debug, Default)]
pub struct PersistenceStats {
    /// Columns successfully decoded from disk instead of generated.
    pub loaded_from_disk: AtomicU64,
    /// Columns handed to the generator because disk had nothing.
    pub generated: AtomicU64,
    /// Columns released from the edit map after being written to disk, because
    /// the cache above evicted them. The counter that says unload-driven
    /// saving is actually reclaiming anything.
    pub unloaded: AtomicU64,
    /// Chunk columns encoded and written across all saves.
    pub columns_written: AtomicU64,
    /// Region files rewritten across all saves.
    pub regions_written: AtomicU64,
    /// Calls to [`WorldSaveHandle::save`] that found nothing dirty and did no
    /// filesystem work at all.
    pub empty_saves: AtomicU64,
    /// Block entities restored from disk into the live registry across all
    /// loads. A nonzero value confirms that persisted block entities reached
    /// the live registry.
    pub block_entities_loaded: AtomicU64,
    /// Block entities encoded into a chunk across all saves.
    pub block_entities_written: AtomicU64,
    /// Pending block and fluid ticks rescheduled from disk across all loads.
    pub scheduled_ticks_loaded: AtomicU64,
    /// Pending block and fluid ticks encoded into a chunk across all saves.
    pub scheduled_ticks_written: AtomicU64,
    /// `std::fs::read` calls against a `.mca` file across all loads — one per
    /// [`RegionCache`] miss, **not** one per column. Compare against
    /// [`Self::loaded_from_disk`]: the value counts cache misses, so LRU
    /// eviction and save invalidation can cause the same region file to be read
    /// more than once during a session.
    pub region_files_read: AtomicU64,
    /// Bytes read from disk across all [`Self::region_files_read`] reads.
    /// The magnitude companion to that counter — see its own doc for why a
    /// byte total is the one the OS page cache cannot make look healthy.
    pub region_bytes_read: AtomicU64,
}

/// One unload hand-off that is valid only for one coordinate and one source
/// transition.
///
/// The generation prevents a delayed acknowledgement from an earlier cache
/// eviction from completing a later eviction of the same chunk. Keeping the
/// coordinate in the token also makes a cross-coordinate acknowledgement fail
/// closed instead of relying on the caller to pair two parallel batches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DurableUnloadToken {
    generation: u64,
    chunk: (i32, i32),
}

/// The only acknowledgement phases a durable unload may occupy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DurableUnloadPhase {
    /// `ChunkSource::unload` has queued the hand-off, but no successful save
    /// has acknowledged it yet.
    Queued,
    /// The save that owns this token completed its required disk writes.
    Durable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DurableUnloadEntry {
    token: DurableUnloadToken,
    phase: DurableUnloadPhase,
}

/// The bounded durable-save acknowledgement ledger for one world.
///
/// There is at most one current entry per chunk. A repeated unload supersedes
/// its predecessor, making the predecessor stale by construction instead of
/// retaining a process-wide acknowledgement history.
#[derive(Debug, Default)]
struct DurableUnloadLedger {
    next_generation: u64,
    pending: HashMap<(i32, i32), DurableUnloadEntry>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DurableUnloadAckResult {
    /// The queued token advanced to the durable phase.
    Accepted,
    /// The same token was already acknowledged.
    Duplicate,
    /// The token is absent, superseded, or names a different chunk.
    Stale,
}

impl DurableUnloadLedger {
    fn enqueue(&mut self, chunk: (i32, i32)) -> DurableUnloadToken {
        self.next_generation = self
            .next_generation
            .checked_add(1)
            .expect("durable unload generation space exhausted");
        let token = DurableUnloadToken {
            generation: self.next_generation,
            chunk,
        };
        self.pending.insert(
            chunk,
            DurableUnloadEntry {
                token,
                phase: DurableUnloadPhase::Queued,
            },
        );
        token
    }

    fn tokens(&self) -> Vec<DurableUnloadToken> {
        let mut tokens: Vec<_> = self.pending.values().map(|entry| entry.token).collect();
        tokens.sort_unstable_by_key(|token| token.chunk);
        tokens
    }

    fn durable_tokens(&self) -> Vec<DurableUnloadToken> {
        let mut tokens: Vec<_> = self
            .pending
            .values()
            .filter(|entry| entry.phase == DurableUnloadPhase::Durable)
            .map(|entry| entry.token)
            .collect();
        tokens.sort_unstable_by_key(|token| token.chunk);
        tokens
    }

    fn acknowledge(&mut self, token: DurableUnloadToken) -> DurableUnloadAckResult {
        let Some(entry) = self.pending.get_mut(&token.chunk) else {
            return DurableUnloadAckResult::Stale;
        };
        if entry.token != token {
            return DurableUnloadAckResult::Stale;
        }
        match entry.phase {
            DurableUnloadPhase::Queued => {
                entry.phase = DurableUnloadPhase::Durable;
                DurableUnloadAckResult::Accepted
            }
            DurableUnloadPhase::Durable => DurableUnloadAckResult::Duplicate,
        }
    }

    fn release(&mut self, token: DurableUnloadToken) -> bool {
        let Some(entry) = self.pending.get(&token.chunk) else {
            return false;
        };
        if entry.token != token || entry.phase != DurableUnloadPhase::Durable {
            return false;
        }
        self.pending.remove(&token.chunk).is_some()
    }
}

/// The state a save needs, shared between the source and its save handle.
#[derive(Debug)]
struct WorldState {
    /// `<world>/dimensions/minecraft/<dimension>/region` — see
    /// [`RegionChunkSource::new`]'s `dimension` parameter.
    region_dir: PathBuf,
    /// `<world>/players/data`, or `None` if it could not be created (a
    /// non-persistable player store). Handed out through [`ChunkSource::world_registries`].
    player_data: Option<crate::player_data::PlayerDataStore>,
    min_y: i32,
    height: i32,
    /// The **authoritative** columns: everything a `set_block` has touched.
    /// Plays exactly the role `OverworldChunkSource::edits` plays for a
    /// generator-only world, which is what keeps `ChunkStore`'s lossless
    /// eviction true.
    edits: Mutex<HashMap<(i32, i32), ChunkColumn>>,
    /// Chunks changed since the last successful save.
    dirty: Mutex<HashSet<(i32, i32)>>,
    /// Chunks the cache above has evicted. An entry stays here from queueing
    /// through its durable acknowledgement and until the authoritative edit is
    /// actually released. See [`WorldSaveHandle::save`]'s unload sweep.
    pending_unload: Mutex<DurableUnloadLedger>,
    /// The world's live block entities — the same registry the tick loop and
    /// the connection task hold, not a copy.
    ///
    /// Reading it non-destructively is required here:
    /// [`crate::BlockEntityRegistry`]'s only other routes are `remove` and
    /// `tick_all`, so saving through them would have desynchronised the
    /// running server from what landed on disk.
    block_entities: BlockEntityHandle,
    /// The world's pending block and fluid ticks — the same queues the tick
    /// loop drains, not a copy. See [`ScheduledTickHandle`].
    scheduled: ScheduledTickHandle,
    stats: PersistenceStats,
    /// Already-parsed region files, keyed by `(rx, rz)`. See [`RegionCache`]'s
    /// own doc — the cache is the single owner of this behavior.
    regions: Mutex<RegionCache>,
}

/// How many distinct region files [`RegionCache`] keeps parsed at once.
///
/// A join at the shipped default (`render_distance = 8`) spans exactly 4
/// distinct files (the count), so this is well above the
/// working set of a session that is not actively crossing region
/// boundaries, while still bounding memory for a long session that roams
/// across many — each entry holds one whole `.mca`'s bytes, so this is not
/// a "cache everything forever" design.
const OPEN_REGION_CAPACITY: usize = 16;

/// A small LRU of already-parsed [`RegionFile`]s.
///
/// # What it is
///
/// `RegionChunkSource::load` caches parsed region files while `ChunkStore`
/// streams hundreds of columns out of the same handful of region files (361
/// column loads against 4 distinct files on a cold join). As long as an entry
/// remains cached, later columns from the same file avoid disk I/O and the
/// header-parse/sanitation pass. LRU eviction and save invalidation can require
/// a subsequent load to parse that file again.
///
/// # How to change it
///
/// - **Invalidate, don't mutate, on write.** [`RegionChunkSource::save_region`]
///   calls [`Self::invalidate`] after it renames a new `.mca` into place,
///   rather than patching the cached entry in place — the file on disk and
///   the parsed struct must never be allowed to diverge silently.
/// - **Recency order, not a hash map alone**, because eviction needs "least
///   recently used", and [`OPEN_REGION_CAPACITY`] is small enough that a
///   linear scan over it is cheaper than a real LRU's bookkeeping.
/// - The entries are `Arc<RegionFile>` so a hit clones a pointer, not the
///   file's bytes.
#[derive(Debug, Default)]
struct RegionCache {
    /// Most-recently-used entry at the front.
    entries: VecDeque<((i32, i32), Arc<RegionFile>)>,
}

impl RegionCache {
    fn get(&mut self, key: (i32, i32)) -> Option<Arc<RegionFile>> {
        let pos = self.entries.iter().position(|(k, _)| *k == key)?;
        let entry = self.entries.remove(pos)?;
        let file = Arc::clone(&entry.1);
        self.entries.push_front(entry);
        Some(file)
    }

    fn insert(&mut self, key: (i32, i32), file: Arc<RegionFile>) {
        self.entries.retain(|(k, _)| *k != key);
        self.entries.push_front((key, file));
        while self.entries.len() > OPEN_REGION_CAPACITY {
            self.entries.pop_back();
        }
    }

    /// Drops a cached entry so the next [`Self::get`] misses and a fresh read
    /// picks up whatever was just written. Called after every region-file
    /// write; never patched in place (see the struct doc).
    fn invalidate(&mut self, key: (i32, i32)) {
        self.entries.retain(|(k, _)| *k != key);
    }
}

/// The shared scheduled-tick queues, re-exported so every call site that
/// spells `crate::region_source::ScheduledTickHandle` keeps working.
///
/// The type itself lives in [`crate::scheduled_tick`] and is portable — see its
/// own doc for why. What stays here is the half that speaks the Anvil save
/// format: the `impl` block below, which is where `chunk_nbt::SavedTick` is
/// read and written.
pub use crate::scheduled_tick::{ScheduledTickHandle, ScheduledTickQueues};

/// The Anvil-save half of [`ScheduledTickHandle`], in the module that owns the
/// save format rather than in the module that owns the type. An inherent `impl`
/// may live in any module of the crate, and these three methods are the only
/// ones that name [`chunk_nbt::SavedTick`] — so this is the boundary that keeps
/// the handle itself free of `std::fs`.
impl ScheduledTickHandle {
    /// Every pending tick in chunk `(cx, cz)`, as [`chunk_nbt::SavedTick`]s
    /// with delays relative to [`game_tick`](Self::game_tick).
    ///
    /// The conversion is vanilla's, inverted: `SavedTick::unpack` loads with
    /// `trigger_tick = current_tick + delay`, so saving is `delay =
    /// trigger_tick - game_tick`. Computed as a **signed** subtraction on
    /// purpose — a tick already overdue at save time yields a negative delay,
    /// which is not an edge case but 1,584 of the 133,051 entries measured in
    /// real vanilla worlds.
    fn saved_ticks_for(&self, cx: i32, cz: i32) -> (Vec<chunk_nbt::SavedTick>, Vec<chunk_nbt::SavedTick>) {
        let now = i64::try_from(self.game_tick()).unwrap_or(i64::MAX);
        let convert = |tick: &crate::scheduled_tick::ScheduledTick<String>| chunk_nbt::SavedTick {
            pos: tick.pos,
            kind: tick.kind.clone(),
            delay: i32::try_from(
                i64::try_from(tick.trigger_tick).unwrap_or(i64::MAX) - now,
            )
            .unwrap_or(i32::MAX),
            priority: tick.priority,
        };
        self.with(|queues| {
            let belongs_to_column = |tick: &crate::scheduled_tick::ScheduledTick<String>| {
                (tick.pos.0.div_euclid(16), tick.pos.2.div_euclid(16)) == (cx, cz)
            };
            (
                queues
                    .block
                    .iter()
                    .filter(|tick| belongs_to_column(tick))
                    .map(convert)
                    .collect(),
                queues
                    .fluid
                    .iter()
                    .filter(|tick| belongs_to_column(tick))
                    .map(convert)
                    .collect(),
            )
        })
    }

    /// The set of chunks holding at least one pending tick in either queue.
    fn chunks_with_pending_ticks(&self) -> HashSet<(i32, i32)> {
        self.with(|queues| {
            queues
                .block
                .iter()
                .map(|tick| (tick.pos.0.div_euclid(16), tick.pos.2.div_euclid(16)))
                .chain(
                    queues
                        .fluid
                        .iter()
                        .map(|tick| (tick.pos.0.div_euclid(16), tick.pos.2.div_euclid(16))),
                )
                .collect()
        })
    }

    /// Hands a loaded chunk's saved ticks to the queues, rebasing each delay
    /// onto the current game tick. Returns how many were handed over.
    ///
    /// A delay so negative that `game_tick + delay` would go below zero
    /// saturates to `0`, i.e. "due immediately" — which is what an overdue tick
    /// means, and the only reading that cannot panic or wrap. `schedule`'s own
    /// `(pos, kind)` dedup then silently drops anything already pending, which
    /// is what makes reloading a chunk idempotent.
    ///
    /// The dedup now happens at *merge* time rather than here (see
    /// [`ScheduledTickHandle::stage`]), so the returned count is "read off disk
    /// and handed over", not "newly present in the queue". The two differ only
    /// when the same `(pos, kind)` is already pending, which reloading a chunk
    /// still absorbs — it just is not subtracted from this counter.
    fn restore(&self, block: &[chunk_nbt::SavedTick], fluid: &[chunk_nbt::SavedTick]) -> u64 {
        if block.is_empty() && fluid.is_empty() {
            return 0;
        }
        let now = i64::try_from(self.game_tick()).unwrap_or(i64::MAX);
        // **Staged, not scheduled directly**, and this is load-bearing rather
        // than an optimisation. The caller is `RegionChunkSource::load`, which
        // the tick thread reaches through `world.column`/`block_state`/
        // `set_block` from *inside* `tick::run_tick_loop`'s own
        // `ScheduledTickHandle::with` region — so taking the queue lock here is
        // a self-deadlock on a non-reentrant `std::sync::Mutex`. It fired the
        // first time a world tick touched a column that exists on disk, which
        // is why a freshly generated world was fine and every *saved* world
        // wedged mid-join with no error. See `ScheduledTickHandle::stage`.
        //
        // The rebase onto `now` still happens here, because this is the half
        // that knows the delays are relative; `stage` carries absolute trigger
        // ticks, so the deferral cannot move when a restored tick fires.
        let staged: Vec<crate::scheduled_tick::StagedTick> = block
            .iter()
            .map(|t| (t, false))
            .chain(fluid.iter().map(|t| (t, true)))
            .map(|(saved, is_fluid)| crate::scheduled_tick::StagedTick {
                pos: saved.pos,
                kind: saved.kind.clone(),
                trigger_tick: (now + i64::from(saved.delay)).max(0) as u64,
                priority: saved.priority,
                fluid: is_fluid,
                insertion_order: None,
            })
            .collect();
        self.stage(staged)
    }
}

/// Refuses to open a world whose stored chunks this build cannot read.
///
/// # Why the check is here and not in [`RegionChunkSource::load`]
///
/// `load` returns `Option<LoadedChunk>` and its `None` means "never saved", which
/// `ChunkSource::column` answers by **generating fresh terrain**. So a per-chunk
/// version check could only report a mismatch by regenerating the chunk — and the
/// regenerated column then enters the edit map on the next `set_block` and is
/// written straight over the original. The per-chunk position is structurally
/// unable to refuse; it can only destroy.
///
/// At open, refusing is total and costs nothing: the constructor returns `Err`,
/// no task has spawned, and not one byte has been written. There is no upgrade
/// path in this repo (see
/// [`lodestone_anvil::require_supported_data_version`]), so this is the whole of
/// the answer and it is deliberate rather than unfinished.
///
/// # What it samples
///
/// The **first** chunk found in the **first** region file that has one. A world
/// is written by one game version, so one chunk answers the question; walking all
/// 89 region files of a real world at every open would put a multi-second scan on
/// the world-open path, which this bounded sample avoids
/// (`docs/world-open-latency.md`). A world with no region directory, no
/// files, or files with no chunks — is accepted, because there is nothing to
/// mis-read.
///
/// # Errors
///
/// [`Error::Anvil`] wrapping
/// [`lodestone_anvil::Error::UnsupportedDataVersion`] when the sampled chunk was
/// written by another game version. An unreadable or unparseable region file is
/// **not** an error here: that is the existing read path's tolerance (a corrupt
/// file is treated as absent), and turning it into an open-time refusal would
/// strand a world for a reason unrelated to versioning.
fn refuse_unreadable_world(region_dir: &Path) -> Result<(), Error> {
    let Ok(entries) = std::fs::read_dir(region_dir) else {
        return Ok(());
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("mca") {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let Ok(region) = RegionFile::parse(&bytes) else {
            continue;
        };
        for local_z in 0..32u8 {
            for local_x in 0..32u8 {
                let Ok(Some(raw)) = region.read_chunk_nbt_bytes(local_x, local_z) else {
                    continue;
                };
                let mut reader = Reader::new(&raw);
                let Ok((_, nbt)) = read_named_nbt(&mut reader) else {
                    continue;
                };
                let found = match &nbt {
                    lodestone_core::Nbt::Compound(fields) => fields
                        .iter()
                        .find(|(name, _)| name == "DataVersion")
                        .and_then(|(_, value)| match value {
                            lodestone_core::Nbt::Int(v) => Some(*v),
                            _ => None,
                        }),
                    _ => None,
                };
                return lodestone_anvil::require_supported_data_version(found)
                    .map_err(Error::Anvil);
            }
        }
    }
    Ok(())
}

/// The chunk a block position belongs to.
///
/// `>> 4` rather than `/ 16`: an arithmetic shift floors, and truncating
/// division would map `x = -1` to chunk `0` instead of `-1`, putting every
/// block entity on the negative side of the origin in the wrong chunk. Same
/// arithmetic as `lodestone_anvil::region::region_and_local`'s `>> 5`, one
/// level down.
#[must_use]
fn chunk_of(pos: BlockPos) -> (i32, i32) {
    (pos.x >> 4, pos.z >> 4)
}

/// A [`ChunkSource`] that loads columns from Anvil region files and retains
/// every edit for saving. See the module docs for where it sits in the stack
/// and why.
#[derive(Debug)]
pub struct RegionChunkSource<S> {
    inner: Arc<S>,
    state: Arc<WorldState>,
}

/// Cloning yields another handle to the **same** world — same edit map, same
/// dirty set, same generator. That is what lets
/// [`crate::IntegratedServer::open_persistent_with_mobs`] hand the world to its
/// `ChunkStore` and still return a live handle to the caller, and it is why the
/// inner source is behind an `Arc` rather than owned outright.
impl<S> Clone for RegionChunkSource<S> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
            state: Arc::clone(&self.state),
        }
    }
}

impl<S: ChunkSource> RegionChunkSource<S> {
    /// Wraps `inner` with persistence rooted at `world_dir`'s `dimension`
    /// subdirectory.
    ///
    /// Creates the region directory eagerly so that a later save cannot fail
    /// for a reason the caller could have been told about at open time.
    pub fn new(
        inner: S,
        world_dir: &Path,
        dimension: Dimension,
        min_y: i32,
        height: i32,
    ) -> Result<Self, Error> {
        let region_dir = world_dir
            .join("dimensions")
            .join("minecraft")
            .join(dimension.dir_name())
            .join("region");
        std::fs::create_dir_all(&region_dir).map_err(io(&region_dir))?;
        // World readability is checked **here**, before any task spawns and
        // before any chunk is read. See `refuse_unreadable_world`'s doc comment for
        // why the check has to be at open rather than per chunk.
        refuse_unreadable_world(&region_dir)?;
        // The player-data store is created eagerly, and a failure is *not* fatal: a world whose
        // `players/data` cannot be created is still playable, it just cannot
        // persist a player — and refusing to open it would be a worse trade than
        // the terrain case, where refusing is the only thing that protects the
        // data. The warning is what stops that being silent.
        let player_data = match crate::player_data::PlayerDataStore::new(world_dir) {
            Ok(store) => Some(store),
            Err(err) => {
                tracing::warn!("player data will not persist for this world: {err}");
                None
            }
        };
        Ok(Self {
            inner: Arc::new(inner),
            state: Arc::new(WorldState {
                region_dir,
                player_data,
                min_y,
                height,
                edits: Mutex::new(HashMap::new()),
                dirty: Mutex::new(HashSet::new()),
                pending_unload: Mutex::new(DurableUnloadLedger::default()),
                block_entities: BlockEntityHandle::default(),
                scheduled: ScheduledTickHandle::default(),
                stats: PersistenceStats::default(),
                regions: Mutex::new(RegionCache::default()),
            }),
        })
    }

    /// The world's block-entity registry, for the server to tick and for
    /// connections to insert into on placement.
    ///
    /// **A persistent world's registry is created here, not by
    /// [`crate::IntegratedServer`]**, and that direction matters: the save
    /// path has to be able to read the registry, and a registry the server
    /// made privately is one persistence can never see — which is exactly the
    /// shape of the shared persistence state. Handing it *out* means there is only one,
    /// by construction.
    #[must_use]
    pub fn block_entities(&self) -> BlockEntityHandle {
        self.state.block_entities.clone()
    }

    /// The world's scheduled-tick queues, for the tick loop to drain and for
    /// the save path to read.
    ///
    /// Handed out from here for exactly the reason
    /// [`block_entities`](Self::block_entities) is: a queue the tick loop owns
    /// privately is a queue persistence can never see, and that was the whole
    /// of the persistence handoff.
    #[must_use]
    pub fn scheduled_ticks(&self) -> ScheduledTickHandle {
        self.state.scheduled.clone()
    }

    /// How many columns the edit map is holding.
    ///
    /// This is the number unload-driven saving exists to bound: before it, the
    /// edit map only ever grew, so a long session in a heavily-built world made
    /// *this* the process's real memory ceiling rather than `ChunkStore`'s 512.
    /// A **count**, not a byte figure, for the reason [`PersistenceStats`]
    /// gives.
    #[must_use]
    pub fn retained_columns(&self) -> usize {
        self.state
            .edits
            .lock()
            .expect("world edit lock poisoned")
            .len()
    }

    /// A cheap handle that can save the world from any thread.
    #[must_use]
    pub fn save_handle(&self) -> WorldSaveHandle {
        WorldSaveHandle {
            state: Arc::clone(&self.state),
        }
    }

    /// Returns the parsed region file at `(rx, rz)`, from [`RegionCache`] on
    /// a hit or from disk on a miss. `None` only when the file is missing or
    /// fails to parse — both legal (see [`Self::load`]'s own doc).
    fn open_region(&self, rx: i32, rz: i32) -> Option<Arc<RegionFile>> {
        if let Some(cached) = self
            .state
            .regions
            .lock()
            .expect("region cache lock poisoned")
            .get((rx, rz))
        {
            return Some(cached);
        }

        let path = self.state.region_dir.join(format!("r.{rx}.{rz}.mca"));
        let bytes = std::fs::read(&path).ok()?;
        self.state
            .stats
            .region_files_read
            .fetch_add(1, Ordering::Relaxed);
        self.state
            .stats
            .region_bytes_read
            .fetch_add(bytes.len() as u64, Ordering::Relaxed);
        let region = Arc::new(RegionFile::parse_owned(bytes).ok()?);
        self.state
            .regions
            .lock()
            .expect("region cache lock poisoned")
            .insert((rx, rz), Arc::clone(&region));
        Some(region)
    }

    /// Reads the column at `(cx, cz)` off disk, or `None` if this world has
    /// never saved it.
    ///
    /// A missing or unparseable region file is `None`, not an error: vanilla
    /// itself treats "file doesn't exist yet" as a legal chunk-less region
    /// (see `lodestone_anvil::region::RegionFile::parse`), and a world's very
    /// first open has no files at all.
    ///
    /// Goes through [`RegionCache`] first: `ChunkStore` streams hundreds of
    /// columns out of a handful of region files, and a cache hit
    /// costs an `Arc` clone; only a genuine miss touches disk, and every
    /// touch is counted in [`PersistenceStats::region_files_read`] /
    /// [`PersistenceStats::region_bytes_read`], so the cache behavior is
    /// checked by counters rather than inferred.
    fn load(&self, cx: i32, cz: i32) -> Option<LoadedChunk> {
        let (rx, rz, local_x, local_z) = region_and_local(cx, cz);
        let region = self.open_region(rx, rz)?;
        let raw = region
            .read_chunk_nbt_bytes_resolving_external(
                local_x,
                local_z,
                cx,
                cz,
                &self.state.region_dir,
            )
            .ok()??;
        let mut reader = Reader::new(&raw);
        let (_, nbt) = read_named_nbt(&mut reader).ok()?;
        let mut column =
            chunk_nbt::column_from_nbt(&nbt, self.state.min_y, self.state.height).ok()?;
        let mut extras = chunk_nbt::extras_from_nbt(&nbt);
        // The column carries its own copy so `encode_chunk` can put them on the
        // wire. The chunk payload includes these entities so clients see the
        // same state as the tick-loop registry. The registry remains the
        // authority for saving, because a live furnace takes precedence over
        // the disk copy.
        column.set_block_entities(extras.block_entities.clone());
        // Repair any block-entity-owning state this saved chunk's own NBT
        // never recorded — see `ChunkColumn::missing_block_entity_states`'s
        // own doc for why that gap exists (`block_entity_for_item` only
        // covers the dozen types this crate simulates) and what it costs a
        // client: a skull, banner, jukebox, … with a correct state and no
        // record draws nothing at all until an unrelated later block update.
        // `Nbt::End` matches what a real save/load round trip already
        // produces for a block-entity type this crate does not otherwise
        // model (`chunk_nbt::block_entity_from_nbt`'s own catch-all arm).
        let missing = column.missing_block_entity_states(cx, cz, &extras.block_entities);
        if !missing.is_empty() {
            for (pos, type_name) in missing {
                extras.block_entities.push((
                    pos,
                    crate::block_entities::BlockEntity::Opaque {
                        id: type_name.to_owned(),
                        nbt: lodestone_core::Nbt::End,
                    },
                ));
            }
            column.set_block_entities(extras.block_entities.clone());
        }
        let restored = self.restore_block_entities(&extras);
        let ticks = self
            .state
            .scheduled
            .restore(&extras.block_ticks, &extras.fluid_ticks);
        self.state
            .stats
            .scheduled_ticks_loaded
            .fetch_add(ticks, Ordering::Relaxed);
        Some(LoadedChunk {
            column,
            holds_block_entities: restored > 0,
        })
    }

    /// Puts a loaded chunk's block entities back into the live registry,
    /// returning how many were inserted.
    ///
    /// **Absent-only.** A position the registry already holds is left alone,
    /// because the live value is by definition newer than the disk one: a
    /// column can be released from the edit map and re-loaded while its
    /// furnace has been ticking the whole time (the registry has no eviction),
    /// and overwriting it would rewind the world every time a chunk left the
    /// cache.
    ///
    /// Cheap enough for the caller's thread — which is the tick or serving
    /// thread — because it is a `Mutex` and a `HashMap` insert per entity, no
    /// I/O, and a chunk with any block entities at all is rare.
    fn restore_block_entities(&self, extras: &ChunkExtras) -> u64 {
        if extras.block_entities.is_empty() {
            return 0;
        }
        let restored = self.state.block_entities.with(|registry| {
            let mut count = 0u64;
            for (pos, entity) in &extras.block_entities {
                if registry.get(*pos).is_none() {
                    registry.insert(*pos, entity.clone());
                    count += 1;
                }
            }
            count
        });
        self.state
            .stats
            .block_entities_loaded
            .fetch_add(restored, Ordering::Relaxed);
        restored
    }
}

/// What [`RegionChunkSource::load`] found on disk.
struct LoadedChunk {
    column: ChunkColumn,
    /// Whether this chunk put anything into the block-entity registry — the
    /// cue for [`ChunkSource::column`] to retain it. See that method for why
    /// retention is required rather than an optimisation.
    holds_block_entities: bool,
}

impl<S: ChunkSource> ChunkSource for RegionChunkSource<S> {
    /// The one implementor that answers `Some`: this source *is* the world on
    /// disk, so these are the registries whose contents a save writes.
    fn world_registries(&self) -> Option<crate::chunk::WorldRegistries> {
        Some(crate::chunk::WorldRegistries {
            block_entities: self.block_entities(),
            scheduled: self.scheduled_ticks(),
            player_data: self.state.player_data.clone(),
        })
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
        {
            let edits = self.state.edits.lock().expect("world edit lock poisoned");
            if let Some(edited) = edits.get(&(cx, cz)) {
                return edited.clone();
            }
        }
        if let Some(loaded) = self.load(cx, cz) {
            self.state
                .stats
                .loaded_from_disk
                .fetch_add(1, Ordering::Relaxed);
            // **A chunk that holds block entities is retained in `edits` from
            // the moment it loads**, which is the one exception to "only
            // `set_block` populates the edit map".
            //
            // It has to be. A furnace's contents change through the container
            // menu, which never touches a block, so such a chunk can be
            // *stale on disk while nothing marks it dirty*. `save_region`
            // carries a chunk it has no edit entry for across as its original
            // compressed bytes — so without this, smelting into a furnace that
            // was loaded rather than placed this session would write the old
            // contents straight back over the new ones, silently.
            if loaded.holds_block_entities {
                let mut edits = self.state.edits.lock().expect("world edit lock poisoned");
                edits.entry((cx, cz)).or_insert_with(|| loaded.column.clone());
            }
            return loaded.column;
        }
        self.state.stats.generated.fetch_add(1, Ordering::Relaxed);
        self.inner.column_at(cx, cz, stage)
    }

    fn block_state(&self, x: i32, y: i32, z: i32) -> String {
        // Goes through `self.column()`, which consults `edits` and disk before
        // the inner source — so the answer reflects a `set_block` edit exactly
        // as a `column()` read would. The wrapper above this
        // (`crate::chunk_store::ChunkStore`) overrides this with the
        // one-cell read that avoids the regeneration.
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        self.column(cx, cz).block_state(lx, y, lz).to_string()
    }

    fn biome_state_at(&self, x: i32, y: i32, z: i32) -> String {
        // Same reasoning as `block_state` above: goes through `self.column()`
        // so a loaded/edited column's biome data is what answers, not a bare
        // regeneration from `self.inner`.
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        self.column(cx, cz).biome_state_at(lx, y, lz).to_string()
    }

    fn set_block(&self, x: i32, y: i32, z: i32, name: &str) {
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);

        // Seeded from `self.column`, which consults disk. Deliberately NOT
        // forwarded to `self.inner`: see the module doc — forwarding would
        // have `OverworldChunkSource` regenerate the column and discard
        // whatever was saved there.
        let seed = {
            let edits = self.state.edits.lock().expect("world edit lock poisoned");
            if edits.contains_key(&(cx, cz)) {
                None
            } else {
                drop(edits);
                Some(self.column(cx, cz))
            }
        };

        let mut edits = self.state.edits.lock().expect("world edit lock poisoned");
        let column = match seed {
            Some(fresh) => edits.entry((cx, cz)).or_insert(fresh),
            None => edits
                .get_mut(&(cx, cz))
                .expect("checked present above, and only this lock inserts"),
        };
        column.set_block(lx, y, lz, name);

        // Marked dirty **while still holding `edits`**, and that ordering is
        // load-bearing rather than incidental. The unload sweep in
        // `WorldSaveHandle::save` decides whether to drop a column by checking
        // that it is not dirty, and it takes these two locks in this same
        // order. If this released `edits` first, the sweep could observe a
        // column that has already been mutated but not yet marked dirty, and
        // drop the player's edit with no error anywhere.
        self.state
            .dirty
            .lock()
            .expect("world dirty lock poisoned")
            .insert((cx, cz));
        drop(edits);
    }

    /// The cache above has evicted this column, so the save path may release
    /// it once it is on disk.
    ///
    /// Nothing is dropped here: this runs on `ChunkStore`'s miss path, which is
    /// frequently the tick thread, so it is a bounded ledger insert and
    /// nothing else. The new token is acknowledged only by
    /// [`WorldSaveHandle::save`] after its region writes succeed; the actual
    /// release happens after that acknowledgement on the blocking pool.
    fn unload(&self, cx: i32, cz: i32) {
        self.state
            .pending_unload
            .lock()
            .expect("world unload lock poisoned")
            .enqueue((cx, cz));
    }

    /// Forwarded to the wrapped generator (`self.inner`) — this wrapper has
    /// no dragon-fight state of its own, and the real flag lives on the End's
    /// `EndChunkSource` underneath it.
    fn claim_dragon_fight_start(&self) -> bool {
        self.inner.claim_dragon_fight_start()
    }
}

/// A thread-independent handle that writes the world out.
///
/// Holds no generator and no cache, so it can be moved into
/// `tokio::task::spawn_blocking` without dragging the tick's data along.
#[derive(Debug, Clone)]
pub struct WorldSaveHandle {
    state: Arc<WorldState>,
}

/// One save invocation prepared by the world owner and consumed by a worker.
///
/// The job carries the durable-unload tokens that were current when the owner
/// handed work off. It is deliberately not `Clone`: running one prepared job
/// twice would turn a completion message into an implicit retry, which hides
/// the one acknowledgement boundary that a later region worker must retain.
/// A token superseded before this job runs is stale and cannot release the
/// newer unload.
#[derive(Debug)]
pub(crate) struct WorldSaveJob {
    handle: WorldSaveHandle,
    unload_tokens: Vec<DurableUnloadToken>,
}

/// The physical region file responsible for a bounded save assignment.
///
/// A world save first snapshots its global dirty selection, then gives every
/// selected column to exactly one file owner. The blocking writer can execute
/// independent owners in bounded batches without reconstructing them from an
/// unordered dirty set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SaveRegionOwner {
    region: (i32, i32),
}

/// One complete file-owner assignment from a world-save snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SaveRegionAssignment {
    owner: SaveRegionOwner,
    chunks: Vec<(i32, i32)>,
}

/// Deterministic hand-off from the global dirty selection to file owners.
///
/// The plan owns neither columns nor I/O state. Its size is bounded by the
/// snapshot selected for this save, and it is dropped once the save worker has
/// visited each assignment. Region and chunk order are canonical so a
/// hash-set-backed dirty selection cannot change which owner runs first.
#[derive(Debug, Clone, PartialEq, Eq)]
struct WorldSaveRegionPlan {
    assignments: Vec<SaveRegionAssignment>,
}

impl WorldSaveRegionPlan {
    fn from_chunks(chunks: impl IntoIterator<Item = (i32, i32)>) -> Self {
        let mut by_region: BTreeMap<(i32, i32), Vec<(i32, i32)>> = BTreeMap::new();
        for chunk @ (cx, cz) in chunks {
            let (rx, rz, _, _) = region_and_local(cx, cz);
            by_region.entry((rx, rz)).or_default().push(chunk);
        }
        let assignments = by_region
            .into_iter()
            .map(|(region, mut chunks)| {
                chunks.sort_unstable();
                chunks.dedup();
                SaveRegionAssignment {
                    owner: SaveRegionOwner { region },
                    chunks,
                }
            })
            .collect();
        Self { assignments }
    }

    /// Splits the canonical owner sequence into bounded dispatch batches.
    ///
    /// The batches preserve the plan's order. That lets a concurrent writer
    /// choose a reproducible first error after every owner in a batch has
    /// completed, while limiting its active native workers.
    fn into_dispatch_batches(
        self,
        max_workers: usize,
    ) -> impl Iterator<Item = Vec<SaveRegionAssignment>> {
        assert!(max_workers > 0, "a save region worker limit must be nonzero");
        let mut assignments = self.assignments.into_iter();
        std::iter::from_fn(move || {
            let batch = assignments.by_ref().take(max_workers).collect::<Vec<_>>();
            (!batch.is_empty()).then_some(batch)
        })
    }
}

/// An owned snapshot of one complete production column awaiting persistence.
///
/// The source is consulted for a column that is pending only because it owns a
/// live block entity or scheduled tick and has not yet entered the edit map.
/// That keeps the native producer complete without making the save handle own
/// a generator or a cache.
#[derive(Debug, Clone)]
pub struct NativeDirtyChunkSnapshot {
    /// The chunk's horizontal column X coordinate.
    pub column_x: i32,
    /// The chunk's horizontal column Z coordinate.
    pub column_z: i32,
    /// The complete retained or source-provided column.
    pub column: ChunkColumn,
}

impl WorldSaveHandle {
    /// Prepares one save for execution outside the world owner's thread.
    ///
    /// This is the worker hand-off boundary: the token snapshot is taken
    /// before a caller puts the job on a blocking pool, while the job itself
    /// owns the only route that can acknowledge those tokens after its writes
    /// finish. The save's dirty-column snapshot remains inside [`WorldSaveJob::save`]
    /// so edits made before the worker starts are still included.
    #[must_use]
    pub(crate) fn begin_save(&self) -> WorldSaveJob {
        WorldSaveJob {
            handle: self.clone(),
            unload_tokens: self.pending_unload_tokens(),
        }
    }

    /// How many chunks are waiting to be written.
    #[must_use]
    pub fn dirty_count(&self) -> usize {
        self.state
            .dirty
            .lock()
            .expect("world dirty lock poisoned")
            .len()
    }

    /// The persistence counters. See [`PersistenceStats`] for why these are
    /// counts rather than timings.
    #[must_use]
    pub fn stats(&self) -> &PersistenceStats {
        &self.state.stats
    }

    /// The persistent world's scheduled-tick queues.
    ///
    /// Native snapshot encoding and reopen staging use the same handle as the
    /// running tick loop, so a save cannot silently omit or restore into a
    /// private queue.
    #[must_use]
    pub fn scheduled_ticks(&self) -> ScheduledTickHandle {
        self.state.scheduled.clone()
    }

    /// Snapshots every column the established save path would write right now.
    ///
    /// This deliberately shares the Anvil save set: ordinary edits, live block
    /// entities, and pending block/fluid ticks all reach the native producer.
    /// A source fallback covers a tick or entity attached to a generated column
    /// that has not also been edited. Live block entities replace the column's
    /// copied list, matching [`Self::extras_for`] and preventing a stale disk
    /// copy from winning over the running registry.
    pub fn native_dirty_chunks(
        &self,
        source: &dyn ChunkSource,
    ) -> Result<Vec<NativeDirtyChunkSnapshot>, Error> {
        let mut pending = self
            .state
            .dirty
            .lock()
            .expect("world dirty lock poisoned")
            .iter()
            .copied()
            .collect::<HashSet<_>>();
        pending.extend(self.block_entity_chunks());
        pending.extend(self.state.scheduled.chunks_with_pending_ticks());

        let mut pending = pending.into_iter().collect::<Vec<_>>();
        pending.sort_unstable();
        let mut snapshots = Vec::with_capacity(pending.len());
        for (column_x, column_z) in pending {
            let mut column = self
                .state
                .edits
                .lock()
                .expect("world edit lock poisoned")
                .get(&(column_x, column_z))
                .cloned();
            if column.is_none() {
                column = Some(source.column(column_x, column_z));
            }
            let mut column = column.expect("source column fallback always returns a column");
            let entities = self.state.block_entities.with(|registry| {
                registry
                    .iter()
                    .filter(|(pos, _)| chunk_of(**pos) == (column_x, column_z))
                    .map(|(pos, entity)| (*pos, entity.clone()))
                    .collect::<Vec<_>>()
            });
            // The live registry is authoritative, including when its list is
            // empty: a removed container must not reappear from a stale column
            // copy retained by the Anvil loader.
            column.set_block_entities(entities);
            snapshots.push(NativeDirtyChunkSnapshot {
                column_x,
                column_z,
                column,
            });
        }
        Ok(snapshots)
    }

    /// Snapshots the current unload hand-offs without removing them.
    ///
    /// Keeping the entries in the ledger until the region writes complete is
    /// what makes a failed save retryable and lets an unload that arrives while
    /// a save is running receive its own later acknowledgement.
    fn pending_unload_tokens(&self) -> Vec<DurableUnloadToken> {
        self.state
            .pending_unload
            .lock()
            .expect("world unload lock poisoned")
            .tokens()
    }

    /// Moves one queued unload into its durable phase.
    ///
    /// This is intentionally private: only [`Self::save`] can establish the
    /// required fact that the affected region writes completed. A caller
    /// cannot acknowledge a queued token merely because it observed the
    /// source-side hand-off.
    fn acknowledge_unload(&self, token: DurableUnloadToken) -> DurableUnloadAckResult {
        self.state
            .pending_unload
            .lock()
            .expect("world unload lock poisoned")
            .acknowledge(token)
    }

    /// The set of chunks that currently hold at least one block entity.
    ///
    /// Reads the live registry through
    /// [`crate::BlockEntityRegistry::iter`], the non-destructive iterator
    /// added for this: `drain_due`-style consumption here would have removed
    /// the world's furnaces as the price of saving them.
    fn block_entity_chunks(&self) -> HashSet<(i32, i32)> {
        self.state.block_entities.with(|registry| {
            registry
                .iter()
                .map(|(pos, _)| chunk_of(*pos))
                .collect::<HashSet<_>>()
        })
    }

    /// Every block entity inside chunk `(cx, cz)`, as the chunk schema wants
    /// them.
    ///
    /// Grouping happens here rather than in [`chunk_nbt`] for the reason
    /// vanilla groups in `SavedTick.filterTickListForChunk` rather than in the
    /// codec: the writer of a single chunk should be handed exactly that
    /// chunk's contents, so an entry landing in the wrong file is a bug in one
    /// place instead of two.
    fn extras_for(&self, cx: i32, cz: i32) -> ChunkExtras {
        let block_entities = self.state.block_entities.with(|registry| {
            registry
                .iter()
                .filter(|(pos, _)| chunk_of(**pos) == (cx, cz))
                .map(|(pos, entity)| (*pos, entity.clone()))
                .collect::<Vec<_>>()
        });
        let (block_ticks, fluid_ticks) = self.state.scheduled.saved_ticks_for(cx, cz);
        ChunkExtras {
            block_entities,
            block_ticks,
            fluid_ticks,
        }
    }

    /// Writes every dirty chunk, grouped into as few region rewrites as
    /// possible. Returns the number of chunk columns written.
    ///
    /// Blocking: call it from `spawn_blocking` or at shutdown, never from the
    /// tick loop.
    ///
    /// On failure the affected chunks are put **back** into the dirty set, so
    /// a transient disk error costs a retry rather than the player's work.
    pub fn save(&self) -> Result<usize, Error> {
        self.begin_save().save()
    }

    /// Executes a prepared save using the durable tokens owned by its worker.
    fn save_with_unload_tokens(
        &self,
        unload_tokens: &[DurableUnloadToken],
    ) -> Result<usize, Error> {
        // A token remains queued while this save is in flight. A later unload
        // of the same coordinate supersedes this snapshot and is rejected as
        // stale below, leaving the current token for the next save.
        let mut pending: HashSet<(i32, i32)> = {
            let mut dirty = self.state.dirty.lock().expect("world dirty lock poisoned");
            dirty.drain().collect()
        };
        // **Every chunk holding a block entity is written on every save**, on
        // top of the dirty set.
        //
        // It is not a `set_block` that changes a furnace's contents or a
        // hopper's cooldown — those move through the container menu and the
        // tick loop, neither of which touches a block — so nothing marks such
        // a chunk dirty and a dirty-only save would persist a container's
        // state exactly once, at placement, and never again.
        //
        // This is still **mutation**-proportional rather than
        // residency-proportional, which is the property `docs/world-save-load.md`
        // and the cost gate care about: it is bounded by the number of
        // block entities in the world, not by the 512-column store. A world
        // with no containers pays nothing.
        pending.extend(self.block_entity_chunks());
        // And every chunk holding a pending tick, for the same reason: nothing
        // marks a chunk dirty when a redstone repeater's tick is scheduled into
        // it, so a dirty-only save would drop the timing and a redstone clock
        // would come back stopped.
        pending.extend(self.state.scheduled.chunks_with_pending_ticks());
        let taken: Vec<(i32, i32)> = pending.into_iter().collect();
        if taken.is_empty() {
            self.state.stats.empty_saves.fetch_add(1, Ordering::Relaxed);
            // Still sweep: a world nobody is building in is exactly the world
            // whose evicted columns should be released, and an early return
            // here would mean memory is only ever reclaimed while the player
            // is placing blocks.
            for &token in unload_tokens {
                let _ = self.acknowledge_unload(token);
            }
            self.release_unloaded();
            return Ok(0);
        }

        let mut written = 0usize;
        let mut first_error = None;
        for batch in WorldSaveRegionPlan::from_chunks(taken)
            .into_dispatch_batches(MAX_CONCURRENT_REGION_SAVES)
        {
            // `save_region` only touches the assignment's physical file. The
            // shared state it does read is independently synchronised, and
            // each worker has a distinct temp-file name for its owner.
            let results = std::thread::scope(|scope| {
                let workers = batch
                    .iter()
                    .map(|assignment| {
                        let (rx, rz) = assignment.owner.region;
                        let chunks = &assignment.chunks;
                        scope.spawn(move || self.save_region(rx, rz, chunks))
                    })
                    .collect::<Vec<_>>();
                workers
                    .into_iter()
                    .map(|worker| worker.join().expect("save region worker panicked"))
                    .collect::<Vec<_>>()
            });

            // Join and consume in canonical plan order, not completion order.
            // A failed owner is re-dirtied, while independent owners in this
            // and later batches still get their own result. That makes retry
            // complete instead of silently dropping work that a serial early
            // return had not reached yet.
            for (assignment, result) in batch.into_iter().zip(results) {
                match result {
                    Ok(count) => {
                        written += count;
                        self.state
                            .stats
                            .regions_written
                            .fetch_add(1, Ordering::Relaxed);
                    }
                    Err(err) => {
                        self.state
                            .dirty
                            .lock()
                            .expect("world dirty lock poisoned")
                            .extend(assignment.chunks);
                        if first_error.is_none() {
                            first_error = Some(err);
                        }
                    }
                }
            }
        }
        if let Some(err) = first_error {
            // The durable token snapshot stays queued until every owner has
            // written successfully; a retry gets a fresh snapshot.
            return Err(err);
        }
        self.state
            .stats
            .columns_written
            .fetch_add(written as u64, Ordering::Relaxed);
        // After the writes, never before: the sweep's whole safety argument is
        // that anything it drops is already on disk. A token replaced by a
        // newer unload is stale and remains queued for that newer save.
        for &token in unload_tokens {
            let _ = self.acknowledge_unload(token);
        }
        self.release_unloaded();
        Ok(written)
    }

    /// Drops evicted columns from the edit map, once they are safely on disk.
    ///
    /// # Why this is lossless, stated as an invariant
    ///
    /// A column enters `edits` only through
    /// [`RegionChunkSource::set_block`], and that same call marks it dirty
    /// while still holding the `edits` lock. `dirty` is cleared only by a
    /// **successful** region write. So:
    ///
    /// > every column in `edits` but not in `dirty` has been written to disk.
    ///
    /// which is exactly the set this releases, and re-reading one costs a disk
    /// load rather than a wrong block. That is the same argument
    /// `ChunkStore`'s eviction rests on, one layer down — with the difference
    /// that the store could always regenerate, and this layer cannot, which is
    /// why the write has to have happened first rather than merely been
    /// queued.
    ///
    /// The two locks are taken in the same order as `set_block` takes them
    /// (`edits`, then `dirty`), which is what makes the not-dirty check
    /// exclude an edit that is mid-flight rather than merely unlikely.
    fn release_unloaded(&self) {
        let candidates = self
            .state
            .pending_unload
            .lock()
            .expect("world unload lock poisoned")
            .durable_tokens();
        if candidates.is_empty() {
            return;
        }

        // Taken before `edits`, for the lock-order reason `save_region`
        // documents. Pending ticks join block entities in the "never release"
        // set on the same argument: a chunk whose only unsaved state is a
        // scheduled tick is not dirty, so releasing it would let the next save
        // carry its old bytes across and the tick would be gone.
        let mut holds_block_entities = self.block_entity_chunks();
        holds_block_entities.extend(self.state.scheduled.chunks_with_pending_ticks());

        let mut released = 0u64;
        let mut releasable: Vec<DurableUnloadToken> = Vec::new();
        {
            let mut edits = self.state.edits.lock().expect("world edit lock poisoned");
            let dirty = self.state.dirty.lock().expect("world dirty lock poisoned");
            for token in candidates {
                let key = token.chunk;
                // **A chunk holding a block entity is never released**, and
                // this is a correctness rule rather than a heuristic. The
                // invariant that makes releasing lossless is "a column in
                // `edits` but not in `dirty` is already on disk in its current
                // form" — and that is false for a container, whose contents go
                // on changing through the menu and the tick loop without ever
                // marking the chunk dirty. Releasing one would leave the next
                // save to carry the chunk across as its old compressed bytes.
                //
                // Bounded by the number of block entities in the world, not by
                // residency, so the memory ceiling unload-driven saving exists
                // to impose still holds.
                if holds_block_entities.contains(&key) {
                    continue;
                }
                if dirty.contains(&key) {
                    // Mutated again since the eviction, so it is not on disk in
                    // its current form. Dropping it now would lose that edit.
                    continue;
                }
                if edits.remove(&key).is_some() {
                    released += 1;
                }
                // Even an unedited/generated column has no authoritative edit
                // to remove. Its durable token must still be retired, or the
                // bounded ledger would retain a phantom unload forever.
                releasable.push(token);
            }
        }

        // Retire tokens **after** both locks are released, never while holding
        // them. Entries skipped above remain in the durable phase and are
        // retried by the next save, so a concurrent mutation or live container
        // cannot turn a temporary deferral into a forgotten unload.
        if !releasable.is_empty() {
            let mut pending = self
                .state
                .pending_unload
                .lock()
                .expect("world unload lock poisoned");
            for token in releasable {
                let _ = pending.release(token);
            }
        }

        self.state
            .stats
            .unloaded
            .fetch_add(released, Ordering::Relaxed);
    }

    /// Rewrites one region file, carrying untouched chunks across verbatim.
    fn save_region(&self, rx: i32, rz: i32, chunks: &[(i32, i32)]) -> Result<usize, Error> {
        let path = self.state.region_dir.join(format!("r.{rx}.{rz}.mca"));
        let existing = std::fs::read(&path).ok().and_then(|b| {
            // A file we cannot parse is treated as absent rather than fatal,
            // matching this crate's read path. The alternative — refusing to
            // save — would strand every future edit behind one bad file.
            RegionFile::parse(&b).ok()
        });

        let dirty: HashSet<(i32, i32)> = chunks.iter().copied().collect();
        let mut entries: Vec<ChunkToWrite> = Vec::new();

        // Untouched chunks first, as their original compressed bytes: no
        // decode, no re-encode. This is what makes saving one chunk in a full
        // region cheap.
        if let Some(region) = &existing {
            for local_z in 0..32u8 {
                for local_x in 0..32u8 {
                    let cx = rx * 32 + i32::from(local_x);
                    let cz = rz * 32 + i32::from(local_z);
                    if dirty.contains(&(cx, cz)) {
                        continue;
                    }
                    let timestamp = region
                        .timestamp(local_x, local_z)
                        .map_err(Error::Anvil)?
                        .unwrap_or(0);
                    match region.read_chunk_raw(local_x, local_z) {
                        Ok(Some(lodestone_anvil::region::RawChunk::Inline {
                            scheme,
                            compressed,
                        })) => entries.push(ChunkToWrite {
                            chunk_x: cx,
                            chunk_z: cz,
                            compressed,
                            scheme,
                            timestamp,
                        }),
                        // Externalised chunks are the one case that must be
                        // resolved and recompressed; `build_region` will
                        // re-externalise if it is still oversized.
                        Ok(Some(lodestone_anvil::region::RawChunk::External { .. })) => {
                            if let Ok(Some(raw)) = region.read_chunk_nbt_bytes_resolving_external(
                                local_x,
                                local_z,
                                cx,
                                cz,
                                &self.state.region_dir,
                            ) {
                                let compressed = SCHEME.compress(&raw).map_err(Error::Anvil)?;
                                entries.push(ChunkToWrite {
                                    chunk_x: cx,
                                    chunk_z: cz,
                                    compressed,
                                    scheme: SCHEME,
                                    timestamp,
                                });
                            }
                        }
                        // Absent, or corrupt in a way the container rejects.
                        // Dropping a chunk we cannot read is the only option
                        // that lets the rest of the region be saved.
                        Ok(None) | Err(_) => {}
                    }
                }
            }
        }

        // Then the dirty ones, encoded fresh.
        let timestamp =
            u32::try_from(lodestone_time::epoch_duration().as_secs()).unwrap_or(u32::MAX);
        let mut count = 0usize;
        // Snapshotted **before** the `edits` lock is taken, never underneath
        // it. The connection task takes the block-entity registry lock and
        // then writes a block through `ChunkSource::set_block` (placing a
        // furnace does exactly that), which is `block_entities` → `edits`;
        // taking them the other way round here would be a lock-order
        // inversion, and a deadlock that only appears when a player places a
        // container during an autosave is not one any test would find.
        let extras_by_chunk: HashMap<(i32, i32), ChunkExtras> = chunks
            .iter()
            .map(|&(cx, cz)| ((cx, cz), self.extras_for(cx, cz)))
            .collect();
        {
            let edits = self.state.edits.lock().expect("world edit lock poisoned");
            for &(cx, cz) in chunks {
                let Some(column) = edits.get(&(cx, cz)) else {
                    continue;
                };
                let empty = ChunkExtras::default();
                let extras = extras_by_chunk.get(&(cx, cz)).unwrap_or(&empty);
                self.state
                    .stats
                    .block_entities_written
                    .fetch_add(extras.block_entities.len() as u64, Ordering::Relaxed);
                self.state.stats.scheduled_ticks_written.fetch_add(
                    (extras.block_ticks.len() + extras.fluid_ticks.len()) as u64,
                    Ordering::Relaxed,
                );
                let nbt = chunk_nbt::column_to_nbt_with(cx, cz, column, extras);
                let mut writer = Writer::default();
                write_named_nbt(&mut writer, "", &nbt).map_err(Error::Nbt)?;
                let compressed = SCHEME.compress(&writer.into_vec()).map_err(Error::Anvil)?;
                entries.push(ChunkToWrite {
                    chunk_x: cx,
                    chunk_z: cz,
                    compressed,
                    scheme: SCHEME,
                    timestamp,
                });
                count += 1;
            }
        }

        // `build_region` allocates sectors first-fit in the order given, so
        // sorting by region-local index yields vanilla's compact layout.
        entries.sort_by_key(|e| {
            let (_, _, lx, lz) = region_and_local(e.chunk_x, e.chunk_z);
            (u32::from(lz) << 5) | u32::from(lx)
        });

        let built = build_region(&entries).map_err(Error::Anvil)?;
        for (cx, cz, bytes) in &built.external {
            let external = self.state.region_dir.join(format!("c.{cx}.{cz}.mcc"));
            std::fs::write(&external, bytes).map_err(io(&external))?;
        }

        // Atomic per region: a half-written `.mca` is indistinguishable from
        // a corrupt one and would cost the player up to 1,024 chunks.
        let temp = self.state.region_dir.join(format!(".r.{rx}.{rz}.mca.tmp"));
        std::fs::write(&temp, &built.bytes).map_err(io(&temp))?;
        std::fs::rename(&temp, &path).map_err(io(&path))?;

        // The bytes on disk just changed out from under `RegionCache` — drop
        // the stale entry rather than patch it, so the next `load` re-reads
        // instead of silently serving pre-save data (see `RegionCache`'s own
        // doc for why this is invalidate-not-mutate).
        self.state
            .regions
            .lock()
            .expect("region cache lock poisoned")
            .invalidate((rx, rz));
        Ok(count)
    }
}

impl WorldSaveJob {
    /// Runs this job's disk work and, only after success, its token hand-off.
    ///
    /// Consuming `self` makes completion linear: a caller must prepare a new
    /// job for a retry rather than replaying an old worker result.
    pub(crate) fn save(self) -> Result<usize, Error> {
        self.handle.save_with_unload_tokens(&self.unload_tokens)
    }
}

/// Unload-driven saving, gated over the real composition.
///
/// # The control, run and observed
///
/// `ChunkStore`'s eviction notification severed (the `self.source.unload(..)`
/// call replaced by a discard), applied in a throwaway worktree and reverted:
///
/// ```text
/// an_evicted_column_is_released_from_the_edit_map_once_it_is_on_disk ... FAILED
///   left: (3, 0)   right: (2, 1)
/// a_column_mutated_after_its_eviction_is_written_then_released      ... FAILED
/// a_save_with_nothing_dirty_still_releases_evicted_columns          ... FAILED
/// the_sweep_defers_a_column_that_is_dirty_when_it_runs              ... ok
/// ```
///
/// `left: (3, 0)` is precisely the no-unload hypothesis the first gate's own
/// message names, which is what makes it a magnitude check rather than a
/// direction. The fourth still passes because it calls `unload` directly
/// instead of through the store — so the failure set says *which* half broke:
/// a severed wire fails three, a broken sweep fails all four.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk_store::ChunkStore;

    const MIN_Y: i32 = -64;
    const HEIGHT: i32 = 384;
    const MARKER: &str = "minecraft:diamond_block";

    #[test]
    fn durable_unload_tokens_are_bounded_and_reject_stale_or_mismatched_replies() {
        let mut ledger = DurableUnloadLedger::default();
        let first = ledger.enqueue((-3, 5));

        assert_eq!(ledger.pending.len(), 1, "one coordinate owns one token");
        assert_eq!(
            ledger.acknowledge(DurableUnloadToken {
                chunk: (-3, 6),
                ..first
            }),
            DurableUnloadAckResult::Stale,
            "a reply for a neighboring chunk cannot acknowledge this unload"
        );
        assert_eq!(
            ledger.acknowledge(first),
            DurableUnloadAckResult::Accepted,
            "only the queued token can enter the durable phase"
        );
        assert_eq!(
            ledger.acknowledge(first),
            DurableUnloadAckResult::Duplicate,
            "a durable acknowledgement cannot be consumed twice"
        );

        // A second eviction of the same coordinate replaces the first token,
        // rather than extending a history that grows with every visit.
        let second = ledger.enqueue((-3, 5));
        assert_eq!(ledger.pending.len(), 1);
        assert_eq!(
            ledger.acknowledge(first),
            DurableUnloadAckResult::Stale,
            "an old coordinate token cannot complete a newer unload"
        );
        assert_eq!(
            ledger.acknowledge(second),
            DurableUnloadAckResult::Accepted
        );
        assert!(ledger.release(second));
        assert!(ledger.pending.is_empty());
    }

    #[test]
    fn save_region_plan_assigns_each_chunk_once_in_canonical_negative_region_order() {
        let plan = WorldSaveRegionPlan::from_chunks([
            (32, 0),
            (-1, 31),
            (31, 0),
            (-33, -33),
            (0, 0),
            (-32, -32),
            (32, 0),
        ]);

        assert_eq!(
            plan.assignments,
            [
                SaveRegionAssignment {
                    owner: SaveRegionOwner { region: (-2, -2) },
                    chunks: vec![(-33, -33)],
                },
                SaveRegionAssignment {
                    owner: SaveRegionOwner { region: (-1, -1) },
                    chunks: vec![(-32, -32)],
                },
                SaveRegionAssignment {
                    owner: SaveRegionOwner { region: (-1, 0) },
                    chunks: vec![(-1, 31)],
                },
                SaveRegionAssignment {
                    owner: SaveRegionOwner { region: (0, 0) },
                    chunks: vec![(0, 0), (31, 0)],
                },
                SaveRegionAssignment {
                    owner: SaveRegionOwner { region: (1, 0) },
                    chunks: vec![(32, 0)],
                },
            ],
            "the global selection must become one deterministic assignment per file owner"
        );

        let batches = plan
            .clone()
            .into_dispatch_batches(MAX_CONCURRENT_REGION_SAVES)
            .collect::<Vec<_>>();
        assert!(
            batches
                .iter()
                .all(|batch| batch.len() <= MAX_CONCURRENT_REGION_SAVES),
            "a save must never fan an unbounded dirty set into native workers"
        );
        assert_eq!(
            batches.into_iter().flatten().collect::<Vec<_>>(),
            plan.assignments,
            "dispatch batches must retain every owner in the canonical plan order"
        );
    }

    /// A failed owner batch must requeue every selected owner and leave its
    /// unload token unacknowledged. The temporary replacement is a filesystem
    /// failure the writer cannot avoid, so this proves retry from the actual
    /// region-save boundary instead of a hand-written error result.
    #[test]
    fn a_failed_region_owner_batch_requeues_all_work_and_defers_durable_acknowledgement() {
        let dir = tempdir("region-owner-retry");
        let source = RegionChunkSource::new(Flat, &dir, Dimension::Overworld, MIN_Y, HEIGHT)
            .expect("open world");
        let handle = source.save_handle();

        // `(0, 0)` and `(32, 0)` have separate physical file owners and
        // therefore land together in the two-owner dispatch batch.
        source.set_block(1, 70, 1, MARKER);
        source.set_block(32 * 16 + 1, 70, 1, "minecraft:gold_block");
        source.unload(0, 0);

        let region_dir = source.state.region_dir.clone();
        std::fs::remove_dir_all(&region_dir).expect("remove region directory for failure fixture");
        std::fs::write(&region_dir, b"not a directory").expect("replace region directory with file");

        assert!(handle.save().is_err(), "both owner writes must observe the forced failure");
        assert_eq!(
            handle.dirty_count(),
            2,
            "every failed owner must be requeued, not only the first canonical owner"
        );
        assert_eq!(
            handle.stats().unloaded.load(Ordering::Relaxed),
            0,
            "a failed owner batch must not acknowledge and release its unload"
        );

        std::fs::remove_file(&region_dir).expect("remove failure fixture");
        std::fs::create_dir_all(&region_dir).expect("restore region directory");

        assert_eq!(handle.save().expect("retry after restoring region directory"), 2);
        assert_eq!(
            (
                handle.dirty_count(),
                source.retained_columns(),
                handle.stats().unloaded.load(Ordering::Relaxed),
            ),
            (0, 1, 1),
            "the retry must persist both owners, then acknowledge only the matching unload"
        );
        assert_eq!(source.block_state(1, 70, 1), MARKER);
        assert_eq!(source.block_state(32 * 16 + 1, 70, 1), "minecraft:gold_block");
    }

    /// A blocking save job must carry its token snapshot from the world owner
    /// to the worker, rather than reading whatever unload happens to be
    /// queued once that worker eventually starts. The second unload is the
    /// discriminator: an implementation that looks up the token in the
    /// worker would acknowledge it and release the edit one save too early.
    #[test]
    fn a_prepared_save_job_cannot_acknowledge_a_newer_same_chunk_unload() {
        let dir = tempdir("save-job-token");
        let source = RegionChunkSource::new(Flat, &dir, Dimension::Overworld, MIN_Y, HEIGHT)
            .expect("open world");
        let handle = source.save_handle();

        source.set_block(1, 70, 1, MARKER);
        source.unload(0, 0);
        let first_job = handle.begin_save();
        // The owner releases this column again before the worker gets CPU.
        // That must replace, not become, the token the first worker owns.
        source.unload(0, 0);

        let first_written = std::thread::spawn(move || first_job.save())
            .join()
            .expect("save worker must not panic")
            .expect("first save worker must write the edit");
        assert_eq!(first_written, 1);
        assert_eq!(
            (
                source.retained_columns(),
                handle.stats().unloaded.load(Ordering::Relaxed)
            ),
            (1, 0),
            "the delayed first worker must not complete the newer unload"
        );

        assert_eq!(
            handle.begin_save().save().expect("second save worker"),
            0,
            "the next worker has no new dirty column, only the current token"
        );
        assert_eq!(
            (
                source.retained_columns(),
                handle.stats().unloaded.load(Ordering::Relaxed)
            ),
            (0, 1),
            "only the job that captured the current token may release the edit"
        );
        assert_eq!(source.block_state(1, 70, 1), MARKER);
    }

    #[derive(Debug)]
    struct Flat;

    impl ChunkSource for Flat {
        fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
            let mut column = ChunkColumn::new(MIN_Y, HEIGHT);
            for z in 0..16 {
                for x in 0..16 {
                    column.set_block(x, 60, z, "minecraft:stone");
                }
            }
            column
        }

        fn block_state(&self, x: i32, y: i32, z: i32) -> String {
            // The plain column-regenerating form; the tests drive edits
            // through `RegionChunkSource` (which does not forward to this
            // inner source), so this never needs to reflect a write here.
            let cx = x.div_euclid(16);
            let cz = z.div_euclid(16);
            let lx = x.rem_euclid(16);
            let lz = z.rem_euclid(16);
            self.column(cx, cz).block_state(lx, y, lz).to_string()
        }

        fn biome_state_at(&self, x: i32, y: i32, z: i32) -> String {
            // The plain column-regenerating form; the tests drive edits
            // through `RegionChunkSource` (which does not forward to this
            // inner source), so this never needs to reflect a write here.
            let cx = x.div_euclid(16);
            let cz = z.div_euclid(16);
            let lx = x.rem_euclid(16);
            let lz = z.rem_euclid(16);
            self.column(cx, cz).biome_state_at(lx, y, lz).to_string()
        }

        // `RegionChunkSource` owns the edit map and deliberately does not
        // forward `set_block` to its inner source, so this is unreachable in
        // the tests. Explicitly discards rather than inheriting a silent
        // default.
        fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {
            // No storage; edits are discarded by design for this fixture.
        }
    }

    /// A Nether world and an End world must land in **different**
    /// region directories from the overworld and from each other, matching
    /// `.cache/mc/survival/world`'s own layout
    /// (`dimensions/minecraft/the_nether/region`,
    /// `dimensions/minecraft/the_end/region`) rather than all three colliding
    /// on `dimensions/minecraft/overworld/region` — which is what every call
    /// site got before `dimension` was threaded through, since `new` hardcoded
    /// `"overworld"` regardless of the caller's intent.
    #[test]
    fn each_dimension_gets_its_own_region_directory() {
        let dir = tempdir("per-dimension-paths");
        let overworld =
            RegionChunkSource::new(Flat, &dir, Dimension::Overworld, MIN_Y, HEIGHT).expect("open overworld");
        let nether =
            RegionChunkSource::new(Flat, &dir, Dimension::Nether, MIN_Y, HEIGHT).expect("open nether");
        let end = RegionChunkSource::new(Flat, &dir, Dimension::End, MIN_Y, HEIGHT).expect("open end");

        assert!(dir.join("dimensions/minecraft/overworld/region").is_dir());
        assert!(dir.join("dimensions/minecraft/the_nether/region").is_dir());
        assert!(dir.join("dimensions/minecraft/the_end/region").is_dir());

        // A block set into one dimension's edit map must not appear as an
        // edit in another's — the collision this test exists to rule out.
        overworld.set_block(1, 70, 1, MARKER);
        nether.set_block(1, 70, 1, MARKER);
        assert_eq!(overworld.retained_columns(), 1);
        assert_eq!(nether.retained_columns(), 1);
        assert_eq!(end.retained_columns(), 0, "the End was never written to");
    }

    fn tempdir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lodestone-unload-4m8k-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create scratch world dir");
        dir
    }

    /// **The unload gate**, over the real composition: a [`ChunkStore`] above a
    /// [`RegionChunkSource`], which is exactly what
    /// `IntegratedServer::open_persistent_with_mobs` builds. Only the capacity
    /// differs — 2 instead of 512 — so eviction is deterministic rather than
    /// needing 513 columns of terrain.
    ///
    /// Testing the two halves separately would prove nothing about the join,
    /// and the join is the whole feature: `ChunkStore` evicting without
    /// `RegionChunkSource` hearing about it is the island this repo keeps
    /// finding.
    #[test]
    fn an_evicted_column_is_released_from_the_edit_map_once_it_is_on_disk() {
        let dir = tempdir("released");
        let source = RegionChunkSource::new(Flat, &dir, Dimension::Overworld, MIN_Y, HEIGHT).expect("open world");
        let handle = source.save_handle();
        let store = ChunkStore::with_capacity(source.clone(), 2);

        // Three edited chunks: (0,0), (1,0), (2,0).
        for cx in 0..3 {
            source.set_block(cx * 16 + 1, 70, 1, MARKER);
        }
        assert_eq!(
            source.retained_columns(),
            3,
            "three edited chunks must be retained before any save"
        );

        // Make them resident in LRU order, so the victim is (0,0) and not a
        // matter of luck.
        for cx in 0..3 {
            let _ = store.column(cx, 0);
        }

        let written = handle.save().expect("save");
        assert_eq!(written, 3, "every edited chunk must be written");

        // **The magnitude check.** Three edits, one eviction: exactly one
        // column is released, so two remain. The no-unload hypothesis predicts
        // 3 retained and 0 released, and a sweep that ignored the dirty set
        // would predict 0 retained and 3 released. Neither is what a correct
        // release does.
        assert_eq!(
            (
                source.retained_columns(),
                handle.stats().unloaded.load(Ordering::Relaxed)
            ),
            (2, 1),
            "one evicted column must be released and the other two kept"
        );

        // And the point of the whole exercise: releasing must cost a disk read,
        // never a block. This reads the chunk that was dropped.
        assert_eq!(
            source.block_state(1, 70, 1),
            MARKER,
            "the released column lost its edit; releasing is only sound while it is reconstructible"
        );
    }

    /// A column evicted and then **mutated again** before the save is still
    /// released by that save — and that is correct, not a leak: the save wrote
    /// the mutation before sweeping, so the on-disk copy is current.
    ///
    /// This test exists because the first version of it asserted the opposite
    /// and failed. Writing down which one is true is the point: the rule is
    /// *"released only when on disk"*, **not** *"never released after a
    /// mutation"*, and the difference decides whether a late edit costs memory
    /// forever or nothing at all.
    #[test]
    fn a_column_mutated_after_its_eviction_is_written_then_released() {
        let dir = tempdir("redirtied");
        let source = RegionChunkSource::new(Flat, &dir, Dimension::Overworld, MIN_Y, HEIGHT).expect("open world");
        let handle = source.save_handle();
        let store = ChunkStore::with_capacity(source.clone(), 1);

        source.set_block(1, 70, 1, MARKER);
        let _ = store.column(0, 0);
        // Evicts (0,0): capacity is 1.
        let _ = store.column(5, 5);

        // Mutated after the eviction, before the save.
        source.set_block(2, 71, 2, "minecraft:gold_block");

        handle.save().expect("save");
        assert_eq!(
            (
                source.retained_columns(),
                handle.stats().unloaded.load(Ordering::Relaxed)
            ),
            (0, 1),
            "the column was written by this save, so releasing it is sound"
        );
        // The load-bearing half: **both** edits come back, including the one
        // made after the eviction. If the save had written a pre-mutation
        // snapshot and then released, this is the assertion that would fail.
        assert_eq!(source.block_state(2, 71, 2), "minecraft:gold_block");
        assert_eq!(source.block_state(1, 70, 1), MARKER);
    }

    /// The sweep's decision rule in isolation: a column that is **dirty at the
    /// moment of the sweep** is deferred, not dropped.
    ///
    /// Reached in production only when a `set_block` lands between a save's
    /// write and its durable acknowledgement/sweep — a genuine concurrent
    /// window, which is why it is exercised by calling
    /// [`WorldSaveHandle::release_unloaded`] directly rather than by racing two
    /// threads and hoping. A timing-dependent gate for this would introduce a
    /// timing-dependent flake.
    #[test]
    fn the_sweep_defers_a_column_that_is_dirty_when_it_runs() {
        let dir = tempdir("deferred");
        let source = RegionChunkSource::new(Flat, &dir, Dimension::Overworld, MIN_Y, HEIGHT).expect("open world");
        let handle = source.save_handle();

        source.set_block(1, 70, 1, MARKER);
        source.unload(0, 0);

        let token = source
            .state
            .pending_unload
            .lock()
            .expect("world unload lock poisoned")
            .tokens()
            .pop()
            .expect("the unload must create one durable token");
        assert_eq!(
            handle.acknowledge_unload(token),
            DurableUnloadAckResult::Accepted,
            "the direct sweep test starts after the durable acknowledgement"
        );

        // Dirty and unloaded at once: exactly the state a mutation between the
        // write and the sweep produces.
        handle.release_unloaded();
        assert_eq!(
            (
                source.retained_columns(),
                handle.stats().unloaded.load(Ordering::Relaxed)
            ),
            (1, 0),
            "a dirty column must survive the sweep"
        );

        // Deferred rather than forgotten: once it is genuinely on disk, the
        // next sweep releases it without needing a second eviction. Without
        // retaining the durable token this would read (1, 0) forever.
        handle.save().expect("save");
        assert_eq!(
            (
                source.retained_columns(),
                handle.stats().unloaded.load(Ordering::Relaxed)
            ),
            (0, 1),
            "the deferred column must be released once it is on disk"
        );
        assert_eq!(source.block_state(1, 70, 1), MARKER);
    }

    /// An idle world still reclaims: the sweep must not sit behind the
    /// "nothing is dirty" early return, or memory is only ever released while
    /// the player is placing blocks.
    #[test]
    fn a_save_with_nothing_dirty_still_releases_evicted_columns() {
        let dir = tempdir("idle");
        let source = RegionChunkSource::new(Flat, &dir, Dimension::Overworld, MIN_Y, HEIGHT).expect("open world");
        let handle = source.save_handle();
        let store = ChunkStore::with_capacity(source.clone(), 1);

        source.set_block(1, 70, 1, MARKER);
        handle.save().expect("first save writes it");
        assert_eq!(source.retained_columns(), 1);

        let _ = store.column(0, 0);
        let _ = store.column(9, 9);

        // Nothing has been mutated since the save above, so this save writes
        // nothing at all — and must still release.
        assert_eq!(handle.save().expect("empty save"), 0);
        assert_eq!(
            (
                source.retained_columns(),
                handle.stats().unloaded.load(Ordering::Relaxed)
            ),
            (0, 1),
            "an empty save must still reclaim an evicted column"
        );
        assert_eq!(source.block_state(1, 70, 1), MARKER);
    }

    /// The discriminating gate over the **real** production stack —
    /// `ChunkStore -> RegionChunkSource -> disk`, exactly what
    /// `IntegratedServer::open_persistent_with_mobs` builds — and against a
    /// **saved world**, not a fresh one: the fixture is reopened from a
    /// directory an earlier session already wrote to, per CLAUDE.md's own
    /// warning that every singleplayer-shaped gate here defaults to a fresh
    /// world and that blind spot has hidden real defects before.
    ///
    /// A `FORCED` ticket reaches `Full` status through `ChunkStore`, and
    /// removing it drives a real [`ChunkSource::unload`] call that
    /// `RegionChunkSource` observes — proven two ways: the cache entry is
    /// actually gone (not merely ticket-unresident), and — for a column
    /// edited *after* the reopen, so there is something in the edit map for
    /// [`WorldSaveHandle::save`]'s sweep to act on — [`PersistenceStats::unloaded`]
    /// advances and [`RegionChunkSource::retained_columns`] shrinks once that
    /// save runs, the same magnitude pair the capacity-eviction gates above
    /// this one use.
    #[test]
    fn a_ticket_removal_unloads_a_column_through_the_real_persistence_stack() {
        let dir = tempdir("ticket_unload");

        // Session 1: write and save one edited chunk, so the directory this
        // test reopens is a **saved world** — the chunk exists on disk before
        // the store under test ever sees it, which a fresh-world fixture
        // cannot exercise.
        {
            let source = RegionChunkSource::new(Flat, &dir, Dimension::Overworld, MIN_Y, HEIGHT).expect("open world");
            source.set_block(1, 70, 1, MARKER);
            source.save_handle().save().expect("seed save");
        }

        // Session 2: reopen the same directory — this is the "saved world"
        // fixture — and wrap it in a real `ChunkStore`, exactly as
        // `IntegratedServer::open_persistent_with_mobs` does.
        let source = RegionChunkSource::new(Flat, &dir, Dimension::Overworld, MIN_Y, HEIGHT).expect("reopen world");
        let handle = source.save_handle();
        let store = std::sync::Arc::new(ChunkStore::new(source.clone()));

        store.set_forced_ticket(1, (0, 0));
        // Drive real read traffic through the store (never call the ticket
        // graph's internals directly) until `maybe_tick_tickets` has checked
        // in at least once.
        for _ in 0..25 {
            let _ = store.column(0, 0);
        }
        assert_eq!(
            store.ticket_status(0, 0),
            crate::ticket::ChunkStatus::Full,
            "precondition: the forced ticket must actually make (0,0) resident"
        );
        assert!(store.is_column_resident(0, 0));
        assert_eq!(
            source.block_state(1, 70, 1),
            MARKER,
            "precondition: the saved edit must survive the reopen and the ticket grant"
        );

        // A fresh edit this session, on top of the one carried over from
        // disk — this is what gives the sweep below something real to defer
        // then release, exactly like the capacity-eviction gates above.
        const SECOND_MARKER: &str = "minecraft:gold_block";
        source.set_block(2, 71, 2, SECOND_MARKER);
        assert_eq!(source.retained_columns(), 1, "one edited column, in the edit map");

        store.remove_forced_ticket(1);
        for _ in 0..25 {
            let _ = store.column(50, 50);
        }

        assert_eq!(
            store.ticket_status(0, 0),
            crate::ticket::ChunkStatus::Empty,
            "the ticket graph must show (0,0) as no longer wanted once the ticket is gone"
        );
        assert!(
            !store.is_column_resident(0, 0),
            "the cache entry must actually be dropped, not merely ticket-unresident"
        );

        // The sweep only *defers* release until the edit is safely on disk —
        // matching every other release gate in this module. A caller (an
        // autosave loop in production) drives that here.
        assert_eq!(handle.save().expect("save"), 1, "the fresh edit must be written");
        assert_eq!(
            handle.stats().unloaded.load(Ordering::Relaxed),
            1,
            "the real persistence layer must observe exactly one unload, once the edit is on disk"
        );
        assert_eq!(
            source.retained_columns(),
            0,
            "the edit map must actually shrink, not just log an unload"
        );
        // And the point of the whole exercise: neither edit is lost. Reading
        // them back after the unload costs a disk load, never a wrong block.
        assert_eq!(
            source.block_state(1, 70, 1),
            MARKER,
            "the edit carried over from session 1 must survive the ticket-driven unload"
        );
        assert_eq!(
            source.block_state(2, 71, 2),
            SECOND_MARKER,
            "the edit made in session 2, right before the unload, must also survive it"
        );
    }

    /// The permanent negative control: the same saved-world fixture, the same
    /// amount of driven traffic, but the ticket is **never removed**. The
    /// chunk must stay resident and `unload` must never fire — without this,
    /// the positive gate above could be passing because eviction runs
    /// unconditionally rather than because it is gated on the ticket.
    #[test]
    fn a_ticket_that_is_never_removed_never_unloads_through_the_real_stack() {
        let dir = tempdir("ticket_no_removal");
        {
            let source = RegionChunkSource::new(Flat, &dir, Dimension::Overworld, MIN_Y, HEIGHT).expect("open world");
            source.set_block(1, 70, 1, MARKER);
            source.save_handle().save().expect("seed save");
        }

        let source = RegionChunkSource::new(Flat, &dir, Dimension::Overworld, MIN_Y, HEIGHT).expect("reopen world");
        let handle = source.save_handle();
        let store = std::sync::Arc::new(ChunkStore::new(source.clone()));

        store.set_forced_ticket(1, (0, 0));
        for _ in 0..25 {
            let _ = store.column(0, 0);
        }
        source.set_block(2, 71, 2, "minecraft:gold_block");
        // No removal here — the control.
        for _ in 0..75 {
            let _ = store.column(50, 50);
        }
        let _ = handle.save();

        assert!(
            store.is_column_resident(0, 0),
            "a forced ticket that was never removed must keep its chunk resident"
        );
        assert_eq!(
            handle.stats().unloaded.load(Ordering::Relaxed),
            0,
            "nothing was removed, so nothing may be unloaded"
        );
        assert_eq!(
            source.retained_columns(),
            1,
            "the edit made under an active ticket must stay in the edit map, saved or not"
        );
    }

    /// The reported regression: a block whose state owns a block entity
    /// (a wither skeleton skull) with a correct state on disk and **no**
    /// block-entity NBT at all — exactly the shape a chunk saved before
    /// `ChunkColumn::missing_block_entity_states` existed already carries,
    /// and exactly the shape the old placement path (`set_block` alone, no
    /// registry insert) produced for any type `block_entity_for_item` did
    /// not cover. Reproduced directly with `set_block` rather than through
    /// `apply_use_item_on`, which lives in `crate::server` and is not
    /// reachable from here — `set_block` writes only the state, which is
    /// the one property this fixture needs from the old buggy path.
    ///
    /// The fresh `RegionChunkSource` over the same directory is a fresh
    /// server process reading the world back: the "joined and it was
    /// already invisible" scenario the owner reported, not a same-session
    /// resend — the discriminating input is a block entity missing from the
    /// chunk as first loaded, not one that could still be patched up by a
    /// later in-memory write.
    #[test]
    fn a_skull_state_with_no_saved_block_entity_gets_one_synthesized_on_load() {
        use crate::block_entities::BlockEntity;

        let dir = tempdir("skull-repair");
        const SKULL: &str = "minecraft:wither_skeleton_skull";
        {
            let source = RegionChunkSource::new(Flat, &dir, Dimension::Overworld, MIN_Y, HEIGHT)
                .expect("open world");
            source.set_block(1, 70, 1, SKULL);
            source.save_handle().save().expect("seed save");
        }

        let source = RegionChunkSource::new(Flat, &dir, Dimension::Overworld, MIN_Y, HEIGHT)
            .expect("reopen world");
        let column = source.column(0, 0);
        let pos = BlockPos::new(1, 70, 1);
        let entity_type = column
            .block_entities()
            .iter()
            .find(|(p, _)| *p == pos)
            .map(|(_, e)| e.type_id());
        assert_eq!(
            entity_type,
            Some("minecraft:skull"),
            "a skull state with no on-disk record must get an empty one synthesized on load, \
             or the client never learns to draw it at all"
        );
    }

    /// The negative control: a state that owns **no** block-entity type
    /// (plain stone via `MARKER`) must never gain a synthesized record —
    /// `ChunkColumn::missing_block_entity_states`'s palette-classification
    /// fast path must reject it outright, not merely happen not to find a
    /// position for it. Without this, the positive gate above could be
    /// passing because the repair fires unconditionally rather than because
    /// it is gated on the state actually owning a block-entity type.
    #[test]
    fn a_plain_block_gets_no_synthesized_block_entity() {
        let dir = tempdir("no-skull-repair");
        {
            let source = RegionChunkSource::new(Flat, &dir, Dimension::Overworld, MIN_Y, HEIGHT)
                .expect("open world");
            source.set_block(1, 70, 1, MARKER);
            source.save_handle().save().expect("seed save");
        }
        let source = RegionChunkSource::new(Flat, &dir, Dimension::Overworld, MIN_Y, HEIGHT)
            .expect("reopen world");
        let column = source.column(0, 0);
        assert!(
            column.block_entities().is_empty(),
            "a state with no block-entity type must never gain a synthesized record"
        );
    }
}
