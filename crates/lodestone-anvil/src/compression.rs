//! The compression-scheme byte that prefixes every chunk payload inside a
//! region file's sector data (and the analogous, always-gzip wrapping of
//! `level.dat`).
//!
//! Four scheme ids exist:
//!
//! | id | name | notes |
//! |---|---|---|
//! | 1 | gzip | |
//! | 2 | zlib (`deflate`) | **the default** |
//! | 3 | uncompressed | |
//! | 4 | LZ4 | |
//!
//! `server.properties`' `region-file-compression` key selects the scheme a
//! server *writes* (defaulting to deflate), and this repo's three live
//! oracles (`.cache/mc/{creative,terrain,survival}/server.properties`) all
//! leave it at that default. So every real `.mca` this crate has been tested
//! against uses scheme 2 — **not** 4. This corrects an assumption in the
//! issue that prompted this crate: LZ4 is an available scheme, not "the
//! variant modern versions write" by default. See `lz4_block.rs`'s module
//! doc for the consequence: the LZ4 codec exists and round-trips against
//! itself, but has no real-file evidence behind it, unlike the other three.
//!
//! A reader must accept whichever scheme id a chunk was written with — a
//! region file can mix schemes across chunks if `region-file-compression`
//! changed mid-life, since each chunk carries its own id.

use crate::{Error, Result};
use std::io::Read;

/// One of the four compression schemes a chunk payload (or `level.dat`) may
/// be wrapped in. Custom compression (scheme id 127) exists only as a
/// forward-compatibility placeholder that a real save immediately errors on
/// decoding (an "unrecognized custom compression" failure, reading a length-
/// prefixed name string it then refuses to honour), so it is deliberately
/// not modelled as a variant here — see
/// [`Error::UnsupportedCompressionScheme`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompressionScheme {
    /// Scheme id 1. Always used for `level.dat`; available but not default
    /// for region-file chunks.
    Gzip,
    /// Scheme id 2. The default for region-file chunks, and the only scheme
    /// any real file this crate has read actually uses.
    Zlib,
    /// Scheme id 3. Stores the chunk NBT with no compression at all.
    Uncompressed,
    /// Scheme id 4. See the module doc and `lz4_block.rs` for the framing
    /// and the evidence gap around it.
    Lz4,
}

impl CompressionScheme {
    /// Maps a scheme id byte (as stored on disk) to a scheme, or `None` for
    /// an id that is not decodable data on its own (0, the "chunk absent"
    /// sentinel, and 127, the reserved "custom compression" placeholder,
    /// both handled by the caller).
    #[must_use]
    pub fn from_id(id: u8) -> Option<Self> {
        match id {
            1 => Some(Self::Gzip),
            2 => Some(Self::Zlib),
            3 => Some(Self::Uncompressed),
            4 => Some(Self::Lz4),
            _ => None,
        }
    }

    /// The on-disk scheme id byte.
    #[must_use]
    pub fn id(self) -> u8 {
        match self {
            Self::Gzip => 1,
            Self::Zlib => 2,
            Self::Uncompressed => 3,
            Self::Lz4 => 4,
        }
    }

    /// Decompresses `data`, which was compressed under this scheme.
    pub fn decompress(self, data: &[u8]) -> Result<Vec<u8>> {
        match self {
            Self::Gzip => {
                let mut out = Vec::new();
                flate2::read::GzDecoder::new(data)
                    .read_to_end(&mut out)
                    .map_err(Error::Io)?;
                Ok(out)
            }
            Self::Zlib => {
                let mut out = Vec::new();
                flate2::read::ZlibDecoder::new(data)
                    .read_to_end(&mut out)
                    .map_err(Error::Io)?;
                Ok(out)
            }
            Self::Uncompressed => Ok(data.to_vec()),
            Self::Lz4 => crate::lz4_block::decode(data),
        }
    }

    /// Compresses `data` under this scheme.
    pub fn compress(self, data: &[u8]) -> Result<Vec<u8>> {
        use std::io::Write;

        match self {
            Self::Gzip => {
                let mut encoder =
                    flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
                encoder.write_all(data).map_err(Error::Io)?;
                encoder.finish().map_err(Error::Io)
            }
            Self::Zlib => {
                let mut encoder =
                    flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
                encoder.write_all(data).map_err(Error::Io)?;
                encoder.finish().map_err(Error::Io)
            }
            Self::Uncompressed => Ok(data.to_vec()),
            Self::Lz4 => Ok(crate::lz4_block::encode(data)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_round_trip() {
        for scheme in [
            CompressionScheme::Gzip,
            CompressionScheme::Zlib,
            CompressionScheme::Uncompressed,
            CompressionScheme::Lz4,
        ] {
            assert_eq!(CompressionScheme::from_id(scheme.id()), Some(scheme));
        }
    }

    #[test]
    fn scheme_zero_and_custom_are_unmapped() {
        // 0 is the "chunk not present" sentinel and 127 is the reserved
        // "custom compression" placeholder — neither is a scheme this type
        // represents.
        assert_eq!(CompressionScheme::from_id(0), None);
        assert_eq!(CompressionScheme::from_id(127), None);
    }

    #[test]
    fn zlib_round_trips() {
        let data = b"minecraft:overworld".repeat(50);
        let compressed = CompressionScheme::Zlib.compress(&data).expect("compress");
        let decompressed = CompressionScheme::Zlib
            .decompress(&compressed)
            .expect("decompress");
        assert_eq!(decompressed, data);
    }

    #[test]
    fn gzip_round_trips() {
        let data = b"level.dat is always gzip, never zlib".repeat(20);
        let compressed = CompressionScheme::Gzip.compress(&data).expect("compress");
        let decompressed = CompressionScheme::Gzip
            .decompress(&compressed)
            .expect("decompress");
        assert_eq!(decompressed, data);
    }

    #[test]
    fn uncompressed_is_the_identity() {
        let data = b"passed through untouched".to_vec();
        let compressed = CompressionScheme::Uncompressed
            .compress(&data)
            .expect("compress");
        assert_eq!(compressed, data);
    }

    #[test]
    fn zlib_stream_carries_a_real_zlib_header() {
        // Predicted-vs-measured: any zlib stream compressed at the default
        // level begins with the two-byte header 0x78 0x9c (CMF=0x78 picks a
        // 32K window and the deflate method, FLG=0x9c selects the default
        // compression-level flag with no preset dictionary and a checksum
        // that makes the 16-bit big-endian header value a multiple of 31).
        // This is also exactly what a real chunk in
        // `.cache/mc/1.16.5/world/region/r.0.0.mca` starts with, checked by
        // hand in `tests/region_real_world.rs`'s doc comment — so this test
        // pins our own encoder to the same header real files carry, not an
        // arbitrary self-consistency fact.
        let compressed = CompressionScheme::Zlib
            .compress(b"anything")
            .expect("compress");
        assert_eq!(&compressed[0..2], &[0x78, 0x9c]);
    }
}
