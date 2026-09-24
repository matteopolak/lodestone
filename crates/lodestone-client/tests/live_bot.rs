//! Live end-to-end bot test against a real vanilla server (Phase 1 gate).
//!
//! Gated behind the `live-v26-2` feature AND `#[ignore]`, so the default
//! `cargo test` stays hermetic and version-free. Run it against a real server
//! (offline mode, flat world) on `127.0.0.1:25565` with:
//!
//! ```text
//! cargo test -p lodestone-client --features live-v26-2 --test live_bot -- --ignored --nocapture
//! ```
//!
//! Unlike `live_join`, which only asserts the transport reaches Play and gets a
//! keep-alive, this exercises the **programmable bot API** end to end: it waits
//! on read-model conditions, reads world/player state the server sent, performs
//! observable actions (chat), and reports. Every assertion is against a
//! server-derived fact — decoded chunk data, server-reported health/position —
//! not against state the client authored locally.
//!
//! Version selection goes through `lodestone-registry`; `lodestone-client` never
//! names a concrete version crate.
#![cfg(feature = "live-v26-2")]

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use lodestone_client::{ChunkSection, ClientBuilder, ClientEvent, LoginProfile, ServerAddress};
use uuid::Uuid;

mod common;
use common::unique_username;

#[tokio::test]
#[ignore = "requires a live Minecraft server on 127.0.0.1:25565"]
async fn bot_joins_reads_world_and_acts() {
    let server = ServerAddress {
        host: "127.0.0.1".into(),
        port: 25565,
    };
    let profile = LoginProfile {
        // Per-run unique: a shared offline-mode name can inherit a persisted dead
        // player, which silently blackouts chunks. See `common::unique_username`.
        username: unique_username(),
        uuid: Uuid::new_v4(),
    };

    let adapter = lodestone_registry::adapter_for_protocol(776)
        .expect("v26-2 family compiled into the registry via the live-v26-2 feature");

    let (mut handle, mut events) = ClientBuilder::new(server, profile, adapter)
        .connect()
        .await
        .expect("connect to live server");

    // Drain the event stream on its own task so the bounded channel never
    // backpressures the driver, and signal when a keep-alive really arrives.
    let (ka_tx, ka_rx) = tokio::sync::oneshot::channel();
    let drain = tokio::spawn(async move {
        let mut ka_tx = Some(ka_tx);
        while let Some(event) = events.recv().await {
            match event {
                ClientEvent::KeepAlive { .. } => {
                    if let Some(tx) = ka_tx.take() {
                        let _ = tx.send(());
                    }
                }
                ClientEvent::Disconnect { reason } => {
                    eprintln!("server disconnected us: {}", reason.to_plain_string());
                    break;
                }
                _ => {}
            }
        }
    });

    // 1. Enter the world.
    handle
        .wait_for_login(Duration::from_secs(30))
        .await
        .expect("should reach Play (Login event)");

    // 2. Wait for world data. If this blacks out, a corpse is the usual cause,
    //    so surface health in the failure message.
    //
    //    NOTE ON COUNT: vanilla streams chunks in *batches*, sending
    //    `chunk_batch_finished` after each and withholding the next once ten go
    //    unacknowledged, until the client returns a `chunk_batch_received` ACK.
    //    That ACK is version-specific flow-control behind `VersionAdapter` (as a
    //    `Directive::Send`), and the v26-2 adapter now models it — so the stream
    //    continues well past the first batch. This test deliberately asserts only
    //    the minimum (`>= 1` chunk) because its subject is decoded terrain queried
    //    by block, not streaming volume; the batch-ack cliff and multi-hundred-
    //    chunk streaming are gated separately by `live_session.rs`.
    let chunks_result = handle.wait_for_chunks(1, Duration::from_secs(30)).await;
    assert!(
        chunks_result.is_ok(),
        "no chunks within 30s (health={:?}; 0.0 => inherited corpse)",
        handle.health()
    );

    // 3. Corpse guard: a server-reported positive health rules out an inherited
    //    dead player.
    let health = handle.health();
    assert!(
        health.is_some_and(|h| h > 0.0),
        "health {health:?} is not positive (0.0 => inherited corpse)"
    );

    // 4. Server-derived world structure, proven through the section-snapshot
    //    surface the mesher uses. This step reads world structure only and does
    //    not depend on player position. Pull owned `Arc<ChunkSection>`
    //    snapshots for a real loaded column (the exact primitive a mesher holds),
    //    read genuine decoded terrain off them with no lock held, and require
    //    more than one distinct block-state id across the column — which can only
    //    come from the server's chunk data.
    let some_chunk = handle
        .loaded_chunks()
        .into_iter()
        .next()
        .expect("at least one chunk is loaded");
    // A 26.2 overworld column spans 24 sections; request them all in one lock and
    // keep the non-air (non-elided) snapshots.
    let requests: Vec<_> = (0..24usize).map(|i| (some_chunk, i)).collect();
    let sections: Vec<Arc<ChunkSection>> = handle
        .sections_at(&requests)
        .into_iter()
        .flatten()
        .collect();
    assert!(
        !sections.is_empty(),
        "the loaded column must expose at least one non-air section snapshot"
    );
    let mut ids = HashSet::new();
    for section in &sections {
        for x in 0..16usize {
            for y in 0..16usize {
                for z in 0..16usize {
                    ids.insert(section.get_block(x, y, z));
                }
            }
        }
    }
    assert!(
        ids.len() >= 2,
        "expected real decoded terrain in the loaded column (>=2 distinct block ids), saw {ids:?}"
    );

    // 5. An observable action the adapter really encodes in Play: chat. (Visible
    //    in the server log; asserting server-side receipt would need a second
    //    client, which is out of scope here.)
    handle.chat("lodestone bot online").expect("send chat");

    // 6. Movement, once the server has placed us. Two assertions, each pinning a
    //    different thing, and neither dressed as the other:
    //
    //    (a) SERVER-DERIVED: the pre-move `position` can only have been written by
    //        a server `TeleportPlayer` — nothing else sets it before we send our
    //        first `Move` (see `set_local_movement`). Requiring it `Some` proves
    //        the server really placed us; a v26-2 that stopped emitting the
    //        placement teleport would fail here.
    //
    //    (b) CLIENT SEND+PREDICT PATH: `walk_to` issues real `ClientAction::Move`
    //        packets, and the driver folds each one into an OPTIMISTIC LOCAL
    //        PREDICTION (`set_local_movement` writes the commanded target directly;
    //        the server only overrides it via a corrective `TeleportPlayer`). So
    //        the position read back after the walk is the driver's own prediction,
    //        NOT server-confirmed displacement. Asserting arrival therefore pins
    //        the handle -> driver -> read-model send path end to end and fails
    //        loudly if `walk_to` silently no-ops — but it is deliberately NOT a
    //        claim that the server moved us. Server-acknowledged displacement
    //        needs a second observer client watching our entity; that is
    //        impl-physics's parity gate, tracked separately.
    let position = handle.position();
    let start = position.expect("server must place us with a TeleportPlayer before we move");
    let target = lodestone_client::Vec3::new(start.x + 4.0, start.y, start.z);
    let outcome = handle
        .walk_to(target, 0.5, Duration::from_secs(5))
        .await
        .expect("walk_to should drive the local prediction without a hard error");
    assert_eq!(
        outcome,
        lodestone_client::WalkOutcome::Arrived,
        "walk_to timed out before the local prediction reached the target: {outcome:?} \
         — a no-op or non-stepping walk_to lands here"
    );
    let predicted = handle
        .position()
        .expect("position stays known after a local-prediction walk");
    let advanced = {
        let mx = predicted.x - start.x;
        let mz = predicted.z - start.z;
        (mx * mx + mz * mz).sqrt()
    };
    assert!(
        advanced >= 3.5,
        "commanded a 4-block walk but the local prediction only advanced {advanced:.3} \
         blocks — walk_to is not stepping"
    );

    eprintln!(
        "REPORT: reached Play, health={health:?}, server_placed_position={position:?}, \
         loaded_chunks={}, distinct_block_ids_in_column={}, \
         local_prediction_after_walk={predicted:?} (advanced {advanced:.3} blocks; \
         local prediction, not server-confirmed)",
        handle.loaded_chunk_count(),
        ids.len(),
    );

    // 7. Confirm the connection is genuinely being kept alive by the server:
    //    vanilla sends a keep-alive roughly every 15s, and our driver answers it
    //    automatically. Wait for one to prove the session is healthy over time,
    //    not just at the join instant.
    let got_keep_alive = tokio::time::timeout(Duration::from_secs(25), ka_rx)
        .await
        .is_ok();
    assert!(got_keep_alive, "never observed a keep-alive within 25s");

    handle.shutdown();
    let outcome = handle.join().await;
    eprintln!("REPORT: session outcome = {outcome:?}");

    drain.abort();
}
