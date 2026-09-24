//! Property: `V770ServerProtocol::decode` must never panic on arbitrary
//! bytes, in any connection state.
//!
//! Every other file in this crate fuzzes the *client* half — decoding
//! packets a server sends us. This one fuzzes the other direction: our
//! *integrated server* decoding packets a connecting client sends it
//! (`lodestone-server`'s `ServerProtocol::decode`, implemented only by
//! `v26-2` — the only family that can host, per `CLAUDE.md`). A malicious or
//! merely buggy client is exactly as untrusted an input source as a
//! malicious server, and this is the one crate in the workspace that hosts.
//!
//! `crates/versions/26.2/src/server_protocol.rs` is owned by another agent
//! at the time this harness was written (serverbound decode work in
//! progress). This file only *calls* its public API — `decode` — through
//! `lodestone_server::ServerProtocol`; it does not read or edit that file's
//! contents, and if this finds a bug there it is reported, not patched here.
//!
//! `decode` returns `ServerBound` directly rather than a `Result` (see the
//! trait doc in `crates/lodestone-server/src/protocol.rs`), so unlike the
//! clientbound side there is no `Err` to distinguish from a clean decode —
//! the only property this file can check is "did not panic".

// `V770ServerProtocol` is the only `ServerProtocol` implementation, so this file
// exists only in a build that compiles `v26-2` in. On by default; see the crate
// manifest's `[features]`.
#![cfg(feature = "v26-2")]

use lodestone_fuzz::catch;
use lodestone_model::ConnectionState;
use lodestone_server::ServerProtocol;
use lodestone_v26_2::V770ServerProtocol;
use proptest::prelude::*;

const STATES: [ConnectionState; 5] = [
    ConnectionState::Handshaking,
    ConnectionState::Status,
    ConnectionState::Login,
    ConnectionState::Configuration,
    ConnectionState::Play,
];

fn serverbound_entries(state: ConnectionState) -> &'static [(&'static str, i32)] {
    use lodestone_v26_2::packet_ids::{configuration, handshaking, login, play, status};
    match state {
        ConnectionState::Handshaking => handshaking::serverbound::ENTRIES,
        ConnectionState::Status => status::serverbound::ENTRIES,
        ConnectionState::Login => login::serverbound::ENTRIES,
        ConnectionState::Configuration => configuration::serverbound::ENTRIES,
        ConnectionState::Play => play::serverbound::ENTRIES,
    }
}

const FIXED_PAYLOADS: &[&[u8]] = &[
    &[],
    &[0x00],
    &[0xFF],
    &[0x7F, 0x7F, 0x7F, 0x7F, 0x7F],
    &[0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF],
    &[0xAA; 64],
];

#[test]
fn deterministic_sweep_over_every_declared_v770_serverbound_packet_id() {
    let proto = V770ServerProtocol;
    let mut cases = 0usize;
    for state in STATES {
        for &(name, id) in serverbound_entries(state) {
            for payload in FIXED_PAYLOADS {
                cases += 1;
                let result = catch(|| proto.decode(state, id, payload));
                assert!(
                    result.is_ok(),
                    "v26-2 serverbound: {name} (id {id}) panicked on {state:?} payload {payload:02x?}: {}",
                    result.unwrap_err(),
                );
            }
        }
    }
    assert!(
        cases > 50,
        "expected well over 50 (packet id x fixed payload) cases, got {cases} — \
         the serverbound packet_ids tables are probably not being reached"
    );
}

fn arb_state() -> impl Strategy<Value = ConnectionState> {
    (0..STATES.len()).prop_map(|i| STATES[i])
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn decode_never_panics(
        state in arb_state(),
        use_declared_id in prop::bool::weighted(0.875),
        id_pick in any::<usize>(),
        arbitrary_id in any::<i32>(),
        payload in prop::collection::vec(any::<u8>(), 0..4096),
    ) {
        let proto = V770ServerProtocol;
        let entries = serverbound_entries(state);
        let packet_id = if use_declared_id && !entries.is_empty() {
            entries[id_pick % entries.len()].1
        } else {
            arbitrary_id
        };

        let result = catch(|| proto.decode(state, packet_id, &payload));
        prop_assert!(
            result.is_ok(),
            "v26-2 serverbound: state {:?} packet_id {} payload len {} panicked: {}",
            state, packet_id, payload.len(), result.unwrap_err(),
        );
    }
}
