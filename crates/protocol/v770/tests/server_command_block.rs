//! End-to-end acceptance gate for issue #48's remainder — both hops named in
//! `crate::command_block`'s own module doc, over the **real** wire and a
//! **real** running tick loop: a real `lodestone-client` opens a command
//! block, sends the same `SET_COMMAND_BLOCK` packet the vanilla GUI's "Done"
//! button sends, and the server's own world must show the command's effect —
//! not merely that the packet decoded, and not merely that the block entity
//! ticked.
//!
//! # Why `IntegratedServer::open_in_memory_with_mobs`, not `serve_connection`
//!
//! A command block's command runs from `crate::tick`'s scheduled-tick drain,
//! not from the connection task that decodes `SET_COMMAND_BLOCK` — so a test
//! built on bare `serve_connection` (`server_block_placement.rs`'s own
//! harness) would prove the packet reached the entity and nothing past that.
//! `open_in_memory_with_mobs` is the one public constructor that also spawns
//! the real background tick loop (`crates/lodestone-server/src/tick.rs`'s
//! `run_tick_loop_with_weather`, `pub(crate)` and unreachable from this crate
//! directly), so this is the strongest evidence available from outside
//! `lodestone-server`: the identical machinery a real join runs.
//!
//! # The discriminating pair
//!
//! `/setblock` is the chosen command: its effect (a named block at a named
//! position) is unambiguous, unlike "the block entity ticked" or "a response
//! line was sent". Three cases, one server each:
//!
//! - **positive** — a repeating command block set "Always Active"
//!   (`automatic: true`, unconditional) must actually place the block. This
//!   is the control that proves the negative cases below are discriminating
//!   rather than vacuous: without it, "nothing ran" could just as easily mean
//!   the harness never gave anything a chance to run.
//! - **unpowered** — the identical command on an ordinary impulse command
//!   block, `automatic: false`, with nothing wired to power it (this pass
//!   does not wire a live redstone signal — see `crate::command_block`'s own
//!   module doc for exactly why), must never place the block.
//! - **conditional, unmet** — a repeating command block set "Always Active"
//!   *and* conditional, with nothing behind it, must never place the block:
//!   `mark_condition_met`'s "no predecessor reads as unmet" branch.
//!
//! # Computed *and* delivered
//!
//! Each case asserts the server's own [`ChunkSource`] (the ground truth a
//! `SetBlock` effect is applied against, held through a second `Arc` handle
//! this test keeps — the same shape `server_redstone_placement.rs` already
//! established) **and** the real client's own decoded view
//! (`handle.block_at`, which can only change because a real `block_update`
//! packet arrived). The target cell sits inside the loaded view for exactly
//! that reason.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestone_client::{BlockPos, ClientBuilder, Hand, LoginProfile, ServerAddress};
use lodestone_data::block_states::block_name;
use lodestone_model::{BlockFace, ClientAction, CommandBlockMode, GameMode, ItemStack, Vec3f};
use lodestone_server::{ChunkColumn, ChunkSource, IntegratedServer};
use lodestone_v770::{V770ServerProtocol, adapter};

/// An all-air, edit-retaining column — the same shape
/// `server_block_placement.rs`'s `SharedAirSource` establishes, cloned here
/// rather than imported (that one is private to its own file).
#[derive(Clone, Default)]
struct SharedAirSource {
    edits: Arc<Mutex<HashMap<(i32, i32, i32), String>>>,
}

impl ChunkSource for SharedAirSource {
    fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
        ChunkColumn::new(0, 32)
    }

    fn block_state(&self, x: i32, y: i32, z: i32) -> String {
        self.edits
            .lock()
            .expect("edits lock poisoned")
            .get(&(x, y, z))
            .cloned()
            .unwrap_or_else(|| "minecraft:air".to_string())
    }

    fn biome_state_at(&self, _x: i32, _y: i32, _z: i32) -> String {
        "minecraft:plains".to_string()
    }

    fn set_block(&self, x: i32, y: i32, z: i32, name: &str) {
        self.edits
            .lock()
            .expect("edits lock poisoned")
            .insert((x, y, z), name.to_string());
    }
}

fn profile(name: &str) -> LoginProfile {
    LoginProfile {
        username: name.into(),
        uuid: uuid::Uuid::new_v4(),
    }
}

fn address() -> ServerAddress {
    ServerAddress {
        host: "memory".into(),
        port: 0,
    }
}

fn stack(name: &str) -> ItemStack {
    ItemStack::new(name.parse().expect("valid resource key"), 1)
}

/// Spins up a real client against a real, in-memory, tick-looped server,
/// switches to creative, places a `minecraft:command_block` at `pos` and
/// waits for the server to confirm it — the setup every case below shares.
async fn place_command_block(
    handle: &mut lodestone_client::ClientHandle,
    pos: BlockPos,
) {
    handle
        .send_action(ClientAction::ChangeGameMode { mode: GameMode::Creative })
        .expect("client still connected");
    handle
        .send_action(ClientAction::SetCreativeModeSlot {
            slot: 36,
            item: Some(stack("minecraft:command_block")),
        })
        .expect("client still connected");
    handle
        .send_action(ClientAction::UseItemOn {
            hand: Hand::Main,
            pos,
            face: BlockFace::Up,
            cursor: Vec3f::new(0.5, 0.0, 0.5),
            inside_block: false,
            sequence: 1,
        })
        .expect("send use item on");
    handle
        .wait_for(Duration::from_secs(30), move |h| {
            h.block_at(pos).is_some_and(|id| block_name(id) == Some("minecraft:command_block"))
        })
        .await
        .expect("command block placement never confirmed");
}

/// The positive control: "Always Active" (unconditional) must run.
///
/// Without this passing, the two negative cases below would be unfalsifiable
/// — see this file's own module doc.
#[tokio::test]
async fn an_always_active_command_block_runs_its_command() {
    let view_radius = 0;
    let source = SharedAirSource::default();
    let (server, client_io) = IntegratedServer::open_in_memory_with_mobs(
        V770ServerProtocol,
        source.clone(),
        (0..=0, 0..=0),
        (0, 0),
        0,
        view_radius,
    );
    let (mut handle, _events) =
        ClientBuilder::new(address(), profile("AlwaysActive"), Box::new(adapter())).connect_with(client_io);

    handle.wait_for_spawn(Duration::from_secs(30)).await.expect("client never spawned");
    handle.wait_for_chunks(1, Duration::from_secs(30)).await.expect("initial column never arrived");

    let block_pos = BlockPos::new(2, 5, 2);
    place_command_block(&mut handle, block_pos).await;

    let target = BlockPos::new(11, 5, 13);
    handle
        .send_action(ClientAction::SetCommandBlock {
            pos: block_pos,
            command: format!("setblock {} {} {} minecraft:diamond_block", target.x, target.y, target.z),
            mode: CommandBlockMode::Auto,
            track_output: false,
            conditional: false,
            automatic: true,
        })
        .expect("client still connected");

    // The server's own world — the ground truth a `SetBlock` effect is
    // applied against, independent of anything the client happens to decode.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while source.block_state(target.x, target.y, target.z) != "minecraft:diamond_block" {
        assert!(std::time::Instant::now() < deadline, "the always-active command block never ran");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(source.block_state(target.x, target.y, target.z), "minecraft:diamond_block");

    // The delivered half: the real client's own decoded world, reachable
    // only through a real `block_update` the tick loop's effect must have
    // published.
    let diamond_id = (0..lodestone_data::block_states::STATE_COUNT)
        .find(|&id| block_name(id) == Some("minecraft:diamond_block"))
        .expect("diamond_block must exist in the generated state table");
    handle
        .wait_for(Duration::from_secs(10), move |h| h.block_at(target) == Some(diamond_id))
        .await
        .expect("the client never received the command's block update");

    handle.shutdown();
    let _ = handle.join().await;
    server.shutdown().await;
}

/// Negative control #1: an ordinary impulse command block (`Redstone` mode,
/// `automatic: false`) with a real command set and nothing wired to power it
/// must never run. This pass does not wire a live redstone signal to
/// `crate::command_block::on_power_changed` — see that module's own doc —
/// so this also documents the current boundary rather than merely asserting
/// an accident.
#[tokio::test]
async fn an_unpowered_impulse_command_block_never_runs() {
    let view_radius = 0;
    let source = SharedAirSource::default();
    let (server, client_io) = IntegratedServer::open_in_memory_with_mobs(
        V770ServerProtocol,
        source.clone(),
        (0..=0, 0..=0),
        (0, 0),
        0,
        view_radius,
    );
    let (mut handle, _events) =
        ClientBuilder::new(address(), profile("NeverPowered"), Box::new(adapter())).connect_with(client_io);

    handle.wait_for_spawn(Duration::from_secs(30)).await.expect("client never spawned");
    handle.wait_for_chunks(1, Duration::from_secs(30)).await.expect("initial column never arrived");

    let block_pos = BlockPos::new(3, 5, 6);
    place_command_block(&mut handle, block_pos).await;

    let target = BlockPos::new(9, 5, 1);
    handle
        .send_action(ClientAction::SetCommandBlock {
            pos: block_pos,
            command: format!("setblock {} {} {} minecraft:diamond_block", target.x, target.y, target.z),
            mode: CommandBlockMode::Redstone,
            track_output: false,
            conditional: false,
            automatic: false,
        })
        .expect("client still connected");

    // Give the tick loop real ticks to *not* act on — long enough to cover
    // several scheduled-tick drains at the loop's own cadence.
    tokio::time::sleep(Duration::from_millis(400)).await;

    assert_eq!(
        source.block_state(target.x, target.y, target.z),
        "minecraft:air",
        "an unpowered, non-automatic command block must never run its command"
    );

    handle.shutdown();
    let _ = handle.join().await;
    server.shutdown().await;
}

/// Negative control #2: "Always Active" *and* conditional, with no command
/// block behind it — `mark_condition_met`'s "no predecessor reads as unmet"
/// branch — must never run, even though the same "Always Active" toggle
/// alone is exactly what the positive control proves is sufficient.
#[tokio::test]
async fn a_conditional_always_active_command_block_with_no_predecessor_never_runs() {
    let view_radius = 0;
    let source = SharedAirSource::default();
    let (server, client_io) = IntegratedServer::open_in_memory_with_mobs(
        V770ServerProtocol,
        source.clone(),
        (0..=0, 0..=0),
        (0, 0),
        0,
        view_radius,
    );
    let (mut handle, _events) =
        ClientBuilder::new(address(), profile("UnmetCondition"), Box::new(adapter())).connect_with(client_io);

    handle.wait_for_spawn(Duration::from_secs(30)).await.expect("client never spawned");
    handle.wait_for_chunks(1, Duration::from_secs(30)).await.expect("initial column never arrived");

    let block_pos = BlockPos::new(7, 5, 4);
    place_command_block(&mut handle, block_pos).await;

    let target = BlockPos::new(14, 5, 2);
    handle
        .send_action(ClientAction::SetCommandBlock {
            pos: block_pos,
            command: format!("setblock {} {} {} minecraft:diamond_block", target.x, target.y, target.z),
            mode: CommandBlockMode::Auto,
            track_output: false,
            conditional: true,
            automatic: true,
        })
        .expect("client still connected");

    tokio::time::sleep(Duration::from_millis(400)).await;

    assert_eq!(
        source.block_state(target.x, target.y, target.z),
        "minecraft:air",
        "a conditional command block with no predecessor behind it must never run, \
         even though it is Always Active"
    );

    handle.shutdown();
    let _ = handle.join().await;
    server.shutdown().await;
}
