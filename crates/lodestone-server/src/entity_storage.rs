//! Per-chunk entity persistence: the `entities/` region set.
//!
//! # What it is
//!
//! The thing that makes a mob and a dropped item survive quitting. Before this,
//! `crate::chunk_nbt` wrote block entities and scheduled ticks and **no
//! `"Entities"` list of any kind** — verified: `grep -n '"Entities"'` across the
//! workspace matched only a comment in `lodestone_anvil::region`'s header. So a
//! reopened world came back with every cow, creeper and dropped diamond deleted,
//! with no error anywhere.
//!
//! # `entities/` is a second, parallel region set — not a field in the chunk
//!
//! Since 1.17 entities do **not** live in the terrain chunk. They live in their
//! own region files under `<world>/dimensions/<ns>/<dim>/entities/r.<rx>.<rz>.mca`,
//! with the same 8 KiB-header sector container the terrain files use, and a
//! completely different root schema:
//!
//! ```text
//! Position: IntArray[2]   -- [chunkX, chunkZ], NOT xPos/zPos ints
//! DataVersion: Int
//! Entities: List<Compound>
//! ```
//!
//! That `Position` int-array is the trap. A terrain chunk carries `xPos`/`yPos`/
//! `zPos` as three separate `Int`s; an entity chunk carries one two-element
//! `IntArray` and no `yPos` at all. Code that reaches for `xPos` here finds
//! nothing and silently reads chunk `(0, 0)`.
//!
//! Provenance: `.cache/mc/survival/world`'s overworld `entities/` set — 19 region
//! files, 880 populated chunks, **2093 entities** — read with a foreign parser
//! (Python `gzip`/`zlib` + `struct.unpack`, sharing no code with this repo). The
//! census that came back is the outside expectation every field below is written
//! against: `item` 510, `sheep` 169, `chicken` 163, `pig` 147, `skeleton` 122,
//! `creeper` 116, `zombie` 115, `cow` 101, `chest_minecart` 99, `bat` 88.
//!
//! # The entity ids are strings, and that is load-bearing
//!
//! `id` is a `String` resource key (`"minecraft:item"`), never an ordinal. This
//! repo has already shipped the ordinal version of this bug once, in a different
//! place — every dropped item arriving as `minecraft:acacia_boat`, because a
//! numeric entity type was read against the wrong table. A saved entity whose
//! type came back as a different mob would be indistinguishable from a spawn bug.
//!
//! # How stale records are cleared, and why it is by UUID
//!
//! A save has to *remove* a mob's old record when the mob walks from chunk A into
//! chunk B, or the next load spawns it twice — and doubling every mob per restart
//! is worse than losing them. The obvious fixes both fail here:
//!
//! - Rewriting only the chunks that currently hold entities leaves A's stale copy.
//! - Rewriting *every* chunk in the file would delete the 2093 entities of a real
//!   vanilla world the moment our sim (which holds none of them) saved once.
//!
//! So [`EntityStorage::save`] clears by **identity**: every entity the live sim
//! holds is in `live_uuids`, so a stored record whose UUID is in that set is one
//! of ours that has moved, and is dropped. A record whose UUID is unknown belongs
//! to something else and is preserved byte-for-byte. This is exact rather than
//! heuristic, and it is why [`SavedEntity::uuid`] is round-tripped rather than
//! regenerated on load.
//!
//! # How to change it, and the gotchas
//!
//! - **A region file is rewritten whole**, exactly as
//!   [`crate::region_source`] documents: untouched chunks are re-emitted as their
//!   *original compressed bytes* without decoding, so the cost is a sector copy.
//!   Only chunks we write, or that hold a live UUID, are decoded.
//! - **The write is atomic per region** — temp file in the same directory, then
//!   `rename`. Same reasoning as the terrain writer.
//! - **`DataVersion` is checked on read**: an entity chunk from
//!   another game version is refused rather than mis-decoded, because a
//!   mis-decoded entity is one we then write back wrong.
//! - **This module holds no lock and no `Arc<Mutex>`.** It is a directory path;
//!   the caller owns the entity population and hands a `Vec` in. That keeps the
//!   ECS proposal-queue migration free of one more piece of shared state.
//!
//! # Dependencies
//!
//! `lodestone_anvil::region` for the container, `lodestone-core` for NBT,
//! `std::fs`.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use lodestone_anvil::CompressionScheme;
use lodestone_anvil::region::{ChunkToWrite, RegionFile, build_region, region_and_local};
use lodestone_core::{Nbt, NbtTag, Reader, Writer, read_named_nbt, write_named_nbt};
use lodestone_model::{ResourceKey, Rotation, Vec3};
use uuid::Uuid;

use crate::region_source::Error;

/// Vanilla's `RegionFileVersion.DEFAULT`, matching the terrain writer.
const SCHEME: CompressionScheme = CompressionScheme::Zlib;

/// One persisted entity, in the subset this server actually simulates.
///
/// Deliberately **not** a full vanilla entity: the oracle files carry `Brain`,
/// `attributes`, `memories` and ~30 more fields per mob, none of which this
/// server models. Those are carried through verbatim in
/// [`extra`](Self::extra) for exactly the reason
/// [`crate::player_data::PlayerData::preserved`] exists — a writer that emitted
/// only what it understands would strip a real world's mobs down to a position
/// and a health value on the first save.
#[derive(Debug, Clone, PartialEq)]
pub struct SavedEntity {
    /// The entity type key, e.g. `minecraft:cow` or `minecraft:item`.
    pub id: ResourceKey,
    /// Vanilla's `UUID` int-array, round-tripped — see the module doc on stale
    /// record clearing for why this must not be regenerated on load.
    pub uuid: Uuid,
    /// Feet position.
    pub pos: Vec3,
    /// Velocity, blocks per tick.
    pub motion: Vec3,
    /// Look direction.
    pub rotation: Rotation,
    /// Health, for a living entity.
    pub health: Option<f32>,
    /// For `minecraft:item` only: the stack it is showing, as
    /// `(item id, count)`.
    pub item: Option<(ResourceKey, u8)>,
    /// For `minecraft:item` only: ticks alive, vanilla's `Age`.
    pub age: Option<i16>,
    /// For `minecraft:item` only: vanilla's `PickupDelay`.
    pub pickup_delay: Option<i16>,
    /// Every other field the source record carried, verbatim.
    pub extra: Vec<(String, Nbt)>,
}

/// The fields [`SavedEntity::to_nbt`] **always** writes, and which are therefore
/// always excluded from [`SavedEntity::extra`].
///
/// Everything else is excluded only if it actually decoded — see
/// [`SavedEntity::from_nbt`]'s "consumed set" note, which is not a style choice
/// but a bug fix paid for by a real vanilla world.
const ALWAYS_WRITTEN: &[&str] = &["id", "UUID", "Pos", "Motion", "Rotation"];

impl SavedEntity {
    /// The chunk column this entity belongs to.
    ///
    /// `floor`, not truncation: `x = -1.5` is chunk `-1`, and truncating
    /// division would file every entity on the negative side of the origin one
    /// chunk too far in — the same arithmetic note
    /// [`crate::region_source`]'s `chunk_of` makes for block positions.
    #[must_use]
    pub fn chunk(&self) -> (i32, i32) {
        (
            (self.pos.x.floor() as i64).div_euclid(16) as i32,
            (self.pos.z.floor() as i64).div_euclid(16) as i32,
        )
    }

    /// Encodes to the compound a vanilla server writes into an `Entities` list.
    #[must_use]
    pub fn to_nbt(&self) -> Nbt {
        let mut fields = vec![
            ("id".to_owned(), Nbt::String(self.id.to_string())),
            ("UUID".to_owned(), Nbt::IntArray(uuid_to_ints(self.uuid))),
            ("Pos".to_owned(), doubles(self.pos)),
            ("Motion".to_owned(), doubles(self.motion)),
            (
                "Rotation".to_owned(),
                Nbt::List {
                    element_type: NbtTag::Float,
                    elements: vec![
                        Nbt::Float(self.rotation.yaw),
                        Nbt::Float(self.rotation.pitch),
                    ],
                },
            ),
        ];
        if let Some(health) = self.health {
            fields.push(("Health".to_owned(), Nbt::Float(health)));
        }
        if let Some((item, count)) = &self.item {
            fields.push((
                "Item".to_owned(),
                Nbt::Compound(vec![
                    ("id".to_owned(), Nbt::String(item.to_string())),
                    ("count".to_owned(), Nbt::Int(i32::from(*count))),
                ]),
            ));
        }
        if let Some(age) = self.age {
            fields.push(("Age".to_owned(), Nbt::Short(age)));
        }
        if let Some(delay) = self.pickup_delay {
            fields.push(("PickupDelay".to_owned(), Nbt::Short(delay)));
        }
        fields.extend(self.extra.iter().cloned());
        Nbt::Compound(fields)
    }

    /// Decodes one entry of an `Entities` list, or `None` if it carries no
    /// usable `id`/`Pos`.
    ///
    /// A single unreadable entity is dropped rather than failing the chunk, on
    /// the same argument [`crate::chunk_nbt`]'s container reader makes: one bad
    /// record must not cost the other 2092.
    ///
    /// # The "consumed set", and the bug it exists to prevent
    ///
    /// [`extra`](Self::extra) is built from the fields this function **did not
    /// actually decode**, not from a static list of the ones it knows about. The
    /// difference is not cosmetic, and the static version shipped a real defect
    /// that `entity_nbt_vanilla_oracle.rs` caught on its first run:
    ///
    /// | field | on `minecraft:item` | on a mob |
    /// |---|---|---|
    /// | `Age` | `Short` — ticks alive | **`Int`** — breeding age, negative for a baby |
    /// | `Health` | **`Short`** — a constant 5 | `Float` — real health |
    ///
    /// The same NBT key means two different things with two different tag types
    /// depending on the entity's *class*. A static exclusion list containing
    /// `"Age"` therefore matched the sheep's `Int` field, failed to decode it
    /// (this code wants a `Short`), and **dropped it from the output**: every
    /// baby sheep in a loaded world would have silently become an adult, with a
    /// clean parse and no error. This is exactly the collision shape
    /// `CLAUDE.md`'s entity-metadata-index rule describes, in NBT rather than in
    /// metadata indices, and the guard has to be "did the decode succeed?"
    /// because *which* classes collide on a given key is not knowable from the
    /// key alone.
    #[must_use]
    pub fn from_nbt(nbt: &Nbt) -> Option<Self> {
        let Some(Nbt::String(id)) = field(nbt, "id") else {
            return None;
        };
        let id: ResourceKey = id.parse().ok()?;
        let pos = read_doubles(field(nbt, "Pos"))?;

        let mut consumed: Vec<&str> = ALWAYS_WRITTEN.to_vec();

        let health = match field(nbt, "Health") {
            Some(Nbt::Float(h)) => {
                consumed.push("Health");
                Some(*h)
            }
            // Anything else — notably an item entity's `Short` — is left for
            // `extra` to carry verbatim.
            _ => None,
        };
        let item = match field(nbt, "Item").and_then(|stack| match field(stack, "id") {
            Some(Nbt::String(item_id)) => {
                let key: ResourceKey = item_id.parse().ok()?;
                let count = match field(stack, "count") {
                    Some(Nbt::Int(c)) => (*c).clamp(0, 255) as u8,
                    Some(Nbt::Byte(c)) => i32::from(*c).clamp(0, 255) as u8,
                    _ => 1,
                };
                Some((key, count))
            }
            _ => None,
        }) {
            Some(stack) => {
                consumed.push("Item");
                Some(stack)
            }
            None => None,
        };
        let age = match field(nbt, "Age") {
            Some(Nbt::Short(a)) => {
                consumed.push("Age");
                Some(*a)
            }
            _ => None,
        };
        let pickup_delay = match field(nbt, "PickupDelay") {
            Some(Nbt::Short(d)) => {
                consumed.push("PickupDelay");
                Some(*d)
            }
            _ => None,
        };

        let extra = match nbt {
            Nbt::Compound(fields) => fields
                .iter()
                .filter(|(name, _)| !consumed.contains(&name.as_str()))
                .cloned()
                .collect(),
            _ => Vec::new(),
        };
        Some(Self {
            id,
            uuid: read_uuid(field(nbt, "UUID")).unwrap_or_else(Uuid::new_v4),
            pos,
            motion: read_doubles(field(nbt, "Motion")).unwrap_or(Vec3::new(0.0, 0.0, 0.0)),
            rotation: read_rotation(field(nbt, "Rotation")).unwrap_or(Rotation::new(0.0, 0.0)),
            health,
            item,
            age,
            pickup_delay,
            extra,
        })
    }
}

/// Reads and writes the `entities/` region set for one dimension.
///
/// Holds a directory path and nothing else — see the module doc on why there is
/// no lock here.
#[derive(Debug, Clone)]
pub struct EntityStorage {
    dir: std::sync::Arc<PathBuf>,
}

impl EntityStorage {
    /// Opens the overworld entity-sidecar path without creating it.
    ///
    /// This is the read-only entry point for migration preflight. In
    /// particular, a conversion preview must not manufacture an empty
    /// `entities/` directory in its source world.
    #[must_use]
    pub fn open_readonly(world_dir: &Path) -> Self {
        Self::open_readonly_for_dimension(world_dir, crate::dimension::Dimension::Overworld)
    }

    /// Opens one built-in dimension's entity-sidecar path without creating it.
    #[must_use]
    pub fn open_readonly_for_dimension(
        world_dir: &Path,
        dimension: crate::dimension::Dimension,
    ) -> Self {
        let dir = world_dir
            .join("dimensions")
            .join("minecraft")
            .join(dimension.dir_name())
            .join("entities");
        Self {
            dir: std::sync::Arc::new(dir),
        }
    }

    /// Roots a store at `world_dir`'s overworld `entities/` directory, creating
    /// it eagerly so a later save cannot fail for a reason the caller could have
    /// been told at world open.
    ///
    /// The path is `<world>/dimensions/minecraft/overworld/entities`, which is
    /// 26.2's real layout — verified against `.cache/mc/survival/world`, **not**
    /// the pre-1.21 `<world>/entities/`. Same sibling-of-`region/` position
    /// [`crate::region_source`] documents for terrain.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] if the directory cannot be created.
    pub fn new(world_dir: &Path) -> Result<Self, Error> {
        Self::new_for_dimension(world_dir, crate::dimension::Dimension::Overworld)
    }

    /// Opens one built-in dimension's writable entity-sidecar path.
    pub fn new_for_dimension(
        world_dir: &Path,
        dimension: crate::dimension::Dimension,
    ) -> Result<Self, Error> {
        let dir = world_dir
            .join("dimensions")
            .join("minecraft")
            .join(dimension.dir_name())
            .join("entities");
        std::fs::create_dir_all(&dir).map_err(|source| Error::Io {
            path: dir.clone(),
            source,
        })?;
        Ok(Self {
            dir: std::sync::Arc::new(dir),
        })
    }

    fn region_path(&self, rx: i32, rz: i32) -> PathBuf {
        self.dir.join(format!("r.{rx}.{rz}.mca"))
    }

    /// Lists every populated entity-sidecar chunk in deterministic coordinate
    /// order without decoding its payload.
    ///
    /// Missing sidecar directories are an empty selection. A malformed region
    /// filename is refused rather than silently omitted, since an operator's
    /// `--all-entities` review must cover every apparent region file.
    pub fn populated_chunks(&self) -> Result<Vec<(i32, i32)>, Error> {
        let entries = match std::fs::read_dir(self.dir.as_path()) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(source) => {
                return Err(Error::Io {
                    path: self.dir.as_path().to_path_buf(),
                    source,
                });
            }
        };
        let mut regions = BTreeMap::new();
        for entry in entries {
            let entry = entry.map_err(|source| Error::Io {
                path: self.dir.as_path().to_path_buf(),
                source,
            })?;
            let path = entry.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("mca") {
                continue;
            }
            if !entry.file_type().map_err(|source| Error::Io {
                path: path.clone(),
                source,
            })?.is_file() {
                return Err(invalid_region_filename(path));
            }
            let Some((region_x, region_z)) = parse_region_name(&path) else {
                return Err(invalid_region_filename(path));
            };
            regions.insert((region_x, region_z), path);
        }
        let mut chunks = Vec::new();
        for ((region_x, region_z), path) in regions {
            let bytes = std::fs::read(&path).map_err(|source| Error::Io {
                path: path.clone(),
                source,
            })?;
            let region = RegionFile::parse(&bytes).map_err(Error::Anvil)?;
            for local_z in 0..32_u8 {
                for local_x in 0..32_u8 {
                    if region.has_chunk(local_x, local_z).map_err(Error::Anvil)? {
                        chunks.push((
                            region_x * 32 + i32::from(local_x),
                            region_z * 32 + i32::from(local_z),
                        ));
                    }
                }
            }
        }
        Ok(chunks)
    }

    /// Every entity stored in chunk `(cx, cz)`.
    ///
    /// A missing region file, or a chunk that has never been written, is an empty
    /// `Vec` — every world's first open, exactly as
    /// [`crate::region_source::RegionChunkSource`] treats a missing terrain
    /// region.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] if the file exists but cannot be read, or
    /// [`Error::Anvil`] if it exists and will not parse — including a
    /// `DataVersion` this build cannot read. A corrupt entity file
    /// is reported rather than silently treated as "this chunk has no mobs",
    /// because the latter is indistinguishable from correct behaviour.
    pub fn load_chunk(&self, cx: i32, cz: i32) -> Result<Vec<SavedEntity>, Error> {
        let (rx, rz, local_x, local_z) = region_and_local(cx, cz);
        let path = self.region_path(rx, rz);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(source) => return Err(Error::Io { path, source }),
        };
        let region = RegionFile::parse(&bytes).map_err(Error::Anvil)?;
        let Some(raw) = region
            .read_chunk_nbt_bytes_resolving_external(local_x, local_z, cx, cz, &self.dir)
            .map_err(Error::Anvil)?
        else {
            return Ok(Vec::new());
        };
        let mut reader = Reader::new(&raw);
        let (_, nbt) = read_named_nbt(&mut reader).map_err(|e| Error::Anvil(lodestone_anvil::Error::Nbt(e)))?;
        entities_from_chunk_nbt(&nbt).map_err(Error::Anvil)
    }

    /// Every entity stored anywhere in the region files covering the inclusive
    /// chunk ranges `cx_range` × `cz_range`.
    ///
    /// This is the world-open load: the caller hands the area its simulation is
    /// authoritative for and gets back everything that was saved inside it. A
    /// chunk outside the range keeps its stored entities untouched on disk.
    ///
    /// # Errors
    ///
    /// As [`load_chunk`](Self::load_chunk).
    pub fn load_area(
        &self,
        cx_range: std::ops::RangeInclusive<i32>,
        cz_range: std::ops::RangeInclusive<i32>,
    ) -> Result<Vec<SavedEntity>, Error> {
        let mut out = Vec::new();
        // Grouped by region file so a 7×7 area reads one file once rather than
        // 49 times — the same reason the writer groups.
        let mut by_region: BTreeMap<(i32, i32), Vec<(i32, i32)>> = BTreeMap::new();
        for cx in cx_range.clone() {
            for cz in cz_range.clone() {
                let (rx, rz, _, _) = region_and_local(cx, cz);
                by_region.entry((rx, rz)).or_default().push((cx, cz));
            }
        }
        for ((rx, rz), chunks) in by_region {
            let path = self.region_path(rx, rz);
            let bytes = match std::fs::read(&path) {
                Ok(bytes) => bytes,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
                Err(source) => return Err(Error::Io { path, source }),
            };
            let region = RegionFile::parse(&bytes).map_err(Error::Anvil)?;
            for (cx, cz) in chunks {
                let (_, _, local_x, local_z) = region_and_local(cx, cz);
                let Some(raw) = region
                    .read_chunk_nbt_bytes_resolving_external(local_x, local_z, cx, cz, &self.dir)
                    .map_err(Error::Anvil)?
                else {
                    continue;
                };
                let mut reader = Reader::new(&raw);
                let (_, nbt) = read_named_nbt(&mut reader)
                    .map_err(|e| Error::Anvil(lodestone_anvil::Error::Nbt(e)))?;
                out.extend(entities_from_chunk_nbt(&nbt).map_err(Error::Anvil)?);
            }
        }
        Ok(out)
    }

    /// Writes `entities` out, grouped into chunks and then into region files, and
    /// clears every stale record belonging to one of them.
    ///
    /// `entities` is the **complete** live population the caller is
    /// authoritative for; anything in the files whose UUID appears in it and is
    /// not written here is removed. See the module doc for why identity, not
    /// footprint bookkeeping, is what makes that exact.
    ///
    /// Returns the number of entities written.
    ///
    /// Blocking. Call it from `spawn_blocking` or at shutdown, never from the
    /// tick loop — the same rule
    /// [`crate::region_source::WorldSaveHandle::save`] states.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] on a filesystem failure or [`Error::Anvil`] if an existing
    /// region file cannot be parsed. Nothing is written for a region that fails,
    /// so a transient error costs a retry rather than the population.
    pub fn save(&self, entities: &[SavedEntity]) -> Result<usize, Error> {
        let live_uuids: HashSet<Uuid> = entities.iter().map(|e| e.uuid).collect();

        let mut by_chunk: HashMap<(i32, i32), Vec<&SavedEntity>> = HashMap::new();
        for entity in entities {
            by_chunk.entry(entity.chunk()).or_default().push(entity);
        }

        // Regions we must open: those receiving entities, plus — because a mob may
        // have walked out of one entirely — every region that already exists. The
        // second half is what makes stale clearing complete; without it a mob that
        // left the last populated region would be duplicated forever.
        let mut regions: HashSet<(i32, i32)> = by_chunk
            .keys()
            .map(|&(cx, cz)| {
                let (rx, rz, _, _) = region_and_local(cx, cz);
                (rx, rz)
            })
            .collect();
        if !live_uuids.is_empty() {
            regions.extend(self.existing_regions()?);
        }

        let mut written = 0usize;
        for (rx, rz) in regions {
            written += self.save_region(rx, rz, &by_chunk, &live_uuids)?;
        }
        Ok(written)
    }

    /// The `(rx, rz)` of every `r.<rx>.<rz>.mca` already in the directory.
    fn existing_regions(&self) -> Result<Vec<(i32, i32)>, Error> {
        let entries = match std::fs::read_dir(self.dir.as_path()) {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(source) => {
                return Err(Error::Io {
                    path: self.dir.as_path().to_path_buf(),
                    source,
                });
            }
        };
        let mut out = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            let Some(rest) = name.strip_prefix("r.") else {
                continue;
            };
            let Some(rest) = rest.strip_suffix(".mca") else {
                continue;
            };
            let mut parts = rest.split('.');
            let (Some(rx), Some(rz), None) = (parts.next(), parts.next(), parts.next()) else {
                continue;
            };
            if let (Ok(rx), Ok(rz)) = (rx.parse(), rz.parse()) {
                out.push((rx, rz));
            }
        }
        Ok(out)
    }

    /// Rewrites one region file. Chunks we are not writing are passed through as
    /// their original compressed bytes unless they hold a live UUID, in which
    /// case they are decoded, filtered and re-emitted.
    fn save_region(
        &self,
        rx: i32,
        rz: i32,
        by_chunk: &HashMap<(i32, i32), Vec<&SavedEntity>>,
        live_uuids: &HashSet<Uuid>,
    ) -> Result<usize, Error> {
        let path = self.region_path(rx, rz);
        let existing = match std::fs::read(&path) {
            Ok(bytes) => Some(RegionFile::parse(&bytes).map_err(Error::Anvil)?),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
            Err(source) => {
                return Err(Error::Io {
                    path: path.clone(),
                    source,
                });
            }
        };

        let timestamp =
            u32::try_from(lodestone_time::epoch_duration().as_secs()).unwrap_or(u32::MAX);

        let mut entries: Vec<ChunkToWrite> = Vec::new();
        let mut written = 0usize;
        for local_z in 0..32u8 {
            for local_x in 0..32u8 {
                let cx = rx * 32 + i32::from(local_x);
                let cz = rz * 32 + i32::from(local_z);

                if let Some(live) = by_chunk.get(&(cx, cz)) {
                    let nbt = chunk_nbt_for(cx, cz, live.iter().map(|e| e.to_nbt()).collect());
                    entries.push(ChunkToWrite {
                        chunk_x: cx,
                        chunk_z: cz,
                        compressed: SCHEME.compress(&encode_chunk(&nbt)?).map_err(Error::Anvil)?,
                        scheme: SCHEME,
                        timestamp,
                    });
                    written += live.len();
                    continue;
                }

                let Some(region) = existing.as_ref() else {
                    continue;
                };
                let stored_timestamp = region
                    .timestamp(local_x, local_z)
                    .map_err(Error::Anvil)?
                    .unwrap_or(0);
                let Some(raw) = region
                    .read_chunk_nbt_bytes_resolving_external(local_x, local_z, cx, cz, &self.dir)
                    .map_err(Error::Anvil)?
                else {
                    continue;
                };
                // Decoded only to ask "does this chunk still hold a record of
                // one of *our* live entities?". When the answer is no — the
                // common case, and the whole of a vanilla world — the original
                // **compressed** bytes go straight back out, untouched.
                let unchanged = |entries: &mut Vec<ChunkToWrite>| -> Result<(), Error> {
                    match region.read_chunk_raw(local_x, local_z) {
                        Ok(Some(lodestone_anvil::region::RawChunk::Inline {
                            scheme,
                            compressed,
                        })) => entries.push(ChunkToWrite {
                            chunk_x: cx,
                            chunk_z: cz,
                            compressed,
                            scheme,
                            timestamp: stored_timestamp,
                        }),
                        // Externalised: resolved above already, so recompress and
                        // let `build_region` re-externalise if still oversized.
                        _ => entries.push(ChunkToWrite {
                            chunk_x: cx,
                            chunk_z: cz,
                            compressed: SCHEME.compress(&raw).map_err(Error::Anvil)?,
                            scheme: SCHEME,
                            timestamp: stored_timestamp,
                        }),
                    }
                    Ok(())
                };

                let mut reader = Reader::new(&raw);
                let Ok((_, nbt)) = read_named_nbt(&mut reader) else {
                    // Unparseable and not ours to fix: carried forward verbatim
                    // rather than dropped. Dropping it would delete somebody's
                    // mobs because *we* could not read the tree.
                    unchanged(&mut entries)?;
                    continue;
                };
                let stored = entity_list(&nbt);
                let kept: Vec<Nbt> = stored
                    .iter()
                    .filter(|entry| {
                        read_uuid(field(entry, "UUID")).is_none_or(|u| !live_uuids.contains(&u))
                    })
                    .cloned()
                    .collect();
                if kept.len() == stored.len() {
                    unchanged(&mut entries)?;
                } else {
                    let nbt = chunk_nbt_for(cx, cz, kept);
                    entries.push(ChunkToWrite {
                        chunk_x: cx,
                        chunk_z: cz,
                        compressed: SCHEME.compress(&encode_chunk(&nbt)?).map_err(Error::Anvil)?,
                        scheme: SCHEME,
                        timestamp,
                    });
                }
            }
        }

        if entries.is_empty() {
            return Ok(0);
        }

        let built = build_region(&entries).map_err(Error::Anvil)?;
        for (cx, cz, bytes) in &built.external {
            let external = self.dir.join(format!("c.{cx}.{cz}.mcc"));
            std::fs::write(&external, bytes).map_err(|source| Error::Io {
                path: external,
                source,
            })?;
        }
        // Atomic per region, exactly like the terrain writer: a half-written
        // `.mca` is indistinguishable from a corrupt one and would cost the
        // player every mob in 1024 chunks.
        let temp = self.dir.join(format!("r.{rx}.{rz}.mca.tmp"));
        std::fs::write(&temp, &built.bytes).map_err(|source| Error::Io {
            path: temp.clone(),
            source,
        })?;
        std::fs::rename(&temp, &path).map_err(|source| Error::Io {
            path: path.clone(),
            source,
        })?;
        Ok(written)
    }
}

/// Builds the root compound of an entity chunk.
///
/// `Position` is an `IntArray` of two — see the module doc on why this is not
/// `xPos`/`zPos`.
#[must_use]
pub fn chunk_nbt_for(cx: i32, cz: i32, entities: Vec<Nbt>) -> Nbt {
    Nbt::Compound(vec![
        ("Position".to_owned(), Nbt::IntArray(vec![cx, cz])),
        (
            "DataVersion".to_owned(),
            Nbt::Int(lodestone_anvil::level_dat::DATA_VERSION_26_2),
        ),
        (
            "Entities".to_owned(),
            Nbt::List {
                element_type: NbtTag::Compound,
                elements: entities,
            },
        ),
    ])
}

/// Decodes an entity chunk's `Entities` list, refusing an unreadable
/// `DataVersion`.
fn entities_from_chunk_nbt(nbt: &Nbt) -> Result<Vec<SavedEntity>, lodestone_anvil::Error> {
    lodestone_anvil::require_supported_data_version(match field(nbt, "DataVersion") {
        Some(Nbt::Int(v)) => Some(*v),
        _ => None,
    })?;
    Ok(entity_list(nbt)
        .iter()
        .filter_map(SavedEntity::from_nbt)
        .collect())
}

fn entity_list(nbt: &Nbt) -> &[Nbt] {
    match field(nbt, "Entities") {
        Some(Nbt::List { elements, .. }) => elements,
        _ => &[],
    }
}

fn parse_region_name(path: &Path) -> Option<(i32, i32)> {
    let stem = path.file_name()?.to_str()?.strip_suffix(".mca")?;
    let mut parts = stem.split('.');
    match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some("r"), Some(x), Some(z), None) => Some((x.parse().ok()?, z.parse().ok()?)),
        _ => None,
    }
}

fn invalid_region_filename(path: PathBuf) -> Error {
    Error::Io {
        path,
        source: std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "entity region file name must be canonical r.<x>.<z>.mca",
        ),
    }
}

fn encode_chunk(nbt: &Nbt) -> Result<Vec<u8>, Error> {
    let mut writer = Writer::default();
    write_named_nbt(&mut writer, "", nbt)
        .map_err(|e| Error::Anvil(lodestone_anvil::Error::Nbt(e)))?;
    Ok(writer.into_vec())
}

fn field<'a>(nbt: &'a Nbt, key: &str) -> Option<&'a Nbt> {
    match nbt {
        Nbt::Compound(fields) => fields
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value),
        _ => None,
    }
}

fn doubles(v: Vec3) -> Nbt {
    Nbt::List {
        element_type: NbtTag::Double,
        elements: vec![Nbt::Double(v.x), Nbt::Double(v.y), Nbt::Double(v.z)],
    }
}

fn read_doubles(nbt: Option<&Nbt>) -> Option<Vec3> {
    let Some(Nbt::List { elements, .. }) = nbt else {
        return None;
    };
    if elements.len() < 3 {
        return None;
    }
    let get = |i: usize| match elements[i] {
        Nbt::Double(d) => Some(d),
        _ => None,
    };
    Some(Vec3::new(get(0)?, get(1)?, get(2)?))
}

fn read_rotation(nbt: Option<&Nbt>) -> Option<Rotation> {
    let Some(Nbt::List { elements, .. }) = nbt else {
        return None;
    };
    if elements.len() < 2 {
        return None;
    }
    let get = |i: usize| match elements[i] {
        Nbt::Float(f) => Some(f),
        _ => None,
    };
    Some(Rotation::new(get(0)?, get(1)?))
}

/// Vanilla's `NbtUtils.createUUID`: the 128 bits as four big-endian `int`s, most
/// significant first. Not a string, and not two longs — a `.dat` written with
/// either is silently unreadable by the real game.
fn uuid_to_ints(uuid: Uuid) -> Vec<i32> {
    let (hi, lo) = uuid.as_u64_pair();
    vec![
        (hi >> 32) as i32,
        (hi & 0xFFFF_FFFF) as i32,
        (lo >> 32) as i32,
        (lo & 0xFFFF_FFFF) as i32,
    ]
}

fn read_uuid(nbt: Option<&Nbt>) -> Option<Uuid> {
    let Some(Nbt::IntArray(parts)) = nbt else {
        return None;
    };
    if parts.len() != 4 {
        return None;
    }
    let word = |i: usize| u64::from(parts[i] as u32);
    Some(Uuid::from_u64_pair(
        (word(0) << 32) | word(1),
        (word(2) << 32) | word(3),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uuid_int_array_round_trips_through_vanillas_own_layout() {
        // The expected numbers come from vanilla's documented layout (four
        // big-endian ints, most significant first), applied by hand to a fixed
        // uuid — not from calling our own reader on our own writer.
        let uuid = Uuid::parse_str("00dd60bd-39a4-381a-bc60-741f6ae2a0c2").expect("valid");
        let ints = uuid_to_ints(uuid);
        assert_eq!(
            ints,
            vec![0x00dd_60bd, 0x39a4_381a_u32 as i32, 0xbc60_741f_u32 as i32, 0x6ae2_a0c2_u32 as i32]
        );
        assert_eq!(read_uuid(Some(&Nbt::IntArray(ints))), Some(uuid));
    }

    #[test]
    fn chunk_of_a_negative_position_floors() {
        let mut entity = sample_item();
        entity.pos = Vec3::new(-0.5, 64.0, -16.5);
        // Truncating division would say (0, -1); flooring says (-1, -2).
        assert_eq!(entity.chunk(), (-1, -2));
    }

    fn sample_item() -> SavedEntity {
        SavedEntity {
            id: "minecraft:item".parse().expect("valid"),
            uuid: Uuid::from_u128(0x1234),
            pos: Vec3::new(1.5, 64.0, 2.5),
            motion: Vec3::new(0.0, -0.16, 0.0),
            rotation: Rotation::new(0.0, 0.0),
            health: None,
            item: Some(("minecraft:gravel".parse().expect("valid"), 2)),
            age: Some(3762),
            pickup_delay: Some(0),
            extra: Vec::new(),
        }
    }

    #[test]
    fn unmodelled_fields_survive_a_round_trip() {
        // The property the module doc calls the most important one: a field this
        // server does not model must come back, or saving a real world's mobs
        // strips them.
        let mut entity = sample_item();
        entity.extra = vec![("Brain".to_owned(), Nbt::Compound(vec![]))];
        let decoded = SavedEntity::from_nbt(&entity.to_nbt()).expect("decodes");
        assert_eq!(decoded.extra, entity.extra);
        assert_eq!(decoded.item, entity.item);
        assert_eq!(decoded.uuid, entity.uuid);
    }

    #[test]
    fn an_entity_chunk_from_another_game_version_is_refused() {
        // The control for the DataVersion check: the detector must actually fire,
        // and it must not fire on our own version.
        let ours = chunk_nbt_for(0, 0, vec![sample_item().to_nbt()]);
        assert_eq!(entities_from_chunk_nbt(&ours).expect("current").len(), 1);

        let Nbt::Compound(mut fields) = ours else {
            unreachable!("chunk_nbt_for builds a compound")
        };
        for (name, value) in &mut fields {
            if name == "DataVersion" {
                *value = Nbt::Int(3955);
            }
        }
        assert!(matches!(
            entities_from_chunk_nbt(&Nbt::Compound(fields)),
            Err(lodestone_anvil::Error::UnsupportedDataVersion { .. })
        ));
    }
}
