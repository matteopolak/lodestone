//! Wire helpers shared by protocol 776's entity packets.
//!
//! The entity packets themselves are decoded inline in [`crate::adapter`] (they
//! lift straight into canonical [`lodestone_model::ClientEvent`]s), but two
//! encodings are subtle enough to isolate and unit-test here:
//!
//! * **Low-precision velocity ([`read_lp_vec3`]).** 26.2 replaced the old
//!   three-`i16` movement encoding with a packed variable-length one (see
//!   `net.minecraft.network.LpVec3`): a leading byte of `0` means the zero
//!   vector, otherwise a byte + byte + big-endian `u32` pack three 15-bit
//!   quantised components plus a 2-bit scale, with an optional trailing varint
//!   carrying the high bits of a larger scale. Getting the bit layout wrong
//!   misaligns every following field, so this is exactly the kind of codec the
//!   project pins with independently computed known-answer vectors.
//! * **Angle bytes ([`unpack_degrees`]).** Rotations travel as a signed byte
//!   where the full circle is 256 steps (`Mth.unpackDegrees`).

use lodestone_core::{Reader, Result};

/// Reads a low-precision velocity vector, returning `(x, y, z)` in blocks/tick.
///
/// Mirrors `LpVec3.read`: a leading `0` byte is the zero vector; otherwise the
/// first two bytes and a big-endian `u32` form a 48-bit buffer whose low three
/// bits hold the scale (bit 2 flags a continuation varint) and whose three
/// 15-bit fields at offsets 3/18/33 are dequantised to `[-1, 1]` and multiplied
/// by the scale.
pub fn read_lp_vec3(reader: &mut Reader<'_>) -> Result<(f64, f64, f64)> {
    let lowest = u64::from(reader.u8()?);
    if lowest == 0 {
        return Ok((0.0, 0.0, 0.0));
    }
    let middle = u64::from(reader.u8()?);
    let highest = u64::from(reader.u32()?);
    let buffer = (highest << 16) | (middle << 8) | lowest;

    let mut scale = lowest & 3;
    if lowest & 4 == 4 {
        // Continuation: the varint carries the scale bits above the low two.
        let continuation = u64::from(reader.var_i32()? as u32);
        scale |= continuation << 2;
    }

    let scale = scale as f64;
    Ok((
        unpack(buffer >> 3) * scale,
        unpack(buffer >> 18) * scale,
        unpack(buffer >> 33) * scale,
    ))
}

/// Dequantises one 15-bit field to `[-1, 1]`, matching `LpVec3.unpack`.
fn unpack(value: u64) -> f64 {
    let quantised = (value & 32767).min(32766) as f64;
    quantised * 2.0 / 32766.0 - 1.0
}

/// Converts a signed-byte angle to degrees, matching `Mth.unpackDegrees`.
///
/// The full circle is 256 steps, so a byte of `64` is 90°.
#[must_use]
pub fn unpack_degrees(packed: i8) -> f32 {
    f32::from(packed) * 360.0 / 256.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestone_core::Reader;

    /// Rust port of `LpVec3.write`, used only to round-trip in tests. Kept out
    /// of production because nothing Lodestone sends encodes velocity yet.
    fn write_lp_vec3(out: &mut Vec<u8>, x: f64, y: f64, z: f64) {
        let chess = x.abs().max(y.abs()).max(z.abs());
        if chess < 3.051_944_088_384_301E-5 {
            out.push(0);
            return;
        }
        let scale = chess.ceil() as i64;
        let is_partial = (scale & 3) != scale;
        let markers = if is_partial { (scale & 3) | 4 } else { scale };
        let pack = |v: f64| {
            ((v / scale as f64) * 0.5 + 0.5)
                .mul_add(32766.0, 0.0)
                .round() as i64
        };
        let buffer = markers | (pack(x) << 3) | (pack(y) << 18) | (pack(z) << 33);
        out.push(buffer as u8);
        out.push((buffer >> 8) as u8);
        out.extend_from_slice(&((buffer >> 16) as u32).to_be_bytes());
        if is_partial {
            // varint of scale >> 2
            let mut s = (scale >> 2) as u32;
            loop {
                let byte = (s & 0x7F) as u8;
                s >>= 7;
                if s != 0 {
                    out.push(byte | 0x80);
                } else {
                    out.push(byte);
                    break;
                }
            }
        }
    }

    fn decode(bytes: &[u8]) -> ((f64, f64, f64), usize) {
        let mut reader = Reader::new(bytes);
        let vec = read_lp_vec3(&mut reader).expect("decodes");
        (vec, reader.remaining())
    }

    // Known-answer vectors computed independently from the decompiled `LpVec3`
    // algorithm (a Python port, not the Rust reader), so a transposed bit field
    // or an off-by-one shift fails here rather than round-tripping happily.

    #[test]
    fn zero_vector_is_a_single_byte() {
        let ((x, y, z), rest) = decode(&[0]);
        assert_eq!((x, y, z), (0.0, 0.0, 0.0));
        assert_eq!(rest, 0, "zero vector must consume exactly one byte");
    }

    #[test]
    fn known_non_partial_vector() {
        // (0.5, -0.3, 1.0) -> scale 1, no continuation: exactly six bytes.
        //
        // These are the bytes a *real* vanilla server sends. `LpVec3.pack` rounds
        // with `Math.round` (half-up = floor(a+0.5)); pack(0.5) is exactly
        // 24574.5, which half-up takes to 24575, giving a low byte of 249. An
        // earlier golden used 241 — the same vector packed with Python's
        // banker's rounding (half-to-even → 24574), a 1-LSB divergence from the
        // wire. Rust's `f64::round` is half-away-from-zero, which agrees with
        // half-up for these non-negative pack operands, so `write_lp_vec3` below
        // reproduces exactly these bytes.
        let bytes = [249u8, 255, 255, 252, 179, 50];
        let ((x, y, z), rest) = decode(&bytes);
        assert_eq!(rest, 0, "six-byte vector must be fully consumed");
        assert!((x - 0.500_031).abs() < 1e-5, "x was {x}");
        assert!((y - (-0.300_006)).abs() < 1e-5, "y was {y}");
        assert!((z - 1.0).abs() < 1e-5, "z was {z}");
    }

    #[test]
    fn known_partial_vector_has_continuation_varint() {
        // (3.9, -3.9, 0.0) -> ceil 4, scale low bits 0 with the continuation
        // flag set, so a trailing varint (1) appears: seven bytes total.
        let bytes = [36u8, 243, 127, 254, 6, 107, 1];
        let ((x, y, z), rest) = decode(&bytes);
        assert_eq!(rest, 0, "seven-byte partial vector must be fully consumed");
        assert!((x - 3.899_896).abs() < 1e-5, "x was {x}");
        assert!((y - (-3.899_896)).abs() < 1e-5, "y was {y}");
        assert!(z.abs() < 1e-5, "z was {z}");
    }

    #[test]
    fn round_trips_a_spread_of_vectors() {
        for &(x, y, z) in &[
            (0.0, 0.0, 0.0),
            (0.5, -0.3, 1.0),
            (3.9, -3.9, 0.0),
            (-1.25, 2.5, -7.0),
            (100.0, -0.01, 42.0),
        ] {
            let mut bytes = Vec::new();
            write_lp_vec3(&mut bytes, x, y, z);
            let ((dx, dy, dz), rest) = decode(&bytes);
            assert_eq!(rest, 0, "trailing bytes after ({x},{y},{z})");
            // Quantisation error scales with the chessboard magnitude.
            let tol = (x.abs().max(y.abs()).max(z.abs()) / 16383.0).max(1e-4);
            assert!((dx - x).abs() <= tol, "x {dx} vs {x}");
            assert!((dy - y).abs() <= tol, "y {dy} vs {y}");
            assert!((dz - z).abs() <= tol, "z {dz} vs {z}");
        }
    }

    #[test]
    fn unpack_degrees_matches_known_angles() {
        assert_eq!(unpack_degrees(0), 0.0);
        assert_eq!(unpack_degrees(64), 90.0);
        assert_eq!(unpack_degrees(-128), -180.0);
    }
}
