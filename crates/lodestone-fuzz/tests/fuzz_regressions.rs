//! Committed regression inputs for defects the fuzz targets actually found.
//!
//! ## Why this file exists at all
//!
//! `no_panic_arbitrary_bytes.rs` generates its input, so whether a given run
//! reaches a defect is a property of proptest's RNG. Issue #450's remote panic
//! failed a full-crate run and **passed a filtered single-target run twice** —
//! an intermittent red is harder to act on than a deterministic one and much
//! easier to write off as flake, which is roughly what happened. So the bytes a
//! target found a bug with get committed here and asserted directly, and the
//! gate stops being probabilistic.
//!
//! `tests/no_panic_arbitrary_bytes.proptest-regressions` (committed alongside)
//! makes proptest replay the same seed first, which is a second, independent
//! path to the same input. Neither replaces the other: the seed file re-runs the
//! whole generator case (and would go stale if the strategy changed), while the
//! fixture here pins the literal payload and asserts *what* the decoder does
//! with it, not merely that it survives.
//!
//! ## What "fixed" means here, and why "no panic" is not enough
//!
//! The #450 multiply panicked in debug and **silently wrapped in release**. A
//! test that only asserts "did not panic" is satisfied by the wrap, i.e. by the
//! worse of the two original outcomes. A test that asserts "did not panic and
//! clamped" is satisfied by inventing a position the packet never named. So
//! every assertion below is about *refusal*: an `AdapterError::Decode`, and
//! **zero** writes reaching the world sink.

// This file drives `lodestone_v340` directly, so it exists only in a build that
// compiles that family in. On by default; see the crate manifest's `[features]`.
#![cfg(feature = "v340")]

use std::sync::{Arc, Mutex};

use lodestone_core::Nbt;
use lodestone_model::{AdapterError, ConnectionState, VersionAdapter};
use lodestone_v340::packet_ids::play;
use lodestone_world::{
    BiomePatch, BlockEntitySync, ChunkPos, ColumnPatch, LightPatch, LoadedChunk, WorldSink,
};

/// A [`WorldSink`] that records the block writes it is asked to perform.
///
/// `lodestone_fuzz::NullSink` discards everything, which is right for "does this
/// panic" but cannot distinguish *refused* from *wrapped to a wrong position* —
/// and that distinction is the entire content of #450's release-mode half.
#[derive(Debug, Default, Clone)]
struct RecordingSink {
    /// `(x, y, z, state)` for every `set_block`, in call order.
    blocks: Arc<Mutex<Vec<(i32, i32, i32, u32)>>>,
}

impl RecordingSink {
    fn writes(&self) -> Vec<(i32, i32, i32, u32)> {
        self.blocks.lock().expect("recording sink poisoned").clone()
    }
}

impl WorldSink for RecordingSink {
    fn load(&mut self, _pos: ChunkPos, _chunk: LoadedChunk) {}
    fn merge(&mut self, _pos: ChunkPos, _patch: ColumnPatch) {}
    fn set_block(&mut self, x: i32, y: i32, z: i32, state: u32) {
        self.blocks
            .lock()
            .expect("recording sink poisoned")
            .push((x, y, z, state));
    }
    fn set_blocks(
        &mut self,
        _section_x: i32,
        _section_y: i32,
        _section_z: i32,
        _blocks: &[(u8, u8, u8, u32)],
    ) {
    }
    fn merge_light(&mut self, _pos: ChunkPos, _patch: LightPatch) {}
    fn merge_biomes(&mut self, _pos: ChunkPos, _patch: BiomePatch) {}
    fn unload(&mut self, _pos: ChunkPos) {}
    fn set_block_entity(&mut self, _x: i32, _y: i32, _z: i32, _type_id: u32, _nbt: Nbt) {}
    fn sync_block_entity(
        &mut self,
        _x: i32,
        _y: i32,
        _z: i32,
        _block_entity_type: Option<u32>,
    ) -> BlockEntitySync {
        BlockEntitySync::ChunkAbsent
    }
}

/// The committed #450 payload, byte for byte.
fn overflow_payload() -> Vec<u8> {
    let path =
        lodestone_fuzz::regression_fixture_path("v340_multi_block_change_chunk_overflow.hex");
    let bytes = lodestone_fuzz::read_hex_fixture(&path);
    assert_eq!(
        bytes,
        vec![8, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0],
        "the fixture must still be the exact twelve bytes proptest shrank to (recorded in \
         tests/no_panic_arbitrary_bytes.proptest-regressions); if this fires, the fixture was \
         edited and no longer has the provenance its header claims"
    );
    bytes
}

/// Same twelve-byte shape with `chunk_x` replaced, so the legal cases below are
/// tied to the offending input rather than being an independently invented
/// packet that might differ in some other field.
fn payload_with_chunk_x(chunk_x: i32) -> Vec<u8> {
    let mut bytes = overflow_payload();
    bytes[0..4].copy_from_slice(&chunk_x.to_be_bytes());
    bytes
}

/// Drives the real `handle_packet` the production driver drives, through a
/// recording sink, catching a panic rather than aborting the binary.
fn decode_multi_block_change(payload: &[u8]) -> (Result<(), String>, Vec<(i32, i32, i32, u32)>) {
    let sink = RecordingSink::default();
    let outcome = {
        let mut sink = sink.clone();
        lodestone_fuzz::catch(move || {
            let adapter = lodestone_v340::V340Adapter::default();
            adapter
                .handle_packet(
                    &mut sink,
                    ConnectionState::Play,
                    play::clientbound::MULTI_BLOCK_CHANGE,
                    payload,
                )
                .map(|_| ())
        })
    };
    let writes = sink.writes();
    match outcome {
        // Flatten "panicked" and "returned Err" into one channel the callers
        // can assert on separately: `Ok(Ok(()))` accepted, `Ok(Err(e))`
        // refused, `Err(p)` panicked.
        Ok(Ok(())) => (Ok(()), writes),
        Ok(Err(AdapterError::Decode(msg))) => (Err(format!("Decode: {msg}")), writes),
        Ok(Err(other)) => (Err(format!("non-Decode error: {other:?}")), writes),
        Err(panic) => (Err(format!("PANIC: {panic}")), writes),
    }
}

/// Issue #450. `chunk_x = 134_217_728` makes `chunk_x * 16` exceed `i32::MAX`.
///
/// Three distinct wrong behaviours this must reject, all of which a bare "did
/// not panic" assertion would accept:
///
/// | behaviour | how this test catches it |
/// |---|---|
/// | debug panic (the filed bug) | `PANIC:` prefix, no `Decode` |
/// | release wrap to `i32::MIN` | a recorded write at a wrapped coordinate |
/// | a silent clamp to the border | a recorded write at all |
#[test]
fn v340_multi_block_change_refuses_a_chunk_coordinate_that_would_overflow() {
    let payload = overflow_payload();
    let (outcome, writes) = decode_multi_block_change(&payload);

    let err = outcome.expect_err(
        "an out-of-range chunk coordinate must be refused, not accepted — accepting it means \
         some block position was invented from a wrapped or clamped multiply",
    );
    assert!(
        err.starts_with("Decode: "),
        "expected a clean AdapterError::Decode refusal; got {err}"
    );
    assert!(
        err.contains("134217728"),
        "the refusal should name the offending coordinate so a real malformed-server report is \
         actionable; got {err}"
    );

    assert!(
        writes.is_empty(),
        "the decoder wrote {} block(s) {writes:?} for a packet it refused. Refusal has to mean \
         nothing reached the world: a wrapped multiply (release-mode #450) or a silent clamp \
         both show up here as a write at a position the packet never named.",
        writes.len()
    );
}

/// The bound is the **world border**, not merely "does not overflow `i32`".
///
/// This is the assertion a `checked_mul`-only fix fails. `chunk_x = 1_875_000`
/// multiplies to 30,000,000 — perfectly representable in `i32`, so `checked_mul`
/// returns `Some` — but it is past `WorldBorder.absoluteMaxSize` (29,999,984),
/// so it names a block position no vanilla world can contain. Its neighbour
/// `1_874_999` lands on exactly 29,999,984 and must be accepted, which is what
/// stops this pair from passing for a decoder that simply refuses everything
/// large.
#[test]
fn v340_multi_block_change_bound_is_the_world_border_not_just_i32_range() {
    const LAST_LEGAL_CHUNK_X: i32 = 29_999_984 / 16; // 1_874_999
    assert_eq!(LAST_LEGAL_CHUNK_X, 1_874_999);
    assert_eq!(LAST_LEGAL_CHUNK_X * 16, 29_999_984);

    let (outcome, writes) = decode_multi_block_change(&payload_with_chunk_x(LAST_LEGAL_CHUNK_X));
    outcome.unwrap_or_else(|err| {
        panic!(
            "chunk x {LAST_LEGAL_CHUNK_X} sits exactly on the world border at block \
             {}, which is legal, but the decoder refused it: {err}",
            LAST_LEGAL_CHUNK_X * 16
        )
    });
    let positions: Vec<(i32, i32, i32)> = writes.iter().map(|&(x, y, z, _)| (x, y, z)).collect();
    assert_eq!(
        positions,
        vec![(29_999_984, 0, 0)],
        "the accepted case must write at chunk_x * 16 exactly; a wrong origin here would mean \
         the guard changed the arithmetic rather than just bounding it"
    );

    let over = LAST_LEGAL_CHUNK_X + 1;
    let (outcome, writes) = decode_multi_block_change(&payload_with_chunk_x(over));
    let err = outcome.expect_err(&format!(
        "chunk x {over} multiplies to {} blocks, past the world border's {}. It fits in an i32, \
         so a checked-multiply-only fix accepts it — the decode-time range check is what refuses \
         it, and this is the assertion that proves the range check exists.",
        i64::from(over) * 16,
        29_999_984,
    ));
    assert!(err.starts_with("Decode: "), "expected a clean refusal; got {err}");
    assert!(writes.is_empty(), "refused packet still wrote {writes:?}");
}

/// Positive control for both tests above: an ordinary chunk coordinate still
/// decodes and still writes where it should. Without this, a decoder that
/// refused *every* `multi_block_change` would satisfy the refusal assertions.
#[test]
fn v340_multi_block_change_still_decodes_an_ordinary_chunk_coordinate() {
    let (outcome, writes) = decode_multi_block_change(&payload_with_chunk_x(3));
    outcome.expect("an ordinary chunk x of 3 must still decode");
    assert_eq!(
        writes.len(),
        1,
        "one record in, one write out; got {writes:?}"
    );
    let (x, y, z, _state) = writes[0];
    assert_eq!(
        (x, y, z),
        (48, 0, 0),
        "chunk x 3 with relative x 0 is block x 48 (3 * 16); a different value means the origin \
         arithmetic changed when the guard was added"
    );
}

/// Negative-side coverage: the multiply overflows in both directions, and
/// `i32::MIN`'s absolute value does not fit in `i32`, so a guard written with
/// `i32::abs` rather than `unsigned_abs` would itself panic here.
#[test]
fn v340_multi_block_change_refuses_deeply_negative_chunk_coordinates() {
    for chunk_x in [i32::MIN, i32::MIN / 16, -1_875_000, -134_217_728] {
        let (outcome, writes) = decode_multi_block_change(&payload_with_chunk_x(chunk_x));
        let err = outcome.expect_err(&format!(
            "chunk x {chunk_x} must be refused, it was accepted instead"
        ));
        assert!(
            err.starts_with("Decode: "),
            "chunk x {chunk_x}: expected a clean refusal, got {err}"
        );
        assert!(
            writes.is_empty(),
            "chunk x {chunk_x}: refused packet still wrote {writes:?}"
        );
    }
}
