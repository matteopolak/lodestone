//! Wire helpers shared by protocol 776's entity packets.
//!
//! The entity packets themselves are decoded inline in [`crate::adapter`] (they
//! lift straight into canonical [`lodestone_model::ClientEvent`]s), but two
//! codecs are subtle enough to isolate and unit-test here, each with both a
//! decode and an encode side (the latter for `server_protocol`'s entity
//! encoders):
//!
//! * **Low-precision velocity ([`read_lp_vec3`]/[`write_lp_vec3`]).** 26.2
//!   replaced the old three-`i16` movement encoding with a packed
//!   variable-length one (confirmed against the decompiled 26.2 network
//!   source): a leading byte
//!   of `0` means the zero vector, otherwise a byte + byte + big-endian `u32`
//!   pack three 15-bit quantised components plus a 2-bit scale, with an
//!   optional trailing varint carrying the high bits of a larger scale.
//!   Getting the bit layout wrong misaligns every following field, so this is
//!   exactly the kind of codec the project pins with independently computed
//!   known-answer vectors.
//! * **Angle bytes ([`unpack_degrees`]/[`pack_degrees`]).** Rotations travel
//!   as a signed byte where the full circle is 256 steps (vanilla's own
//!   angle-byte pack/unpack helpers).

use lodestone_core::{Reader, Result, Writer};

/// Reads a low-precision velocity vector, returning `(x, y, z)` in blocks/tick.
///
/// Mirrors vanilla's own low-precision-vector reader: a leading `0` byte is
/// the zero vector; otherwise the
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

/// Dequantises one 15-bit field to `[-1, 1]`, matching vanilla's own
/// low-precision-vector unpack helper.
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

/// Converts a degree angle to vanilla's signed-byte wire form, the exact
/// inverse of [`unpack_degrees`] (`Mth.packDegrees`): the full circle is 256
/// steps, and the result wraps modulo 360° the same way the byte itself does.
#[must_use]
pub fn pack_degrees(degrees: f32) -> i8 {
    ((degrees * 256.0 / 360.0).round() as i32 & 0xFF) as u8 as i8
}

/// Writes a low-precision velocity vector, the encode-side mirror of
/// [`read_lp_vec3`] (vanilla's own low-precision-vector writer): a single
/// `0` byte for the (near-)zero vector, otherwise a packed 48-bit buffer
/// (two bytes plus a big-endian
/// `u32`) carrying three 15-bit quantised components and a 2-bit scale, with
/// a trailing scale varint when the scale overflows those two bits.
pub fn write_lp_vec3(w: &mut Writer, x: f64, y: f64, z: f64) {
    let chess = x.abs().max(y.abs()).max(z.abs());
    if chess < 3.051_944_088_384_301E-5 {
        w.u8(0);
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
    w.u8(buffer as u8);
    w.u8((buffer >> 8) as u8);
    w.u32((buffer >> 16) as u32);
    if is_partial {
        w.var_i32((scale >> 2) as i32);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestone_core::{Reader, Writer};

    fn write_lp_vec3(x: f64, y: f64, z: f64) -> Vec<u8> {
        let mut w = Writer::default();
        super::write_lp_vec3(&mut w, x, y, z);
        w.into_vec()
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
        // These are the bytes a *real* vanilla server sends. Vanilla's own
        // pack helper rounds with its own rounding helper (half-up =
        // floor(a+0.5)); pack(0.5) is exactly
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
            let bytes = write_lp_vec3(x, y, z);
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

    #[test]
    fn pack_degrees_is_the_inverse_of_unpack_degrees() {
        assert_eq!(pack_degrees(0.0), 0);
        assert_eq!(pack_degrees(90.0), 64);
        assert_eq!(pack_degrees(-180.0), -128);
        // Every representable byte round-trips through degrees and back.
        for packed in i8::MIN..=i8::MAX {
            assert_eq!(
                pack_degrees(unpack_degrees(packed)),
                packed,
                "byte {packed}"
            );
        }
    }

    #[test]
    fn write_lp_vec3_reproduces_the_known_wire_bytes() {
        // Same vectors `read_lp_vec3`'s known-answer tests above decode from
        // real vanilla bytes — asserting the encoder reproduces those exact
        // bytes (not just that decode(encode(x)) == x) catches a codec that
        // agrees with itself but not with the wire.
        assert_eq!(write_lp_vec3(0.0, 0.0, 0.0), vec![0]);
        assert_eq!(
            write_lp_vec3(0.5, -0.3, 1.0),
            vec![249, 255, 255, 252, 179, 50]
        );
        assert_eq!(
            write_lp_vec3(3.9, -3.9, 0.0),
            vec![36, 243, 127, 254, 6, 107, 1]
        );
    }
}
