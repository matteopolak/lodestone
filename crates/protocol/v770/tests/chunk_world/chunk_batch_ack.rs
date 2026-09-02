//! Hermetic tests for protocol 776 chunk-batch acknowledgement dispatch.
//!
//! `chunk_batch_start` is an empty timing marker; `chunk_batch_finished` carries
//! a VarInt batch size and must be answered with a serverbound
//! `chunk_batch_received` carrying a big-endian f32 rate, or the server halts
//! chunk delivery after ten unacknowledged batches. A zero-size batch is a no-op
//! in the estimator, so it acknowledges the deterministic seed rate 3.5 — a
//! golden the timing-dependent cases cannot provide.

use lodestone_model::{ConnectionState, Directive, VersionAdapter};
use lodestone_v770::V770Adapter;
use lodestone_v770::packet_ids::play;
use lodestone_world::World;

fn handle(adapter: &V770Adapter, id: i32, payload: &[u8]) -> Vec<Directive> {
    adapter
        .handle_packet(&mut World::new(), ConnectionState::Play, id, payload)
        .expect("handle chunk batch packet")
}

#[test]
fn chunk_batch_start_is_an_empty_timing_marker() {
    let adapter = V770Adapter::new();
    assert_eq!(
        handle(&adapter, play::clientbound::CHUNK_BATCH_START, &[]),
        vec![]
    );
}

#[test]
fn chunk_batch_start_rejects_trailing_bytes() {
    let adapter = V770Adapter::new();
    let result = adapter.handle_packet(
        &mut World::new(),
        ConnectionState::Play,
        play::clientbound::CHUNK_BATCH_START,
        &[0x00],
    );
    assert!(result.is_err(), "a trailing byte must fail, got {result:?}");
}

#[test]
fn chunk_batch_finished_sends_received_ack() {
    let adapter = V770Adapter::new();
    // batch_size 5 (VarInt 0x05).
    match handle(&adapter, play::clientbound::CHUNK_BATCH_FINISHED, &[0x05]).as_slice() {
        [Directive::Send { packet_id, payload }] => {
            assert_eq!(*packet_id, play::serverbound::CHUNK_BATCH_RECEIVED);
            assert_eq!(payload.len(), 4, "rate is a single big-endian f32");
            let rate = f32::from_be_bytes(payload[..].try_into().unwrap());
            assert!(
                rate.is_finite() && rate > 0.0,
                "desired rate must be finite and positive, got {rate}"
            );
        }
        other => panic!("expected a single CHUNK_BATCH_RECEIVED send, got {other:?}"),
    }
}

#[test]
fn empty_batch_acknowledges_the_seed_rate() {
    let adapter = V770Adapter::new();
    // batch_size 0 is a no-op in the estimator, so the seed rate 3.5 is reported.
    match handle(&adapter, play::clientbound::CHUNK_BATCH_FINISHED, &[0x00]).as_slice() {
        [Directive::Send { packet_id, payload }] => {
            assert_eq!(*packet_id, play::serverbound::CHUNK_BATCH_RECEIVED);
            let rate = f32::from_be_bytes(payload[..].try_into().unwrap());
            assert_eq!(rate, 3.5);
        }
        other => panic!("expected a single CHUNK_BATCH_RECEIVED send, got {other:?}"),
    }
}
