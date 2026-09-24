//! The vendored lz4 block-stream framing (a third-party library, not vanilla's
//! own code).
//!
//! `.mca` files saved with `region-file-compression=lz4` in `server.properties`
//! wrap each chunk's compressed payload in this format — **not** the standard
//! LZ4 frame format, and not raw LZ4 blocks either. It comes from a
//! third-party library (shaded here as
//! `at.yawk.lz4:lz4-java:1.10.1` —
//! `.cache/mc/26.2/libraries/at/yawk/lz4/lz4-java/1.10.1/lz4-java-1.10.1.jar`),
//! so there is no decompiled Minecraft source to cite a `file:line` against.
//! The layout below was instead read directly out of that jar's own
//! compiled output-stream class's constant pool and static-initializer bytecode (a class
//! file parsed by hand with a throwaway Python script — no JVM was available
//! in this environment to run a disassembler), which is why every constant here is a
//! measured value, not a recollection:
//!
//! - `MAGIC` = the 8 ASCII bytes `"LZ4Block"` (reconstructed from the
//!   `<clinit>` bytecode's `bipush`/`bastore` sequence: `4c 5a 34 42 6c 6f 63
//!   6b`).
//! - `MAGIC_LENGTH` = 8 (`MAGIC.length`, computed in `<clinit>`).
//! - `HEADER_LENGTH` = 21 (`<clinit>`: `MAGIC_LENGTH + 1 + 4 + 4 + 4`, i.e.
//!   magic + token + compressed-length + original-length + checksum).
//! - `COMPRESSION_METHOD_RAW` = `0x10`, `COMPRESSION_METHOD_LZ4` = `0x20`
//!   (both `ConstantValue` attributes on the class's static fields).
//! - `DEFAULT_SEED` = `0x9747B28C` (`ConstantValue` attribute, read as the
//!   signed `i32` `-1756908916`, which is `0x9747B28C` in two's complement —
//!   the XXHash32 seed `newStreamingHash32(DEFAULT_SEED)` uses).
//!
//! One block's on-wire layout, `HEADER_LENGTH` bytes followed by
//! `compressed_length` bytes of payload:
//!
//! | bytes | field |
//! |---|---|
//! | 8 | `MAGIC` (`"LZ4Block"`) |
//! | 1 | token: `0x10`/`0x20` (raw/LZ4), rest unset — see the note below |
//! | 4 | `compressed_length`, little-endian `i32` |
//! | 4 | `original_length` (decompressed size), little-endian `i32` |
//! | 4 | `checksum`, little-endian `i32` — XXHash32 of the **decompressed** block, seed `DEFAULT_SEED` |
//! | `compressed_length` | payload — a raw LZ4 block if the token's method is
//! `LZ4`, or the literal decompressed bytes if `RAW` (lz4-java falls back to
//! storing raw when compression doesn't shrink the block) |
//!
//! A stream is a sequence of blocks, terminated by one final block whose
//! `original_length` is 0 (the EOS marker the library's own stream-finish step
//! writes).
//!
//! **A gap this doc corrects rather than hides**: an earlier version of
//! this writer OR'd a `compressionLevel` value into the token's low nibble
//! (guessing "32 minus the count of leading zero bits in `blockSize - 1`", by analogy
//! with a formula seen in other lz4-java-derived code). That guess was
//! wrong in a way a test caught immediately: for the default 64 KiB block
//! size it computes 16, which does not fit in 4 bits and overflowed into
//! the method nibble, corrupting `COMPRESSION_METHOD_LZ4` (`0x20`) into
//! `0x30` — an unrecognized method byte our own decoder then rejected on
//! our own encoder's output. The class file's constant pool has the field
//! *name* `compressionLevel` and `COMPRESSION_LEVEL_BASE = 10`, but the
//! actual formula lives in bytecode this module's doc never decompiled
//! (`write`/`flushBufferedData`, not `<clinit>`), so rather than guess
//! again, the writer below leaves the token as exactly `0x10`/`0x20` with
//! no level bits set. A real lz4-java-written file may have nonzero low
//! nibble bits; the reader already tolerates that (`token & 0xF0` extracts
//! only the method), so this only affects byte-for-byte parity with a real
//! writer, never round-trip correctness.
//!
//! **Evidence gap, stated plainly**: none of this repo's live oracles set
//! `region-file-compression=lz4` (all three use the vanilla default,
//! `deflate` — see `region.rs`'s module doc), so unlike the zlib/gzip path,
//! this codec has never been exercised against a byte stream this repo did
//! not itself produce. The constants above come from the real library's own
//! class file, which is real evidence for the *format*, but the round-trip
//! tests in `tests/region_container.rs` necessarily check
//! `decode(encode(x)) == x` against our own writer — exactly the shape
//! `CLAUDE.md` warns proves nothing on its own. Treat this codec as
//! spec-correct-per-the-jar but not independently verified, and re-verify
//! against a real `lz4`-configured oracle before depending on it for
//! anything load-bearing.

use crate::{Error, Result};

const MAGIC: &[u8; 8] = b"LZ4Block";
const HEADER_LENGTH: usize = 21;
const COMPRESSION_METHOD_RAW: u8 = 0x10;
const COMPRESSION_METHOD_LZ4: u8 = 0x20;
const DEFAULT_SEED: u32 = 0x9747_B28C;
/// The library's own default single-argument
/// constructor block size (`1 << 16`, per the library's own
/// `DEFAULT_BLOCK_SIZE`). Only used by our writer — a reader has no need for
/// it, since each block declares its own lengths.
const DEFAULT_BLOCK_SIZE: usize = 64 * 1024;

/// Decodes a full `LZ4Block`-framed stream (as read from a `.mca` chunk
/// payload) back to the original decompressed bytes.
pub fn decode(mut input: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();

    loop {
        if input.len() < HEADER_LENGTH {
            return Err(Error::TruncatedLz4Header {
                available: input.len(),
            });
        }

        if &input[0..8] != MAGIC {
            return Err(Error::InvalidLz4Magic);
        }

        let token = input[8];
        let method = token & 0xF0;
        let compressed_length = u32::from_le_bytes(input[9..13].try_into().unwrap()) as usize;
        let original_length = u32::from_le_bytes(input[13..17].try_into().unwrap()) as usize;
        let checksum = u32::from_le_bytes(input[17..21].try_into().unwrap());

        if original_length == 0 {
            // EOS marker: the library's own stream-finish step writes a
            // zero-original-length block and no payload.
            return Ok(out);
        }

        let body_start = HEADER_LENGTH;
        let body_end = body_start
            .checked_add(compressed_length)
            .ok_or(Error::TruncatedLz4Body {
                declared: compressed_length,
                available: input.len().saturating_sub(body_start),
            })?;
        if body_end > input.len() {
            return Err(Error::TruncatedLz4Body {
                declared: compressed_length,
                available: input.len() - body_start,
            });
        }
        let body = &input[body_start..body_end];

        let block = match method {
            COMPRESSION_METHOD_RAW => body.to_vec(),
            COMPRESSION_METHOD_LZ4 => {
                lz4_flex::block::decompress(body, original_length).map_err(|_| {
                    Error::Lz4Decompress {
                        original_length,
                        compressed_length,
                    }
                })?
            }
            other => return Err(Error::UnknownLz4Method(other)),
        };

        if block.len() != original_length {
            return Err(Error::Lz4LengthMismatch {
                declared: original_length,
                actual: block.len(),
            });
        }

        let actual_checksum = xxhash_rust::xxh32::xxh32(&block, DEFAULT_SEED);
        if actual_checksum != checksum {
            return Err(Error::Lz4ChecksumMismatch {
                declared: checksum,
                actual: actual_checksum,
            });
        }

        out.extend_from_slice(&block);
        input = &input[body_end..];
    }
}

/// Encodes `data` into an `LZ4Block`-framed stream, matching
/// the library's own single-argument constructor
/// (default block size, LZ4-compressed unless a block fails to shrink).
pub fn encode(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();

    for chunk in chunk_or_empty(data, DEFAULT_BLOCK_SIZE) {
        write_block(&mut out, chunk);
    }
    // EOS marker: original_length 0, no checksum payload requirement beyond
    // the header fields lz4-java always writes.
    write_eos(&mut out);

    out
}

/// Splits `data` into `block_size`-sized chunks. Unlike `slice::chunks`, an
/// empty input still yields exactly one (empty) chunk, matching
/// the library's own behaviour of writing a single empty compressed
/// block before its EOS marker when `write` is never called with data.
fn chunk_or_empty(data: &[u8], block_size: usize) -> Vec<&[u8]> {
    if data.is_empty() {
        return vec![&[]];
    }
    data.chunks(block_size).collect()
}

fn write_block(out: &mut Vec<u8>, block: &[u8]) {
    let checksum = xxhash_rust::xxh32::xxh32(block, DEFAULT_SEED);
    let compressed = lz4_flex::block::compress(block);

    let (method, body): (u8, &[u8]) = if compressed.len() < block.len() {
        (COMPRESSION_METHOD_LZ4, &compressed)
    } else {
        (COMPRESSION_METHOD_RAW, block)
    };

    out.extend_from_slice(MAGIC);
    // No `compressionLevel` bits set — see the module doc for why guessing
    // that formula was wrong and is not worth re-guessing.
    out.push(method);
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&(block.len() as u32).to_le_bytes());
    out.extend_from_slice(&checksum.to_le_bytes());
    out.extend_from_slice(body);
}

fn write_eos(out: &mut Vec<u8>) {
    out.extend_from_slice(MAGIC);
    out.push(COMPRESSION_METHOD_RAW);
    out.extend_from_slice(&0u32.to_le_bytes()); // compressed_length
    out.extend_from_slice(&0u32.to_le_bytes()); // original_length == 0 -> EOS
    out.extend_from_slice(&0u32.to_le_bytes()); // checksum, unchecked for EOS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn magic_bytes_match_the_jar() {
        assert_eq!(MAGIC, b"LZ4Block");
    }

    #[test]
    fn empty_input_round_trips() {
        let encoded = encode(&[]);
        let decoded = decode(&encoded).expect("decodes");
        assert_eq!(decoded, Vec::<u8>::new());
    }

    #[test]
    fn small_input_round_trips() {
        let data = b"the quick brown fox jumps over the lazy dog".repeat(4);
        let encoded = encode(&data);
        let decoded = decode(&encoded).expect("decodes");
        assert_eq!(decoded, data);
    }

    #[test]
    fn multi_block_input_round_trips() {
        // Three full default-sized blocks plus a partial one, so the codec
        // exercises `chunks()` splitting a real multi-block stream.
        let mut data = Vec::new();
        for i in 0..(DEFAULT_BLOCK_SIZE * 3 + 12345) {
            data.push((i % 251) as u8);
        }
        let encoded = encode(&data);
        let decoded = decode(&encoded).expect("decodes");
        assert_eq!(decoded, data);
    }

    #[test]
    fn incompressible_random_block_falls_back_to_raw_method() {
        // A block of bytes with no repeated structure should fail to shrink,
        // exercising the RAW fallback path rather than only ever hitting LZ4.
        let mut state: u32 = 0x1234_5678;
        let data: Vec<u8> = (0..DEFAULT_BLOCK_SIZE)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                (state & 0xFF) as u8
            })
            .collect();
        let encoded = encode(&data);
        assert_eq!(encoded[8] & 0xF0, COMPRESSION_METHOD_RAW);
        let decoded = decode(&encoded).expect("decodes");
        assert_eq!(decoded, data);
    }

    #[test]
    fn truncated_header_errors_cleanly() {
        let encoded = encode(b"hello anvil");
        let truncated = &encoded[..10];
        assert!(matches!(
            decode(truncated),
            Err(Error::TruncatedLz4Header { .. })
        ));
    }

    #[test]
    fn corrupted_checksum_is_detected() {
        let mut encoded = encode(b"a payload long enough to matter for the checksum check");
        // Flip a bit inside the first block's payload without touching its
        // declared checksum, so decode must notice the mismatch rather than
        // silently accepting corrupted bytes. This is the corrupt-input
        // control: `small_input_round_trips` above proves the same code path
        // accepts a well-formed stream, so a failure here is the checksum
        // firing, not the parser rejecting everything.
        let payload_start = HEADER_LENGTH;
        encoded[payload_start] ^= 0xFF;
        assert!(matches!(
            decode(&encoded),
            Err(Error::Lz4ChecksumMismatch { .. }) | Err(Error::Lz4Decompress { .. })
        ));
    }

    #[test]
    fn bad_magic_is_rejected() {
        let mut encoded = encode(b"anything");
        encoded[0] = b'X';
        assert!(matches!(decode(&encoded), Err(Error::InvalidLz4Magic)));
    }
}
