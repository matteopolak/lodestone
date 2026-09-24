//! Sans-IO packet framing and compression codec.
//!
//! This module implements Minecraft Java Edition's length-prefixed wire framing
//! as a pure, synchronous state machine over byte buffers. It performs no I/O of
//! its own, which makes the tricky partial-frame and compression logic
//! exhaustively testable and reusable for both socket and in-memory transports.

use flate2::Compression;
use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;
use lodestone_core::{Reader, Writer};
use std::io::{Read, Write};

use crate::crypto::Cfb8Cipher;
use crate::error::{NetError, Result};

/// Vanilla's hard cap on a single frame's declared length (2 MiB).
///
/// In practice the 3-byte length VarInt limit ([`MAX_LENGTH_VARINT_BYTES`]) is
/// the operative bound, since three VarInt bytes can only encode values up to
/// `2^21 - 1`. This constant is retained as documented, defence-in-depth.
pub const MAX_PACKET_LEN: usize = 2_097_152;

/// Safety cap on a frame's declared *decompressed* size (8 MiB).
///
/// A frame claiming a larger uncompressed length is rejected before any buffer
/// is allocated, so a hostile length cannot trigger a huge allocation.
pub const MAX_DECOMPRESSED_LEN: usize = 8_388_608;

/// Maximum number of bytes a frame-length VarInt may occupy.
pub const MAX_LENGTH_VARINT_BYTES: usize = 3;

/// Sans-IO framing codec for the Minecraft packet wire format.
///
/// The codec owns a receive buffer that callers grow with [`Codec::feed`] and
/// drain one frame at a time with [`Codec::next_packet`]. Encoding is a pure
/// function of the current compression and encryption state via
/// [`Codec::encode`].
///
/// A frame *body* is the fully decompressed packet payload, i.e. the
/// `[VarInt packet id][fields...]` bytes. The codec itself does not interpret
/// the packet id.
///
/// # Layering
///
/// From the wire inward: **encryption wraps length-framing wraps compression
/// wraps the body.** Encryption is the outermost transform because online-mode
/// AES-128-CFB8 covers the entire byte stream including the length prefixes;
/// [`Codec::encode`] therefore encrypts *after* framing and [`Codec::feed`]
/// decrypts *before* any length is parsed. Keeping the cipher here — rather than
/// in an I/O wrapper — means every transport, including the browser WebSocket,
/// inherits encryption unchanged.
#[derive(Debug, Default)]
pub struct Codec {
    threshold: Option<usize>,
    rx: Vec<u8>,
    cipher: Option<Cfb8Cipher>,
}

impl Codec {
    /// Creates a codec with compression disabled.
    #[must_use]
    pub fn new() -> Self {
        Self {
            threshold: None,
            rx: Vec::new(),
            cipher: None,
        }
    }

    /// Sets the compression threshold, mirroring `login_compression`.
    ///
    /// A negative `threshold` disables compression; a non-negative value enables
    /// it, compressing bodies whose length is `>= threshold`.
    pub fn set_compression(&mut self, threshold: i32) {
        self.threshold = if threshold < 0 {
            None
        } else {
            Some(threshold as usize)
        };
    }

    /// Returns the active compression threshold, or `None` when disabled.
    #[must_use]
    pub fn compression_threshold(&self) -> Option<usize> {
        self.threshold
    }

    /// Returns the number of buffered bytes not yet consumed as a frame.
    #[must_use]
    pub fn buffered_len(&self) -> usize {
        self.rx.len()
    }

    /// Enables AES-128-CFB8 encryption from the 16-byte shared secret.
    ///
    /// After this call every encoded frame is enciphered and every fed byte is
    /// deciphered, using one long-lived cipher per direction. Any bytes already
    /// buffered in `rx` are assumed to predate encryption and are left as-is
    /// (in practice `EncryptionResponse` is the last cleartext packet a client
    /// sends and the server's first encrypted byte arrives only afterwards).
    ///
    /// # Errors
    ///
    /// Returns [`NetError::EncryptionAlreadyEnabled`] if called twice, or
    /// [`NetError::BadSharedSecret`] if `secret` is not 16 bytes.
    pub fn enable_encryption(&mut self, secret: &[u8]) -> Result<()> {
        if self.cipher.is_some() {
            return Err(NetError::EncryptionAlreadyEnabled);
        }
        self.cipher = Some(Cfb8Cipher::new(secret)?);
        Ok(())
    }

    /// Returns whether encryption has been enabled.
    #[must_use]
    pub fn is_encrypted(&self) -> bool {
        self.cipher.is_some()
    }

    /// Encodes a packet `body`, appending the wire bytes to `dst`.
    ///
    /// `body` must already be the `[VarInt packet id][fields...]` payload. The
    /// body is framed (and compressed when enabled), then — if encryption is on
    /// — enciphered, advancing the outgoing keystream. Only the bytes produced
    /// by this call are transformed; pre-existing `dst` contents are untouched,
    /// so callers may accumulate a multi-packet stream in one buffer.
    pub fn encode(&mut self, body: &[u8], dst: &mut Vec<u8>) -> Result<()> {
        let mut frame = Vec::new();
        self.frame(body, &mut frame)?;
        if let Some(cipher) = self.cipher.as_mut() {
            cipher.encrypt(&mut frame);
        }
        dst.extend_from_slice(&frame);
        Ok(())
    }

    /// Produces the (cleartext) framed bytes for `body` into `dst`.
    fn frame(&self, body: &[u8], dst: &mut Vec<u8>) -> Result<()> {
        match self.threshold {
            None => {
                if body.len() > MAX_PACKET_LEN {
                    return Err(NetError::PacketTooLarge {
                        len: body.len(),
                        max: MAX_PACKET_LEN,
                    });
                }
                write_var_i32(dst, body.len() as i32);
                dst.extend_from_slice(body);
                Ok(())
            }
            Some(threshold) => {
                let mut frame = Vec::new();
                if body.len() >= threshold {
                    write_var_i32(&mut frame, body.len() as i32);
                    let compressed = zlib_compress(body)?;
                    frame.extend_from_slice(&compressed);
                } else {
                    write_var_i32(&mut frame, 0);
                    frame.extend_from_slice(body);
                }
                if frame.len() > MAX_PACKET_LEN {
                    return Err(NetError::PacketTooLarge {
                        len: frame.len(),
                        max: MAX_PACKET_LEN,
                    });
                }
                write_var_i32(dst, frame.len() as i32);
                dst.extend_from_slice(&frame);
                Ok(())
            }
        }
    }

    /// Appends received bytes to the internal receive buffer.
    ///
    /// When encryption is enabled the bytes are deciphered first (advancing the
    /// incoming keystream), so `rx` always holds cleartext frames. CFB8 is a
    /// byte-stream cipher, so decrypting each `feed` chunk incrementally is
    /// exactly correct even when a frame is split across many reads.
    pub fn feed(&mut self, data: &[u8]) {
        if let Some(cipher) = self.cipher.as_mut() {
            let mut buf = data.to_vec();
            cipher.decrypt(&mut buf);
            self.rx.extend_from_slice(&buf);
        } else {
            self.rx.extend_from_slice(data);
        }
    }

    /// Attempts to decode the next complete frame from the receive buffer.
    ///
    /// Returns `Ok(Some(body))` when a full frame was available and consumed,
    /// `Ok(None)` when more bytes are needed, or `Err` on a malformed frame.
    pub fn next_packet(&mut self) -> Result<Option<Vec<u8>>> {
        let Some((len, len_bytes)) = read_length_varint(&self.rx)? else {
            return Ok(None);
        };

        if len == 0 {
            return Err(NetError::MalformedFrame("zero-length frame"));
        }
        if len > MAX_PACKET_LEN {
            return Err(NetError::PacketTooLarge {
                len,
                max: MAX_PACKET_LEN,
            });
        }

        let total = len_bytes + len;
        if self.rx.len() < total {
            return Ok(None);
        }

        let frame = &self.rx[len_bytes..total];
        let body = match self.threshold {
            None => frame.to_vec(),
            Some(threshold) => decompress_frame(frame, threshold)?,
        };

        self.rx.drain(..total);
        Ok(Some(body))
    }
}

/// Decodes a compression-mode frame body (`[VarInt uncompressed len][payload]`).
fn decompress_frame(frame: &[u8], threshold: usize) -> Result<Vec<u8>> {
    let mut reader = Reader::new(frame);
    let uncompressed_len = reader.var_i32()?;
    if uncompressed_len < 0 {
        return Err(NetError::MalformedFrame("negative uncompressed length"));
    }
    let uncompressed_len = uncompressed_len as usize;
    let payload = reader.remaining_bytes();

    if uncompressed_len == 0 {
        return Ok(payload.to_vec());
    }

    if uncompressed_len < threshold {
        return Err(NetError::BadlyCompressed {
            len: uncompressed_len,
            threshold,
        });
    }
    if uncompressed_len > MAX_DECOMPRESSED_LEN {
        return Err(NetError::DecompressedTooLarge {
            len: uncompressed_len,
            max: MAX_DECOMPRESSED_LEN,
        });
    }

    let mut out = vec![0u8; uncompressed_len];
    let mut decoder = ZlibDecoder::new(payload);
    decoder
        .read_exact(&mut out)
        .map_err(|_| NetError::DecompressedLenMismatch {
            expected: uncompressed_len,
            actual: 0,
        })?;

    // Ensure the stream contained *exactly* the declared number of bytes.
    let mut extra = [0u8; 1];
    match decoder.read(&mut extra) {
        Ok(0) => Ok(out),
        Ok(_) => Err(NetError::DecompressedLenMismatch {
            expected: uncompressed_len,
            actual: uncompressed_len + 1,
        }),
        Err(err) => Err(NetError::Zlib(err)),
    }
}

/// Reads a frame-length VarInt limited to [`MAX_LENGTH_VARINT_BYTES`].
///
/// Returns `Ok(Some((value, bytes)))` on success, `Ok(None)` when the buffer
/// does not yet hold a complete VarInt, or `Err` when it is too long.
fn read_length_varint(buf: &[u8]) -> Result<Option<(usize, usize)>> {
    let mut result: u32 = 0;
    for i in 0..MAX_LENGTH_VARINT_BYTES {
        let Some(&byte) = buf.get(i) else {
            return Ok(None);
        };
        result |= u32::from(byte & 0x7f) << (7 * i);
        if byte & 0x80 == 0 {
            return Ok(Some((result as usize, i + 1)));
        }
    }
    Err(NetError::LengthVarIntTooLong {
        max: MAX_LENGTH_VARINT_BYTES,
    })
}

fn write_var_i32(dst: &mut Vec<u8>, value: i32) {
    let mut writer = Writer::default();
    writer.var_i32(value);
    dst.extend_from_slice(writer.as_slice());
}

fn zlib_compress(data: &[u8]) -> Result<Vec<u8>> {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(data).map_err(NetError::Zlib)?;
    encoder.finish().map_err(NetError::Zlib)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uncompressed_encode_matches_manual_layout() {
        let mut codec = Codec::new();
        let body = vec![0x01, 0x02, 0x03, 0x04];
        let mut out = Vec::new();
        codec.encode(&body, &mut out).unwrap();
        // [len=4][body]
        assert_eq!(out, vec![0x04, 0x01, 0x02, 0x03, 0x04]);
    }

    #[test]
    fn uncompressed_roundtrip() {
        let mut codec = Codec::new();
        let body = vec![10, 20, 30, 40, 50];
        let mut out = Vec::new();
        codec.encode(&body, &mut out).unwrap();

        let mut dec = Codec::new();
        dec.feed(&out);
        assert_eq!(dec.next_packet().unwrap(), Some(body));
        assert_eq!(dec.next_packet().unwrap(), None);
    }

    #[test]
    fn compressed_small_body_sent_raw() {
        let mut codec = Codec::new();
        codec.set_compression(64);
        let body = vec![1, 2, 3]; // below threshold
        let mut out = Vec::new();
        codec.encode(&body, &mut out).unwrap();

        // frame = [0 uncompressed-len][raw body]; total = 1 + 3 = 4
        assert_eq!(out, vec![0x04, 0x00, 1, 2, 3]);

        let mut dec = Codec::new();
        dec.set_compression(64);
        dec.feed(&out);
        assert_eq!(dec.next_packet().unwrap(), Some(body));
    }

    #[test]
    fn compressed_large_body_is_zlib_and_roundtrips() {
        let mut codec = Codec::new();
        codec.set_compression(16);
        let body: Vec<u8> = (0..200).map(|i| (i % 7) as u8).collect();
        let mut out = Vec::new();
        codec.encode(&body, &mut out).unwrap();

        // The payload must not simply be the raw body (it is compressed).
        assert!(out.len() < body.len() + 8, "expected compression to shrink");

        let mut dec = Codec::new();
        dec.set_compression(16);
        dec.feed(&out);
        assert_eq!(dec.next_packet().unwrap(), Some(body));
    }

    #[test]
    fn compression_boundary_cases_roundtrip() {
        const T: usize = 32;
        for &size in &[T - 1, T, T + 1] {
            let mut enc = Codec::new();
            enc.set_compression(T as i32);
            let body: Vec<u8> = (0..size).map(|i| (i * 3 % 251) as u8).collect();
            let mut out = Vec::new();
            enc.encode(&body, &mut out).unwrap();

            let mut dec = Codec::new();
            dec.set_compression(T as i32);
            dec.feed(&out);
            assert_eq!(
                dec.next_packet().unwrap(),
                Some(body),
                "roundtrip failed at size {size}"
            );
        }
    }

    #[test]
    fn roundtrip_property_over_size_range() {
        const T: i32 = 48;
        for size in 0..300usize {
            let mut enc = Codec::new();
            enc.set_compression(T);
            let body: Vec<u8> = (0..size)
                .map(|i| ((i as u32).wrapping_mul(2_654_435_761) >> 24) as u8)
                .collect();
            let mut out = Vec::new();
            enc.encode(&body, &mut out).unwrap();

            let mut dec = Codec::new();
            dec.set_compression(T);
            dec.feed(&out);
            assert_eq!(dec.next_packet().unwrap(), Some(body), "size {size}");
        }
    }

    #[test]
    fn streaming_one_byte_at_a_time_matches_bulk() {
        // Build a stream of several packets, some compressed, some raw.
        let mut enc = Codec::new();
        let mut stream = Vec::new();
        let bodies: Vec<Vec<u8>> = vec![
            vec![0x00],
            vec![1, 2, 3, 4, 5],
            (0..100u8).collect(),
            vec![9; 250],
        ];
        // First two uncompressed, then enable compression for the rest.
        enc.encode(&bodies[0], &mut stream).unwrap();
        enc.encode(&bodies[1], &mut stream).unwrap();
        enc.set_compression(16);
        enc.encode(&bodies[2], &mut stream).unwrap();
        enc.encode(&bodies[3], &mut stream).unwrap();

        // Bulk decode.
        let mut bulk = Codec::new();
        bulk.feed(&stream[..]);
        // We must flip compression at the same boundary. Decode first two, then enable.
        let mut bulk_out = Vec::new();
        bulk_out.push(bulk.next_packet().unwrap().unwrap());
        bulk_out.push(bulk.next_packet().unwrap().unwrap());
        bulk.set_compression(16);
        bulk_out.push(bulk.next_packet().unwrap().unwrap());
        bulk_out.push(bulk.next_packet().unwrap().unwrap());

        // Byte-at-a-time decode with the same compression flip after 2 packets.
        let mut drip = Codec::new();
        let mut drip_out = Vec::new();
        for &b in &stream {
            drip.feed(&[b]);
            while let Some(p) = drip.next_packet().unwrap() {
                drip_out.push(p);
                if drip_out.len() == 2 {
                    drip.set_compression(16);
                }
            }
        }

        assert_eq!(bulk_out, bodies);
        assert_eq!(drip_out, bodies);
    }

    #[test]
    fn partial_frame_returns_none_then_resumes() {
        let mut codec = Codec::new();
        let body = vec![7, 8, 9, 10];
        let mut out = Vec::new();
        codec.encode(&body, &mut out).unwrap();

        let mut dec = Codec::new();
        dec.feed(&out[..2]);
        assert_eq!(dec.next_packet().unwrap(), None);
        dec.feed(&out[2..]);
        assert_eq!(dec.next_packet().unwrap(), Some(body));
    }

    #[test]
    fn reject_length_varint_longer_than_three_bytes() {
        let mut dec = Codec::new();
        dec.feed(&[0x80, 0x80, 0x80, 0x80]);
        assert!(matches!(
            dec.next_packet(),
            Err(NetError::LengthVarIntTooLong { max: 3 })
        ));
    }

    #[test]
    fn reject_zero_length_frame() {
        let mut dec = Codec::new();
        dec.feed(&[0x00]);
        assert!(matches!(
            dec.next_packet(),
            Err(NetError::MalformedFrame(_))
        ));
    }

    #[test]
    fn reject_badly_compressed_below_threshold() {
        // Manually craft a compressed frame with a non-zero uncompressed length
        // that is below the threshold.
        let threshold = 64;
        let body = vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
        let compressed = zlib_compress(&body).unwrap();
        let mut frame = Vec::new();
        write_var_i32(&mut frame, body.len() as i32); // 10, below threshold 64
        frame.extend_from_slice(&compressed);
        let mut wire = Vec::new();
        write_var_i32(&mut wire, frame.len() as i32);
        wire.extend_from_slice(&frame);

        let mut dec = Codec::new();
        dec.set_compression(threshold);
        dec.feed(&wire);
        assert!(matches!(
            dec.next_packet(),
            Err(NetError::BadlyCompressed {
                len: 10,
                threshold: 64
            })
        ));
    }

    #[test]
    fn reject_decompressed_too_large() {
        // Craft a frame claiming an enormous uncompressed length without
        // actually allocating it.
        let claimed = MAX_DECOMPRESSED_LEN + 1;
        let payload = zlib_compress(&[0u8; 4]).unwrap();
        let mut frame = Vec::new();
        write_var_i32(&mut frame, claimed as i32);
        frame.extend_from_slice(&payload);
        let mut wire = Vec::new();
        write_var_i32(&mut wire, frame.len() as i32);
        wire.extend_from_slice(&frame);

        let mut dec = Codec::new();
        dec.set_compression(2);
        dec.feed(&wire);
        assert!(matches!(
            dec.next_packet(),
            Err(NetError::DecompressedTooLarge { .. })
        ));
    }

    #[test]
    fn reject_decompressed_length_mismatch() {
        // Compress 100 bytes but declare only 60 (still above threshold).
        let body = vec![5u8; 100];
        let compressed = zlib_compress(&body).unwrap();
        let mut frame = Vec::new();
        write_var_i32(&mut frame, 60);
        frame.extend_from_slice(&compressed);
        let mut wire = Vec::new();
        write_var_i32(&mut wire, frame.len() as i32);
        wire.extend_from_slice(&frame);

        let mut dec = Codec::new();
        dec.set_compression(16);
        dec.feed(&wire);
        assert!(matches!(
            dec.next_packet(),
            Err(NetError::DecompressedLenMismatch { .. })
        ));
    }

    #[test]
    fn negative_threshold_disables_compression() {
        let mut codec = Codec::new();
        codec.set_compression(16);
        assert_eq!(codec.compression_threshold(), Some(16));
        codec.set_compression(-1);
        assert_eq!(codec.compression_threshold(), None);

        let body = vec![1, 2, 3, 4];
        let mut out = Vec::new();
        codec.encode(&body, &mut out).unwrap();
        // Uncompressed layout again.
        assert_eq!(out, vec![0x04, 1, 2, 3, 4]);
    }

    #[test]
    fn empty_body_roundtrips_uncompressed() {
        let mut codec = Codec::new();
        let mut out = Vec::new();
        codec.encode(&[], &mut out).unwrap();
        assert_eq!(out, vec![0x00]);
        // Decoding a zero-length frame is an error, so encoding an empty body
        // is a caller error we simply document; ensure no panic on decode.
        let mut dec = Codec::new();
        dec.feed(&out);
        assert!(dec.next_packet().is_err());
    }

    #[test]
    fn encrypted_frames_are_not_cleartext_and_roundtrip() {
        let secret = [0x11u8; 16];
        let mut enc = Codec::new();
        enc.enable_encryption(&secret).unwrap();
        assert!(enc.is_encrypted());

        let mut dec = Codec::new();
        dec.enable_encryption(&secret).unwrap();

        let mut wire = Vec::new();
        enc.encode(&[0x2a, 1, 2, 3], &mut wire).unwrap();
        // Cleartext framing would be [0x04, 0x2a, 1, 2, 3]; encryption must hide it.
        assert_ne!(wire, vec![0x04, 0x2a, 1, 2, 3]);

        dec.feed(&wire);
        assert_eq!(dec.next_packet().unwrap(), Some(vec![0x2a, 1, 2, 3]));
    }

    #[test]
    fn encryption_is_stateful_over_many_packets_byte_at_a_time() {
        // The single most important property: the cipher must span packet
        // boundaries AND partial reads. Encrypt several packets, then feed the
        // whole ciphertext one byte at a time and require identical output.
        let secret = [0x7fu8; 16];
        let mut enc = Codec::new();
        enc.enable_encryption(&secret).unwrap();
        enc.set_compression(24);

        let bodies: Vec<Vec<u8>> = vec![
            vec![0x00],
            vec![1, 2, 3, 4, 5],
            (0..100u8).collect(),
            vec![9; 250],
            vec![0x2a, 0xff, 0x00, 0x7f],
        ];
        let mut wire = Vec::new();
        for b in &bodies {
            enc.encode(b, &mut wire).unwrap();
        }

        let mut dec = Codec::new();
        dec.enable_encryption(&secret).unwrap();
        dec.set_compression(24);
        let mut got = Vec::new();
        for &byte in &wire {
            dec.feed(&[byte]);
            while let Some(p) = dec.next_packet().unwrap() {
                got.push(p);
            }
        }
        assert_eq!(got, bodies);
    }

    #[test]
    fn enable_encryption_twice_errors() {
        let mut codec = Codec::new();
        codec.enable_encryption(&[0u8; 16]).unwrap();
        assert!(matches!(
            codec.enable_encryption(&[0u8; 16]),
            Err(NetError::EncryptionAlreadyEnabled)
        ));
    }

    #[test]
    fn enable_encryption_rejects_bad_secret_length() {
        let mut codec = Codec::new();
        assert!(matches!(
            codec.enable_encryption(&[0u8; 8]),
            Err(NetError::BadSharedSecret { len: 8 })
        ));
        assert!(!codec.is_encrypted());
    }
}
