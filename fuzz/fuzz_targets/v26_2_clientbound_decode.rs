//! libFuzzer target: `V770Adapter::handle_packet` (clientbound decode) must
//! never panic on arbitrary bytes, in any connection state, at any packet id.
//!
//! This is coverage-guided, in-process fuzzing — unlike
//! `crates/lodestone-fuzz`'s proptest suite (bounded-iteration, runs under a
//! plain `cargo test`, no corpus), libFuzzer explores the decoder's branch
//! structure directly and persists a corpus of inputs that found new coverage,
//! so it can run far longer and go far deeper than a `ProptestConfig::with_cases`
//! budget allows. It has **no oracle for correctness** — only "did not panic" —
//! see `docs/fuzz-harness.md` for that property's scope and `CLAUDE.md`'s
//! evidence standards for why a malformed-packet-from-a-hostile-server panic is
//! the highest-value target here: a real client accepts arbitrary bytes from
//! whatever server it's told to join.
//!
//! Reuses `lodestone_fuzz::decode_clientbound` (the same entry point
//! `crates/lodestone-fuzz/tests/no_panic_arbitrary_bytes.rs` drives) rather than
//! re-deriving adapter construction, so this target automatically covers
//! whatever packet ids the generated `packet_ids` tables declare — it does not
//! hand-list them.
//!
//! Input layout, from the raw fuzzer bytes:
//! - byte 0: connection state selector (`% 5`)
//! - bytes 1..5: packet id (`i32::from_le_bytes`, zero-padded if fewer than 4
//!   bytes remain)
//! - remaining bytes: the packet payload
//!
//! A panic here means a real bug — decoding a clientbound packet must never
//! panic regardless of state or payload, matching the property
//! `no_panic_arbitrary_bytes.rs` already checks for all four families under
//! proptest; this target is the same property under coverage-guided search for
//! v26-2 specifically, the one family that also hosts (`ServerProtocol`), and
//! v26-2 is by far the most actively developed of the four (see `CLAUDE.md`'s
//! "new gameplay work targets v26-2" default).

#![no_main]

use libfuzzer_sys::fuzz_target;
use lodestone_fuzz::{Family, decode_clientbound};
use lodestone_model::ConnectionState;

const STATES: [ConnectionState; 5] = [
    ConnectionState::Handshaking,
    ConnectionState::Status,
    ConnectionState::Login,
    ConnectionState::Configuration,
    ConnectionState::Play,
];

fuzz_target!(|data: &[u8]| {
    if data.is_empty() {
        return;
    }
    let state = STATES[(data[0] as usize) % STATES.len()];

    let mut id_bytes = [0u8; 4];
    let id_len = data.len().saturating_sub(1).min(4);
    id_bytes[..id_len].copy_from_slice(&data[1..1 + id_len]);
    let packet_id = i32::from_le_bytes(id_bytes);

    let payload = &data[(1 + id_len).min(data.len())..];

    // A panic propagates out of the closure and libFuzzer reports the crash
    // with the exact input that triggered it (saved to `fuzz/artifacts/`) —
    // deliberately NOT wrapped in `lodestone_fuzz::catch`, which exists for
    // property tests that must keep running past one failing case; a fuzz
    // target's whole job is to let the crash surface.
    let _ = decode_clientbound(Family::V770, state, packet_id, payload);
});
