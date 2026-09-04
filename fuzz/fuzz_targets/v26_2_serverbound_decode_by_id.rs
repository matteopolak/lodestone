//! libFuzzer target: `V770ServerProtocol::decode` (serverbound decode), with
//! a real generated packet id selected for each input instead of an arbitrary
//! integer that usually reaches the unknown-id fallback.
//!
//! The sibling `v26_2_serverbound_decode.rs` deliberately retains arbitrary
//! ids because a peer can send an id outside the declared table. This target
//! spends one input byte on an index into the generated per-state serverbound
//! table, so mutations reach every declared decode arm without guessing a
//! four-billion-value `i32`.
//!
//! Input layout:
//! - byte 0: connection-state selector (`% 5`)
//! - byte 1: index into that state's serverbound `(name, id)` table
//! - remaining bytes: the untrusted packet payload
//!
//! A panic is deliberately allowed to escape so libFuzzer reports and saves
//! the exact input that reached it.

#![no_main]

use libfuzzer_sys::fuzz_target;
use lodestone_fuzz::Family;
use lodestone_model::ConnectionState;
use lodestone_server::ServerProtocol;
use lodestone_v26_2::V770ServerProtocol;

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
    let entries = Family::V770.serverbound_entries(state);
    if entries.is_empty() {
        return;
    }
    let (_name, packet_id) = entries[(data[1] as usize) % entries.len()];

    let proto = V770ServerProtocol;
    let _ = proto.decode(state, packet_id, &data[2..]);
});
