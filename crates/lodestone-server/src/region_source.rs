//! World persistence: a [`ChunkSource`] backed by Anvil region files on disk
//! (issue [#437](https://github.com/matteopolak/lodestone/issues/437)).
//!
//! # What it is
//!
//! The thing that makes a singleplayer world survive quitting. Before this,
//! `lodestone-server` had **no save or load path at all** — nothing in the
//! crate read or wrote a world — and `lodestone-anvil` was a declared island
//! with zero production callers (verified: no `Cargo.toml` in the workspace
//! named it). [`RegionChunkSource`] is that crate's first caller.
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
//!
//! # How to change it, and the gotchas
//!
//! - **A region file is rewritten whole.** `lodestone_anvil::region` builds a
//!   complete `.mca` in one pass and has no incremental single-chunk update
//!   (issue #437's body flags this). [`WorldSaveHandle::save`] therefore reads
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
//! The world directory, passed to [`RegionChunkSource::new`]. Region files
//! land in `<world>/dimensions/minecraft/overworld/region/r.<rx>.<rz>.mca`,
//! which is 26.2's real layout (verified against `.cache/mc/survival/world`,
//! **not** the pre-1.21 `<world>/region/`). Chunks are written with
//! [`CompressionScheme::Zlib`], vanilla's `RegionFileVersion.DEFAULT`.
//!
//! # Dependencies
//!
//! `lodestone-anvil` for the container, [`crate::chunk_nbt`] for the schema,
//! and `std::fs`. Target-gated to non-wasm by `lib.rs` — a browser world has
//! no filesystem.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use lodestone_anvil::CompressionScheme;
use lodestone_anvil::region::{ChunkToWrite, RegionFile, build_region, region_and_local};
use lodestone_core::{Reader, Writer, read_named_nbt, write_named_nbt};

use crate::chunk::{ChunkColumn, ChunkSource};
use crate::chunk_nbt;

/// Vanilla's `RegionFileVersion.DEFAULT`, and the only scheme any real file
/// this repo has read actually uses.
const SCHEME: CompressionScheme = CompressionScheme::Zlib;

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
/// Issue [#437](https://github.com/matteopolak/lodestone/issues/437) made
/// blocks survive a restart but left the seed unstored, and the two together
/// are worse than neither: chunks the player *had* visited come back from disk
/// while chunks they had not are regenerated from a **different** seed, so the
/// world is discontinuous exactly at the edge of where they explored. A
/// blocks-only gate structurally cannot see it, because every block such a gate
/// checks is one that was saved. Issue
/// [#468](https://github.com/matteopolak/lodestone/issues/468).
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
/// two traps, both of which have already cost an issue each:
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

    /// Stamps `Time` and `LastPlayed` and writes the file back.
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
    pub fn write(&self, session_ticks: u64) -> Result<(), Error> {
        let mut level = self.level.lock().expect("level.dat lock poisoned");
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
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
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
}

/// The state a save needs, shared between the source and its save handle.
#[derive(Debug)]
struct WorldState {
    /// `<world>/dimensions/minecraft/overworld/region`.
    region_dir: PathBuf,
    min_y: i32,
    height: i32,
    /// The **authoritative** columns: everything a `set_block` has touched.
    /// Plays exactly the role `OverworldChunkSource::edits` plays for a
    /// generator-only world, which is what keeps `ChunkStore`'s lossless
    /// eviction true.
    edits: Mutex<HashMap<(i32, i32), ChunkColumn>>,
    /// Chunks changed since the last successful save.
    dirty: Mutex<HashSet<(i32, i32)>>,
    /// Chunks the cache above has evicted, which may therefore be dropped from
    /// [`Self::edits`] once they are safely on disk. See
    /// [`WorldSaveHandle::save`]'s unload sweep.
    pending_unload: Mutex<HashSet<(i32, i32)>>,
    stats: PersistenceStats,
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
    /// Wraps `inner` with persistence rooted at `world_dir`.
    ///
    /// Creates the region directory eagerly so that a later save cannot fail
    /// for a reason the caller could have been told about at open time.
    pub fn new(inner: S, world_dir: &Path, min_y: i32, height: i32) -> Result<Self, Error> {
        let region_dir = world_dir
            .join("dimensions")
            .join("minecraft")
            .join("overworld")
            .join("region");
        std::fs::create_dir_all(&region_dir).map_err(io(&region_dir))?;
        Ok(Self {
            inner: Arc::new(inner),
            state: Arc::new(WorldState {
                region_dir,
                min_y,
                height,
                edits: Mutex::new(HashMap::new()),
                dirty: Mutex::new(HashSet::new()),
                pending_unload: Mutex::new(HashSet::new()),
                stats: PersistenceStats::default(),
            }),
        })
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

    /// Reads the column at `(cx, cz)` off disk, or `None` if this world has
    /// never saved it.
    ///
    /// A missing or unparseable region file is `None`, not an error: vanilla
    /// itself treats "file doesn't exist yet" as a legal chunk-less region
    /// (see `lodestone_anvil::region::RegionFile::parse`), and a world's very
    /// first open has no files at all.
    fn load(&self, cx: i32, cz: i32) -> Option<ChunkColumn> {
        let (rx, rz, local_x, local_z) = region_and_local(cx, cz);
        let path = self.state.region_dir.join(format!("r.{rx}.{rz}.mca"));
        let bytes = std::fs::read(&path).ok()?;
        let region = RegionFile::parse(&bytes).ok()?;
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
        chunk_nbt::column_from_nbt(&nbt, self.state.min_y, self.state.height).ok()
    }
}

impl<S: ChunkSource> ChunkSource for RegionChunkSource<S> {
    fn column(&self, cx: i32, cz: i32) -> ChunkColumn {
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
            return loaded;
        }
        self.state.stats.generated.fetch_add(1, Ordering::Relaxed);
        self.inner.column(cx, cz)
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
    /// frequently the tick thread, so it is a `HashSet` insert and nothing
    /// else. The actual release happens inside [`WorldSaveHandle::save`],
    /// which already runs on the blocking pool.
    fn unload(&self, cx: i32, cz: i32) {
        self.state
            .pending_unload
            .lock()
            .expect("world unload lock poisoned")
            .insert((cx, cz));
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

impl WorldSaveHandle {
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

    /// Writes every dirty chunk, grouped into as few region rewrites as
    /// possible. Returns the number of chunk columns written.
    ///
    /// Blocking: call it from `spawn_blocking` or at shutdown, never from the
    /// tick loop.
    ///
    /// On failure the affected chunks are put **back** into the dirty set, so
    /// a transient disk error costs a retry rather than the player's work.
    pub fn save(&self) -> Result<usize, Error> {
        let taken: Vec<(i32, i32)> = {
            let mut dirty = self.state.dirty.lock().expect("world dirty lock poisoned");
            dirty.drain().collect()
        };
        if taken.is_empty() {
            self.state.stats.empty_saves.fetch_add(1, Ordering::Relaxed);
            // Still sweep: a world nobody is building in is exactly the world
            // whose evicted columns should be released, and an early return
            // here would mean memory is only ever reclaimed while the player
            // is placing blocks.
            self.release_unloaded();
            return Ok(0);
        }

        let mut by_region: BTreeMap<(i32, i32), Vec<(i32, i32)>> = BTreeMap::new();
        for &(cx, cz) in &taken {
            let (rx, rz, _, _) = region_and_local(cx, cz);
            by_region.entry((rx, rz)).or_default().push((cx, cz));
        }

        let mut written = 0usize;
        for ((rx, rz), chunks) in by_region {
            match self.save_region(rx, rz, &chunks) {
                Ok(n) => {
                    written += n;
                    self.state
                        .stats
                        .regions_written
                        .fetch_add(1, Ordering::Relaxed);
                }
                Err(err) => {
                    // Re-dirty everything this region owned before bailing.
                    let mut dirty = self.state.dirty.lock().expect("world dirty lock poisoned");
                    dirty.extend(chunks);
                    return Err(err);
                }
            }
        }
        self.state
            .stats
            .columns_written
            .fetch_add(written as u64, Ordering::Relaxed);
        // After the writes, never before: the sweep's whole safety argument is
        // that anything it drops is already on disk.
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
        let candidates: Vec<(i32, i32)> = {
            let mut pending = self
                .state
                .pending_unload
                .lock()
                .expect("world unload lock poisoned");
            if pending.is_empty() {
                return;
            }
            pending.drain().collect()
        };

        let mut released = 0u64;
        let mut deferred: Vec<(i32, i32)> = Vec::new();
        {
            let mut edits = self.state.edits.lock().expect("world edit lock poisoned");
            let dirty = self.state.dirty.lock().expect("world dirty lock poisoned");
            for key in candidates {
                if dirty.contains(&key) {
                    // Mutated again since the eviction, so it is not on disk in
                    // its current form. Dropping it now would lose that edit.
                    deferred.push(key);
                    continue;
                }
                if edits.remove(&key).is_some() {
                    released += 1;
                }
            }
        }

        // Re-queued **after** both locks are released, never while holding
        // them: the drain above takes `pending_unload` before `edits`, so
        // taking it again underneath `edits` would invert the order against a
        // concurrent sweep and could deadlock. Requeuing at all is what makes
        // the skip above a deferral rather than a leak — a column skipped once
        // would otherwise sit in the edit map until it happened to be evicted
        // a second time.
        if !deferred.is_empty() {
            let mut pending = self
                .state
                .pending_unload
                .lock()
                .expect("world unload lock poisoned");
            pending.extend(deferred);
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
        let timestamp = u32::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
        )
        .unwrap_or(u32::MAX);
        let mut count = 0usize;
        {
            let edits = self.state.edits.lock().expect("world edit lock poisoned");
            for &(cx, cz) in chunks {
                let Some(column) = edits.get(&(cx, cz)) else {
                    continue;
                };
                let nbt = chunk_nbt::column_to_nbt(cx, cz, column);
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
        Ok(count)
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
        let source = RegionChunkSource::new(Flat, &dir, MIN_Y, HEIGHT).expect("open world");
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
        let source = RegionChunkSource::new(Flat, &dir, MIN_Y, HEIGHT).expect("open world");
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
    /// write and its sweep — a genuine concurrent window, which is why it is
    /// exercised by calling [`WorldSaveHandle::release_unloaded`] directly
    /// rather than by racing two threads and hoping. A timing-dependent gate
    /// for this would be exactly the flake this repo just spent an issue
    /// removing.
    #[test]
    fn the_sweep_defers_a_column_that_is_dirty_when_it_runs() {
        let dir = tempdir("deferred");
        let source = RegionChunkSource::new(Flat, &dir, MIN_Y, HEIGHT).expect("open world");
        let handle = source.save_handle();

        source.set_block(1, 70, 1, MARKER);
        source.unload(0, 0);

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
        // the requeue this would read (1, 0) forever.
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
        let source = RegionChunkSource::new(Flat, &dir, MIN_Y, HEIGHT).expect("open world");
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
}
