//! Live proof that an item frame's **contents** survive the whole production
//! chain: a real vanilla 26.2 server's `ItemFrame.DATA_ITEM` metadata, through
//! the real adapter and the real ECS ingest, into the `EntityDraw` the real
//! `Extract` schedule publishes and `gpu/world_items.rs` reads.
//!
//! # Why this is driven through `Sim` and not `EntityInterpolator`
//!
//! `EntityInterpolator` is a harness: a caller has to bridge each raw
//! `EntityView` into ingest components by hand, so a gate built on it asserts
//! against components the *test* inserted. That is exactly the blindness this
//! gate exists to close — `item_frame_pixels.rs` measured 1334 px for a framed
//! item while production drew zero, because it built its own `EntityDraw` with
//! `item: Some(..)` and the producer refused to supply one:
//! `extract_entity_draws` narrowed the recorded stack to
//! `ITEM_ENTITY_TYPE_PATH`, so an item frame, a framed filled map and a
//! projectile's live tint all read `None` forever.
//!
//! `Sim::entity_draws` is the accessor `app/redraw.rs` calls every frame, on
//! the `App` `Sim::new` builds. Nothing here supplies the item id, the count or
//! the rotation; all three come off the wire.
#![cfg(feature = "live")]

use std::time::{Duration, Instant};

use lodestone::config::{Config, Mode};
use lodestone::sim::Sim;
use lodestone_testsupport::unique_username;

const HOST: &str = "127.0.0.1";
const PORT: u16 = 25570;
const PROTOCOL: i32 = lodestone::config::DEFAULT_PROTOCOL;

/// The fixture the gate builds over RCON once it has joined: every frame in
/// the world is removed, then a north-facing `item_frame` at `(6, -59, 7)`
/// holding one diamond turned to step 3 is summoned against a stone block.
fn fixture_commands() -> Vec<String> {
    vec![
        "kill @e[type=minecraft:item_frame]".to_owned(),
        "setblock 6 -59 8 minecraft:stone replace".to_owned(),
        "summon minecraft:item_frame 6 -59 7 {Facing:2b,Item:{id:\"minecraft:diamond\",count:1},ItemRotation:3b,Fixed:1b}".to_owned(),
    ]
}
const FRAME_TYPE: &str = "item_frame";
const FRAMED_ITEM: &str = "diamond";
const FRAMED_ROTATION: u8 = 3;

#[test]
#[ignore = "requires the creative oracle on 127.0.0.1:25570 (scripts/live-oracles/creative.sh), its item_frame fixture, and --features live"]
fn a_server_placed_item_frame_carries_its_stack_into_the_extracted_draw() {
    let config = Config {
        mode: Mode::Window,
        host: HOST.to_owned(),
        port: PORT,
        ..Config::default()
    };
    let mut sim = Sim::new(config);
    sim.connect_as(HOST.to_owned(), PORT, PROTOCOL, unique_username());

    // The server only places a block or entity in a chunk that is loaded, so
    // the fixture waits for the join to stream the spawn area.
    let loaded = Instant::now() + Duration::from_secs(45);
    while sim.chunk_count() < 4 && Instant::now() < loaded {
        sim.step(1.0 / 20.0);
        std::thread::sleep(Duration::from_millis(50));
    }
    let mut rcon = crate::prepare::connect(crate::prepare::CREATIVE_RCON, "just oracle-creative");
    crate::prepare::run_all(&mut rcon, &fixture_commands());

    let deadline = Instant::now() + Duration::from_secs(60);
    let mut found = None;
    while Instant::now() < deadline {
        sim.step(1.0 / 20.0);
        // The stack rides a *separate* metadata packet from the spawn, so the
        // frame is tracked for a tick or two before its contents land. Waiting
        // for the item is what makes the assertions below about the wiring
        // rather than about poll timing; the timeout still fails if it never
        // arrives.
        if let Some(draw) = sim
            .entity_draws()
            .into_iter()
            .find(|d| {
                d.type_path.as_ref() == FRAME_TYPE
                    && d.item.is_some()
                    && d.feet.x.floor() == 6.0
                    && d.feet.z.floor() == 7.0
            })
        {
            found = Some(draw);
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    let frames: Vec<String> = sim
        .entity_draws()
        .iter()
        .filter(|d| d.type_path.as_ref() == FRAME_TYPE)
        .map(|d| format!("{:?} item={:?} rot={}", d.feet, d.item, d.item_frame_rotation))
        .collect();

    let draw = found.unwrap_or_else(|| {
        panic!(
            "no item_frame draw carried a stack within 60s. Frames extracted: {frames:?}. \
             Fix: check that the creative oracle accepted the fixture commands."
        );
    });

    eprintln!("live framed item: {frames:?}");

    assert_eq!(
        draw.item.as_ref().map(lodestone_assets::ResourceLocation::path),
        Some(FRAMED_ITEM),
        "the frame's stack must be the one the server put in it, not a default",
    );
    // The rotation shares its metadata index with a `Display`'s interpolation
    // duration, so it is guarded on `MetadataClass::ItemFrame`. A wrong guard
    // drops the field silently and the item draws unturned.
    assert_eq!(
        draw.item_frame_rotation, FRAMED_ROTATION,
        "ItemFrame.DATA_ROTATION must survive the metadata fold",
    );
}
