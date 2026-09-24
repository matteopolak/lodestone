//! Per-section point-of-interest persistence: the `poi/` region set and its
//! interaction with the server's world-state behavior.
//!
//! # What it is
//!
//! The reader/writer for the third region-file set. Entities and terrain each
//! have their own; a point of interest — a bed, workstation, bell, or lit
//! nether portal — is keyed by *section*, not by block or chunk. Region files
//! under `poi/` are read and written with the same atomic replacement rules as
//! the other persistent region sets.
//!
//! # `poi/` is a *third* parallel region set, and its chunk schema agrees with
//! neither sibling
//!
//! `crate::entity_storage`'s module doc already names the trap between a
//! terrain chunk's `xPos`/`yPos`/`zPos` and an entity chunk's two-element
//! `Position` int array. A POI chunk carries **neither** — verified against
//! `.cache/mc/survival/world`'s own `poi/` files, read with a foreign parser
//! (Python `struct.unpack`, sharing no code with this repo) before a line of
//! Rust here existed:
//!
//! ```text
//! DataVersion: Int
//! Sections: Compound { "<sectionY>": { Valid: Byte, Records: List<{
//!     pos: IntArray[3],   -- absolute block [x, y, z], NOT section-relative
//!     type: String,       -- e.g. "minecraft:nether_portal"
//!     free_tickets: Int?  -- OMITTED, not zero, when a claim has taken every ticket
//! }> } }
//! ```
//!
//! `free_tickets` is an optional integer with default `0`; the encoder omits a
//! field when it equals that default, so **absence
//! means zero free tickets**, not "unclaimed" — the opposite of what the name
//! suggests on a skim. The oracle world confirms this directly: every
//! `minecraft:bee_nest` and `minecraft:nether_portal` record (both registered
//! with `maxTickets 0`) omits the field, and three
//! `minecraft:meeting` (bell, `maxTickets 32`) records carry explicit `28` or
//! `29` — a real village's bell partway claimed by villagers pathing to a
//! meeting. A decoder that reads "absent" as "unclaimed" rather than "zero"
//! would report a bell that villagers have already claimed seven times over as
//! having all 32 tickets free.
//!
//! **A POI chunk has no `Position` field of any kind** — unlike both terrain
//! (`xPos`/`zPos`) and entities (`Position` IntArray[2]). The generic section
//! storage never writes one; the chunk's coordinate is carried *only* by which slot in
//! the region container it occupies. Code that goes looking for a `Position`
//! or `xPos` key to double-check which chunk it decoded will find nothing —
//! trust the region container's own `(local_x, local_z)`, exactly as
//! [`load_chunk`](PoiStorage::load_chunk) does.
//!
//! Provenance: `.cache/mc/survival/world`'s overworld `poi/` set — 29 region
//! files, 124 chunks carrying `Sections`, 150 sections, **210 records** — plus
//! the Nether's own 4-file set (1 chunk, 1 section, 6 records, all
//! `nether_portal`). `DataVersion` is `4903` in both, matching
//! [`lodestone_anvil::level_dat::DATA_VERSION_26_2`]. Per-type census: 127
//! `fisherman`, 43 `home`, 23 `bee_nest`, 6 `nether_portal`, 4 `farmer`, 3
//! `meeting`, 3 `shepherd`, 1 `cartographer`. The committed POI oracle test
//! asserts this exactly: an off-by-one selects the wrong POI type.
//!
//! # Occupancy: what `free_tickets` represents
//!
//! A POI's whole purpose is answering "is this claimable", and every type has
//! a maximum simultaneous claim count — [`max_tickets`]. [`PoiRecord::has_space`]
//! is the query a villager or a portal
//! search actually wants; [`PoiRecord::is_occupied`] is the inverse used for
//! "does this village have a claimed bell". **A gate that only checks "the POI
//! exists" cannot tell a store that honours claims from one that hands out an
//! all-tickets-taken record anyway.** `poi_persistence_round_trip.rs` checks
//! this distinction through a full occupancy record.
//!
//! # How stored records are addressed, and why there is no identity-clearing
//! logic here
//!
//! A point of interest is a fixed block position, and
//! [`PoiRecord::pos`] *is* its identity. A caller that wants to persist the
//! current POI state for a chunk hands [`PoiStorage::save`] the **complete**
//! [`PoiChunk`] for every chunk it is authoritative for; any chunk **not**
//! mentioned is left byte-for-byte untouched on disk, exactly like
//! [`crate::region_source`]'s terrain writer treats an unloaded chunk. This is
//! this positional identity means the complete authoritative chunk state can
//! be written without a UUID relocation ledger.
//!
//! # How to change it, and the gotchas
//!
//! - **A region file is rewritten whole**, exactly as
//!   [`crate::entity_storage`] and [`crate::region_source`] both document:
//!   untouched chunks are re-emitted as their original compressed bytes.
//! - **The write is atomic per region** — temp file, then `rename`.
//! - **`DataVersion` is checked on read**, same as every other
//!   region set here.
//! - **Two dimensions, not one.** Unlike [`crate::entity_storage`] and
//!   [`crate::region_source`], which are overworld-only by established scope,
//!   [`PoiStorage::new`] takes a [`Dimension`] — a lit nether portal is a POI
//!   in *both* the overworld and the Nether, and [`crate::portal::PortalIndex`]
//!   already tracks both. The subdirectory name is derived from
//!   [`Dimension::key`] rather than hand-matched a second time, so a future
//!   dimension needs no edit here.
//! - **This module holds no lock.** Same reasoning as
//!   [`crate::entity_storage`]: it is a directory path, and the caller owns
//!   the live POI state.
//!
//! # What is *not* in scope, on purpose
//!
//! Nothing here populates a POI from a block scan — no caller
//! re-derives POI from placed blocks yet, so [`PoiSection::valid`] is carried
//! through rather than acted on. What *is* wired: [`crate::integrated`]
//! calls [`Self::load_all`] at world open (one store per dimension, restoring
//! [`crate::portal::PortalIndex`] before the first connection is served) and
//! [`Self::save`] on the same autosave interval and shutdown path
//! `crate::entity_storage`'s own wiring uses — see that module's own doc for
//! why "a two-line follow-up" undersold it: two dimensions need two stores,
//! and a portal's position is unbounded, so the restore reads the whole store
//! ([`Self::load_all`]) rather than one guessed range. [`crate::portal::poi_records_for_index`],
//! [`crate::portal::restore_index_from_poi`] and
//! [`crate::portal::poi_chunks_for_index`] are the three conversions, proven
//! by `poi_persistence_round_trip.rs`'s
//! `a_portal_index_round_trips_through_the_poi_store` gate and by
//! `tests/portal_persistence_restart.rs`'s restart gate.
//!
//! # Dependencies
//!
//! `lodestone_anvil::region` for the container (the same one
//! [`crate::entity_storage`] and [`crate::region_source`] use — this is a
//! third *instance*, not new container code), `lodestone-core` for NBT,
//! `lodestone-model` for [`BlockPos`]/[`ResourceKey`], `std::fs`.

use std::collections::{BTreeMap, HashMap};
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};

use lodestone_anvil::CompressionScheme;
use lodestone_anvil::region::{ChunkToWrite, RawChunk, RegionFile, build_region, region_and_local};
use lodestone_core::{Nbt, NbtTag, Reader, Writer, read_named_nbt, write_named_nbt};
use lodestone_model::{BlockPos, ResourceKey};

use crate::dimension::Dimension;
use crate::region_source::Error;

/// Compression used by the three persistent region sets.
const SCHEME: CompressionScheme = CompressionScheme::Zlib;

/// Maximum simultaneous claims by resource path. All built-in keys use the
/// `minecraft` namespace, so matching on [`ResourceKey::path`] avoids
/// allocating a full [`ResourceKey`] per arm.
///
/// An unrecognised type gets `0`, the same result as a block that is not a POI.
/// This conservative default prevents an unknown type from handing out claims
/// that the rest of the server cannot validate.
#[must_use]
pub fn max_tickets(poi_type: &ResourceKey) -> i32 {
    match poi_type.path() {
        "armorer" | "butcher" | "cartographer" | "cleric" | "farmer" | "fisherman"
        | "fletcher" | "leatherworker" | "librarian" | "mason" | "shepherd" | "toolsmith"
        | "weaponsmith" | "home" => 1,
        "meeting" => 32,
        _ => 0,
    }
}

/// One point-of-interest record — `PoiRecord`.
#[derive(Debug, Clone, PartialEq)]
pub struct PoiRecord {
    /// The block this POI is anchored to.
    pub pos: BlockPos,
    /// The POI type key, e.g. `minecraft:nether_portal`.
    pub poi_type: ResourceKey,
    /// Remaining simultaneous claims. See the module doc: **zero** is what an
    /// *absent* `free_tickets` field on disk means, not "unclaimed".
    pub free_tickets: i32,
}

impl PoiRecord {
    /// Creates a freshly discovered POI with its type's full ticket count.
    #[must_use]
    pub fn new(pos: BlockPos, poi_type: ResourceKey) -> Self {
        let free_tickets = max_tickets(&poi_type);
        Self {
            pos,
            poi_type,
            free_tickets,
        }
    }

    /// Returns whether at least one simultaneous claim remains.
    #[must_use]
    pub fn has_space(&self) -> bool {
        self.free_tickets > 0
    }

    /// Returns whether at least one ticket has been claimed.
    #[must_use]
    pub fn is_occupied(&self) -> bool {
        self.free_tickets != max_tickets(&self.poi_type)
    }

    /// Claims one ticket, returning `false` without mutation when none remain.
    pub fn acquire_ticket(&mut self) -> bool {
        if self.free_tickets <= 0 {
            return false;
        }
        self.free_tickets -= 1;
        true
    }

    /// Releases one ticket, returning `false` without mutation when already at
    /// the type's maximum.
    pub fn release_ticket(&mut self) -> bool {
        if self.free_tickets >= max_tickets(&self.poi_type) {
            return false;
        }
        self.free_tickets += 1;
        true
    }

    /// Encodes the record fields used by the POI interchange format.
    fn to_nbt(&self) -> Nbt {
        let mut fields = vec![
            (
                "pos".to_owned(),
                Nbt::IntArray(vec![self.pos.x, self.pos.y, self.pos.z]),
            ),
            ("type".to_owned(), Nbt::String(self.poi_type.to_string())),
        ];
        // The optional field is omitted exactly at its default of zero. See
        // the module doc for why absence means no free tickets.
        if self.free_tickets != 0 {
            fields.push(("free_tickets".to_owned(), Nbt::Int(self.free_tickets)));
        }
        Nbt::Compound(fields)
    }

    /// Decodes the record fields. `None` if `pos` or `type` do not parse; a
    /// single bad record must not discard the rest of the section.
    fn from_nbt(nbt: &Nbt) -> Option<Self> {
        let pos = match field(nbt, "pos") {
            Some(Nbt::IntArray(parts)) if parts.len() == 3 => {
                BlockPos::new(parts[0], parts[1], parts[2])
            }
            _ => return None,
        };
        let poi_type: ResourceKey = match field(nbt, "type") {
            Some(Nbt::String(s)) => s.parse().ok()?,
            _ => return None,
        };
        let free_tickets = match field(nbt, "free_tickets") {
            Some(Nbt::Int(v)) => *v,
            _ => 0,
        };
        Some(Self {
            pos,
            poi_type,
            free_tickets,
        })
    }
}

/// One section's worth of POI records — `PoiSection`.
#[derive(Debug, Clone, Default)]
pub struct PoiSection {
    /// `PoiSection.isValid` — whether this section's records are believed to
    /// match the blocks currently there. Nothing in this codebase re-derives
    /// POI from a block scan yet (see the module doc's scope note), so a
    /// Sections built here are valid at construction; the field preserves the
    /// validity flag when an existing section is read and written again.
    pub valid: bool,
    /// Every record in the section, keyed by nothing but its own `pos` — see
    /// [`Self::add`] for how a collision at the same position is resolved.
    pub records: Vec<PoiRecord>,
}

impl PoiSection {
    /// An empty, valid section — `new PoiSection(setDirty)`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            valid: true,
            records: Vec::new(),
        }
    }

    /// Adds a POI at `pos`, inlined with the record insertion it performs.
    ///
    /// Three cases are distinguished: a new position inserts and returns
    /// `true`; the same type at the position is a no-op returning `false`; a
    /// different type replaces the existing record and returns `true`.
    pub fn add(&mut self, pos: BlockPos, poi_type: ResourceKey) -> bool {
        if let Some(idx) = self.records.iter().position(|r| r.pos == pos) {
            if self.records[idx].poi_type == poi_type {
                return false;
            }
            self.records[idx] = PoiRecord::new(pos, poi_type);
            return true;
        }
        self.records.push(PoiRecord::new(pos, poi_type));
        true
    }

    /// Inserts an already-built record verbatim, replacing whatever was at
    /// `record.pos`. Unlike [`Self::add`], this keeps `record`'s own
    /// `free_tickets` rather than resetting it to the type's full count —
    /// for a caller that already knows a record's exact state, such as a
    /// conversion from a live in-memory index
    /// ([`crate::portal::poi_records_for_index`]) or a reload from disk,
    /// as opposed to [`Self::add`]'s use when a block was *just* discovered
    /// and its ticket count starts full.
    ///
    /// Returns `true` if this was a new position, `false` if it replaced an
    /// existing record.
    pub fn insert_record(&mut self, record: PoiRecord) -> bool {
        if let Some(idx) = self.records.iter().position(|r| r.pos == record.pos) {
            self.records[idx] = record;
            return false;
        }
        self.records.push(record);
        true
    }

    /// Removes the record at `pos`, returning `true` when one was present.
    pub fn remove(&mut self, pos: BlockPos) -> bool {
        let before = self.records.len();
        self.records.retain(|r| r.pos != pos);
        self.records.len() != before
    }

    /// Returns the record at `pos`, read-only.
    #[must_use]
    pub fn get(&self, pos: BlockPos) -> Option<&PoiRecord> {
        self.records.iter().find(|r| r.pos == pos)
    }

    /// The same record, mutable — for [`PoiRecord::acquire_ticket`]/
    /// [`PoiRecord::release_ticket`] callers.
    pub fn get_mut(&mut self, pos: BlockPos) -> Option<&mut PoiRecord> {
        self.records.iter_mut().find(|r| r.pos == pos)
    }

    /// Returns records matching a type predicate and occupancy state. The
    /// section is small enough that a linear scan is the appropriate index.
    pub fn records_matching<'a>(
        &'a self,
        mut type_predicate: impl FnMut(&ResourceKey) -> bool + 'a,
        occupancy: Occupancy,
    ) -> impl Iterator<Item = &'a PoiRecord> + 'a {
        self.records
            .iter()
            .filter(move |r| type_predicate(&r.poi_type))
            .filter(move |r| occupancy.test(r))
    }

    fn to_nbt(&self) -> Nbt {
        Nbt::Compound(vec![
            ("Valid".to_owned(), Nbt::Byte(i8::from(self.valid))),
            (
                "Records".to_owned(),
                Nbt::List {
                    element_type: NbtTag::Compound,
                    elements: self.records.iter().map(PoiRecord::to_nbt).collect(),
                },
            ),
        ])
    }

    /// Reads the optional `Valid` flag; omission decodes to `false`, while
    /// [`Self::new`] creates an in-memory section with `valid = true`.
    fn from_nbt(nbt: &Nbt) -> Self {
        let valid = matches!(field(nbt, "Valid"), Some(Nbt::Byte(b)) if *b != 0);
        let records = match field(nbt, "Records") {
            Some(Nbt::List { elements, .. }) => {
                elements.iter().filter_map(PoiRecord::from_nbt).collect()
            }
            _ => Vec::new(),
        };
        Self { valid, records }
    }
}

/// Claim state a record query can require.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Occupancy {
    /// At least one ticket remains available for a new claim.
    HasSpace,
    /// At least one ticket has been claimed.
    IsOccupied,
    /// No claim-state filter.
    Any,
}

impl Occupancy {
    fn test(self, record: &PoiRecord) -> bool {
        match self {
            Self::HasSpace => record.has_space(),
            Self::IsOccupied => record.is_occupied(),
            Self::Any => true,
        }
    }
}

/// Every section of one chunk column — the root of one `poi/` chunk NBT tree.
#[derive(Debug, Clone, Default)]
pub struct PoiChunk {
    /// Keyed by signed section Y (for example, `-4..=19` in the overworld).
    /// Sections with no POI are absent from the map.
    pub sections: BTreeMap<i32, PoiSection>,
}

impl PoiChunk {
    fn to_nbt(&self) -> Nbt {
        let sections: Vec<(String, Nbt)> = self
            .sections
            .iter()
            .map(|(y, section)| (y.to_string(), section.to_nbt()))
            .collect();
        Nbt::Compound(vec![
            ("Sections".to_owned(), Nbt::Compound(sections)),
            (
                "DataVersion".to_owned(),
                Nbt::Int(lodestone_anvil::level_dat::DATA_VERSION_26_2),
            ),
        ])
    }

    /// Refuses an unreadable `DataVersion`, same as every other
    /// region set.
    fn from_nbt(nbt: &Nbt) -> Result<Self, lodestone_anvil::Error> {
        lodestone_anvil::require_supported_data_version(match field(nbt, "DataVersion") {
            Some(Nbt::Int(v)) => Some(*v),
            _ => None,
        })?;
        let mut sections = BTreeMap::new();
        if let Some(Nbt::Compound(entries)) = field(nbt, "Sections") {
            for (key, value) in entries {
                if let Ok(y) = key.parse::<i32>() {
                    sections.insert(y, PoiSection::from_nbt(value));
                }
            }
        }
        Ok(Self { sections })
    }

    /// Total records across every section — the count
    /// [`PoiStorage::save`]/[`PoiStorage::save_region`] report.
    fn record_count(&self) -> usize {
        self.sections.values().map(|s| s.records.len()).sum()
    }
}

/// Reads and writes the `poi/` region set for one dimension.
///
/// Holds a directory path and nothing else — see the module doc on why
/// there is no lock here, matching [`crate::entity_storage::EntityStorage`].
#[derive(Debug, Clone)]
pub struct PoiStorage {
    dir: std::sync::Arc<PathBuf>,
}

impl PoiStorage {
    /// Roots a store at `world_dir`'s `<dimension>/poi` directory, creating
    /// it eagerly so a later save cannot fail for a reason the caller could
    /// have been told at world open — same reasoning as
    /// [`crate::entity_storage::EntityStorage::new`].
    ///
    /// The subdirectory name comes from [`Dimension::key`]
    /// (`"minecraft:overworld"` → `overworld`, `"minecraft:the_nether"` →
    /// `the_nether`), verified against `.cache/mc/survival/world`'s own
    /// layout: `dimensions/minecraft/overworld/poi` and
    /// `dimensions/minecraft/the_nether/poi`, siblings of each dimension's
    /// own `region/` and `entities/`.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] if the directory cannot be created.
    pub fn new(world_dir: &Path, dimension: Dimension) -> Result<Self, Error> {
        let dim_folder = dimension
            .key()
            .strip_prefix("minecraft:")
            .unwrap_or(dimension.key());
        let dir = world_dir
            .join("dimensions")
            .join("minecraft")
            .join(dim_folder)
            .join("poi");
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

    /// Every section stored in chunk column `(cx, cz)`.
    ///
    /// A missing region file, or a chunk never written, is
    /// [`PoiChunk::default`] — an empty section map — matching how
    /// [`crate::entity_storage::EntityStorage::load_chunk`] treats a chunk
    /// with no saved entities.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] if the file exists but cannot be read, or
    /// [`Error::Anvil`] if it exists and will not parse, including a
    /// `DataVersion` this build cannot read.
    pub fn load_chunk(&self, cx: i32, cz: i32) -> Result<PoiChunk, Error> {
        let (rx, rz, local_x, local_z) = region_and_local(cx, cz);
        let path = self.region_path(rx, rz);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Ok(PoiChunk::default());
            }
            Err(source) => return Err(Error::Io { path, source }),
        };
        let region = RegionFile::parse(&bytes).map_err(Error::Anvil)?;
        let Some(raw) = region
            .read_chunk_nbt_bytes_resolving_external(local_x, local_z, cx, cz, &self.dir)
            .map_err(Error::Anvil)?
        else {
            return Ok(PoiChunk::default());
        };
        let mut reader = Reader::new(&raw);
        let (_, nbt) = read_named_nbt(&mut reader).map_err(|e| Error::Anvil(lodestone_anvil::Error::Nbt(e)))?;
        PoiChunk::from_nbt(&nbt).map_err(Error::Anvil)
    }

    /// The `(rx, rz)` of every `r.<rx>.<rz>.mca` already in the directory —
    /// mirrors [`crate::entity_storage::EntityStorage`]'s own private helper of
    /// the same name and shape.
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

    /// Every section on disk, across every region file, regardless of chunk
    /// coordinate.
    ///
    /// Unlike [`Self::load_area`], this takes no range: a portal may be lit
    /// anywhere the player has walked, and [`crate::portal::PortalIndex`]'s own
    /// doc names exactly the failure of guessing a radius instead — the first
    /// return trip after a restart falls back to a small local scan and,
    /// beyond it, builds a duplicate. Restoring the *whole* store at world open
    /// (see `crate::integrated`) is what closes that gap rather than narrowing
    /// it. Affordable because the record count is small — the oracle world's
    /// own overworld set is 210 records across 124 chunks, nothing like the
    /// scale `region_source`'s terrain reads.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] on a filesystem failure, or [`Error::Anvil`] if a region
    /// file exists and will not parse, including an unreadable `DataVersion`.
    pub fn load_all(&self) -> Result<Vec<PoiSection>, Error> {
        let mut out = Vec::new();
        for (rx, rz) in self.existing_regions()? {
            let path = self.region_path(rx, rz);
            let bytes = std::fs::read(&path).map_err(|source| Error::Io {
                path: path.clone(),
                source,
            })?;
            let region = RegionFile::parse(&bytes).map_err(Error::Anvil)?;
            for local_z in 0..32u8 {
                for local_x in 0..32u8 {
                    let cx = rx * 32 + i32::from(local_x);
                    let cz = rz * 32 + i32::from(local_z);
                    let Some(raw) = region
                        .read_chunk_nbt_bytes_resolving_external(local_x, local_z, cx, cz, &self.dir)
                        .map_err(Error::Anvil)?
                    else {
                        continue;
                    };
                    let mut reader = Reader::new(&raw);
                    let (_, nbt) = read_named_nbt(&mut reader)
                        .map_err(|e| Error::Anvil(lodestone_anvil::Error::Nbt(e)))?;
                    let chunk = PoiChunk::from_nbt(&nbt).map_err(Error::Anvil)?;
                    out.extend(chunk.sections.into_values());
                }
            }
        }
        Ok(out)
    }

    /// Every populated chunk in the inclusive ranges `cx_range` × `cz_range`,
    /// keyed by chunk coordinate. A chunk with no saved POI is simply absent
    /// from the map (rather than present with an empty [`PoiChunk`]), so the
    /// caller can distinguish "nothing here" from "loaded, zero POI" if it
    /// ever needs to.
    ///
    /// # Errors
    ///
    /// As [`Self::load_chunk`].
    pub fn load_area(
        &self,
        cx_range: RangeInclusive<i32>,
        cz_range: RangeInclusive<i32>,
    ) -> Result<HashMap<(i32, i32), PoiChunk>, Error> {
        let mut out = HashMap::new();
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
                out.insert((cx, cz), PoiChunk::from_nbt(&nbt).map_err(Error::Anvil)?);
            }
        }
        Ok(out)
    }

    /// Returns every POI matching `type_predicate` and `occupancy` whose block
    /// position lies within `radius` blocks of `center`. Distance is Euclidean,
    /// and the disk is read from exactly the chunk-column range it can reach.
    /// A raid trigger uses `radius: 64` with `Occupancy::IsOccupied`, then
    /// averages the returned village records into its raid center; this method
    /// returns records without choosing that reduction.
    ///
    /// # Errors
    /// As [`Self::load_area`].
    pub fn occupied_in_range(
        &self,
        mut type_predicate: impl FnMut(&ResourceKey) -> bool,
        center: BlockPos,
        radius: i32,
        occupancy: Occupancy,
    ) -> Result<Vec<BlockPos>, Error> {
        let radius_sq = i64::from(radius) * i64::from(radius);
        // +1: a POI whose *chunk* is just outside the radius's chunk-aligned
        // bounding box can still have a block within `radius` of `center`
        // (the radius is measured in blocks, not chunks), so the loaded area
        // must be at least one chunk wider on every side than a naive
        // `radius / 16` — the exact real-distance filter below is what
        // trims the extra chunks' out-of-range records back out.
        let chunk_radius = radius.div_euclid(16) + 1;
        let center_cx = center.x.div_euclid(16);
        let center_cz = center.z.div_euclid(16);
        let area = self.load_area(
            (center_cx - chunk_radius)..=(center_cx + chunk_radius),
            (center_cz - chunk_radius)..=(center_cz + chunk_radius),
        )?;
        let mut out = Vec::new();
        for chunk in area.values() {
            for section in chunk.sections.values() {
                for record in section.records_matching(&mut type_predicate, occupancy) {
                    let dx = i64::from(record.pos.x - center.x);
                    let dy = i64::from(record.pos.y - center.y);
                    let dz = i64::from(record.pos.z - center.z);
                    if dx * dx + dy * dy + dz * dz <= radius_sq {
                        out.push(record.pos);
                    }
                }
            }
        }
        Ok(out)
    }

    /// Writes the given chunks' **complete** POI state to disk. A chunk not
    /// present in `chunks` is left byte-for-byte untouched — see the module
    /// doc on why POI needs no identity-clearing pass the way
    /// [`crate::entity_storage::EntityStorage::save`] does.
    ///
    /// Returns the total record count written.
    ///
    /// Blocking — call from `spawn_blocking` or at shutdown, never the tick
    /// loop, matching every other region writer here.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] on a filesystem failure or [`Error::Anvil`] if an
    /// existing region file cannot be parsed.
    pub fn save(&self, chunks: &HashMap<(i32, i32), PoiChunk>) -> Result<usize, Error> {
        let mut by_region: BTreeMap<(i32, i32), ()> = BTreeMap::new();
        for &(cx, cz) in chunks.keys() {
            let (rx, rz, _, _) = region_and_local(cx, cz);
            by_region.insert((rx, rz), ());
        }
        let mut written = 0usize;
        for (rx, rz) in by_region.into_keys() {
            written += self.save_region(rx, rz, chunks)?;
        }
        Ok(written)
    }

    /// Rewrites one region file. Chunks not in `chunks` pass through as their
    /// original compressed bytes, unchanged — the same shape
    /// [`crate::entity_storage::EntityStorage::save_region`] and
    /// [`crate::region_source`]'s terrain writer both use.
    fn save_region(
        &self,
        rx: i32,
        rz: i32,
        chunks: &HashMap<(i32, i32), PoiChunk>,
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

                if let Some(chunk) = chunks.get(&(cx, cz)) {
                    let nbt = chunk.to_nbt();
                    entries.push(ChunkToWrite {
                        chunk_x: cx,
                        chunk_z: cz,
                        compressed: SCHEME.compress(&encode_chunk(&nbt)?).map_err(Error::Anvil)?,
                        scheme: SCHEME,
                        timestamp,
                    });
                    written += chunk.record_count();
                    continue;
                }

                let Some(region) = existing.as_ref() else {
                    continue;
                };
                let stored_timestamp = region
                    .timestamp(local_x, local_z)
                    .map_err(Error::Anvil)?
                    .unwrap_or(0);
                match region.read_chunk_raw(local_x, local_z).map_err(Error::Anvil)? {
                    Some(RawChunk::Inline { scheme, compressed }) => {
                        entries.push(ChunkToWrite {
                            chunk_x: cx,
                            chunk_z: cz,
                            compressed,
                            scheme,
                            timestamp: stored_timestamp,
                        });
                    }
                    Some(RawChunk::External { .. }) => {
                        let raw = region
                            .read_chunk_nbt_bytes_resolving_external(
                                local_x, local_z, cx, cz, &self.dir,
                            )
                            .map_err(Error::Anvil)?
                            .unwrap_or_default();
                        entries.push(ChunkToWrite {
                            chunk_x: cx,
                            chunk_z: cz,
                            compressed: SCHEME.compress(&raw).map_err(Error::Anvil)?,
                            scheme: SCHEME,
                            timestamp: stored_timestamp,
                        });
                    }
                    None => continue,
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

fn field<'a>(nbt: &'a Nbt, key: &str) -> Option<&'a Nbt> {
    match nbt {
        Nbt::Compound(fields) => fields
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value),
        _ => None,
    }
}

fn encode_chunk(nbt: &Nbt) -> Result<Vec<u8>, Error> {
    let mut writer = Writer::default();
    write_named_nbt(&mut writer, "", nbt)
        .map_err(|e| Error::Anvil(lodestone_anvil::Error::Nbt(e)))?;
    Ok(writer.into_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lodestone-poi-storage-x7q3-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn portal_type() -> ResourceKey {
        "minecraft:nether_portal".parse().expect("valid")
    }

    fn home_type() -> ResourceKey {
        "minecraft:home".parse().expect("valid")
    }

    #[test]
    fn max_tickets_matches_poi_types_bootstrap() {
        // Pairwise-distinct spot checks cover ordinary, multi-claim, portal,
        // and unrecognised POI types.
        assert_eq!(max_tickets(&home_type()), 1);
        assert_eq!(
            max_tickets(&"minecraft:meeting".parse().expect("valid")),
            32
        );
        assert_eq!(max_tickets(&portal_type()), 0);
        assert_eq!(
            max_tickets(&"minecraft:bee_nest".parse().expect("valid")),
            0
        );
    }

    #[test]
    fn a_fresh_record_starts_with_its_types_full_ticket_count() {
        let home = PoiRecord::new(BlockPos::new(11, 70, -4), home_type());
        assert_eq!(home.free_tickets, 1);
        assert!(home.has_space());
        assert!(!home.is_occupied());

        let portal = PoiRecord::new(BlockPos::new(-56, 74, -107), portal_type());
        assert_eq!(portal.free_tickets, 0);
        assert!(!portal.has_space());
        // A zero-max-ticket type reads as "not occupied" — it was never
        // claimable to begin with, matching `isOccupied`'s "!= maxTickets"
        // definition rather than "== 0".
        assert!(!portal.is_occupied());
    }

    /// The discriminating property named in the module doc: a fully-claimed
    /// POI must not come back from an availability query. Two pairwise-
    /// distinct positions of the *same* type, one claimed and one not — a
    /// query that ignores occupancy entirely would return both.
    #[test]
    fn an_occupied_poi_is_excluded_from_a_has_space_query() {
        let mut section = PoiSection::new();
        let available_pos = BlockPos::new(3, 65, 9);
        let claimed_pos = BlockPos::new(-17, 68, 22);
        section.add(available_pos, home_type());
        section.add(claimed_pos, home_type());
        assert!(section.get_mut(claimed_pos).expect("added").acquire_ticket());
        assert_eq!(section.get(claimed_pos).expect("added").free_tickets, 0);

        let found: Vec<BlockPos> = section
            .records_matching(|t| t.path() == "home", Occupancy::HasSpace)
            .map(|r| r.pos)
            .collect();
        assert_eq!(
            found,
            vec![available_pos],
            "the claimed home must not be offered as available"
        );

        // Control: the detector actually distinguishes the two states —
        // `Occupancy::Any` returns both, so the exclusion above is the
        // occupancy filter's doing, not an artifact of only one record
        // existing.
        let any: Vec<BlockPos> = section
            .records_matching(|t| t.path() == "home", Occupancy::Any)
            .map(|r| r.pos)
            .collect();
        assert_eq!(any.len(), 2, "control: both records exist and match the type");
    }

    #[test]
    fn adding_the_same_type_twice_at_one_position_is_a_no_op() {
        let mut section = PoiSection::new();
        let pos = BlockPos::new(4, 70, -9);
        assert!(section.add(pos, home_type()));
        assert!(!section.add(pos, home_type()));
        assert_eq!(section.records.len(), 1);
    }

    #[test]
    fn adding_a_different_type_at_the_same_position_overwrites() {
        let mut section = PoiSection::new();
        let pos = BlockPos::new(4, 70, -9);
        section.add(pos, home_type());
        assert!(section.add(pos, portal_type()));
        assert_eq!(section.records.len(), 1);
        assert_eq!(section.get(pos).expect("present").poi_type, portal_type());
    }

    #[test]
    fn free_tickets_round_trips_through_nbt_and_omits_only_the_zero_case() {
        // Pairwise-distinct positions and a mix of zero/non-zero tickets, so
        // a transposition of `pos`/`type`/`free_tickets` cannot survive.
        let mut occupied_home = PoiRecord::new(BlockPos::new(11, 70, -4), home_type());
        assert!(occupied_home.acquire_ticket());
        let unclaimed_portal = PoiRecord::new(BlockPos::new(-385, 71, -897), portal_type());
        let partial_meeting = PoiRecord {
            pos: BlockPos::new(200, 80, 33),
            poi_type: "minecraft:meeting".parse().expect("valid"),
            free_tickets: 28,
        };

        for record in [&occupied_home, &unclaimed_portal, &partial_meeting] {
            let nbt = record.to_nbt();
            if record.free_tickets == 0 {
                assert!(
                    field(&nbt, "free_tickets").is_none(),
                    "a zero free_tickets must be omitted, matching the codec's default"
                );
            } else {
                assert_eq!(
                    field(&nbt, "free_tickets"),
                    Some(&Nbt::Int(record.free_tickets))
                );
            }
            let decoded = PoiRecord::from_nbt(&nbt).expect("decodes");
            assert_eq!(&decoded, record);
        }
    }

    #[test]
    fn a_poi_chunk_round_trips_through_the_real_save_path() {
        let dir = tempdir("chunk-round-trip");
        let storage = PoiStorage::new(&dir, Dimension::Overworld).expect("create");

        // Pairwise-distinct chunk coordinates and section Ys so a
        // transposition of chunk-x/chunk-z, or of section keys, cannot
        // survive unnoticed.
        let mut section_four = PoiSection::new();
        section_four.add(BlockPos::new(65, 71, 22), home_type());
        let mut section_six = PoiSection::new();
        section_six.add(BlockPos::new(68, 103, 26), portal_type());
        let mut chunk = PoiChunk::default();
        chunk.sections.insert(4, section_four);
        chunk.sections.insert(6, section_six);

        let mut to_save = HashMap::new();
        to_save.insert((4, 1), chunk.clone());
        let written = storage.save(&to_save).expect("save");
        assert_eq!(written, 2);

        // A second, untouched neighbour chunk in the same region must come
        // back empty rather than erroring — proves the pass-through path
        // for chunks outside `to_save` did not corrupt the region.
        let neighbour = storage.load_chunk(4, 2).expect("load");
        assert!(neighbour.sections.is_empty());

        let loaded = storage.load_chunk(4, 1).expect("load");
        assert_eq!(loaded.sections.len(), 2);
        let loaded_four = loaded.sections.get(&4).expect("section 4 present");
        assert_eq!(loaded_four.records.len(), 1);
        assert_eq!(loaded_four.records[0].pos, BlockPos::new(65, 71, 22));
        assert_eq!(loaded_four.records[0].poi_type, home_type());
        let loaded_six = loaded.sections.get(&6).expect("section 6 present");
        assert_eq!(loaded_six.records[0].poi_type, portal_type());

        // A second save that omits chunk (4, 1) must leave it untouched —
        // the "no identity clearing" contract the module doc states.
        let empty: HashMap<(i32, i32), PoiChunk> = HashMap::new();
        storage.save(&empty).expect("save with nothing does nothing");
        let still_there = storage.load_chunk(4, 1).expect("load");
        assert_eq!(still_there.sections.len(), 2, "an untouched chunk must survive a save that does not name it");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_poi_chunk_from_another_game_version_is_refused() {
        let ours = PoiChunk::default().to_nbt();
        assert!(PoiChunk::from_nbt(&ours).is_ok());

        let Nbt::Compound(mut fields) = ours else {
            unreachable!("to_nbt builds a compound")
        };
        for (name, value) in &mut fields {
            if name == "DataVersion" {
                *value = Nbt::Int(3955);
            }
        }
        assert!(matches!(
            PoiChunk::from_nbt(&Nbt::Compound(fields)),
            Err(lodestone_anvil::Error::UnsupportedDataVersion { .. })
        ));
    }

    /// The property [`PoiStorage::load_all`] exists for: it must find records
    /// **regardless of which region file they landed in**, not just the one a
    /// caller happened to guess. Two chunks placed far enough apart to land in
    /// different `.mca` region files (each region spans 32 chunks, so a gap
    /// past 32 chunks guarantees a different region), so a version that only
    /// scanned one region would silently miss the second.
    #[test]
    fn load_all_finds_records_across_multiple_region_files() {
        let dir = tempdir("load-all");
        let storage = PoiStorage::new(&dir, Dimension::Overworld).expect("create");

        let mut near = PoiSection::new();
        near.add(BlockPos::new(5, 70, 5), home_type());
        let mut far = PoiSection::new();
        far.add(BlockPos::new(4001, 71, -19), portal_type());

        let mut near_chunk = PoiChunk::default();
        near_chunk.sections.insert(4, near);
        let mut far_chunk = PoiChunk::default();
        far_chunk.sections.insert(4, far);

        let mut to_save = HashMap::new();
        // (0, 0) and (250, -1): region (0,0) vs region (7,-1) — genuinely
        // different `.mca` files.
        to_save.insert((0, 0), near_chunk);
        to_save.insert((250, -1), far_chunk);
        let written = storage.save(&to_save).expect("save");
        assert_eq!(written, 2);

        let all = storage.load_all().expect("load_all");
        let total_records: usize = all.iter().map(|s| s.records.len()).sum();
        assert_eq!(
            total_records, 2,
            "load_all must see records from every region file, not just one"
        );
        let types: Vec<&str> = {
            let mut v: Vec<&str> = all
                .iter()
                .flat_map(|s| s.records.iter())
                .map(|r| r.poi_type.path())
                .collect();
            v.sort_unstable();
            v
        };
        assert_eq!(types, vec!["home", "nether_portal"]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `occupied_in_range`'s whole reason to exist over a chunk-box scan:
    /// real Euclidean distance, not the padded chunk bounding box the load
    /// itself uses. Four pairwise-distinct positions, each excluded by a
    /// *different* one of the three filters, so no single assertion could
    /// pass by accident:
    ///
    /// - `inside`: occupied `home`, 30 blocks out — must be returned.
    /// - `far`: occupied `home`, inside the padded chunk box
    ///   (`chunk_radius = 64/16 + 1 = 5` chunks covers it) but its real
    ///   distance is `sqrt(60² + 60²) ≈ 84.9 > 64` — proves the query filters
    ///   by real distance, not merely by which chunks got loaded.
    /// - `unoccupied`: a `home` well inside 64 blocks that was never
    ///   claimed — excluded by the `Occupancy::IsOccupied` filter.
    /// - `wrong_type`: an occupied `nether_portal` a few blocks from centre —
    ///   excluded by the type predicate.
    #[test]
    fn occupied_in_range_filters_by_real_distance_type_and_occupancy() {
        let dir = tempdir("occupied-in-range");
        let storage = PoiStorage::new(&dir, Dimension::Overworld).expect("create");

        let center = BlockPos::new(0, 70, 0);
        let inside_pos = BlockPos::new(30, 70, 0);
        let far_pos = BlockPos::new(60, 70, 60);
        let unoccupied_pos = BlockPos::new(10, 70, -20);
        let wrong_type_pos = BlockPos::new(5, 70, 5);

        let mut to_save: HashMap<(i32, i32), PoiChunk> = HashMap::new();
        let mut place = |pos: BlockPos, ty: ResourceKey, claim: bool| {
            let cx = pos.x.div_euclid(16);
            let cz = pos.z.div_euclid(16);
            let mut record = PoiRecord::new(pos, ty);
            if claim {
                assert!(record.acquire_ticket(), "test fixture: type must have a ticket to claim");
            }
            let chunk = to_save.entry((cx, cz)).or_insert_with(PoiChunk::default);
            let section = chunk
                .sections
                .entry(pos.y.div_euclid(16))
                .or_insert_with(PoiSection::new);
            assert!(section.insert_record(record));
        };
        place(inside_pos, home_type(), true);
        place(far_pos, home_type(), true);
        place(unoccupied_pos, home_type(), false);
        place(wrong_type_pos, portal_type(), false);

        let written = storage.save(&to_save).expect("save");
        assert_eq!(written, 4);

        let found = storage
            .occupied_in_range(|t| t.path() == "home", center, 64, Occupancy::IsOccupied)
            .expect("query");
        assert_eq!(
            found,
            vec![inside_pos],
            "only the occupied home strictly inside the real 64-block radius must come back"
        );

        // Control: `Occupancy::Any` still respects distance and type, so the
        // three exclusions above are the filters at work, not an artifact of
        // the query returning nothing regardless of input.
        let mut any = storage
            .occupied_in_range(|t| t.path() == "home", center, 64, Occupancy::Any)
            .expect("query");
        any.sort_by_key(|p| (p.x, p.y, p.z));
        let mut expected_any = vec![inside_pos, unoccupied_pos];
        expected_any.sort_by_key(|p| (p.x, p.y, p.z));
        assert_eq!(
            any, expected_any,
            "control: both in-range homes exist regardless of occupancy"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A directory that does not exist yet (a dimension never written to) is
    /// an empty result, not an error — matching [`PoiStorage::load_chunk`]'s
    /// own not-found handling.
    #[test]
    fn load_all_on_an_empty_store_is_empty_not_an_error() {
        let dir = tempdir("load-all-empty");
        let storage = PoiStorage::new(&dir, Dimension::Nether).expect("create");
        let all = storage.load_all().expect("load_all on an empty store");
        assert!(all.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_poi_chunk_carries_no_position_field() {
        // The trap named in the module doc: unlike both siblings, a POI
        // chunk's coordinate lives only in the region container.
        let mut chunk = PoiChunk::default();
        chunk.sections.insert(0, PoiSection::new());
        let nbt = chunk.to_nbt();
        assert!(field(&nbt, "Position").is_none());
        assert!(field(&nbt, "xPos").is_none());
    }
}
