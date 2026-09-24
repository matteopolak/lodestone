//! Live item-component acceptance test.
//!
//! Gated behind the `live-item` feature AND `#[ignore]`, so the default
//! `cargo test` stays hermetic. Run it against the real vanilla 26.2 survival
//! server (`lodestone-survival`, game `127.0.0.1:25565`, RCON `:25566`,
//! password `lodestone`) with:
//!
//! ```text
//! cargo test -p lodestone-v26-2 --features live-item --test live \
//!     live_item_components:: -- --ignored --nocapture
//! ```
//!
//! ## Why this test cannot be a self-round-trip
//!
//! `HANDOFF.md` records the rule: when we own both sides of a round-trip test it
//! cannot detect a shared misunderstanding of the wire format. Our own encoder
//! and decoder could agree on a *wrong* `DataComponentPatch` layout and a
//! self-round-trip would pass while the real server disconnects us. So this gate
//! joins the **real** server, has it (not us) serialise items carrying
//! components, and asserts our decoder recovers them. The pre-fix code
//! fail-closed on any component patch, which the driver treated as fatal —
//! receiving any real item ended the session.
//!
//! Set `LODESTONE_CAPTURE_FIXTURES=1` to refresh the complete potion-contents
//! fixture from the packet bytes captured before adapter decoding.

#![cfg(feature = "live-item")]

use std::path::PathBuf;
use std::time::Duration;

use lodestone_core::{Reader, Writer};
use lodestone_model::{
    ClientEvent, ConnectionState, Directive, ItemStack, LoginProfile, ServerAddress,
    VersionAdapter,
};
use lodestone_net::Connection;
use lodestone_testsupport::{unique_username, RconClient};
use lodestone_v26_2::packet_ids::play;
use lodestone_v26_2::V770Adapter;
use lodestone_world::World;
use tokio::net::TcpStream;
use uuid::Uuid;

const SERVER_ADDR: &str = "127.0.0.1:25565";
const RCON_ADDR: &str = "127.0.0.1:25566";
const RCON_PASSWORD: &str = "lodestone";

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

fn to_hex_fixture(header: &str, bytes: &[u8]) -> String {
    let mut out = String::new();
    for line in header.lines() {
        out.push_str("# ");
        out.push_str(line);
        out.push('\n');
    }
    for chunk in bytes.chunks(16) {
        let row: Vec<String> = chunk.iter().map(|byte| format!("{byte:02x}")).collect();
        out.push_str(&row.join(" "));
        out.push('\n');
    }
    out
}

fn stack_bytes(payload: &[u8]) -> &[u8] {
    let mut reader = Reader::new(payload);
    reader.var_i32().expect("window id");
    reader.var_i32().expect("state id");
    reader.i16().expect("slot index");
    &payload[reader.position()..]
}

fn reconcile_fixture(name: &str, header: &str, captured: &[u8]) {
    let path = fixture_path(name);
    if std::env::var("LODESTONE_CAPTURE_FIXTURES").as_deref() == Ok("1") {
        std::fs::write(&path, to_hex_fixture(header, captured)).expect("write captured fixture");
        println!("wrote fixture {}", path.display());
        return;
    }

    let expected = std::fs::read_to_string(&path).unwrap_or_else(|err| {
        panic!(
            "missing fixture {}: {err} — re-capture with LODESTONE_CAPTURE_FIXTURES=1",
            path.display()
        )
    });
    let bytes: Vec<u8> = expected
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .flat_map(str::split_whitespace)
        .map(|token| u8::from_str_radix(token, 16).expect("fixture hex byte"))
        .collect();
    assert_eq!(
        stack_bytes(&bytes),
        stack_bytes(captured),
        "the external server's stack encoding drifted from {name}"
    );
}

/// The command that repairs the missing precondition, printed when the server
/// cannot be reached so the gate fails loudly rather than skipping.
const REPAIR: &str = "recreate the survival oracle with: ./scripts/live-oracles/survival.sh \
    (expected a vanilla 26.2 survival server on 127.0.0.1:25565, RCON :25566)";

async fn apply(conn: &mut Connection<TcpStream>, state: &mut ConnectionState, directive: Directive) {
    match directive {
        Directive::Send { packet_id, payload } => {
            conn.write_packet(packet_id, &payload)
                .await
                .expect("write packet");
        }
        Directive::SetState(next) => *state = next,
        Directive::SetCompression(threshold) => conn.set_compression(threshold),
        Directive::Disconnect(reason) => {
            panic!("server disconnected us: {}", reason.to_plain_string())
        }
        _ => {}
    }
}

/// Acknowledges a finished chunk batch so vanilla keeps streaming.
async fn ack_chunk_batch(conn: &mut Connection<TcpStream>) {
    let mut w = Writer::default();
    w.f32(32.0);
    conn.write_packet(play::serverbound::CHUNK_BATCH_RECEIVED, &w.into_vec())
        .await
        .expect("ack chunk batch");
}

/// Pulls the requested stack out of whichever container packet carries it.
fn item_from_event(event: &ClientEvent, expected_item: &str) -> Option<ItemStack> {
    let matches = |item: &Option<ItemStack>| -> Option<ItemStack> {
        item.as_ref()
            .filter(|s| s.item.to_string() == expected_item)
            .cloned()
    };
    match event {
        ClientEvent::ContainerSlot { item, .. } => matches(item),
        ClientEvent::ContainerContent { items, .. } => items.iter().find_map(matches),
        _ => None,
    }
}

/// A live session held open so a test can prove it survived the item decode.
struct LiveSession {
    conn: Connection<TcpStream>,
    state: ConnectionState,
    world: World,
    adapter: V770Adapter,
}

impl LiveSession {
    /// Joins the server, gives the player `item_spec`, and reads until the
    /// expected item arrives — feeding every real packet through the adapter,
    /// which must never turn a component item into an error.
    async fn join_and_give_item(
        item_spec: &str,
        expected_item: &str,
        require_slot_packet: bool,
    ) -> (Self, ItemStack, Vec<u8>) {
        let server = ServerAddress {
            host: "127.0.0.1".into(),
            port: 25565,
        };
        let username = unique_username();
        let profile = LoginProfile {
            username: username.clone(),
            uuid: Uuid::new_v4(),
        };
        let adapter = V770Adapter::new();

        let mut conn = match Connection::connect(SERVER_ADDR).await {
            Ok(conn) => conn,
            Err(err) => panic!("could not reach {SERVER_ADDR}: {err}. {REPAIR}"),
        };
        let mut state = ConnectionState::Handshaking;
        for directive in adapter.begin_login(&profile, &server).expect("begin login") {
            apply(&mut conn, &mut state, directive).await;
        }

        // Drive the join until the world is streaming, so the player entity
        // exists server-side before we `give`.
        let mut world = World::new();
        let joined = tokio::time::timeout(Duration::from_secs(60), async {
            loop {
                let (packet_id, payload) = match conn.read_packet().await {
                    Ok(Some(p)) => p,
                    Ok(None) => return false,
                    Err(err) => panic!("read error during join: {err}"),
                };
                if state == ConnectionState::Play
                    && packet_id == play::clientbound::CHUNK_BATCH_FINISHED
                {
                    ack_chunk_batch(&mut conn).await;
                    return true;
                }
                if let Ok(directives) =
                    adapter.handle_packet(&mut world, state, packet_id, &payload)
                {
                    for directive in directives {
                        apply(&mut conn, &mut state, directive).await;
                    }
                }
            }
        })
        .await
        .expect("timed out joining the world");
        assert!(joined, "connection closed before reaching Play");

        // The server authors the bytes; `give` lands the item in the player's
        // inventory, which the server pushes to us as a container packet.
        let mut rcon = RconClient::connect(RCON_ADDR, RCON_PASSWORD)
            .unwrap_or_else(|err| panic!("could not reach RCON {RCON_ADDR}: {err}. {REPAIR}"));
        let give = format!("give {username} {item_spec} 1");
        let reply = rcon.cmd(&give);
        println!("rcon give reply: {reply}");
        let lower = reply.to_lowercase();
        assert!(
            !lower.contains("incorrect") && !lower.contains("unknown") && !lower.contains("error"),
            "server rejected the give command, so the SNBT is wrong: {reply}"
        );

        let found = tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let (packet_id, payload) = match conn.read_packet().await {
                    Ok(Some(p)) => p,
                    Ok(None) => return None,
                    Err(err) => panic!("read error after give: {err}"),
                };
                if state == ConnectionState::Play
                    && packet_id == play::clientbound::CHUNK_BATCH_FINISHED
                {
                    ack_chunk_batch(&mut conn).await;
                    continue;
                }
                let directives = adapter
                    .handle_packet(&mut world, state, packet_id, &payload)
                    .unwrap_or_else(|err| {
                        panic!(
                            "adapter failed to decode a real server packet (id {packet_id}): \
                             {err} — a component-carrying item must not error"
                        )
                    });
                for directive in &directives {
                    if let Directive::Emit(event) = directive {
                        if let Some(stack) = item_from_event(event, expected_item)
                            && (!require_slot_packet
                                || matches!(event, ClientEvent::ContainerSlot { .. }))
                        {
                            return Some((stack, payload));
                        }
                    }
                }
                for directive in directives {
                    apply(&mut conn, &mut state, directive).await;
                }
            }
        })
        .await
        .expect("timed out waiting for the requested item")
        .expect("connection closed before the requested item arrived");

        (
            Self {
                conn,
                state,
                world,
                adapter,
            },
            found.0,
            found.1,
        )
    }

    /// Reads one more real packet and decodes it, proving the session is alive.
    async fn assert_still_alive(mut self) {
        let alive = tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                match self.conn.read_packet().await {
                    Ok(Some((packet_id, payload))) => {
                        if self.state == ConnectionState::Play
                            && packet_id == play::clientbound::CHUNK_BATCH_FINISHED
                        {
                            ack_chunk_batch(&mut self.conn).await;
                        }
                        let _ = self.adapter.handle_packet(
                            &mut self.world,
                            self.state,
                            packet_id,
                            &payload,
                        );
                        return true;
                    }
                    Ok(None) => return false,
                    Err(err) => panic!("read error proving liveness: {err}"),
                }
            }
        })
        .await
        .expect("timed out proving the session survived");
        assert!(
            alive,
            "session must survive decoding a component-carrying item"
        );
    }
}

/// The positive gate: a diamond pickaxe carrying only components we model
/// (custom name, damage, enchantment) decodes fully, and the session lives.
#[tokio::test]
#[ignore = "requires the live 26.2 survival oracle (scripts/live-oracles/survival.sh)"]
async fn modeled_components_decode_and_keep_session() {
    let (session, stack, _) = LiveSession::join_and_give_item(
        "minecraft:diamond_pickaxe[minecraft:custom_name='Lodey',minecraft:damage=250,\
         minecraft:enchantments={\"minecraft:efficiency\":4}]",
        "minecraft:diamond_pickaxe",
        false,
    )
    .await;
    println!("decoded pickaxe: {stack:?}");

    let name = stack
        .components
        .custom_name
        .as_ref()
        .expect("pickaxe must carry a custom name")
        .to_plain_string();
    assert_eq!(name, "Lodey", "custom name must decode intact");
    assert_eq!(stack.components.damage, Some(250), "damage must decode intact");
    assert_eq!(
        stack.components.enchantments.len(),
        1,
        "exactly one enchantment expected, got {:?}",
        stack.components.enchantments
    );
    assert_eq!(
        stack.components.enchantments[0].level, 4,
        "enchantment level must decode intact"
    );
    assert_eq!(stack.count, 1, "stack count must decode intact");
    assert!(
        !stack.components.has_unmodeled,
        "an all-modeled item must not be flagged as partial"
    );

    session.assert_still_alive().await;
}

/// The tolerance gate: a pickaxe that also carries an **unmodeled** component
/// (`repair_cost`, registry id 19 — higher than every modeled id, so it sorts
/// last on the wire) must still decode the modeled components that precede it,
/// flag the stack as partial, and keep the session alive. This is the exact
/// forward-compatibility property the fail-closed policy destroyed: an
/// unrecognised component is a degraded item, never an outage.
#[tokio::test]
#[ignore = "requires the live 26.2 survival oracle (scripts/live-oracles/survival.sh)"]
async fn unmodeled_component_is_tolerated_and_keeps_session() {
    let (session, stack, _) = LiveSession::join_and_give_item(
        "minecraft:diamond_pickaxe[minecraft:custom_name='Lodey',minecraft:damage=250,\
         minecraft:enchantments={\"minecraft:efficiency\":4},minecraft:repair_cost=7]",
        "minecraft:diamond_pickaxe",
        false,
    )
    .await;
    println!("decoded partial pickaxe: {stack:?}");

    // Everything that sorts before the unmodeled component still decodes.
    assert_eq!(
        stack
            .components
            .custom_name
            .as_ref()
            .map(|t| t.to_plain_string()),
        Some("Lodey".to_owned()),
        "modeled components before the unmodeled one must still decode"
    );
    assert_eq!(stack.components.damage, Some(250));
    assert_eq!(stack.components.enchantments.len(), 1);
    // The unmodeled `repair_cost` halted the patch — the stack is flagged.
    assert!(
        stack.components.has_unmodeled,
        "an unmodeled component must flag the stack as partial"
    );

    session.assert_still_alive().await;
}

/// Captures the complete `potion_contents` shape from the external 26.2
/// dedicated server. The hermetic sibling replays the resulting packet through
/// the public adapter, so this gate is also the independent source for its wire
/// expectations.
#[tokio::test]
#[ignore = "requires the live 26.2 survival oracle (scripts/live-oracles/survival.sh)"]
async fn potion_contents_capture_covers_holder_effect_and_name() {
    let item_spec = "minecraft:potion[minecraft:potion_contents={\
        potion:\"minecraft:swiftness\",custom_color:1193046,custom_effects:[{\
        id:\"minecraft:poison\",amplifier:1,duration:45,ambient:false,\
        show_particles:true,show_icon:true}],custom_name:\"night_vision\"}]";
    let (session, stack, payload) =
        LiveSession::join_and_give_item(item_spec, "minecraft:potion", true).await;

    reconcile_fixture(
        "potion_contents_complete.hex",
        "Captured from the external 26.2 dedicated-server survival oracle (protocol 776).\n\
         Packet: clientbound container_set_slot, captured before adapter decoding.\n\
         Source command: give <session player> minecraft:potion[minecraft:potion_contents={\n\
           potion:\"minecraft:swiftness\",custom_color:1193046,custom_effects:[{\n\
           id:\"minecraft:poison\",amplifier:1,duration:45,ambient:false,\n\
           show_particles:true,show_icon:true}],custom_name:\"night_vision\"}] 1\n\
         Re-capture: start scripts/live-oracles/survival.sh, then run\n\
         LODESTONE_CAPTURE_FIXTURES=1 cargo test -p lodestone-v26-2\n\
         --features live-item --test live\n\
         live_item_components::potion_contents_capture_covers_holder_effect_and_name\n\
         -- --ignored --nocapture\n\
         The leading window/state/slot fields are session-scoped; drift comparison\n\
         starts at the stack count. The payload covers a base holder, fixed color,\n\
         one custom effect with explicit flags, and a component-local name.",
        &payload,
    );

    assert_eq!(stack.components.potion, Some(13));
    assert_eq!(stack.components.potion_color, Some(0xFF12_3456));
    assert_eq!(stack.components.potion_custom_effects.len(), 1);
    assert_eq!(stack.components.potion_custom_effects[0].amplifier, 1);
    assert_eq!(stack.components.potion_custom_effects[0].duration_ticks, 45);
    assert_eq!(
        stack.components.potion_custom_name.as_deref(),
        Some("night_vision")
    );
    assert!(!stack.components.has_unmodeled);

    session.assert_still_alive().await;
}
