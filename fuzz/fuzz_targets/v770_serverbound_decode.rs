//! libFuzzer target: `V770ServerProtocol::decode` (serverbound decode) must
//! never panic on arbitrary bytes, in any connection state.
//!
//! The other half of `v770_clientbound_decode.rs`'s
//! threat model: this is the attack surface our *integrated server* faces
//! from a connecting client, not what a server can do to us. v770 is the only
//! family fuzzed on this side because it is the only one that implements
//! `ServerProtocol` (`CLAUDE.md`: only v770 can host). Mirrors
//! `crates/lodestone-fuzz/tests/no_panic_v770_serverbound.rs`'s proptest
//! coverage of the exact same property, under coverage-guided search instead
//! of a bounded case count.
//!
//! Input layout: byte 0 selects the connection state (`% 5`), the rest is the
//! raw payload passed straight to `decode` with `packet_id = 0` swept by
//! libFuzzer's own mutation of byte 0's neighbours — packet id is folded into
//! the payload bytes rather than parsed out separately here, because
//! `decode`'s `packet_id` and `payload` arguments are both attacker-controlled
//! on a real connection (the frame's packet-id varint is itself untrusted
//! input the caller decodes before this function ever sees it) and treating
//! byte 1..5 as the id keeps this target's input shape identical to the
//! clientbound one for corpus-sharing purposes.
//!
//! `decode` returns `ServerBound` directly (no `Result`), so unlike the
//! clientbound side there is no clean-`Err` outcome to distinguish — the only
//! property this target checks is "did not panic", exactly as
//! `no_panic_v770_serverbound.rs`'s own module doc states.

#![no_main]

use libfuzzer_sys::fuzz_target;
use lodestone_model::ConnectionState;
use lodestone_server::ServerProtocol;
use lodestone_v770::V770ServerProtocol;

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

    let proto = V770ServerProtocol;
    let _ = proto.decode(state, packet_id, payload);
});
