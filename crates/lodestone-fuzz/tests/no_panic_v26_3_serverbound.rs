//! Property: the hosted 777 `ServerProtocol::decode` must never panic on
//! arbitrary bytes, in any connection state.
//!
//! The hosted release translates every incoming id by name before it reaches the
//! shared decoders and carries its own layouts for the packets that changed
//! (`punch`, `accept_teleportation`, `sign_update`, the renumbered
//! `player_action`). That translation layer is hostile-input surface the base
//! layout fuzz in `no_panic_v26_2_serverbound.rs` never reaches, so this file
//! drives it through the release the integrated server actually hosts, with the
//! ids the 26.3 client sends.
//!
//! `decode` returns `ServerBound` directly rather than a `Result`, so the only
//! property available is "did not panic".

#![cfg(feature = "v26-3")]

use lodestone_fuzz::catch;
use lodestone_model::ConnectionState;
use lodestone_server::ServerProtocol;
use proptest::prelude::*;

const STATES: [ConnectionState; 5] = [
    ConnectionState::Handshaking,
    ConnectionState::Status,
    ConnectionState::Login,
    ConnectionState::Configuration,
    ConnectionState::Play,
];

fn serverbound_entries(state: ConnectionState) -> &'static [(&'static str, i32)] {
    use lodestone_v26_3::packet_ids::{configuration, handshaking, login, play, status};
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
    &[0x01, 0x00],
    &[0x7F, 0x7F, 0x7F, 0x7F, 0x7F],
    &[0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF],
    &[0xAA; 64],
];

#[test]
fn deterministic_sweep_over_every_declared_777_serverbound_packet_id() {
    let proto = lodestone_v26_3::server_protocol();
    let mut cases = 0usize;
    for state in STATES {
        for &(name, id) in serverbound_entries(state) {
            for payload in FIXED_PAYLOADS {
                cases += 1;
                let result = catch(|| proto.decode(state, id, payload));
                assert!(
                    result.is_ok(),
                    "v26-3 serverbound: {name} (id {id}) panicked on {state:?} payload {payload:02x?}: {}",
                    result.unwrap_err(),
                );
            }
        }
    }
    assert!(
        cases > 50,
        "expected well over 50 (packet id x fixed payload) cases, got {cases} — \
         the 26.3 serverbound packet_ids tables are probably not being reached"
    );
}

/// The control: the sweep must reach the packets whose layout changed in 26.3,
/// or it would pass over a table that holds none of the translated packets.
#[test]
fn the_sweep_reaches_the_packets_with_release_specific_layouts() {
    let names: Vec<&str> = serverbound_entries(ConnectionState::Play).iter().map(|(n, _)| *n).collect();
    for expected in ["minecraft:punch", "minecraft:accept_teleportation", "minecraft:player_action"] {
        assert!(names.contains(&expected), "{expected} is not in the 26.3 serverbound table");
    }
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
        let proto = lodestone_v26_3::server_protocol();
        let entries = serverbound_entries(state);
        let packet_id = if use_declared_id && !entries.is_empty() {
            entries[id_pick % entries.len()].1
        } else {
            arbitrary_id
        };

        let result = catch(|| proto.decode(state, packet_id, &payload));
        prop_assert!(
            result.is_ok(),
            "v26-3 serverbound: state {:?} packet_id {} payload len {} panicked: {}",
            state, packet_id, payload.len(), result.unwrap_err(),
        );
    }
}
