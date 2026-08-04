//! Property: `lodestone_net::Codec` — the frame-level length/compression
//! codec every packet passes through *before* any `VersionAdapter` sees
//! it — must never panic, and must always terminate, on arbitrary bytes.
//!
//! Issue #282's own body calls this codec out by name as the reason its
//! scope could stay narrow: `MAX_LENGTH_VARINT_BYTES`, `MAX_PACKET_LEN`, and
//! `MAX_DECOMPRESSED_LEN` in `crates/lodestone-net/src/codec.rs` bound the
//! length prefix and both compressed/decompressed sizes *before allocating*,
//! so "the immediate risk is lower than a malformed packet panics the
//! server" — but, per the issue, "that conclusion currently rests on manual
//! code reading, not on any automated adversarial-input regression net."
//! This file is that regression net for this one piece.

use lodestone_fuzz::catch;
use lodestone_net::Codec;
use proptest::prelude::*;

/// Feeds `data` in `chunk_sizes`-sized pieces (falling back to feeding the
/// rest in one call once `chunk_sizes` runs out), draining `next_packet()`
/// after every feed. Mirrors real usage: a socket read never hands the codec
/// a whole frame in one call, per `partial_frame_returns_none_then_resumes`
/// in `codec.rs`'s own test module.
fn drive(codec: &mut Codec, data: &[u8], chunk_sizes: &[usize]) {
    let mut offset = 0;
    let mut chunk_idx = 0;

    while offset < data.len() {
        let remaining = data.len() - offset;
        let take = if chunk_idx < chunk_sizes.len() {
            chunk_sizes[chunk_idx].clamp(1, remaining)
        } else {
            remaining
        };
        chunk_idx += 1;

        codec.feed(&data[offset..offset + take]);
        offset += take;

        // Draining to `None`/`Err` each feed, capped, proves `next_packet`
        // terminates rather than looping forever on adversarial input — the
        // brief's explicit "decode must terminate" property. A codec that
        // never returns `None`/`Err` and instead spins would hit this cap
        // and fail the assertion below instead of hanging the test suite.
        for _ in 0..10_000 {
            match codec.next_packet() {
                Ok(Some(_)) => continue,
                Ok(None) | Err(_) => break,
            }
        }
    }
}

#[test]
fn known_malformed_inputs_never_panic() {
    let cases: &[&[u8]] = &[
        &[],
        &[0x00],
        // A length VarInt longer than MAX_LENGTH_VARINT_BYTES (3): four
        // continuation bytes.
        &[0x80, 0x80, 0x80, 0x80],
        // A claimed frame length far larger than MAX_PACKET_LEN, no body.
        &[0xFF, 0xFF, 0xFF, 0x7F],
        &[0xAA; 256],
    ];
    for &compression in &[None, Some(0i32), Some(16i32)] {
        for case in cases {
            let result = catch(|| {
                let mut codec = Codec::new();
                if let Some(threshold) = compression {
                    codec.set_compression(threshold);
                }
                drive(&mut codec, case, &[]);
            });
            assert!(
                result.is_ok(),
                "Codec panicked on {case:02x?} (compression {compression:?}): {}",
                result.unwrap_err()
            );
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn feed_and_drain_never_panics(
        data in prop::collection::vec(any::<u8>(), 0..8192),
        chunk_sizes in prop::collection::vec(1usize..64, 0..64),
        compression_threshold in prop::option::of(0i32..64),
    ) {
        let result = catch(|| {
            let mut codec = Codec::new();
            if let Some(threshold) = compression_threshold {
                codec.set_compression(threshold);
            }
            drive(&mut codec, &data, &chunk_sizes);
        });
        prop_assert!(
            result.is_ok(),
            "Codec panicked on {} bytes fed in {} chunks (compression {:?}): {}",
            data.len(), chunk_sizes.len(), compression_threshold, result.unwrap_err(),
        );
    }
}
