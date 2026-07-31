//! Live capture + acceptance gate for `minecraft:tool` on the wire.
//!
//! Gated behind the `live-tool` feature AND `#[ignore]`, so the default
//! `cargo test` stays hermetic. Run it against the real vanilla 26.2 survival
//! server (`lodestone-survival`, game `127.0.0.1:25565`, RCON `:25566`,
//! password `lodestone`) with:
//!
//! ```text
//! cargo test -p lodestone-v770 --features live-tool \
//!     --test live_tool_component -- --ignored --nocapture
//! ```
//!
//! # What this gate establishes
//!
//! Two things, and the *first* is the one that changed the design:
//!
//! 1. **A plain vanilla pickaxe sends an empty component patch.**
//!    [`a_plain_pickaxe_sends_no_tool_component`] asks the real server to put a
//!    stock `minecraft:diamond_pickaxe` in the player's main hand and captures
//!    the `container_set_slot` it authors. The patch is `00 00` — nothing added,
//!    nothing removed. A clientbound stack is a *delta* from the item's
//!    prototype component map, and 26.2 registers a pickaxe's `minecraft:tool`
//!    in that prototype, so decoding the wire is by itself **not enough** to
//!    make a pickaxe dig faster. That is why [`lodestone_data::tool`] also
//!    carries a per-item prototype census.
//! 2. **An explicit `minecraft:tool` decodes rule-for-rule.**
//!    [`an_explicit_tool_component_round_trips_from_the_server`] gives the
//!    player a pickaxe with a hand-written tool component and asserts every
//!    field comes back, from bytes the server — not our encoder — produced.
//!
//! Plus [`an_unmodeled_component_alongside_a_tool_still_fails_open`], which
//! keeps the fail-open contract honest now that the patch decoder has one more
//! branch to fall through.
//!
//! # Why the bytes are checked in
//!
//! `HANDOFF.md` records the rule: an expected value must originate outside the
//! code under test. Validating a decoder against bytes our own encoder produced
//! closes perfectly over a shared misunderstanding. So the captured payloads are
//! written to `tests/fixtures/` and replayed with no server attached. This gate
//! re-captures and asserts the fixtures still match reality.
//!
//! Set `LODESTONE_CAPTURE_FIXTURES=1` to rewrite the fixtures from this run.
//!
//! Per §12.52 this fails rather than skips when it cannot run.

#![cfg(feature = "live-tool")]

use std::path::PathBuf;
use std::time::Duration;

use lodestone_core::{Reader, Writer};
use lodestone_model::{
    ClientEvent, ConnectionState, Directive, ItemStack, LoginProfile, ServerAddress, ToolBlocks,
    ToolPatch, VersionAdapter,
};
use lodestone_net::Connection;
use lodestone_testsupport::{RconClient, unique_username};
use lodestone_v770::V770Adapter;
use lodestone_v770::packet_ids::play;
use lodestone_world::World;
use tokio::net::TcpStream;
use uuid::Uuid;

const SERVER_ADDR: &str = "127.0.0.1:25565";
const RCON_ADDR: &str = "127.0.0.1:25566";
const RCON_PASSWORD: &str = "lodestone";

/// The command that repairs the missing precondition, printed when the server
/// cannot be reached so the gate fails loudly rather than skipping.
const REPAIR: &str = "recreate the survival oracle with: ./scripts/live-oracles/survival.sh \
    (expected a vanilla 26.2 survival server on 127.0.0.1:25565, RCON :25566)";

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

/// Renders a captured payload as the reviewable hex text the fixtures use:
/// a provenance header, then 16 bytes per line.
fn to_hex_fixture(header: &str, bytes: &[u8]) -> String {
    let mut out = String::new();
    for line in header.lines() {
        if line.is_empty() {
            out.push_str("#\n");
        } else {
            out.push_str("# ");
            out.push_str(line);
            out.push('\n');
        }
    }
    for chunk in bytes.chunks(16) {
        let row: Vec<String> = chunk.iter().map(|b| format!("{b:02x}")).collect();
        out.push_str(&row.join(" "));
        out.push('\n');
    }
    out
}

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

struct LiveSession {
    conn: Connection<TcpStream>,
    state: ConnectionState,
    world: World,
    adapter: V770Adapter,
    rcon: RconClient,
    username: String,
}

impl LiveSession {
    /// Joins the server and drives the handshake until the world is streaming,
    /// so the player entity exists server-side before anything is equipped.
    async fn join() -> Self {
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
                if let Ok(directives) = adapter.handle_packet(&mut world, state, packet_id, &payload)
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

        let rcon = RconClient::connect(RCON_ADDR, RCON_PASSWORD)
            .unwrap_or_else(|err| panic!("could not reach RCON {RCON_ADDR}: {err}. {REPAIR}"));

        Self {
            conn,
            state,
            world,
            adapter,
            rcon,
            username,
        }
    }

    /// Puts `item_spec` (an item id with optional `[…]` components) in the
    /// joined player's main hand and reads real packets until the matching
    /// `container_set_slot` arrives, returning the raw payload exactly as the
    /// server wrote it.
    ///
    /// The payload is stashed verbatim **before** the adapter sees it, and the
    /// correlation is done on the packet stream rather than on our own decode,
    /// so the captured bytes stay independent of the code under test.
    async fn capture_mainhand(&mut self, item_spec: &str) -> Vec<u8> {
        let command = format!(
            "execute at {} run item replace entity {} weapon.mainhand with {item_spec}",
            self.username, self.username
        );
        let reply = self.rcon.cmd(&command);
        println!("rcon item replace reply: {reply}");
        let lower = reply.to_lowercase();
        assert!(
            !lower.contains("incorrect")
                && !lower.contains("unknown")
                && !lower.contains("expected")
                && !lower.contains("error"),
            "server rejected the item replace, so the spec is wrong: {reply}"
        );

        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let (packet_id, payload) = match self.conn.read_packet().await {
                    Ok(Some(p)) => p,
                    Ok(None) => panic!("connection closed before the slot update arrived"),
                    Err(err) => panic!("read error after item replace: {err}"),
                };
                if self.state == ConnectionState::Play
                    && packet_id == play::clientbound::CHUNK_BATCH_FINISHED
                {
                    ack_chunk_batch(&mut self.conn).await;
                    continue;
                }
                let captured = (self.state == ConnectionState::Play
                    && packet_id == play::clientbound::CONTAINER_SET_SLOT)
                    .then(|| payload.clone());

                let directives = self
                    .adapter
                    .handle_packet(&mut self.world, self.state, packet_id, &payload)
                    .unwrap_or_else(|err| {
                        panic!(
                            "adapter failed to decode a real server packet (id {packet_id}): \
                             {err} — a tool-carrying item must never be fatal"
                        )
                    });
                let is_ours = directives.iter().any(|directive| {
                    matches!(
                        directive,
                        Directive::Emit(ClientEvent::ContainerSlot { item: Some(stack), .. })
                            if stack.item.to_string() == "minecraft:diamond_pickaxe"
                    )
                });
                for directive in directives {
                    apply(&mut self.conn, &mut self.state, directive).await;
                }
                if let Some(payload) = captured
                    && is_ours
                {
                    return payload;
                }
            }
        })
        .await
        .expect("timed out waiting for the main-hand slot update")
    }

    /// Reads one more real packet, proving the session survived the decode.
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
        assert!(alive, "session must survive decoding a tool-carrying item");
    }
}

/// Compares the freshly captured payload against the checked-in fixture,
/// rewriting it when `LODESTONE_CAPTURE_FIXTURES=1`.
///
/// The leading window id / state id are session-scoped (the state id counts
/// container revisions), so only the stack itself — from the count VarInt
/// onwards — is compared byte-for-byte.
fn reconcile_fixture(name: &str, header: &str, captured: &[u8]) {
    let path = fixture_path(name);
    if std::env::var("LODESTONE_CAPTURE_FIXTURES").as_deref() == Ok("1") {
        std::fs::create_dir_all(path.parent().expect("fixture dir")).expect("create fixture dir");
        std::fs::write(&path, to_hex_fixture(header, captured)).expect("write fixture");
        println!("wrote fixture {}", path.display());
        return;
    }
    let text = std::fs::read_to_string(&path).unwrap_or_else(|err| {
        panic!(
            "missing fixture {}: {err} — re-capture with LODESTONE_CAPTURE_FIXTURES=1",
            path.display()
        )
    });
    let expected = stack_bytes(&from_hex_fixture(&text));
    assert_eq!(
        expected,
        stack_bytes(captured),
        "the server's stack encoding no longer matches the checked-in fixture {name}; \
         the wire changed — re-capture with LODESTONE_CAPTURE_FIXTURES=1 and re-review",
    );
}

/// Parses the hex-text fixture format back into bytes.
fn from_hex_fixture(text: &str) -> Vec<u8> {
    text.lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .flat_map(str::split_whitespace)
        .map(|tok| u8::from_str_radix(tok, 16).expect("fixture hex byte"))
        .collect()
}

/// Strips a `container_set_slot`'s session-scoped header (window id, state id,
/// slot index), leaving the stack encoding.
fn stack_bytes(payload: &[u8]) -> Vec<u8> {
    let mut reader = Reader::new(payload);
    reader.var_i32().expect("window id");
    reader.var_i32().expect("state id");
    reader.i16().expect("slot index");
    payload[reader.position()..].to_vec()
}

/// Runs a captured payload back through the public adapter seam and returns the
/// stack it yielded.
fn replay(payload: &[u8]) -> ItemStack {
    let adapter = V770Adapter::new();
    let mut world = World::new();
    let directives = adapter
        .handle_packet(
            &mut world,
            ConnectionState::Play,
            play::clientbound::CONTAINER_SET_SLOT,
            payload,
        )
        .expect("replaying a real container_set_slot must not error");
    directives
        .into_iter()
        .find_map(|d| match d {
            Directive::Emit(ClientEvent::ContainerSlot { item, .. }) => item,
            _ => None,
        })
        .expect("the slot must carry an item")
}

/// **The load-bearing capture.** A stock diamond pickaxe, straight from the
/// server, carries an *empty* component patch — no `minecraft:tool` at all.
///
/// This is the fact that decides the whole design: if the wire carried the tool
/// component, decoding it would be sufficient. It does not, so the client must
/// know the item's prototype component itself. The assertion below is therefore
/// an assertion of an **absence**, and its control is the sibling test
/// [`an_explicit_tool_component_round_trips_from_the_server`] — same code path,
/// same decoder, same assertion shape, but with a tool actually present. If the
/// detector were broken, that test would report `Inherited` too and fail.
#[tokio::test]
#[ignore = "requires the live 26.2 survival oracle (scripts/live-oracles/survival.sh)"]
async fn a_plain_pickaxe_sends_no_tool_component() {
    let mut session = LiveSession::join().await;
    let payload = session.capture_mainhand("minecraft:diamond_pickaxe").await;
    println!("captured plain pickaxe container_set_slot: {payload:02x?}");

    reconcile_fixture(
        "tool_component_absent_plain_pickaxe.hex",
        "Captured from vanilla 26.2 (protocol 776) survival oracle.\n\
         Packet: clientbound container_set_slot, from\n\
         `item replace entity <player> weapon.mainhand with minecraft:diamond_pickaxe`.\n\
         \n\
         ..        VarInt window id  (session-scoped; not compared)\n\
         ..        VarInt state id   (container revision; not compared)\n\
         ..        i16    slot index (not compared)\n\
         01        VarInt stack count = 1\n\
         c6 07     VarInt item registry id 966 = minecraft:diamond_pickaxe\n\
         00        DataComponentPatch: 0 added\n\
         00        DataComponentPatch: 0 removed\n\
         \n\
         THE POINT OF THIS FIXTURE: those last two bytes are the whole patch.\n\
         A real diamond pickaxe sends NO minecraft:tool. The component lives in\n\
         the item's prototype component map (ToolMaterial.applyToolProperties),\n\
         and a clientbound stack transmits only the delta from that prototype,\n\
         so the client is expected to already know it. Decoding the wire alone\n\
         can never make a pickaxe dig faster; the per-item census in\n\
         src/generated/tools.rs is what does.",
        &payload,
    );

    let stack = replay(&payload);
    assert_eq!(stack.item.to_string(), "minecraft:diamond_pickaxe");
    assert_eq!(stack.count, 1);
    assert!(
        !stack.components.has_unmodeled,
        "a stock pickaxe carries no components at all, so nothing may be flagged"
    );
    assert_eq!(
        stack.components.tool,
        ToolPatch::Inherited,
        "a stock pickaxe sends no minecraft:tool — this is the finding the \
         prototype census exists to answer"
    );

    session.assert_still_alive().await;
}

/// The control for the absence above, and the wire-decode gate proper: when the
/// server *is* asked to author a `minecraft:tool`, every field survives the
/// round trip from its bytes.
#[tokio::test]
#[ignore = "requires the live 26.2 survival oracle (scripts/live-oracles/survival.sh)"]
async fn an_explicit_tool_component_round_trips_from_the_server() {
    let mut session = LiveSession::join().await;
    let payload = session
        .capture_mainhand(
            "minecraft:diamond_pickaxe[minecraft:tool=\
             {rules:[\
             {blocks:\"#minecraft:incorrect_for_diamond_tool\",correct_for_drops:false},\
             {blocks:[\"minecraft:stone\",\"minecraft:obsidian\"],speed:12.5f}\
             ],default_mining_speed:1.5f,damage_per_block:3}]",
        )
        .await;
    println!("captured tool-carrying container_set_slot: {payload:02x?}");

    reconcile_fixture(
        "tool_component_explicit.hex",
        "Captured from vanilla 26.2 (protocol 776) survival oracle.\n\
         Packet: clientbound container_set_slot, from `item replace … with`\n\
         minecraft:diamond_pickaxe[minecraft:tool={rules:[\n\
           {blocks:\"#minecraft:incorrect_for_diamond_tool\",correct_for_drops:false},\n\
           {blocks:[\"minecraft:stone\",\"minecraft:obsidian\"],speed:12.5f}\n\
         ],default_mining_speed:1.5f,damage_per_block:3}]\n\
         \n\
         ..        VarInt window id  (session-scoped; not compared)\n\
         ..        VarInt state id   (container revision; not compared)\n\
         ..        i16    slot index (not compared)\n\
         01        VarInt stack count = 1\n\
         c6 07     VarInt item registry id 966 = minecraft:diamond_pickaxe\n\
         01        DataComponentPatch: 1 added\n\
         00        DataComponentPatch: 0 removed\n\
         1c        component type id 28 = minecraft:tool\n\
         \n\
         then Tool.STREAM_CODEC:\n\
           02                    VarInt rule count = 2\n\
           rule 1:\n\
             00                  HolderSet discriminator 0 = named tag follows\n\
             .. <utf8>           Identifier \"minecraft:incorrect_for_diamond_tool\"\n\
             00                  optional(f32) speed: absent\n\
             01 00               optional(bool) correct_for_drops: present, false\n\
           rule 2:\n\
             03                  HolderSet discriminator = 2 direct holders + 1\n\
             01                  Holder = block registry id 1, AS-IS (minecraft:stone)\n\
             c1 01               Holder = block registry id 193, AS-IS (minecraft:obsidian)\n\
             01 41 48 00 00      optional(f32) speed: present, 12.5\n\
             00                  optional(bool) correct_for_drops: absent\n\
           3f c0 00 00           f32 default_mining_speed = 1.5\n\
           03                    VarInt damage_per_block = 3\n\
           01                    bool can_destroy_blocks_in_creative = true\n\
         \n\
         Note the two HolderSet shapes and the two independently-absent\n\
         optionals: rule 1 denies drops with no speed, rule 2 sets a speed with\n\
         no verdict. Collapsing either pair is the bug this fixture catches.\n\
         Note above all that the direct holders are NOT offset by one: only the\n\
         set-size discriminator is. `holderSet` delegates to\n\
         `ByteBufCodecs.holderRegistry`, which writes the raw registry id, not to\n\
         `ByteBufCodecs.holder`, which reserves 0 for an inline definition and so\n\
         writes id + 1. We shipped `id + 1` first; the hermetic test agreed with\n\
         it because it encoded the same way, and these captured bytes are what\n\
         disproved it (`01`, not `02`, for stone).\n\
         Note also that can_destroy_blocks_in_creative is present on the wire\n\
         even though the SNBT never mentioned it — it is a codec field with a\n\
         default, not an optional.",
        &payload,
    );

    let stack = replay(&payload);
    assert!(
        !stack.components.has_unmodeled,
        "minecraft:tool is modeled, so a tool-only patch must decode completely"
    );
    let ToolPatch::Set(tool) = &stack.components.tool else {
        panic!(
            "the server sent a minecraft:tool but we decoded {:?}",
            stack.components.tool
        );
    };

    assert_eq!(tool.default_mining_speed(), 1.5);
    assert_eq!(tool.damage_per_block, 3);
    assert!(
        tool.can_destroy_blocks_in_creative,
        "the codec always writes this field; vanilla's default is true"
    );
    assert_eq!(tool.rules.len(), 2);

    assert_eq!(
        tool.rules[0].blocks,
        ToolBlocks::Tag(
            "minecraft:incorrect_for_diamond_tool"
                .parse()
                .expect("tag key")
        )
    );
    assert_eq!(tool.rules[0].speed(), None);
    assert_eq!(tool.rules[0].correct_for_drops, Some(false));

    assert_eq!(
        tool.rules[1].blocks,
        ToolBlocks::Blocks(vec![1, 193]),
        "an explicit block set arrives as minecraft:block *registry* ids"
    );
    assert_eq!(tool.rules[1].speed(), Some(12.5));
    assert_eq!(tool.rules[1].correct_for_drops, None);

    session.assert_still_alive().await;
}

/// The fail-open contract, re-proven now that the patch decoder has a
/// `minecraft:tool` branch: a stack carrying a tool **and** a component this
/// build does not model still yields the tool, is flagged partial, and never
/// becomes an error.
///
/// `minecraft:tool` is component id 28 and `minecraft:custom_data` is id 0;
/// vanilla emits a patch in component-registry order, so the unmodeled one here
/// sorts *first* and the tool is never reached. That is the harsher of the two
/// orderings and exactly the intended behaviour: everything after an unmodeled
/// component is unreadable, because a clientbound patch length-prefixes neither
/// itself nor its components.
#[tokio::test]
#[ignore = "requires the live 26.2 survival oracle (scripts/live-oracles/survival.sh)"]
async fn an_unmodeled_component_alongside_a_tool_still_fails_open() {
    let mut session = LiveSession::join().await;
    let payload = session
        .capture_mainhand(
            "minecraft:diamond_pickaxe[\
             minecraft:custom_data={lodestone:1},\
             minecraft:tool={rules:[{blocks:\"#minecraft:mineable/pickaxe\",speed:9.0f}]}]",
        )
        .await;
    println!("captured partial container_set_slot: {payload:02x?}");

    let stack = replay(&payload);
    assert_eq!(
        stack.item.to_string(),
        "minecraft:diamond_pickaxe",
        "the item key is decoded before any component, so it always survives"
    );
    assert_eq!(stack.count, 1);
    assert!(
        stack.components.has_unmodeled,
        "minecraft:custom_data is unmodeled and must flag the stack as partial"
    );
    assert_eq!(
        stack.components.tool,
        ToolPatch::Inherited,
        "custom_data (id 0) sorts before tool (id 28), so decoding stopped \
         before the tool was read — a partial stack, never an error"
    );

    // The control: the same session, the same code path, with only the
    // unmodeled component removed. If `has_unmodeled` were stuck on, or the
    // tool branch unreachable, this would not flip.
    let payload = session
        .capture_mainhand(
            "minecraft:diamond_pickaxe[minecraft:tool=\
             {rules:[{blocks:\"#minecraft:mineable/pickaxe\",speed:9.0f}]}]",
        )
        .await;
    let stack = replay(&payload);
    assert!(
        !stack.components.has_unmodeled,
        "control: with the unmodeled component gone, nothing may be flagged"
    );
    let ToolPatch::Set(tool) = &stack.components.tool else {
        panic!("control: expected the tool to decode, got {:?}", stack.components.tool);
    };
    assert_eq!(tool.rules.len(), 1);
    assert_eq!(tool.rules[0].speed(), Some(9.0));

    session.assert_still_alive().await;
}
