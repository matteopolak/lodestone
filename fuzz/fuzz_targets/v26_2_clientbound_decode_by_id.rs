//! libFuzzer target: `V770Adapter::handle_packet` (clientbound decode), fuzzed
//! by **selecting a real packet id** rather than searching for one.
//!
//! `v26_2_clientbound_decode.rs` derives `packet_id` from 4 raw fuzzer bytes
//! (`i32::from_le_bytes`), so libFuzzer has to *discover* which of the
//! relatively few valid ids per state land on an actual decode arm — the rest
//! of the ~4-billion-value `i32` space falls through to the "unknown packet"
//! catch-all, which is not new coverage and gives the mutator nothing to climb
//! toward a specific id. In practice this biases fuzzing time toward whichever
//! packets are easiest to *stumble onto* rather than spreading budget evenly —
//! exactly the risk named in issue #549: "a target covering every clientbound
//! packet id rather than the ones that look risky." The OOM this harness
//! already found (issue #549's own motivating incident) was in a hand-written
//! decode arm whose packet id is not especially easy to reach by chance; there
//! is no reason to assume every other rarely-hit arm is safe just because
//! nothing has found it yet.
//!
//! This target instead spends one fuzzer byte on an **index into the real
//! per-state packet-id table** (`Family::clientbound_entries`, the same
//! generated table production code and `v26_2_clientbound_decode.rs` both use),
//! so every declared packet id is one small-integer mutation away rather than
//! a 1-in-4-billion guess. `v26_2_clientbound_decode.rs` is not redundant with
//! this: it also explores id values *outside* the declared table (a server
//! sending an id we do not recognise at all), which this target does not, by
//! construction. Keep both.
//!
//! Input layout, from the raw fuzzer bytes:
//! - byte 0: connection state selector (`% 5`)
//! - byte 1: index into that state's clientbound `(name, id)` table
//!   (`% entries.len()`, or the target returns early if the state has none)
//! - remaining bytes: the packet payload
//!
//! Same panic contract as `v26_2_clientbound_decode.rs`: a panic here is a real
//! bug and is deliberately not caught, so libFuzzer reports and saves the
//! crashing input.

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
    if data.len() < 2 {
        return;
    }
    let state = STATES[(data[0] as usize) % STATES.len()];

    let entries = Family::V770.clientbound_entries(state);
    if entries.is_empty() {
        return;
    }
    let (_name, packet_id) = entries[(data[1] as usize) % entries.len()];

    let payload = &data[2..];

    let _ = decode_clientbound(Family::V770, state, packet_id, payload);
});
