//! World representation for Lodestone's multi-version Minecraft client.
//!
//! This crate holds the **version-free** data structures for a loaded world:
//! [`PalettedContainer`], [`ChunkSection`], [`ChunkColumn`], per-column light
//! ([`ColumnLight`]), [`Heightmaps`], [`BlockEntity`] records, and a
//! [`ChunkPos`]-keyed [`World`] store. It knows how blocks, biomes and light are
//! *stored* and how that storage maps to the wire format, but it deliberately
//! knows nothing about the *meaning* of any particular value. Entries are opaque
//! non-negative integer ids (for block states, the version-specific global
//! block-state registry id; for biomes, the biome registry id). Translating
//! those ids to and from version-specific semantics is the job of a
//! protocol/version crate, not this one.
//!
//! # Why paletted storage
//!
//! A 1.18+ chunk column is 24 sections of 4096 blocks. Stored naively as one
//! `u16` per block that is `24 * 4096 * 2 = 196 KiB` *per column*, and at a
//! render distance of 32 (`4225` columns) roughly **830 MiB** of block data
//! alone. That is untenable next to GPU buffers on a 16 GiB machine.
//!
//! The fix — and, not coincidentally, exactly what vanilla stores and sends on
//! the wire — is a [`PalettedContainer`]: a bit-packed array of `N` entries
//! backed by one of three strategies that transition automatically as the
//! content changes:
//!
//! * **Single value** — the whole container is one value and stores *no* index
//!   array at all. Most sections are pure air or pure stone, so this is the
//!   single biggest saving.
//! * **Indirect** — a small palette of distinct values plus indices bit-packed
//!   at the smallest width that fits (clamped to a per-kind minimum).
//! * **Direct** — indices are the raw global ids with no palette, used once the
//!   palette grows past a per-kind ceiling.
//!
//! Empty sections are elided entirely (`None`) rather than allocating a zeroed
//! section, so a mostly-air column costs almost nothing.
//!
//! # Bit packing
//!
//! Since Minecraft 1.16 the packed indices do **not** straddle `i64`
//! boundaries: each long holds `floor(64 / bits_per_entry)` entries packed from
//! the low bits up, with the remaining high bits left as padding. See
//! [`PackedArray`] for the exact layout, which is verified against hand-built
//! golden longs in the tests.
//!
//! # Version-specific framing
//!
//! The bit packing, palette selection, thresholds and index order above are all
//! structural and shared across every protocol family. Only one detail of the
//! *wire* format is version-conditional: how the trailing long array is framed.
//! Vanilla removed the VarInt length prefix in **1.21.5 (protocol 770)**, so
//! [`LongArrayFraming`] carries that choice on a [`PaletteKind`]. A version
//! crate states its convention once (`FixedSize` for ≥ 1.21.5, `Prefixed` for
//! ≤ 1.21.4) and the same shared codec obeys — no per-version duplication of the
//! packing logic.
//!
//! # Memory recycling
//!
//! For a fixed entry count the packed backing store has only a small set of
//! size classes (one per bits-per-entry value). [`PackedArray::from_longs`]
//! lets a future size-classed free pool hand a recycled `Vec<u64>` straight
//! into a container, so buffers can be reused as chunks stream in and out
//! without reworking this API. Every decode path — containers, heightmaps, and
//! (implicitly) the light arrays — routes through that seam, and [`World::unload`]
//! returns the whole [`LoadedChunk`] so its buffers can be reclaimed. No pool is
//! implemented here, and this library never installs a `#[global_allocator]` —
//! that is an application decision.
//!
//! # Light
//!
//! Sky and block light are, naively, the largest single consumer of chunk
//! memory (two 2 KiB nibble arrays per section). [`ColumnLight`] elides uniform
//! sections to a one-byte tag — all-zero underground block light and full sky
//! light above terrain are overwhelmingly common — mirroring vanilla's
//! `DataLayer` and the light-update packet's present/empty section masks.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod block_entity;
mod column;
mod container;
mod heightmap;
mod light;
mod packed;
mod section;
mod world;

pub use block_entity::BlockEntity;
pub use column::ChunkColumn;
pub use container::{LongArrayFraming, PaletteKind, PalettedContainer};
pub use heightmap::{Heightmap, Heightmaps, height_bits};
pub use light::{ColumnLight, LightData, NibbleArray, SectionLight};
pub use packed::PackedArray;
pub use section::ChunkSection;
pub use world::{ChunkPos, ColumnPatch, LightPatch, LoadedChunk, World, WorldSink};

/// Errors produced when decoding world structures from untrusted wire data.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WorldError {
    /// A container declared a bits-per-entry value this kind cannot represent.
    #[error("invalid bits-per-entry {bits} for a container of {entry_count} entries")]
    InvalidBits {
        /// The rejected bits-per-entry value.
        bits: u32,
        /// The container's fixed entry count.
        entry_count: usize,
    },
    /// A packed long array did not have the length implied by bits and count.
    #[error("expected {expected} packed longs but found {actual}")]
    WrongLongCount {
        /// Number of longs required by the layout.
        expected: usize,
        /// Number of longs actually present.
        actual: usize,
    },
    /// A palette length was negative, zero where forbidden, or absurdly large.
    #[error("invalid palette length {0}")]
    InvalidPaletteLength(i64),
    /// A packed index referenced a palette slot that does not exist.
    #[error("packed index {index} is out of range for palette length {palette_len}")]
    PaletteIndexOutOfRange {
        /// The offending index value.
        index: u32,
        /// Number of entries in the palette.
        palette_len: usize,
    },
    /// An underlying core codec error (for example, an unexpected end of input).
    #[error(transparent)]
    Core(#[from] lodestone_core::Error),
    /// A light data array was not exactly 2048 bytes (4096 nibbles).
    #[error("light data array must be 2048 bytes, found {0}")]
    InvalidLightArrayLength(usize),
    /// A light-update list did not carry one array per set mask bit.
    #[error("light update list has {actual} arrays but {expected} sections are flagged present")]
    LightUpdateCountMismatch {
        /// Number of arrays required by the present-section mask.
        expected: usize,
        /// Number of arrays actually supplied.
        actual: usize,
    },
    /// A light mask referenced a section outside the column's light range.
    #[error("light mask bit {bit} is outside the {count} light sections")]
    LightSectionOutOfRange {
        /// The offending section index.
        bit: usize,
        /// Number of light sections in the column.
        count: usize,
    },
}

/// Convenient result alias for world codec operations.
pub type Result<T> = core::result::Result<T, WorldError>;

/// Bridges a [`WorldError`] back into a [`lodestone_core::Error`].
///
/// This is orphan-legal here — `WorldError` is local to this crate — which lets
/// version crates route world codecs through the core `Result` (for example, the
/// derive-generated `decode_with`, which hardcodes `lodestone_core::Result`)
/// with plain `?` instead of a per-call conversion helper.
///
/// A [`WorldError::Core`] round-trips losslessly: it unwraps back to the exact
/// inner [`lodestone_core::Error`] rather than being stringified through
/// [`Custom`](lodestone_core::Error::Custom). Every other variant carries no
/// core error to preserve, so it degrades to `Custom` with its `Display` text.
impl From<WorldError> for lodestone_core::Error {
    fn from(err: WorldError) -> Self {
        match err {
            WorldError::Core(inner) => inner,
            other => lodestone_core::Error::Custom(other.to_string()),
        }
    }
}

#[cfg(test)]
mod error_bridge_tests {
    use super::WorldError;

    #[test]
    fn core_error_round_trips_losslessly() {
        // A core error wrapped in `WorldError::Core` must come back as the exact
        // same core variant, not a stringified `Custom` — otherwise a value that
        // round-trips through the derive path silently degrades.
        let original = lodestone_core::Error::UnexpectedEof;
        let wrapped = WorldError::from(original.clone());
        let unwrapped: lodestone_core::Error = wrapped.into();
        assert_eq!(unwrapped, original);
        assert!(!matches!(unwrapped, lodestone_core::Error::Custom(_)));
    }

    #[test]
    fn world_only_variant_degrades_to_custom() {
        // A genuinely world-specific error has no core error to preserve, so it
        // is expected to carry its `Display` text through `Custom`.
        let err = WorldError::InvalidBits {
            bits: 99,
            entry_count: 4096,
        };
        let text = err.to_string();
        let core: lodestone_core::Error = err.into();
        match core {
            lodestone_core::Error::Custom(message) => assert_eq!(message, text),
            other => panic!("expected Custom, got {other:?}"),
        }
    }
}
