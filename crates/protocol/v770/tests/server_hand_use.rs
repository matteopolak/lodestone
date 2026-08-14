//! Spot check: a right-click actually operates a block on a real
//! served connection.
//!
//! `crate::hand_use`'s own unit tests cover the five families' *decisions*. The
//! only thing they cannot see is whether `apply_use_item_on` reaches them at all —
//! before this, `UseItemOn` against a door fell through to the placement branch
//! and silently did nothing. So this drives the real `V770ServerProtocol` and reads
//! `block_update` off the wire.

use std::time::Duration;

use lodestone_core::{Reader, Writer};
use lodestone_net::{Connection, Transport};
use lodestone_server::{ChunkColumn, ChunkSource, IntegratedServer};
use lodestone_v770::V770ServerProtocol;
use lodestone_v770::packet_ids::{configuration, login, play};
use uuid::Uuid;

mod common;
use common::unique_username;

const MIN_Y: i32 = -64;
const HEIGHT: i32 = 384;
const FLOOR_TOP_Y: i32 = 64;

/// Local `(x, z)` of the door's lower half, and of the lever.
const DOOR_X: i32 = 4;
const DOOR_Z: i32 = 4;
const LEVER_X: i32 = 6;
const LEVER_Z: i32 = 6;

/// Flat stone floor with a closed oak door (both halves) and an unpowered wall
/// lever standing on it.
struct FixtureSource;

impl ChunkSource for FixtureSource {
    fn column(&self, cx: i32, cz: i32) -> ChunkColumn {
        let mut column = ChunkColumn::new(MIN_Y, HEIGHT);
        for x in 0..16 {
            for z in 0..16 {
                for y in MIN_Y..=FLOOR_TOP_Y {
                    column.set_block(x, y, z, "minecraft:stone");
                }
            }
        }
        if (cx, cz) == (0, 0) {
            column.set_block(
                DOOR_X,
                FLOOR_TOP_Y + 1,
                DOOR_Z,
                "minecraft:oak_door[facing=north,half=lower,hinge=left,open=false,powered=false]",
            );
            column.set_block(
                DOOR_X,
                FLOOR_TOP_Y + 2,
                DOOR_Z,
                "minecraft:oak_door[facing=north,half=upper,hinge=left,open=false,powered=false]",
            );
            column.set_block(
                LEVER_X,
                FLOOR_TOP_Y + 1,
                LEVER_Z,
                "minecraft:lever[face=floor,facing=north,powered=false]",
            );
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
        // `IntegratedServer` wraps this in a `ChunkStore`, which is what retains
        // edits; this fixture needs no retention of its own.
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

/// Serverbound `use_item_on`: VarInt hand, packed block pos, VarInt face, three
/// `f32` cursor coords, `bool` inside, `bool` world border, VarInt sequence.
fn use_item_on_bytes(x: i32, y: i32, z: i32, sequence: i32) -> Vec<u8> {
    let mut w = Writer::default();
    w.var_i32(0); // main hand
    let packed = ((i64::from(x) & 0x3FF_FFFF) << 38)
        | ((i64::from(z) & 0x3FF_FFFF) << 12)
        | (i64::from(y) & 0xFFF);
    w.i64(packed);
    w.var_i32(1); // face: up
    w.f32(0.5);
    w.f32(1.0);
    w.f32(0.5);
    w.bool(false); // inside
    w.bool(false); // hit world border
    w.var_i32(sequence);
    w.into_vec()
}

async fn drain<T: Transport>(client: &mut Connection<T>) -> Vec<(i32, Vec<u8>)> {
    const QUIET: Duration = Duration::from_millis(220);
    let mut out = Vec::new();
    while let Ok(Ok(Some(packet))) = tokio::time::timeout(QUIET, client.read_packet()).await {
        out.push(packet);
    }
    out
}

/// `(x, y, z, state_id)` of every `block_update`: packed pos then a VarInt state
/// id.
fn block_updates(packets: &[(i32, Vec<u8>)]) -> Vec<(i32, i32, i32, i32)> {
    packets
        .iter()
        .filter(|(id, _)| *id == play::clientbound::BLOCK_UPDATE)
        .map(|(_, payload)| {
            let mut r = Reader::new(payload);
            let packed = r.i64().expect("block_update pos");
            let state = r.var_i32().expect("block_update state");
            let x = (packed >> 38) as i32;
            let y = ((packed << 52) >> 52) as i32;
            let z = ((packed << 26) >> 38) as i32;
            (x, y, z, state)
        })
        .collect()
}

/// The registry id of a full block-state string, resolved the same three-tier way
/// `server_protocol.rs` does (exact match, then the block's lowest id, then air).
/// The expected values below come from `lodestone_data`'s generated table, not from
/// our encoder.
fn state_id(state: &str) -> u32 {
    use lodestone_data::block_states::{STATE_COUNT, block_name, properties};
    let (name, raw) = match state.split_once('[') {
        Some((n, rest)) => (n, rest.strip_suffix(']').unwrap_or(rest)),
        None => (state, ""),
    };
    let mut wanted: Vec<(&str, &str)> = if raw.is_empty() {
        Vec::new()
    } else {
        raw.split(',').filter_map(|p| p.split_once('=')).collect()
    };
    wanted.sort_unstable();
    let mut fallback = None;
    for id in 0..STATE_COUNT {
        if block_name(id) != Some(name) {
            continue;
        }
        if fallback.is_none() {
            fallback = Some(id);
        }
        let mut have: Vec<(&str, &str)> = properties(id).unwrap_or(&[]).to_vec();
        have.sort_unstable();
        if have == wanted {
            return id;
        }
    }
    fallback.expect("the block must exist in the 26.2 table")
}

async fn join<T: Transport>(client: &mut Connection<T>) {
    client.write_packet(0, &handshake_bytes()).await.unwrap();
    client
        .write_packet(0, &hello_bytes(&unique_username(), Uuid::new_v4()))
        .await
        .unwrap();
    let _ = client.read_packet().await;
    client
        .write_packet(login::serverbound::LOGIN_ACKNOWLEDGED, &[])
        .await
        .unwrap();
    let _ = client.read_packet().await;
    client
        .write_packet(configuration::serverbound::FINISH_CONFIGURATION, &[])
        .await
        .unwrap();
    drain(client).await;
}

fn serve() -> Connection<tokio::io::DuplexStream> {
    let (server, client_io) =
        IntegratedServer::open_in_memory(V770ServerProtocol, FixtureSource, 1);
    // The handle owns the connection task; leak it for the test's lifetime.
    std::mem::forget(server);
    Connection::new(client_io)
}

/// Right-clicking a closed door opens **both halves**, and clicking again closes
/// them.
///
/// Asserted by state id per position, not by "some block_update arrived" — the
/// placement branch produces its own updates for the clicked and neighbour cells,
/// so a count-only assertion would pass on those and prove nothing.
#[tokio::test]
async fn right_clicking_a_door_opens_and_closes_both_halves() {
    let mut client = serve();
    join(&mut client).await;

    let lower_y = FLOOR_TOP_Y + 1;
    let upper_y = FLOOR_TOP_Y + 2;
    let open_lower =
        state_id("minecraft:oak_door[facing=north,half=lower,hinge=left,open=true,powered=false]")
            as i32;
    let open_upper =
        state_id("minecraft:oak_door[facing=north,half=upper,hinge=left,open=true,powered=false]")
            as i32;
    let shut_lower =
        state_id("minecraft:oak_door[facing=north,half=lower,hinge=left,open=false,powered=false]")
            as i32;

    client
        .write_packet(
            play::serverbound::USE_ITEM_ON,
            &use_item_on_bytes(DOOR_X, lower_y, DOOR_Z, 1),
        )
        .await
        .unwrap();
    let opened = block_updates(&drain(&mut client).await);

    assert!(
        opened.contains(&(DOOR_X, lower_y, DOOR_Z, open_lower)),
        "the lower half must be reported open; got {opened:?}"
    );
    assert!(
        opened.contains(&(DOOR_X, upper_y, DOOR_Z, open_upper)),
        "and the upper half too — a door is two blocks and vanilla moves both; got \
         {opened:?}"
    );

    client
        .write_packet(
            play::serverbound::USE_ITEM_ON,
            &use_item_on_bytes(DOOR_X, lower_y, DOOR_Z, 2),
        )
        .await
        .unwrap();
    let closed = block_updates(&drain(&mut client).await);
    assert!(
        closed.contains(&(DOOR_X, lower_y, DOOR_Z, shut_lower)),
        "a second click must close it again; got {closed:?}"
    );
}

/// Flipping a lever by hand reaches `powered=true` on the wire.
///
/// The point is not the lever's own state but that a player can now *drive*
/// redstone at all: `redstone.rs` would propagate the signal, but before this
/// was wired up nothing could set it.
#[tokio::test]
async fn flipping_a_lever_by_hand_powers_it() {
    let mut client = serve();
    join(&mut client).await;

    let y = FLOOR_TOP_Y + 1;
    let on = state_id("minecraft:lever[face=floor,facing=north,powered=true]") as i32;

    client
        .write_packet(
            play::serverbound::USE_ITEM_ON,
            &use_item_on_bytes(LEVER_X, y, LEVER_Z, 1),
        )
        .await
        .unwrap();
    let updates = block_updates(&drain(&mut client).await);
    assert!(
        updates.contains(&(LEVER_X, y, LEVER_Z, on)),
        "the lever must report powered=true; got {updates:?}"
    );
}
