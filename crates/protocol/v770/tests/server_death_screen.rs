//! **The server half of the death screen** — the real [`V770ServerProtocol`],
//! served over the in-memory transport, killed by a real fall.
//!
//! # What was broken
//!
//! The client's death screen was complete and reached **zero pixels**.
//! `Screen::Death`, `death_frame`, the Respawn button, the `Dead` marker
//! component and the `client_command(perform_respawn)` encoder all existed and
//! were all unit-tested; `docs/death-screen.md` describes the whole thing. What
//! did not exist was any server that sent the packet that raises it. Our server
//! sent `set_health(0.0)` and stopped there, and **`set_health` does not open the
//! death screen** — not here, and not in vanilla, whose
//! `ClientPacketListener.handleSetHealth`
//! (`.cache/mc/26.2/client-src/net/minecraft/client/multiplayer/ClientPacketListener.java`)
//! calls only `hurtTo`/`setFoodLevel`/`setSaturation`. The screen comes from
//! `handlePlayerCombatKill` at `:1845-1855`.
//!
//! So the observable symptom was a player pinned at zero hearts with no screen,
//! no respawn button and no way out — which is how the owner reported it: the
//! server appearing to hang.
//!
//! # Where the expected values come from
//!
//! Three independent outside sources, none of them our encoder:
//!
//! * **The packet ids** are Mojang's own generated `packets.json` for 26.2, via
//!   `crate::packet_ids` — `player_combat_kill` is `68` and `respawn` is `82`.
//! * **The wire layout** is `ClientboundPlayerCombatKillPacket`'s own record
//!   (`record (int playerId, Component message)`, a VarInt id then
//!   `ComponentSerialization.TRUSTED_STREAM_CODEC`), hand-built into
//!   [`golden_combat_kill_body`] rather than obtained from the encoder under
//!   test.
//! * **The semantics** come from [`V770Adapter`], which is an *independently
//!   authored* decoder that predates this encoder and was validated against a
//!   real vanilla 26.2 server by `live_respawn.rs` (which kills a player via
//!   RCON and reads the real server's own `player_combat_kill`). Feeding our
//!   bytes through it is therefore not `decode(encode(x)) == x` between two
//!   halves of one belief — it is a check against a decoder a real server has
//!   already satisfied.
//!
//! # Controls, and what each one rules out
//!
//! * [`a_survivable_fall_sends_health_but_never_the_death_packet`] is the
//!   negative control on the *same* mechanism: a 4-block fall really does deal
//!   damage and really does send `set_health`, and must send **no** id-68 packet.
//!   Without it, `a_lethal_fall_sends_the_death_packet` would pass against a
//!   server that sent the death packet on every hit, or on every movement
//!   packet.
//! * [`death_is_announced_exactly_once_per_life`] rules out the opposite
//!   failure: a latch-free implementation that re-announced on every subsequent
//!   sample would leave a real client rebuilding its death screen forever.
//! * Both assert an exact predicted **health**, not merely "less than 20", per
//!   `CLAUDE.md`'s magnitude species — the fall formula is
//!   `floor(distance + 1e-6 - 3.0)` damage points off 20.

use std::time::Duration;

use lodestone_core::{Nbt, Reader, Writer, read_network_nbt, write_network_nbt};
use lodestone_model::{
    ClientEvent, ConnectionState, Directive, Text, TextContent, VersionAdapter,
};
use lodestone_net::{Connection, Transport, memory_pair};
use lodestone_server::{
    BlockEntityHandle, ChunkColumn, ChunkSource, MobHandle, NoEntities, serve_connection,
};
use lodestone_v770::packet_ids::{configuration, login, play};
use lodestone_v770::{V770Adapter, V770ServerProtocol};
use lodestone_world::World;
use uuid::Uuid;

mod common;
use common::unique_username;

/// Full health, from `V770ServerProtocol::begin_play_at`'s fresh-spawn
/// `SetHealth` and vanilla's `Attributes.MAX_HEALTH` default.
const MAX_HEALTH: f32 = 20.0;

/// Vanilla's `Attributes.SAFE_FALL_DISTANCE` default. Written here rather than
/// imported so the predicted health below is derived from the vanilla constant
/// and not from `lodestone_server::fall`'s copy of it.
const SAFE_FALL_DISTANCE: f64 = 3.0;

/// The exact damage a fall of `blocks` deals, from
/// `LivingEntity.calculateFallDamage`/`calculateFallPower`
/// (`LivingEntity.java`): `floor((d + 1e-6 - safe) * 1.0 * 1.0)`.
fn fall_damage(blocks: f64) -> f32 {
    let raw = (blocks + 1.0e-6 - SAFE_FALL_DISTANCE).floor();
    if raw > 0.0 { raw as f32 } else { 0.0 }
}

/// An all-air world. Deliberate: the subject is the *damage and death* path,
/// which is driven purely by the `on_ground` flag the client reports, and an
/// air world means no terrain interaction can accidentally supply the landing.
struct AirSource;

impl ChunkSource for AirSource {
    fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
        ChunkColumn::new(-64, 384)
    }

    fn block_state(&self, x: i32, y: i32, z: i32) -> String {
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        self.column(cx, cz)
            .block_state(x.rem_euclid(16), y, z.rem_euclid(16))
            .to_string()
    }

    fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {
        // No storage; this fixture never edits terrain.
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

/// Hand-written serverbound `move_player_pos`: `f64`×3 then the flags byte,
/// per `ServerboundMovePlayerPacket.Pos`. `on_ground` is bit `0x01`.
fn pos_bytes(x: f64, y: f64, z: f64, on_ground: bool) -> Vec<u8> {
    let mut w = Writer::default();
    w.f64(x);
    w.f64(y);
    w.f64(z);
    w.u8(u8::from(on_ground));
    w.into_vec()
}

/// Hand-written serverbound `client_command`: one VarInt ordinal, `0` being
/// `PERFORM_RESPAWN`.
fn client_command_bytes(action: i32) -> Vec<u8> {
    let mut w = Writer::default();
    w.var_i32(action);
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

/// Joins and returns the local player's entity id **as the server itself
/// announced it** in the `login` packet.
///
/// Read off the wire rather than written as a literal: the property that matters
/// is that the death packet names the same entity the client latched onto at
/// join (`V770ServerProtocol`'s private `LOCAL_PLAYER_ENTITY_ID`, which a test
/// cannot import and should not duplicate). A literal here would still pass if
/// join and death drifted apart.
async fn join<T: Transport>(client: &mut Connection<T>, name: &str, uuid: Uuid) -> i32 {
    client.write_packet(0, &handshake_bytes()).await.unwrap();
    client.write_packet(0, &hello_bytes(name, uuid)).await.unwrap();
    let _ = common::read_login_packet(client).await;
    client
        .write_packet(login::serverbound::LOGIN_ACKNOWLEDGED, &[])
        .await
        .unwrap();
    let _ = common::read_login_packet(client).await;
    client
        .write_packet(configuration::serverbound::FINISH_CONFIGURATION, &[])
        .await
        .unwrap();
    let joined = drain(client).await;
    // `ClientboundLoginPacket`'s first field is a raw big-endian `i32` entity id.
    let login_body = joined
        .iter()
        .find(|(id, _)| *id == play::clientbound::LOGIN)
        .map(|(_, p)| p.clone())
        .expect("the join must include a login packet");
    Reader::new(&login_body).i32().expect("login entity id")
}

/// Sends one movement sample and returns everything the server said back.
async fn step<T: Transport>(
    client: &mut Connection<T>,
    y: f64,
    on_ground: bool,
) -> Vec<(i32, Vec<u8>)> {
    client
        .write_packet(
            play::serverbound::MOVE_PLAYER_POS,
            &pos_bytes(0.0, y, 0.0, on_ground),
        )
        .await
        .unwrap();
    drain(client).await
}

/// The `health` field of every `set_health` in `packets`, in order.
///
/// Layout per `ClientboundSetHealthPacket`: `f32` health, VarInt food, `f32`
/// saturation.
fn healths(packets: &[(i32, Vec<u8>)]) -> Vec<f32> {
    packets
        .iter()
        .filter(|(id, _)| *id == play::clientbound::SET_HEALTH)
        .map(|(_, payload)| Reader::new(payload).f32().expect("set_health health"))
        .collect()
}

fn combat_kills(packets: &[(i32, Vec<u8>)]) -> Vec<Vec<u8>> {
    packets
        .iter()
        .filter(|(id, _)| *id == play::clientbound::PLAYER_COMBAT_KILL)
        .map(|(_, payload)| payload.clone())
        .collect()
}

fn respawns(packets: &[(i32, Vec<u8>)]) -> usize {
    packets
        .iter()
        .filter(|(id, _)| *id == play::clientbound::RESPAWN)
        .count()
}

/// A hand-built `player_combat_kill` body: a VarInt player id, then a
/// network-form NBT chat component (root tag id, no root name).
///
/// Written from `ClientboundPlayerCombatKillPacket`'s record definition, not
/// obtained from the encoder under test — see this file's module docs.
fn golden_combat_kill_body(player_id: i32, key: &str, victim: &str) -> Vec<u8> {
    let component = Nbt::Compound(vec![
        ("translate".to_owned(), Nbt::String(key.to_owned())),
        (
            "with".to_owned(),
            Nbt::List {
                element_type: lodestone_core::NbtTag::Compound,
                elements: vec![Nbt::Compound(vec![(
                    "text".to_owned(),
                    Nbt::String(victim.to_owned()),
                )])],
            },
        ),
    ]);
    let mut w = Writer::default();
    w.var_i32(player_id);
    write_network_nbt(&mut w, &component).expect("golden component encodes");
    w.into_vec()
}

/// Spawns a served connection over an in-memory pair and returns the client end.
fn serve() -> Connection<tokio::io::DuplexStream> {
    let (client_io, server_io) = memory_pair();
    tokio::spawn(async move {
        let mut conn = Connection::new(server_io);
        let _ = serve_connection(
            &mut conn,
            &V770ServerProtocol,
            &AirSource,
            &NoEntities,
            0,
            &BlockEntityHandle::default(),
            &MobHandle::default(),
        )
        .await;
    });
    Connection::new(client_io)
}

/// A 30-block fall kills a full-health player outright, and the server says so
/// with `player_combat_kill` — the packet that raises the death screen.
///
/// **Predicted, not merely signed**: 30 blocks is
/// `floor(30.000001 - 3.0) = 27` damage points against 20 health, so the
/// health that arrives is exactly `0.0`. A 27-point hit on a 20-point player is
/// the arithmetic that makes this a *lethal* fall rather than a large one.
#[tokio::test]
async fn a_lethal_fall_sends_the_death_packet() {
    let mut client = serve();
    let name = unique_username();
    let player_entity_id = join(&mut client, &name, Uuid::new_v4()).await;

    // Airborne at 100, then landing at 70: a 30-block fall.
    let _ = step(&mut client, 100.0, false).await;
    let landing = step(&mut client, 70.0, true).await;

    let damage = fall_damage(30.0);
    assert_eq!(damage, 27.0, "the fall formula must predict 27, not merely 'a lot'");
    assert!(
        damage > MAX_HEALTH,
        "precondition: {damage} points must exceed {MAX_HEALTH} health, or this fall is \
         survivable and the gate is testing the wrong thing"
    );

    assert_eq!(
        healths(&landing),
        vec![0.0],
        "one set_health, at exactly zero"
    );

    let kills = combat_kills(&landing);
    assert_eq!(
        kills.len(),
        1,
        "exactly one player_combat_kill (id {}) must follow the lethal hit; got {} — \
         with none, the client sits at zero hearts with no death screen",
        play::clientbound::PLAYER_COMBAT_KILL,
        kills.len()
    );

    // Byte-exact against the hand-built golden. The id is asserted non-zero
    // first: a `0` would make the leading VarInt indistinguishable from a
    // default-initialised field, so the comparison would be weaker than it looks.
    assert_ne!(
        player_entity_id, 0,
        "the server's own login entity id must be non-zero for this comparison to \
         distinguish a real id from an uninitialised one"
    );
    assert_eq!(
        kills[0],
        golden_combat_kill_body(player_entity_id, "death.attack.fall", &name),
        "the body must be a VarInt player id then a network-NBT translatable \
         `death.attack.fall` carrying the victim's name"
    );

    // And through the independently-authored decoder that a real vanilla
    // server has already satisfied (`live_respawn.rs`).
    let adapter = V770Adapter::new();
    let directives = adapter
        .handle_packet(
            &mut World::new(),
            ConnectionState::Play,
            play::clientbound::PLAYER_COMBAT_KILL,
            &kills[0],
        )
        .expect("our own bytes must decode through the real client adapter");
    let message = match directives.as_slice() {
        [Directive::Emit(ClientEvent::Death { message })] => message.clone(),
        other => panic!("expected exactly one ClientEvent::Death, got {other:?}"),
    };
    match &message.content {
        TextContent::Translate { key, with, .. } => {
            assert_eq!(key, "death.attack.fall");
            assert_eq!(with.len(), 1);
            assert_eq!(with[0], Text::literal(&name));
        }
        other => panic!("the death message must survive as translatable, got {other:?}"),
    }
}

/// **The negative control.** A 4-block fall genuinely hurts — so the detector is
/// live — and must produce no death packet at all.
///
/// Predicted exactly: `floor(4.000001 - 3.0) = 1` damage point, so health is
/// `19.0`. Asserting the value rather than "less than 20" is what makes this a
/// control on the *damage* path as well as the death path: a server that
/// silently applied no damage would also send no death packet, and this test
/// would pass vacuously if it only checked for the packet's absence.
#[tokio::test]
async fn a_survivable_fall_sends_health_but_never_the_death_packet() {
    let mut client = serve();
    let name = unique_username();
    let _ = join(&mut client, &name, Uuid::new_v4()).await;

    let _ = step(&mut client, 100.0, false).await;
    let landing = step(&mut client, 96.0, true).await;

    let damage = fall_damage(4.0);
    assert_eq!(damage, 1.0, "a 4-block fall is exactly 1 damage point");
    assert_eq!(
        healths(&landing),
        vec![MAX_HEALTH - damage],
        "premise: the fall really did land and really did hurt — without this the \
         absence below proves nothing"
    );
    assert!(
        combat_kills(&landing).is_empty(),
        "a surviving player must get no player_combat_kill"
    );
}

/// A dead player's own respawn request must produce the `respawn` packet, not
/// just refilled health.
///
/// This is the half that fails most confusingly when missing: the client clears
/// its `Dead` marker on `ClientEvent::Respawned`, which its adapter decodes from
/// `respawn` (id 82) alone. A server that answered by resetting vitals and
/// sending `set_health(20.0)` would refill the hearts *behind* a death screen
/// that never closes — so the assertion here is on the respawn packet, with the
/// health as a premise rather than the subject.
#[tokio::test]
async fn a_respawn_request_from_a_dead_player_sends_the_respawn_packet() {
    let mut client = serve();
    let name = unique_username();
    let _ = join(&mut client, &name, Uuid::new_v4()).await;

    let _ = step(&mut client, 100.0, false).await;
    let landing = step(&mut client, 70.0, true).await;
    assert_eq!(
        healths(&landing),
        vec![0.0],
        "precondition: the player must actually be dead before requesting a respawn"
    );

    client
        .write_packet(play::serverbound::CLIENT_COMMAND, &client_command_bytes(0))
        .await
        .unwrap();
    let after = drain(&mut client).await;

    assert_eq!(
        respawns(&after),
        1,
        "exactly one respawn packet (id {}) must answer perform_respawn; without it the \
         client's `Dead` marker is never cleared and the death screen stays up forever",
        play::clientbound::RESPAWN
    );
    assert_eq!(
        healths(&after),
        vec![MAX_HEALTH],
        "and the health that follows it must be full"
    );

    // The respawn must precede the health, or the hearts refill behind a screen
    // that is still up. Ordering, not merely presence.
    let respawn_at = after
        .iter()
        .position(|(id, _)| *id == play::clientbound::RESPAWN)
        .expect("respawn present");
    let health_at = after
        .iter()
        .position(|(id, _)| *id == play::clientbound::SET_HEALTH)
        .expect("set_health present");
    assert!(
        respawn_at < health_at,
        "respawn (index {respawn_at}) must be sent before set_health (index {health_at})"
    );

    // The respawn body must decode through the real client adapter — the same
    // outside check the death packet gets above. `Respawn` carries the dimension
    // window every subsequent chunk is framed against, so a malformed one is
    // worse than a missing one.
    let payload = after
        .iter()
        .find(|(id, _)| *id == play::clientbound::RESPAWN)
        .map(|(_, p)| p.clone())
        .expect("respawn present");
    let adapter = V770Adapter::new();
    let directives = adapter
        .handle_packet(
            &mut World::new(),
            ConnectionState::Play,
            play::clientbound::RESPAWN,
            &payload,
        )
        .expect("our respawn bytes must decode through the real client adapter");
    assert!(
        directives
            .iter()
            .any(|d| matches!(d, Directive::Emit(ClientEvent::Respawned { .. }))),
        "the respawn must reach `ClientEvent::Respawned` — the only thing that clears \
         the client's `Dead` marker; got {directives:?}"
    );
}

/// A **living** player's respawn request must be ignored, mirroring vanilla's
/// `handleClientCommand` guard (`this.player.getHealth() > 0.0F` → return).
///
/// The control for the test above: without it, that one passes against a server
/// that answers `perform_respawn` unconditionally, which would let a client
/// teleport itself back to spawn at will.
#[tokio::test]
async fn a_living_players_respawn_request_is_ignored() {
    let mut client = serve();
    let _ = join(&mut client, &unique_username(), Uuid::new_v4()).await;

    // Establish that the connection is live and answering, so a later empty
    // drain is a decision rather than a dead socket.
    let alive = step(&mut client, 100.0, true).await;
    assert!(
        combat_kills(&alive).is_empty(),
        "premise: a standing player is not dead"
    );

    client
        .write_packet(play::serverbound::CLIENT_COMMAND, &client_command_bytes(0))
        .await
        .unwrap();
    let after = drain(&mut client).await;
    assert_eq!(
        respawns(&after),
        0,
        "a living player's perform_respawn must be a no-op"
    );
}

/// Death is announced **once** per life, then again after a respawn — never
/// repeatedly while the player lies dead.
///
/// The property holds because every `PlayerVitals` damage entry point returns
/// `None` once health is zero, so no further hit *lands* and nothing re-triggers
/// the announcement. That is a property of those guards rather than of the
/// death wiring, which is exactly why it needs its own gate: a future
/// unconditional damage path would break it silently, and a real client would
/// rebuild its death screen on every movement packet.
#[tokio::test]
async fn death_is_announced_exactly_once_per_life() {
    let mut client = serve();
    let name = unique_username();
    let _ = join(&mut client, &name, Uuid::new_v4()).await;

    let _ = step(&mut client, 100.0, false).await;
    let first = step(&mut client, 70.0, true).await;
    assert_eq!(combat_kills(&first).len(), 1, "the killing blow announces once");

    // Keep falling and landing while dead. Each of these would have been a
    // lethal fall on its own.
    let mut later = Vec::new();
    for _ in 0..3 {
        later.extend(step(&mut client, 200.0, false).await);
        later.extend(step(&mut client, 70.0, true).await);
    }
    assert!(
        combat_kills(&later).is_empty(),
        "a dead player must not be re-killed; got {} further announcements",
        combat_kills(&later).len()
    );
    assert!(
        healths(&later).is_empty(),
        "and no further set_health either — the vitals guards make the hit a no-op"
    );

    // After a respawn the announcement re-arms, which is the other half of
    // "exactly once per life".
    client
        .write_packet(play::serverbound::CLIENT_COMMAND, &client_command_bytes(0))
        .await
        .unwrap();
    let _ = drain(&mut client).await;
    let _ = step(&mut client, 200.0, false).await;
    let second_life = step(&mut client, 70.0, true).await;
    assert_eq!(
        combat_kills(&second_life).len(),
        1,
        "a respawned player who dies again must get a second death screen"
    );
}

/// The respawn teleport resets the fall accumulator, so the first landing after
/// a respawn is not charged for the drop the player died from.
///
/// Without `FallTracker::reset` clearing its `last_y` reference (not just the
/// distance), the sample after a respawn is diffed against the y the player
/// died at. Predicted exactly: the player dies falling to `y = 70`, respawns at
/// the world spawn, and then makes a **2-block** fall, which is inside
/// `SAFE_FALL_DISTANCE` and must therefore deal **zero** damage — health stays
/// at `20.0`. A stale reference would instead bank the respawn's own vertical
/// displacement and the first landing would hurt.
#[tokio::test]
async fn a_respawn_does_not_carry_fall_distance_into_the_next_life() {
    let mut client = serve();
    let _ = join(&mut client, &unique_username(), Uuid::new_v4()).await;

    let _ = step(&mut client, 300.0, false).await;
    let landing = step(&mut client, 70.0, true).await;
    assert_eq!(
        healths(&landing),
        vec![0.0],
        "precondition: a 230-block fall must be lethal"
    );

    client
        .write_packet(play::serverbound::CLIENT_COMMAND, &client_command_bytes(0))
        .await
        .unwrap();
    let after = drain(&mut client).await;
    assert_eq!(healths(&after), vec![MAX_HEALTH], "precondition: full health restored");

    // A 2-block fall: inside the safe distance, so zero damage.
    let _ = step(&mut client, 66.0, false).await;
    let small = step(&mut client, 64.0, true).await;
    assert_eq!(
        fall_damage(2.0),
        0.0,
        "2 blocks is inside SAFE_FALL_DISTANCE by arithmetic"
    );
    assert!(
        healths(&small).is_empty(),
        "a 2-block fall after a respawn must deal no damage; a set_health here means \
         the respawn carried stale fall state into the new life, got {:?}",
        healths(&small)
    );
    assert!(combat_kills(&small).is_empty(), "and certainly no second death");
}

/// Guards the one thing [`golden_combat_kill_body`] cannot: that the id it is
/// compared at is the id Mojang assigns.
///
/// `packets.json` for 26.2 gives `minecraft:player_combat_kill` id `68` and
/// `minecraft:respawn` id `82`. Both are read here off `crate::packet_ids`,
/// which is generated from that file — so a regenerated table that moved either
/// one fails loudly instead of making every assertion above compare the wrong
/// packet.
#[test]
fn the_death_and_respawn_packet_ids_are_mojangs() {
    assert_eq!(play::clientbound::PLAYER_COMBAT_KILL, 68);
    assert_eq!(play::clientbound::RESPAWN, 82);
    assert_eq!(play::serverbound::CLIENT_COMMAND, 12);
}

/// The network-NBT the encoder writes is readable as NBT at all — a cheap guard
/// against a length-prefixed-vs-raw mistake in the component append, which would
/// otherwise show up only as a decode failure inside the adapter check above.
#[test]
fn the_combat_kill_component_is_bare_network_nbt() {
    let body = golden_combat_kill_body(0, "death.attack.fall", "Steve");
    let mut r = Reader::new(&body);
    assert_eq!(r.var_i32().expect("player id"), 0);
    let component = read_network_nbt(&mut r).expect("component reads as network NBT");
    let text = Text::from_nbt(&component);
    match &text.content {
        TextContent::Translate { key, .. } => assert_eq!(key, "death.attack.fall"),
        other => panic!("expected a translatable component, got {other:?}"),
    }
}
