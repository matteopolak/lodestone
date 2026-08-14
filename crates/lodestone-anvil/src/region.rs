//! The Anvil region file (`.mca`) container format.
//!
//! This module is deliberately generic over what NBT tree a chunk holds — it
//! reads and writes "an arbitrary NBT blob at a given chunk coordinate", so
//! the exact same code works unchanged for entity storage
//! (`EntityStorage.java`'s separate `entities/` region files) once that
//! lands; nothing here parses a chunk's own schema (`SerializableChunkData.java`,
//! a different problem for a different module).
//!
//! # The container, cited against `.cache/mc/26.2/src/`
//!
//! `net/minecraft/world/level/chunk/storage/RegionFile.java`:
//!
//! - Each `.mca` file holds a 32×32 grid of chunks (`RegionFileStorage.getRegionFile`'s
//!   filename `r.<regionX>.<regionZ>.mca`), one region covering chunk
//!   coordinates `[regionX*32, regionX*32+31] × [regionZ*32, regionZ*32+31]`
//!   — `ChunkPos.getRegionX`/`getRegionLocalX`
//!   (`x >> 5` / `x & 31`; both operations are exact in Rust's two's
//!   complement `i32` too, including for negative coordinates).
//! - An 8192-byte (`SECTOR_BYTES * 2`, the `RegionFile.header` field) header: 1024
//!   big-endian `i32` **location** entries (the `offsets` field),
//!   then 1024 big-endian `i32` **timestamp** entries (the `timestamps`
//!   field), indexed by `localX + localZ*32`
//!   (`getOffsetIndex`).
//! - A location entry packs `sectorNumber << 8 | sectorCount`
//!   (`packSectorOffset`/`getSectorNumber`/`getNumSectors`);
//!   `0` means "chunk not present" (`CHUNK_NOT_PRESENT`)
//!   — **not** "corrupt". An all-zero header is a
//!   legal, empty region file.
//! - A "sector" is 4096 bytes (`SECTOR_BYTES`); sectors
//!   0 and 1 are always the header (`usedSectors.force(0, 2)`, in
//!   `RegionFile`'s constructor), so no chunk payload's `sectorNumber` is ever `<
//!   2`.
//! - At `sectorNumber * 4096`: a 5-byte chunk header (`CHUNK_HEADER_SIZE`)
//!   — a big-endian `i32` `length`, then one
//!   compression-scheme byte (see `compression.rs`) — followed by
//!   `length - 1` bytes of (still-)compressed payload
//!   (`getChunkDataInputStream`; the `- 1`/`+ 1`
//!   asymmetry is because `length` counts the scheme byte too:
//!   `ChunkBuffer.close`).
//! - If the scheme byte has bit `0x80` set (`EXTERNAL_STREAM_FLAG`, tested by
//!   `isExternalStreamChunk`), the payload is not inline: the true
//!   scheme is `versionId & !0x80` (`getExternalChunkVersion`)
//!   and the compressed bytes live in a sibling
//!   `c.<chunkX>.<chunkZ>.mcc` file (`EXTERNAL_FILE_EXTENSION`,
//!   `getExternalChunkPath`) with **no**
//!   envelope of its own — just the raw compressed bytes
//!   (`writeToExternalFile`/`createExternalChunkInputStream`). This triggers
//!   once a chunk needs `>= 256` sectors (`EXTERNAL_CHUNK_THRESHOLD`, tested
//!   in `RegionFile.write`),
//!   i.e. roughly a 1 MiB compressed payload — large enough that no test
//!   fixture in this crate exercises it; the write side is implemented from
//!   the source above but is unverified beyond its own round-trip test.
//! - `RegionBitmap` (`RegionBitmap.java`) is vanilla's sector allocator: a
//!   first-fit scan from sector 0 for the first run of `size` consecutive
//!   free sectors (`RegionBitmap.allocate`). The writer below
//!   implements the same first-fit-from-zero policy, so a region built here
//!   in ascending chunk order packs identically to how vanilla would if it
//!   wrote the same chunks in the same order — see
//!   `tests/region_container.rs` for a sector-offset prediction that checks
//!   this, though note it predicts *our own* allocator's output, which is a
//!   self-consistency check, not evidence of vanilla byte-for-byte parity
//!   (nothing here has been checked against vanilla's actual sector
//!   placement on a real multi-chunk file).
//! - `RegionFile.close`/`padToFullSector` pads
//!   the file to a whole number of sectors; the writer here does the
//!   equivalent by construction (every write is sector-aligned already).

use crate::{CompressionScheme, Error, Result};
use std::collections::BTreeMap;
use std::path::Path;

/// Bytes per sector (`RegionFile.SECTOR_BYTES`).
pub const SECTOR_BYTES: usize = 4096;
/// Sectors reserved for the header (`RegionFile`'s constructor: `usedSectors.force(0, 2)`).
pub const HEADER_SECTORS: usize = 2;
/// Header size in bytes: 1024 location `i32`s + 1024 timestamp `i32`s.
pub const HEADER_BYTES: usize = SECTOR_BYTES * HEADER_SECTORS;
/// Chunks per region file side (`RegionFile.getOffsetIndex`: `localZ * 32`).
pub const CHUNKS_PER_SIDE: usize = 32;
/// Location/timestamp table entry count.
const TABLE_ENTRIES: usize = CHUNKS_PER_SIDE * CHUNKS_PER_SIDE;
/// The 4-byte length prefix + 1-byte compression-scheme byte
/// (`RegionFile.CHUNK_HEADER_SIZE`).
const CHUNK_HEADER_SIZE: usize = 5;
/// `RegionFile.EXTERNAL_STREAM_FLAG`.
const EXTERNAL_STREAM_FLAG: u8 = 0x80;
/// `RegionFile.EXTERNAL_CHUNK_THRESHOLD`: a chunk
/// needing this many sectors or more is stored in a sibling `.mcc` file
/// instead.
const EXTERNAL_CHUNK_THRESHOLD_SECTORS: usize = 256;

/// Derives `(regionX, regionZ, localX, localZ)` from an absolute chunk
/// coordinate, matching `ChunkPos.getRegionX`/`getRegionLocalX`:
/// `coord >> 5` and `coord & 31`.
#[must_use]
pub fn region_and_local(chunk_x: i32, chunk_z: i32) -> (i32, i32, u8, u8) {
    (
        chunk_x >> 5,
        chunk_z >> 5,
        (chunk_x & 31) as u8,
        (chunk_z & 31) as u8,
    )
}

fn offset_index(local_x: u8, local_z: u8) -> Result<usize> {
    if local_x as usize >= CHUNKS_PER_SIDE || local_z as usize >= CHUNKS_PER_SIDE {
        return Err(Error::LocalCoordOutOfRange {
            x: local_x,
            z: local_z,
        });
    }
    Ok(local_x as usize + local_z as usize * CHUNKS_PER_SIDE)
}

fn pack_sector_offset(sector_number: u32, sector_count: u8) -> u32 {
    (sector_number << 8) | u32::from(sector_count)
}

fn sector_number(location: u32) -> u32 {
    (location >> 8) & 0x00FF_FFFF
}

fn sector_count(location: u32) -> u8 {
    (location & 0xFF) as u8
}

fn sectors_needed(byte_len: usize) -> usize {
    byte_len.div_ceil(SECTOR_BYTES)
}

/// Where a chunk's payload sits, straight out of the location table — no
/// decompression, no NBT parsing. Exists mainly so tests can predict and then
/// assert an exact sector number/count rather than only checking that a read
/// "worked".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkLocation {
    /// Absolute sector index into the file (sector 0 is the header's first
    /// half).
    pub sector_number: u32,
    /// Number of consecutive 4096-byte sectors the chunk occupies.
    pub sector_count: u8,
}

/// A chunk payload as read straight from the container, before
/// decompression: the compression scheme it declares, and either its inline
/// compressed bytes or a marker that the real bytes live in a sibling
/// `.mcc` file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RawChunk {
    /// Compressed bytes stored inline in this region file's sectors.
    Inline {
        /// The scheme the inline bytes are compressed under.
        scheme: CompressionScheme,
        /// The still-compressed payload bytes.
        compressed: Vec<u8>,
    },
    /// The payload is oversized and lives in `c.<chunkX>.<chunkZ>.mcc`
    /// (`EXTERNAL_STREAM_FLAG`) — see the module doc.
    External {
        /// The scheme the external file's bytes are compressed under.
        scheme: CompressionScheme,
    },
}

/// A parsed `.mca` file: the location/timestamp header, plus the full raw
/// byte contents (so sector payloads can be sliced out on demand).
#[derive(Debug, Clone)]
pub struct RegionFile {
    locations: [u32; TABLE_ENTRIES],
    timestamps: [u32; TABLE_ENTRIES],
    bytes: Vec<u8>,
}

impl RegionFile {
    /// Parses `bytes` as a region file's raw on-disk contents.
    ///
    /// An empty input is treated as a brand-new, never-saved region — legal
    /// and chunk-less, matching vanilla's own header initialization when
    /// `RegionFile`'s constructor opens a file that doesn't exist yet
    /// (`this.file.read(...)` returning `-1`, which skips the whole
    /// sanitation loop and leaves every location/timestamp at its
    /// zero-initialized default). A **nonzero** length shorter than the
    /// full 8192-byte header is different: vanilla's own constructor warns
    /// about this case (`"has truncated header"`) rather than accepting it,
    /// so this is the one input this parser rejects outright instead of
    /// degrading gracefully.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        Self::parse_owned(bytes.to_vec())
    }

    /// As [`Self::parse`], but takes ownership of an already-owned buffer
    /// instead of copying a borrowed one.
    ///
    /// This exists because [`Self::parse`]'s `bytes: &[u8]` forces every
    /// caller through a copy at its `bytes.to_vec()` — fine for a borrowed
    /// slice, but a caller that just did `std::fs::read` already owns a
    /// `Vec<u8>` and was paying for a **second** full-file copy to hand it
    /// in as a slice. `read_from_file` below is that caller; this is the
    /// primitive that lets it stop.
    pub fn parse_owned(bytes: Vec<u8>) -> Result<Self> {
        if bytes.is_empty() {
            return Ok(Self {
                locations: [0; TABLE_ENTRIES],
                timestamps: [0; TABLE_ENTRIES],
                bytes: vec![0u8; HEADER_BYTES],
            });
        }
        if bytes.len() < HEADER_BYTES {
            return Err(Error::TruncatedRegionHeader {
                available: bytes.len(),
            });
        }

        let mut locations = [0u32; TABLE_ENTRIES];
        let mut timestamps = [0u32; TABLE_ENTRIES];
        for (i, slot) in locations.iter_mut().enumerate() {
            *slot = read_be_u32(&bytes, i * 4);
        }
        for (i, slot) in timestamps.iter_mut().enumerate() {
            *slot = read_be_u32(&bytes, SECTOR_BYTES + i * 4);
        }

        // Mirror `RegionFile`'s own constructor-time sanitation: a location
        // entry whose sector overlaps
        // the header, whose sector count is 0, or whose sector range runs
        // past the end of the file is treated as "chunk not present" rather
        // than trusted as-is. Vanilla does this once at open time rather
        // than per-read, and only zeroes the *location* — timestamps are
        // left untouched, matching `RegionFile.java`'s own
        // `this.offsets.put(i, 0)` (no corresponding `timestamps.put`).
        // Without this, a hand-corrupted or foreign file with a location
        // entry pointing into the header itself would have
        // `read_chunk_raw` misinterpret header bytes as chunk data instead
        // of cleanly reporting the chunk absent.
        for location in &mut locations {
            if *location == 0 {
                continue;
            }
            let number = sector_number(*location);
            let count = sector_count(*location);
            let out_of_bounds = number < HEADER_SECTORS as u32
                || count == 0
                || (number as u64) * SECTOR_BYTES as u64 > bytes.len() as u64;
            if out_of_bounds {
                *location = 0;
            }
        }

        Ok(Self {
            locations,
            timestamps,
            bytes,
        })
    }

    /// Reads and parses `path` as a region file.
    ///
    /// Uses [`Self::parse_owned`] rather than [`Self::parse`] so the bytes
    /// `std::fs::read` allocates are moved straight into the result instead
    /// of being copied a second time.
    pub fn read_from_file(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path).map_err(Error::Io)?;
        Self::parse_owned(bytes)
    }

    /// Whether the location table has a nonzero entry for this chunk.
    pub fn has_chunk(&self, local_x: u8, local_z: u8) -> Result<bool> {
        let idx = offset_index(local_x, local_z)?;
        Ok(self.locations[idx] != 0)
    }

    /// The chunk's timestamp (epoch seconds, vanilla's `RegionFile.getTimestamp`),
    /// if present.
    pub fn timestamp(&self, local_x: u8, local_z: u8) -> Result<Option<u32>> {
        let idx = offset_index(local_x, local_z)?;
        if self.locations[idx] == 0 {
            return Ok(None);
        }
        Ok(Some(self.timestamps[idx]))
    }

    /// The chunk's sector location, if present.
    pub fn locate_chunk(&self, local_x: u8, local_z: u8) -> Result<Option<ChunkLocation>> {
        let idx = offset_index(local_x, local_z)?;
        let location = self.locations[idx];
        if location == 0 {
            return Ok(None);
        }
        Ok(Some(ChunkLocation {
            sector_number: sector_number(location),
            sector_count: sector_count(location),
        }))
    }

    /// Reads the chunk's payload straight out of its sectors, without
    /// decompressing. `Ok(None)` means the chunk is legitimately absent
    /// (location entry zero). An `Err` means the chunk *is* claimed present
    /// but its own declared metadata doesn't match the bytes actually
    /// there — the corrupt-input case, distinct from "not saved yet".
    pub fn read_chunk_raw(&self, local_x: u8, local_z: u8) -> Result<Option<RawChunk>> {
        let Some(location) = self.locate_chunk(local_x, local_z)? else {
            return Ok(None);
        };

        let sector_start = location.sector_number as usize * SECTOR_BYTES;
        let sector_span = location.sector_count as usize * SECTOR_BYTES;
        let sector_end =
            sector_start
                .checked_add(sector_span)
                .ok_or(Error::ChunkSectorOutOfBounds {
                    sector_number: location.sector_number,
                    sector_count: location.sector_count,
                    file_len: self.bytes.len(),
                })?;
        if sector_end > self.bytes.len() || sector_span < CHUNK_HEADER_SIZE {
            return Err(Error::ChunkSectorOutOfBounds {
                sector_number: location.sector_number,
                sector_count: location.sector_count,
                file_len: self.bytes.len(),
            });
        }

        let sector = &self.bytes[sector_start..sector_end];
        let declared_length = read_be_u32(sector, 0) as usize;
        if declared_length == 0 {
            return Err(Error::ChunkStreamMissing { local_x, local_z });
        }
        let version_byte = sector[4];
        let stream_length = declared_length - 1;

        let is_external = version_byte & EXTERNAL_STREAM_FLAG != 0;
        let scheme_id = version_byte & !EXTERNAL_STREAM_FLAG;
        let scheme = CompressionScheme::from_id(scheme_id)
            .ok_or(Error::UnsupportedCompressionScheme(scheme_id))?;

        if is_external {
            return Ok(Some(RawChunk::External { scheme }));
        }

        let available = sector.len() - CHUNK_HEADER_SIZE;
        if stream_length > available {
            return Err(Error::ChunkStreamTruncated {
                declared: stream_length,
                available,
            });
        }

        Ok(Some(RawChunk::Inline {
            scheme,
            compressed: sector[CHUNK_HEADER_SIZE..CHUNK_HEADER_SIZE + stream_length].to_vec(),
        }))
    }

    /// Reads and decompresses a chunk's raw NBT bytes (still-encoded named
    /// NBT — pass through [`lodestone_core::read_named_nbt`] to get a tree).
    /// Returns `Err(Error::ExternalChunkNeedsResolver)` for an external
    /// chunk; use [`Self::read_chunk_nbt_bytes_resolving_external`] when the
    /// region's chunks might be oversized.
    pub fn read_chunk_nbt_bytes(&self, local_x: u8, local_z: u8) -> Result<Option<Vec<u8>>> {
        match self.read_chunk_raw(local_x, local_z)? {
            None => Ok(None),
            Some(RawChunk::External { .. }) => Err(Error::ExternalChunkNeedsResolver),
            Some(RawChunk::Inline { scheme, compressed }) => {
                Ok(Some(scheme.decompress(&compressed)?))
            }
        }
    }

    /// As [`Self::read_chunk_nbt_bytes`], but resolves an external chunk by
    /// reading `<external_dir>/c.<chunk_x>.<chunk_z>.mcc` — vanilla's own
    /// naming (`RegionFile.getExternalChunkPath`), where
    /// `chunk_x`/`chunk_z` are **absolute** chunk coordinates, not the
    /// region-local ones this file's location table is indexed by.
    pub fn read_chunk_nbt_bytes_resolving_external(
        &self,
        local_x: u8,
        local_z: u8,
        chunk_x: i32,
        chunk_z: i32,
        external_dir: &Path,
    ) -> Result<Option<Vec<u8>>> {
        match self.read_chunk_raw(local_x, local_z)? {
            None => Ok(None),
            Some(RawChunk::Inline { scheme, compressed }) => {
                Ok(Some(scheme.decompress(&compressed)?))
            }
            Some(RawChunk::External { scheme }) => {
                let path = external_dir.join(format!("c.{chunk_x}.{chunk_z}.mcc"));
                let compressed = std::fs::read(&path).map_err(Error::Io)?;
                Ok(Some(scheme.decompress(&compressed)?))
            }
        }
    }
}

fn read_be_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_be_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

/// One chunk to place into a region file being built.
#[derive(Debug, Clone)]
pub struct ChunkToWrite {
    /// Absolute chunk X coordinate (used only to derive the region-local
    /// index and, if the payload turns out to be oversized, the external
    /// `.mcc` filename).
    pub chunk_x: i32,
    /// Absolute chunk Z coordinate.
    pub chunk_z: i32,
    /// Already-compressed payload bytes.
    pub compressed: Vec<u8>,
    /// The scheme `compressed` was compressed under.
    pub scheme: CompressionScheme,
    /// Epoch-second timestamp to record for this chunk.
    pub timestamp: u32,
}

/// The result of building a region file: the `.mca` bytes themselves, plus
/// any oversized chunks that had to be externalized (each needing its own
/// `c.<chunk_x>.<chunk_z>.mcc` file written alongside the `.mca`, with no
/// envelope of its own — just the bytes here, verbatim).
#[derive(Debug, Clone)]
pub struct BuiltRegion {
    /// The complete `.mca` file contents.
    pub bytes: Vec<u8>,
    /// `(chunk_x, chunk_z, compressed_bytes)` for every chunk that exceeded
    /// [`EXTERNAL_CHUNK_THRESHOLD_SECTORS`] sectors and was externalized.
    pub external: Vec<(i32, i32, Vec<u8>)>,
}

/// Builds a region file from a set of already-compressed chunk payloads.
///
/// Sector allocation is first-fit from sector 2 onward (the same policy as
/// vanilla's `RegionBitmap.allocate`), scanning
/// chunks in the order given — pass chunks in ascending `(region-local
/// index)` order for a deterministic, minimal-size layout; any order
/// produces a valid file, just not necessarily the most compact one.
///
/// All of `entries` must belong to the same region (same `chunk_x >> 5,
/// chunk_z >> 5`); this is a container-format primitive, not a
/// multi-region splitter.
pub fn build_region(entries: &[ChunkToWrite]) -> Result<BuiltRegion> {
    let mut locations = [0u32; TABLE_ENTRIES];
    let mut timestamps = [0u32; TABLE_ENTRIES];
    let mut body = Vec::new();
    let mut external = Vec::new();
    // Sectors 0 and 1 (the header) are always used.
    let mut used_sectors: usize = HEADER_SECTORS;

    for entry in entries {
        let (_, _, local_x, local_z) = region_and_local(entry.chunk_x, entry.chunk_z);
        let idx = offset_index(local_x, local_z)?;

        let inline_len = CHUNK_HEADER_SIZE + entry.compressed.len();
        let needed = sectors_needed(inline_len);

        if needed >= EXTERNAL_CHUNK_THRESHOLD_SECTORS {
            // Oversized: a one-sector stub in the region file, real bytes
            // externalized (`RegionFile.write`'s oversized-chunk branch).
            let sector_number = used_sectors as u32;
            let mut stub = vec![0u8; SECTOR_BYTES];
            stub[0..4].copy_from_slice(&1u32.to_be_bytes());
            stub[4] = entry.scheme.id() | EXTERNAL_STREAM_FLAG;
            body.extend_from_slice(&stub);
            used_sectors += 1;

            locations[idx] = pack_sector_offset(sector_number, 1);
            timestamps[idx] = entry.timestamp;
            external.push((entry.chunk_x, entry.chunk_z, entry.compressed.clone()));
            continue;
        }

        let sector_number = used_sectors as u32;
        let mut chunk_sectors = vec![0u8; needed * SECTOR_BYTES];
        chunk_sectors[0..4].copy_from_slice(&((entry.compressed.len() + 1) as u32).to_be_bytes());
        chunk_sectors[4] = entry.scheme.id();
        chunk_sectors[CHUNK_HEADER_SIZE..CHUNK_HEADER_SIZE + entry.compressed.len()]
            .copy_from_slice(&entry.compressed);
        body.extend_from_slice(&chunk_sectors);
        used_sectors += needed;

        let sector_count = u8::try_from(needed).map_err(|_| Error::ChunkTooManySectors {
            sectors: needed,
        })?;
        locations[idx] = pack_sector_offset(sector_number, sector_count);
        timestamps[idx] = entry.timestamp;
    }

    let mut bytes = Vec::with_capacity(HEADER_BYTES + body.len());
    for location in locations {
        bytes.extend_from_slice(&location.to_be_bytes());
    }
    for timestamp in timestamps {
        bytes.extend_from_slice(&timestamp.to_be_bytes());
    }
    bytes.extend_from_slice(&body);

    Ok(BuiltRegion { bytes, external })
}

/// Convenience over [`build_region`] for the common case: a set of chunk NBT
/// trees, compressed under a single scheme (vanilla writes an entire file
/// under whatever `region-file-compression` is currently configured, though
/// nothing stops per-chunk mixing at the container level — see the module
/// doc).
pub fn build_region_from_nbt(
    chunks: &BTreeMap<(i32, i32), lodestone_core::Nbt>,
    scheme: CompressionScheme,
    timestamp: u32,
) -> Result<BuiltRegion> {
    let mut entries = Vec::with_capacity(chunks.len());
    for (&(chunk_x, chunk_z), nbt) in chunks {
        let mut writer = lodestone_core::Writer::default();
        lodestone_core::write_named_nbt(&mut writer, "", nbt).map_err(Error::Nbt)?;
        let compressed = scheme.compress(&writer.into_vec())?;
        entries.push(ChunkToWrite {
            chunk_x,
            chunk_z,
            compressed,
            scheme,
            timestamp,
        });
    }
    build_region(&entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestone_core::Nbt;

    fn sample_chunk_nbt(x: i32, z: i32) -> Nbt {
        Nbt::Compound(vec![
            ("xPos".to_string(), Nbt::Int(x)),
            ("zPos".to_string(), Nbt::Int(z)),
            ("DataVersion".to_string(), Nbt::Int(4903)),
        ])
    }

    #[test]
    fn empty_bytes_parse_as_an_empty_region() {
        let region = RegionFile::parse(&[]).expect("empty region parses");
        assert!(!region.has_chunk(0, 0).expect("in range"));
        assert_eq!(region.locate_chunk(0, 0).expect("in range"), None);
    }

    #[test]
    fn truncated_nonzero_header_is_rejected() {
        // The corrupt-input control for header parsing: a file that exists
        // but is shorter than the 8192-byte header is what vanilla's own
        // constructor warns about, distinct from the
        // zero-length "never saved" case just above, which must NOT error.
        let err = RegionFile::parse(&[0u8; 100]).expect_err("truncated header must error");
        assert!(matches!(err, Error::TruncatedRegionHeader { available: 100 }));
    }

    #[test]
    fn out_of_range_local_coordinate_errors() {
        let region = RegionFile::parse(&[]).expect("empty region parses");
        assert!(matches!(
            region.has_chunk(32, 0),
            Err(Error::LocalCoordOutOfRange { x: 32, z: 0 })
        ));
    }

    #[test]
    fn region_and_local_matches_chunk_pos_formula() {
        // Predicted from `ChunkPos.getRegionX`/`getRegionLocalX` (`x >> 5`,
        // `x & 31`), not measured against any file — a pure arithmetic check
        // that negative
        // coordinates floor rather than truncate, which is where a naive
        // `%`-based reimplementation would diverge from Java's `&`.
        assert_eq!(region_and_local(0, 0), (0, 0, 0, 0));
        assert_eq!(region_and_local(31, 31), (0, 0, 31, 31));
        assert_eq!(region_and_local(32, 32), (1, 1, 0, 0));
        assert_eq!(region_and_local(-1, -1), (-1, -1, 31, 31));
        assert_eq!(region_and_local(-32, -32), (-1, -1, 0, 0));
        assert_eq!(region_and_local(-33, -33), (-2, -2, 31, 31));
    }

    #[test]
    fn single_chunk_round_trips_with_a_predicted_sector_offset() {
        let mut chunks = BTreeMap::new();
        chunks.insert((0, 0), sample_chunk_nbt(0, 0));
        let built = build_region_from_nbt(&chunks, CompressionScheme::Zlib, 12345)
            .expect("builds");
        assert!(built.external.is_empty());

        // Prediction: the only chunk lands at sector 2 (the first sector
        // after the 2-sector header), and — since a tiny 3-field compound
        // compresses to well under 4096 bytes — occupies exactly 1 sector.
        // File length is therefore exactly 3 sectors (12288 bytes): 2 header
        // + 1 chunk.
        let region = RegionFile::parse(&built.bytes).expect("parses");
        let location = region
            .locate_chunk(0, 0)
            .expect("in range")
            .expect("chunk present");
        assert_eq!(
            location,
            ChunkLocation {
                sector_number: 2,
                sector_count: 1,
            },
            "predicted sector_number=2, sector_count=1 for the region's only chunk"
        );
        assert_eq!(built.bytes.len(), 3 * SECTOR_BYTES);
        assert_eq!(
            region.timestamp(0, 0).expect("in range"),
            Some(12345)
        );

        let raw = region
            .read_chunk_nbt_bytes(0, 0)
            .expect("reads")
            .expect("present");
        let mut reader = lodestone_core::Reader::new(&raw);
        let (_, decoded) = lodestone_core::read_named_nbt(&mut reader).expect("decodes");
        assert_eq!(decoded, sample_chunk_nbt(0, 0));
    }

    #[test]
    fn multiple_chunks_pack_into_consecutive_sectors() {
        let mut chunks = BTreeMap::new();
        for x in 0..3 {
            chunks.insert((x, 0), sample_chunk_nbt(x, 0));
        }
        let built = build_region_from_nbt(&chunks, CompressionScheme::Zlib, 1)
            .expect("builds");
        let region = RegionFile::parse(&built.bytes).expect("parses");

        // Predicted: three small chunks, each 1 sector, packed in ascending
        // local-index order starting right after the header.
        for (i, x) in (0..3).enumerate() {
            let location = region
                .locate_chunk(x as u8, 0)
                .expect("in range")
                .expect("present");
            assert_eq!(location.sector_number, 2 + i as u32);
            assert_eq!(location.sector_count, 1);
        }
        assert_eq!(built.bytes.len(), 5 * SECTOR_BYTES);
    }

    #[test]
    fn absent_chunk_reads_as_none_not_an_error() {
        let mut chunks = BTreeMap::new();
        chunks.insert((0, 0), sample_chunk_nbt(0, 0));
        let built = build_region_from_nbt(&chunks, CompressionScheme::Zlib, 1)
            .expect("builds");
        let region = RegionFile::parse(&built.bytes).expect("parses");

        // Control: chunk (0,0) IS present and reads Ok(Some(_)); chunk
        // (1,0) was never written and must read Ok(None) — proving "empty
        // sector table" degrades cleanly rather than the whole file being
        // treated as absent or erroring.
        assert!(region.read_chunk_nbt_bytes(0, 0).expect("reads").is_some());
        assert!(region.read_chunk_nbt_bytes(1, 0).expect("reads").is_none());
    }

    #[test]
    fn corrupt_declared_length_errors_without_panicking() {
        // Corrupt-input control: hand-build a one-chunk region, then inflate
        // its declared stream length past what the sector actually holds.
        // The control half is `single_chunk_round_trips_with_a_predicted_sector_offset`
        // above, which proves the same code path accepts a well-formed
        // file — so a failure here is the length check firing, not the
        // parser rejecting everything indiscriminately.
        let mut chunks = BTreeMap::new();
        chunks.insert((0, 0), sample_chunk_nbt(0, 0));
        let built = build_region_from_nbt(&chunks, CompressionScheme::Zlib, 1)
            .expect("builds");
        let mut corrupt = built.bytes;
        let chunk_header_start = 2 * SECTOR_BYTES;
        // Declared length says "4096 bytes of payload", vastly more than
        // the single sector actually reserved for this chunk.
        corrupt[chunk_header_start..chunk_header_start + 4]
            .copy_from_slice(&4096u32.to_be_bytes());

        let region = RegionFile::parse(&corrupt).expect("header still parses");
        let err = region
            .read_chunk_nbt_bytes(0, 0)
            .expect_err("declared length exceeding the sector must error, not panic");
        assert!(matches!(err, Error::ChunkStreamTruncated { .. }));
    }

    #[test]
    fn unsupported_compression_scheme_id_errors_cleanly() {
        let mut chunks = BTreeMap::new();
        chunks.insert((0, 0), sample_chunk_nbt(0, 0));
        let built = build_region_from_nbt(&chunks, CompressionScheme::Zlib, 1)
            .expect("builds");
        let mut corrupt = built.bytes;
        let scheme_byte = 2 * SECTOR_BYTES + 4;
        // 127 is `RegionFileVersion.VERSION_CUSTOM` — a real, reserved id
        // that this crate deliberately does not implement (see
        // `compression.rs`'s doc); 99 is not a real id at all. Either way,
        // this must be a clean error, not a panic or a silent
        // misinterpretation of the bytes as some other scheme.
        corrupt[scheme_byte] = 99;

        let region = RegionFile::parse(&corrupt).expect("header still parses");
        let err = region
            .read_chunk_nbt_bytes(0, 0)
            .expect_err("unknown scheme id must error");
        assert!(matches!(err, Error::UnsupportedCompressionScheme(99)));
    }

    #[test]
    fn oversized_chunk_is_externalized() {
        // A payload that needs >= 256 sectors (~1 MiB) must not be inlined —
        // build a compressed blob that size directly (bypassing real
        // compression, since incompressible input this large is the whole
        // point) and confirm it takes the external path.
        let oversized_compressed = vec![0xABu8; EXTERNAL_CHUNK_THRESHOLD_SECTORS * SECTOR_BYTES];
        let entries = vec![ChunkToWrite {
            chunk_x: 5,
            chunk_z: 9,
            compressed: oversized_compressed.clone(),
            scheme: CompressionScheme::Uncompressed,
            timestamp: 1,
        }];
        let built = build_region(&entries).expect("builds");
        assert_eq!(built.external.len(), 1);
        assert_eq!(built.external[0].0, 5);
        assert_eq!(built.external[0].1, 9);
        assert_eq!(built.external[0].2, oversized_compressed);

        let region = RegionFile::parse(&built.bytes).expect("parses");
        let raw = region
            .read_chunk_raw(5, 9)
            .expect("in range")
            .expect("present");
        assert!(matches!(
            raw,
            RawChunk::External {
                scheme: CompressionScheme::Uncompressed
            }
        ));
        assert!(matches!(
            region.read_chunk_nbt_bytes(5, 9),
            Err(Error::ExternalChunkNeedsResolver)
        ));
    }

    #[test]
    fn a_location_entry_pointing_into_the_header_degrades_to_absent() {
        // Mirrors `RegionFile`'s own constructor-time sanitation: a header whose
        // location table claims sector 1 (inside the header itself, which
        // is always sectors 0-1) must be treated as "not present", not
        // trusted and misread as chunk data. Control: chunk (1,0) is a
        // genuine, untouched chunk in the same file and must still read
        // fine — proving this is the corrupt entry being degraded, not the
        // whole file failing closed.
        let mut chunks = BTreeMap::new();
        chunks.insert((0, 0), sample_chunk_nbt(0, 0));
        chunks.insert((1, 0), sample_chunk_nbt(1, 0));
        let built = build_region_from_nbt(&chunks, CompressionScheme::Zlib, 1)
            .expect("builds");
        let mut corrupt = built.bytes;
        // Location entry for (0,0) is index 0, at header bytes [0..4).
        // Overwrite it to claim sector 1 (inside the header) with a
        // plausible sector count.
        corrupt[0..4].copy_from_slice(&pack_sector_offset(1, 1).to_be_bytes());

        let region = RegionFile::parse(&corrupt).expect("header still parses");
        assert_eq!(
            region.locate_chunk(0, 0).expect("in range"),
            None,
            "a location claiming a header sector must degrade to absent"
        );
        assert!(
            region
                .locate_chunk(1, 0)
                .expect("in range")
                .is_some(),
            "the untouched sibling chunk must still be readable"
        );
    }
}
