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
                stats: PersistenceStats::default(),
            }),
        })
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
        drop(edits);

        self.state
            .dirty
            .lock()
            .expect("world dirty lock poisoned")
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
        Ok(written)
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
