//! Live proof that a **real vanilla 26.2 server's** sign block-entity bytes
//! reach `lodestone_shell::block_entities::sign_spawns` as drawable spans.
//!
//! # Why this gate exists, and what every prior sign gate could not see
//!
//! Every other sign gate in this tree builds its own `SignText`/`SignSpawn`
//! in-process, or hands `SignText::parse` an `Nbt` a test author typed. That
//! proves the renderer and the parser against *our own idea* of the wire
//! shape, which is exactly the closed loop that shipped a JSON-shaped sign
//! parser: two earlier wire probes both set their sign over RCON with SNBT
//! that already contained quotes, so the capture agreed with the writer and
//! neither could see the real encoding.
//!
//! This gate takes its input from the only source that is genuinely outside
//! this codebase — a vanilla server's own `SignText.DIRECT_CODEC` output,
//! decoded by the real adapter into the real `World` and gathered by the
//! real production function.
//!
//! # The fixture, and why it is not all-plain
//!
//! Under `NbtOps` a `ListTag` must be homogeneous. `LINES_CODEC` is
//! `ComponentSerialization.CODEC.listOf()`, so the moment **one** line carries
//! a style the whole `messages` list becomes a list of compounds — and every
//! *unstyled* sibling is then wrapped as `{"": "text"}` rather than staying a
//! bare string. An all-plain fixture is a list of bare strings and cannot see
//! that; a mixed one is the discriminating input.
#![cfg(feature = "live")]

use std::time::{Duration, Instant};

use lodestone::net::{NetClient, NetUpdate};
use lodestone_testsupport::unique_username;

const HOST: &str = "127.0.0.1";
const PORT: u16 = 25570;
const PROTOCOL: i32 = 776;

/// The two probe signs this gate reads.
///
/// **Nothing in this repository places them** — this doc used to claim "both
/// are placed by the shell fixture below" and there is no such fixture, which
/// cost an agent a confused run. `scripts/live-oracles/creative.sh` does not
/// mention signs either. Put them there over RCON on `:25571` (password
/// `lodestone`) before running:
///
/// ```text
/// setblock 3 -59 3 minecraft:oak_sign[rotation=0]{front_text:{messages:[{text:"REDLINE",color:"red"},{text:"BOLDY",bold:1b},"plain",""]}} replace
/// setblock 5 -59 3 minecraft:oak_sign[rotation=0]{front_text:{messages:["allplain","second","",""]}} replace
/// ```
///
/// The SNBT above is the *author's* shape; what this gate reads is whatever
/// `SignText.DIRECT_CODEC` re-encodes onto the wire, which is the outside
/// source. Note in particular that the mixed sign's four `messages` go out as
/// a `TAG_Compound` list with the two unstyled elements boxed as
/// `{"": "plain"}` / `{"": ""}` — not as the four elements written here.
const MIXED: [i32; 3] = [3, -59, 3];
const PLAIN: [i32; 3] = [5, -59, 3];

#[test]
#[ignore = "requires the creative oracle on 127.0.0.1:25570 (scripts/live-oracles/creative.sh) and --features live"]
fn live_sign_lines_reach_spans() {
    let net = NetClient::connect_as(HOST.into(), PORT, PROTOCOL, None, unique_username());
    let deadline = Instant::now() + Duration::from_secs(45);
    let mut logged_in = false;
    let mut last_err: Option<String> = None;
    while Instant::now() < deadline {
        for u in net.poll() {
            match u {
                NetUpdate::LoggedIn { .. } => logged_in = true,
                NetUpdate::Error(e) => last_err = Some(e),
                NetUpdate::Disconnected(r) => {
                    last_err = Some(format!("disconnected: {}", r.to_plain_string()));
                }
                _ => {}
            }
        }
        if logged_in && net.loaded_chunks().len() >= 4 {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(logged_in, "never logged in: {last_err:?}");

    let handle = net.shared_handle();

    // Dump every sign block entity the client actually holds, straight off the
    // wire-decoded world, before any of this crate's own parsing.
    {
        let client = handle.get().expect("client");
        let store = client.chunk_world();
        let world = store.read();
        for pos in client.loaded_chunks() {
            let Some(chunk) = world.get(lodestone_world::ChunkPos { x: pos.x, z: pos.z }) else {
                continue;
            };
            for be in &chunk.block_entities {
                let nbt = format!("{:?}", be.nbt);
                if nbt.contains("front_text") {
                    eprintln!(
                        "WIRE sign at ({}, {}, {}) type {} -> {nbt}",
                        pos.x * 16 + i32::from(be.rel_x),
                        be.y,
                        pos.z * 16 + i32::from(be.rel_z),
                        be.type_id,
                    );
                }
            }
        }
    }

    let spawns = lodestone::block_entities::sign_spawns(&handle, glam::Vec3::new(4.0, -59.0, 3.0));
    for s in &spawns {
        eprintln!("SPAWN {:?} front lines {:?}", s.pos, s.front.lines);
    }

    let mixed = spawns
        .iter()
        .find(|s| s.pos == MIXED)
        .expect("the mixed-style probe sign reached sign_spawns");
    let plain = spawns
        .iter()
        .find(|s| s.pos == PLAIN)
        .expect("the all-plain probe sign reached sign_spawns");

    let text = |lines: &[Vec<lodestone_world::SignTextSpan>; 4], i: usize| -> String {
        lines[i].iter().map(|s| s.text.as_str()).collect()
    };

    assert_eq!(text(&plain.front.lines, 0), "allplain");
    assert_eq!(text(&plain.front.lines, 1), "second");

    assert_eq!(text(&mixed.front.lines, 0), "REDLINE");
    assert_eq!(text(&mixed.front.lines, 1), "BOLDY");
    // The line the heterogeneous-list wrapper hides: vanilla emits it as
    // `{"": "plain"}` because two of its siblings are styled compounds.
    assert_eq!(text(&mixed.front.lines, 2), "plain");
    assert_eq!(mixed.front.lines[0][0].color, Some(0x00ff_5555));
    assert!(mixed.front.lines[1][0].bold);
}
