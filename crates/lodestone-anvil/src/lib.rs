//! # `lodestone-anvil`
//!
//! ## What it is
//!
//! A version-free reader/writer for Minecraft's on-disk world persistence
//! formats: the Anvil region file (`.mca`, issue
//! [#298](https://github.com/matteopolak/lodestone/issues/298)) and
//! `level.dat` world metadata (issue
//! [#300](https://github.com/matteopolak/lodestone/issues/300)). It is the
//! first thing in this repo that can save or load a world at all — before
//! this crate, `grep -rln 'RegionFile\|\.mca\b|region_file|Anvil\b'` across
//! every `.rs` file in the workspace returned nothing.
//!
//! ## How it works
//!
//! Two independent container formats, both built on the NBT codec already
//! in `lodestone-core` (`read_named_nbt`/`write_named_nbt` — both formats use
//! the classic *named*-root NBT form, an empty-string root name, not the
//! nameless "network NBT" form the protocol crates use elsewhere):
//!
//! - [`region`]: the `.mca` container — an 8 KiB header of 1024 sector
//!   locations + 1024 timestamps, 4 KiB sector-addressed chunk payloads,
//!   oversized-chunk externalization to `.mcc` files. Deliberately generic
//!   over the NBT tree a chunk holds; parses no chunk *schema*
//!   (`SerializableChunkData.java`'s territory), only the envelope around
//!   it, so the same code is meant to be reusable for entity-region storage
//!   later without changes.
//! - [`level_dat`]: `level.dat`'s container — an un-chunked, gzip-wrapped
//!   NBT file, and the `DataVersion` field inside it. Everything else in
//!   `LevelData` (seed, spawn, gamerules, weather, ...) is explicitly
//!   *not* modelled yet — issue #300's own text says to sequence that
//!   against whichever issue settles each field's in-memory representation
//!   first, rather than guess a schema here that would need a second pass
//!   per subsystem landed afterward.
//! - [`compression`]: the compression-scheme byte shared by both formats
//!   (gzip/zlib/none/lz4 for region chunks; always gzip for `level.dat`).
//! - [`lz4_block`]: the third-party `net.jpountz.lz4` block-stream framing
//!   the `lz4` region scheme uses — its own module because the framing has
//!   nothing to do with Minecraft's own format and is reverse-engineered
//!   from a `.jar`'s class file rather than cited against decompiled
//!   Minecraft source.
//!
//! ## How to change it, and the gotchas
//!
//! - **This crate is deliberately not wired into `lodestone-server`.**
//!   Nothing in the workspace calls into it yet — a declared island, per
//!   `HANDOFF.md`'s standing-ledger convention and tracked on
//!   [#436](https://github.com/matteopolak/lodestone/issues/436). The
//!   wiring itself — hooking chunk load/save into whatever chunk source
//!   `lodestone-server` currently has, deciding the in-memory chunk schema
//!   a `Nbt` tree maps to/from, and `level.dat` load/save on world open —
//!   is issue [#437](https://github.com/matteopolak/lodestone/issues/437).
//!   Land that wiring there, not here; this crate should stay usable
//!   without depending on `lodestone-server`, `lodestone-world`, or any
//!   protocol crate (verified: `cargo tree -p lodestone-anvil` pulls in
//!   only `lodestone-core`, `flate2`, `thiserror`, `lz4_flex`,
//!   `xxhash-rust`, and their own transitive dependencies).
//! - **The container format and the chunk NBT schema are two different
//!   problems** (issue #298's own stated trap). Don't grow `region` a
//!   dependency on chunk internals to "make reading more convenient" —
//!   that dependency belongs in #437's wiring code, operating on the
//!   [`lodestone_core::Nbt`] tree this crate hands back.
//! - **Zero-length input and short-but-nonzero input are different
//!   errors**, and mixing them up regresses a real vanilla behaviour: an
//!   empty/nonexistent region file is a legal, chunk-less region (vanilla
//!   itself treats "file doesn't exist yet" this way); a file that exists
//!   but is shorter than the 8192-byte header is corrupt and
//!   [`region::RegionFile::parse`] rejects it. See that function's doc and
//!   `tests/region_container.rs`'s `truncated_nonzero_header_is_rejected`
//!   for the control that proves the distinction is actually enforced.
//! - **The LZ4 compression scheme has no real-file evidence behind it** —
//!   see `lz4_block`'s module doc for why (none of this repo's oracles
//!   configure `region-file-compression: lz4`) and re-verify against a real
//!   `lz4`-configured server before relying on it for anything real.
//!
//! ## Configuration
//!
//! None — this crate has no env vars or flags. `region-file-compression` is
//! a *server* setting (`server.properties`) that only ever appears here as
//! the compression-scheme byte on already-produced bytes; this crate does
//! not choose it, callers do (via [`compression::CompressionScheme`]
//! passed to [`region::build_region`]/[`region::build_region_from_nbt`]).
//!
//! ## Dependencies
//!
//! `lodestone-core` (NBT codec, shared with the protocol crates),
//! `flate2` (gzip/zlib, already a workspace dependency for packet
//! compression), and two dependencies added directly to this crate's own
//! `Cargo.toml` rather than the root workspace manifest — `lz4_flex` (raw
//! LZ4 block compression) and `xxhash-rust` (the LZ4 block-stream
//! checksum). No filesystem framework, no async runtime, no protocol crate.

pub mod compression;
mod lz4_block;
pub mod level_dat;
pub mod region;

pub use compression::CompressionScheme;

/// Convenient result alias for this crate's operations.
pub type Result<T> = core::result::Result<T, Error>;

/// Errors this crate's readers and writers can produce.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// An underlying filesystem operation failed.
    #[error("io error: {0}")]
    Io(#[source] std::io::Error),
    /// The NBT codec (`lodestone-core`) failed to decode or encode a value.
    #[error("nbt error: {0}")]
    Nbt(#[source] lodestone_core::Error),

    /// A region file's byte length was nonzero but shorter than the
    /// mandatory 8192-byte header. Distinct from a zero-length input, which
    /// [`region::RegionFile::parse`] treats as a legal empty region.
    #[error("region header truncated: {available} bytes available, need 8192")]
    TruncatedRegionHeader {
        /// Bytes actually available.
        available: usize,
    },
    /// A region-local chunk coordinate was outside `0..32`.
    #[error("region-local coordinate ({x}, {z}) out of the 0..32 range")]
    LocalCoordOutOfRange {
        /// The out-of-range local X.
        x: u8,
        /// The out-of-range local Z.
        z: u8,
    },
    /// A chunk's location entry pointed at sectors the file doesn't
    /// actually contain (or fewer than the 5-byte chunk header needs).
    #[error(
        "chunk sector out of bounds: sector {sector_number} x {sector_count} sectors, file is {file_len} bytes"
    )]
    ChunkSectorOutOfBounds {
        /// The declared starting sector.
        sector_number: u32,
        /// The declared sector count.
        sector_count: u8,
        /// The file's actual byte length.
        file_len: usize,
    },
    /// A chunk's location entry was nonzero but its declared stream length
    /// was 0 — present in the table, but with no data
    /// (`RegionFile.java:131-134`'s "Chunk is allocated, but stream is
    /// missing").
    #[error("chunk ({local_x}, {local_z}) is allocated but its stream is missing")]
    ChunkStreamMissing {
        /// Region-local X.
        local_x: u8,
        /// Region-local Z.
        local_z: u8,
    },
    /// A chunk's declared stream length exceeded the bytes actually
    /// available in its allocated sectors.
    #[error("chunk stream truncated: declared {declared} bytes, {available} available")]
    ChunkStreamTruncated {
        /// The declared payload length.
        declared: usize,
        /// The bytes actually available in the allocated sectors.
        available: usize,
    },
    /// A chunk declared a compression-scheme byte this crate does not
    /// recognize as decodable (i.e. not one of the four ids in
    /// [`compression::CompressionScheme`], and not the "chunk absent"
    /// sentinel 0). Notably includes 127 (`RegionFileVersion.VERSION_CUSTOM`),
    /// which vanilla itself immediately errors on decoding.
    #[error("unsupported compression scheme id {0}")]
    UnsupportedCompressionScheme(u8),
    /// A chunk's compressed bytes needed more than 254 sectors while being
    /// written inline (shouldn't happen: [`region::build_region`]
    /// externalizes anything at or above the 256-sector threshold before
    /// this could occur; kept as a defensive check rather than an
    /// `unwrap`).
    #[error("chunk needs {sectors} sectors, more than a single location entry can address")]
    ChunkTooManySectors {
        /// The number of sectors computed for this chunk.
        sectors: usize,
    },
    /// [`region::RegionFile::read_chunk_nbt_bytes`] was called on a chunk
    /// stored externally (a sibling `.mcc` file) without a resolver able to
    /// read it. Use
    /// [`region::RegionFile::read_chunk_nbt_bytes_resolving_external`]
    /// instead.
    #[error("chunk is stored externally (.mcc); use read_chunk_nbt_bytes_resolving_external")]
    ExternalChunkNeedsResolver,

    /// An LZ4-block-framed stream ended (or was truncated) before a full
    /// 21-byte block header could be read.
    #[error("lz4 block header truncated: {available} bytes available, need 21")]
    TruncatedLz4Header {
        /// Bytes actually available.
        available: usize,
    },
    /// An LZ4-block-framed stream's magic bytes did not match `"LZ4Block"`.
    #[error("lz4 block stream has an invalid magic value")]
    InvalidLz4Magic,
    /// An LZ4 block's declared compressed length exceeded the bytes
    /// actually remaining in the stream.
    #[error("lz4 block body truncated: declared {declared} bytes, {available} available")]
    TruncatedLz4Body {
        /// The declared compressed length.
        declared: usize,
        /// The bytes actually remaining.
        available: usize,
    },
    /// An LZ4 block's token declared a compression method other than
    /// `COMPRESSION_METHOD_RAW` (0x10) or `COMPRESSION_METHOD_LZ4` (0x20).
    #[error("lz4 block has an unknown compression method byte {0:#x}")]
    UnknownLz4Method(u8),
    /// The `lz4_flex` block decompressor rejected an LZ4 block's bytes.
    #[error(
        "lz4 block failed to decompress (declared original length {original_length}, compressed length {compressed_length})"
    )]
    Lz4Decompress {
        /// The block's declared decompressed length.
        original_length: usize,
        /// The block's declared compressed length.
        compressed_length: usize,
    },
    /// An LZ4 block decompressed to a different length than its header
    /// declared.
    #[error("lz4 block length mismatch: declared {declared}, decompressed to {actual}")]
    Lz4LengthMismatch {
        /// The declared decompressed length.
        declared: usize,
        /// The actual decompressed length.
        actual: usize,
    },
    /// An LZ4 block's XXHash32 checksum did not match its decompressed
    /// bytes.
    #[error("lz4 block checksum mismatch: header declared {declared:#x}, computed {actual:#x}")]
    Lz4ChecksumMismatch {
        /// The checksum recorded in the block header.
        declared: u32,
        /// The checksum actually computed over the decompressed bytes.
        actual: u32,
    },

    /// `level.dat`'s gzip wrapper failed to decode as gzip at all (wrong
    /// magic bytes) — distinct from [`Error::Io`], which covers a
    /// well-formed gzip stream that still fails (e.g. a CRC mismatch).
    #[error("level.dat is not a valid gzip stream")]
    NotGzip,
    /// `level.dat`'s root NBT compound had no `"Data"` field.
    #[error(r#"level.dat has no top-level "Data" compound"#)]
    MissingDataCompound,
    /// `level.dat`'s `"Data"` compound had no `"DataVersion"` field, or it
    /// was not an `Int`.
    #[error(r#"level.dat's "Data" compound has no integer "DataVersion" field"#)]
    MissingDataVersion,
}
