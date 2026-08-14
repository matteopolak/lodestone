//! **A singleplayer world's tab list is empty, even of the player who opened
//! it** — owner-reported ("pressing Tab alone shows a tiny empty list"),
//! measured on the wire, from a real join to a real [`IntegratedServer`].
//!
//! # Why this is not `server_player_entity_stream.rs` again
//!
//! That file (and `lan_player_stream.rs` for the `bind` constructor) already
//! prove the *mechanism* — `crate::players::PlayerRegistry` and
//! `crate::server::stream_pass` correctly put the viewer in their own roster
//! once a `PlayerAwareSource` is composed. Both drive that composition by hand.
//!
//! `IntegratedServer::open_in_memory_with_mobs` / `open_persistent_with_mobs`
//! — the constructors real singleplayer and persistent-world sessions actually
//! call — are a **third**, independent composition site, and before this fix
//! neither wired a `PlayerAwareSource` at all: the connection's `EntitySource`
//! was a bare `LiveMobSource` clone, whose `EntitySource::players()` default
//! answers `None`. `stream_pass`'s tab-list branch is gated on that being
//! `Some`, so no `player_info_update` — not even one naming the local player —
//! was ever sent. Every assertion in the other two files stayed green the
//! entire time, because neither one calls through `integrated.rs`. That is the
//! island failure mode this file exists to close: the wiring, not the
//! mechanism, and only the real production entry point can prove it.
//!
//! **Verified as a control**: reverting `open_in_memory_with_mobs_using`'s
//! `conn_entities` binding to a bare `live_mobs.clone()` (its pre-fix form)
//! makes [`a_singleplayer_join_lists_the_local_player_in_its_own_tab_list`]
//! fail with *"a singleplayer join must receive a player_info_update naming
//! itself — got zero PLAYER_INFO_UPDATE packets of any kind"*.

use std::time::Duration;

use lodestone_core::{Decode, Reader, Writer};
use lodestone_net::{Connection, Transport};
use lodestone_server::{ChunkColumn, ChunkSource, IntegratedServer};
use lodestone_testsupport::unique_username;
use lodestone_v770::V770ServerProtocol;
use lodestone_v770::packet_ids::{configuration, login, play};
use lodestone_v770::packets::player_info::PlayerInfoUpdate;
use uuid::Uuid;

const MIN_Y: i32 = -64;
const HEIGHT: i32 = 384;

/// A flat, solid floor everywhere — the same shape `server_no_demo_mobs.rs`
/// uses, so the join's world-spawn search terminates quickly instead of
/// scanning every candidate in an all-air world.
struct FlatSource;

impl ChunkSource for FlatSource {
    fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
        let mut column = ChunkColumn::new(MIN_Y, HEIGHT);
        for x in 0..16 {
            for z in 0..16 {
                for y in MIN_Y..=64 {
                    column.set_block(x, y, z, "minecraft:stone");
                }
            }
        }
        column
    }

    fn block_state(&self, x: i32, y: i32, z: i32) -> String {
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        self.column(cx, cz)
            .block_state(x.rem_euclid(16), y, z.rem_euclid(16))
            .to_string()
    }

    fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {
        // Read-only fixture.
    }
}

fn handshake_bytes() -> Vec<u8> {
    let mut w = Writer::default();
    w.var_i32(776);
    w.string("localhost");
    w.u16(25565);
    w.var_i32(2);
    w.into_vec()
}

fn hello_bytes(name: &str, uuid: Uuid) -> Vec<u8> {
    let mut w = Writer::default();
    w.string(name);
    w.uuid(uuid);
    w.into_vec()
}

async fn drain<T: Transport>(client: &mut Connection<T>) -> Vec<(i32, Vec<u8>)> {
    const QUIET: Duration = Duration::from_millis(250);
    let mut out = Vec::new();
    while let Ok(Ok(Some(packet))) = tokio::time::timeout(QUIET, client.read_packet()).await {
        out.push(packet);
    }
    out
}

/// Drives handshake → login → configuration → play and returns every
/// clientbound packet received on the way, in order.
async fn join<T: Transport>(client: &mut Connection<T>, name: &str, uuid: Uuid) -> Vec<(i32, Vec<u8>)> {
    client.write_packet(0, &handshake_bytes()).await.unwrap();
    client.write_packet(0, &hello_bytes(name, uuid)).await.unwrap();
    let mut seen = Vec::new();
    if let Ok(Some(p)) = client.read_packet().await {
        seen.push(p);
    }
    client
        .write_packet(login::serverbound::LOGIN_ACKNOWLEDGED, &[])
        .await
        .unwrap();
    if let Ok(Some(p)) = client.read_packet().await {
        seen.push(p);
    }
    client
        .write_packet(configuration::serverbound::FINISH_CONFIGURATION, &[])
        .await
        .unwrap();
    seen.extend(drain(client).await);
    seen
}

/// Every `(uuid, name)` pair carried by every `player_info_update` in
/// `packets`, decoded with the pre-existing client-side decoder — the same
/// independent-decoder shape `server_player_entity_stream.rs` uses, so
/// agreement is evidence about the wire rather than about one encoder
/// mirroring itself.
fn roster(packets: &[(i32, Vec<u8>)]) -> Vec<(Uuid, Option<String>)> {
    packets
        .iter()
        .filter(|(id, _)| *id == play::clientbound::PLAYER_INFO_UPDATE)
        .flat_map(|(_, payload)| {
            let mut r = Reader::new(payload);
            let decoded = PlayerInfoUpdate::decode(&mut r, lodestone_core::Ctx { version: 776 })
                .expect("our own player_info_update must parse under the client-side decoder");
            decoded.entries
        })
        .map(|entry| (entry.uuid, entry.name))
        .collect()
}

/// The whole bug, in one join: a singleplayer session must list the joining
/// player in their own tab list, exactly as vanilla always does.
///
/// `open_in_memory_with_mobs` rather than `open_persistent_with_mobs`: both
/// share the same `open_in_memory_with_mobs_using` composition this fix
/// touches (see that function's own doc comment), so the ephemeral
/// constructor is the cheaper way to exercise it.
#[tokio::test]
async fn a_singleplayer_join_lists_the_local_player_in_its_own_tab_list() {
    let name = unique_username();
    let uuid = Uuid::from_u128(0x5163_0000_0000_0000_0000_0000_0000_0001);

    let (_server, client_io) = IntegratedServer::open_in_memory_with_mobs(
        V770ServerProtocol,
        FlatSource,
        (-2..=2, -2..=2),
        (8, 8),
        0,
        1,
    );
    let mut client = Connection::new(client_io);
    let seen = join(&mut client, &name, uuid).await;

    // Premise: the join actually completed, or an empty roster would be
    // meaningless — it could just as well mean the session never reached Play.
    assert!(
        seen.iter().any(|(id, _)| *id == play::clientbound::LOGIN),
        "premise: the join must have reached Play"
    );

    let entries = roster(&seen);
    assert!(
        entries.iter().any(|(u, _)| *u == uuid),
        "a singleplayer join must receive a player_info_update naming itself — \
         got {} PLAYER_INFO_UPDATE entries: {:?}",
        entries.len(),
        entries
    );
    assert_eq!(
        entries.iter().find(|(u, _)| *u == uuid).and_then(|(_, n)| n.as_deref()),
        Some(name.as_str()),
        "the self entry's ADD_PLAYER name must be this connection's own login name"
    );
}
