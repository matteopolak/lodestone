//! Live **block placement / item use through the real client stack**, against the
//! SURVIVAL oracle (`lodestone-survival`, game :25565, RCON :25566).
//!
//! Placement is the inverse of `mining.rs` and, like it, cannot be proven by a
//! self-round-trip: predicting a placement and asserting our own prediction
//! proves nothing, because the whole point is that the *server* is authoritative
//! and may refuse. So every assertion here reads the **server's** block over RCON.
//! The whole path runs through the public client:
//!   1. `ClientBuilder::connect()` — the real transport + v770 adapter (resolved
//!      through the registry; `lodestone-game` still names no version crate).
//!   2. Drive the **actual** [`Placement`] machine and lower its emitted
//!      [`ClientAction::UseItemOn`] onto the wire through `ClientHandle::send_action`
//!      — a real `use_item_on` encode. Nothing is hand-constructed.
//!   3. Assert the **server** reports (or refuses) the block, read over RCON on a
//!      **force-loaded** chunk (an unloaded chunk makes `execute if block` always
//!      report "Test failed", which has cost time here before).
//!
//! ## What it gates
//!
//! - **A block actually places.** Clicking the top face of a solid block with a
//!   stone item makes the server report stone in the cell above — server truth.
//! - **A rejected placement rolls back.** The client optimistically predicts a
//!   placement into a cell the server knows is bedrock; the server refuses, the
//!   cell stays bedrock, and [`Placement::reconcile`] flags `corrected` — the
//!   rollback is observed, not silently trusted. This is also the **negative
//!   control**: it is a placement we drove that must *not* result in our block.
//! - **Sneak-vs-interact ordering.** Right-clicking a chest opens it (we observe
//!   the real `ScreenOpened` event and place nothing); sneaking + right-click on
//!   the same chest places a block against it (server reports stone).
//!
//! ## Two server facts this test is built around (26.2 `ServerGamePacketListenerImpl`)
//!
//! - **The load gate.** `handleUseItemOn` is dropped until `hasClientLoaded()`
//!   (:1343), and the real driver never sends `player_loaded`, so the server
//!   auto-loads us only after ~60 ticks (~3s). Every placement is therefore
//!   **retried** until the server reflects it, which both clears the gate and
//!   tolerates the first few dropped attempts.
//! - **The player's held item is the server's source of truth.** We seed
//!   `weapon.mainhand` with a plain `minecraft:stone` (zero data components, so
//!   the v770 item decoder handles it cleanly) and the server places *that*.
//!
//! ## Run it
//!
//! ```text
//! cargo test -p lodestone-game --features live-place \
//!     --test live_place -- --ignored --nocapture
//! ```
#![cfg(feature = "live-place")]

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lodestone_client::{ClientBuilder, ClientEvent, ClientHandle, LoginProfile, ServerAddress};
use lodestone_game::placement::{
    OrientationKind, Placement, PlacementWorld, UseOnContext, UseOnDecision,
};
use lodestone_model::math::{BlockPos, Rotation, Vec3, Vec3f};
use lodestone_model::action::PlayerInput;
use lodestone_model::{BlockFace, ClientAction, Hand, Identifier};
use lodestone_testsupport::{AsyncRconClient as Rcon, poll_until, unique_username};
use uuid::Uuid;

const HOST: &str = "127.0.0.1";
const PORT: u16 = 25565;
const RCON_PORT: u16 = 25566;
const RCON_PASSWORD: &str = "lodestone";

/// The number of independent live assertions this gate must reach. A miswritten
/// early return that skipped, say, the rollback check would otherwise pass
/// silently; asserting the count at the end makes a skipped gate fail loudly.
const EXPECTED_CHECKS: usize = 4;

fn mc(path: &str) -> Identifier {
    format!("minecraft:{path}").parse().unwrap()
}

/// A placement world configured from what the test itself set over RCON, so
/// `Placement::use_on`'s decision is driven by known truth and the *server* is
/// the authority on the outcome. This plays the role a real driver's
/// registry-backed block classification would.
#[derive(Default)]
struct Fixture {
    replaceable: Vec<BlockPos>,
    interactable: Vec<BlockPos>,
}
impl PlacementWorld for Fixture {
    fn is_replaceable(&self, pos: BlockPos) -> bool {
        self.replaceable.contains(&pos)
    }
    fn is_interactable(&self, pos: BlockPos) -> bool {
        self.interactable.contains(&pos)
    }
}

/// The server's own truth for a block: `execute if block ... <block>` reports
/// "Test passed" only on a match. The authority for every assertion here.
async fn block_is(rcon: &mut Rcon, pos: BlockPos, block: &str) -> bool {
    let resp = rcon
        .cmd(&format!(
            "execute if block {} {} {} {block}",
            pos.x, pos.y, pos.z
        ))
        .await;
    resp.contains("Test passed")
}

/// Sets a block server-side and confirms it by reading it back, so it is robust
/// even when the cell already held that block (`setblock` then reports "Could
/// not set the block" but the state we want is there).
async fn set_block(rcon: &mut Rcon, pos: BlockPos, block: &str) -> bool {
    rcon.cmd(&format!("setblock {} {} {} {block}", pos.x, pos.y, pos.z))
        .await;
    block_is(rcon, pos, block).await
}

fn block_center(pos: BlockPos) -> Vec3 {
    Vec3 {
        x: f64::from(pos.x) + 0.5,
        y: f64::from(pos.y) + 0.5,
        z: f64::from(pos.z) + 0.5,
    }
}

/// Lower a `use_on` decision's action onto the wire.
fn send(handle: &ClientHandle, decision: &UseOnDecision) {
    let action = match decision {
        UseOnDecision::Interact { action }
        | UseOnDecision::Place { action, .. }
        | UseOnDecision::Nothing { action } => action.clone(),
    };
    let _ = handle.send_action(action);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires the lodestone-survival server on 127.0.0.1:25565 (RCON :25566)"]
async fn block_placement_round_trips_through_client() {
    println!("=== LIVE BLOCK-PLACEMENT (protocol 776, survival :25565) ===");

    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("lodestone_client=debug")),
        )
        .with_test_writer()
        .try_init();

    let user = unique_username();
    println!("player = {user}");

    let server = ServerAddress {
        host: HOST.into(),
        port: PORT,
    };
    let profile = LoginProfile {
        username: user.clone(),
        uuid: Uuid::new_v4(),
    };
    let adapter = lodestone_registry::adapter_for_protocol(776)
        .expect("v770 family compiled into the registry via lodestone-client/live-v770");

    let (mut handle, mut events) = ClientBuilder::new(server, profile, adapter)
        .connect()
        .await
        .expect(
            "connect to lodestone-survival on 127.0.0.1:25565 — recreate it with \
             ./scripts/live-oracles/survival.sh",
        );

    // Capture ScreenOpened (the chest-opened signal) while draining the stream so
    // the driver's bounded channel never backpressures.
    let opened: Arc<Mutex<Vec<i32>>> = Arc::new(Mutex::new(Vec::new()));
    let opened_w = Arc::clone(&opened);
    let drain = tokio::spawn(async move {
        while let Some(event) = events.recv().await {
            match event {
                ClientEvent::ScreenOpened { window_id, .. } => {
                    opened_w.lock().unwrap().push(window_id);
                }
                ClientEvent::Disconnect { reason } => {
                    eprintln!("!!! driver saw Disconnect: {}", reason.to_plain_string());
                    break;
                }
                _ => {}
            }
        }
    });

    // Reach Play: the server must know our player before RCON targets it.
    let ready = poll_until(Duration::from_secs(30), Duration::from_millis(100), || async {
        handle
            .players()
            .into_iter()
            .find(|p| p.name.as_deref() == Some(user.as_str()))
    })
    .await;
    assert!(
        ready.is_some(),
        "player {user} never appeared in the live tab list — is lodestone-survival on :25565 in Play? (alive={})",
        handle.is_alive()
    );
    println!("player is in-game");

    let mut rcon = Rcon::connect((HOST, RCON_PORT), RCON_PASSWORD)
        .await
        .expect("connect RCON on 127.0.0.1:25566 (password 'lodestone') — is lodestone-survival up?");

    // op (bypass spawn protection), survival gamemode, and safety effects — a
    // death would auto-respawn/teleport the player and strand later commands.
    println!("  RCON op       -> {:?}", rcon.cmd(&format!("op {user}")).await);
    println!(
        "  RCON gamemode -> {:?}",
        rcon.cmd(&format!("gamemode survival {user}")).await
    );
    for eff in [
        "minecraft:resistance 999999 255 true",
        "minecraft:regeneration 999999 9 true",
        "minecraft:fire_resistance 999999 0 true",
        "minecraft:saturation 999999 9 true",
    ] {
        let _ = rcon.cmd(&format!("effect give {user} {eff}")).await;
    }

    // Give the player a plain stone block in the main hand — zero data components,
    // so the v770 item decoder handles the resulting container_set_slot cleanly.
    println!(
        "  RCON give     -> {:?}",
        rcon.cmd(&format!(
            "item replace entity {user} weapon.mainhand with minecraft:stone 64"
        ))
        .await
    );

    let pos = poll_until(Duration::from_secs(15), Duration::from_millis(200), || async {
        handle.position()
    })
    .await
    .expect("client never reported a position");
    let bx = pos.x.floor() as i32;
    let by = pos.y.floor() as i32;
    let bz = pos.z.floor() as i32;
    println!("  player feet block = ({bx}, {by}, {bz})");

    // Force-load the whole working area so `execute if block` reads real state,
    // not "chunk not loaded" (which always reports "Test failed").
    for (cx, cz) in [(bx - 16, bz - 16), (bx + 16, bz + 16)] {
        let _ = rcon.cmd(&format!("forceload add {cx} {cz}")).await;
    }

    let mut checks = 0usize;

    // ---------------------------------------------------------------------
    // Check 1: a real placement lands on the server.
    //
    // Click the top face of a solid stone block two east; the item goes into the
    // air cell above it. base is non-replaceable, the cell above is air.
    // ---------------------------------------------------------------------
    let base = BlockPos::new(bx + 2, by, bz);
    let target = BlockPos::new(bx + 2, by + 1, bz);
    assert!(
        set_block(&mut rcon, base, "minecraft:stone").await,
        "failed to place the base block to click on"
    );
    assert!(
        set_block(&mut rcon, target, "minecraft:air").await,
        "failed to clear the placement target to air"
    );

    let world = Fixture {
        replaceable: vec![target],
        ..Fixture::default()
    };
    let mut machine = Placement::new();
    // Aim at the base block so the server accepts the reach/hit. Orientation is
    // Fixed for stone, so the context rotation does not affect the prediction;
    // the server derives any facing from its own record of our look.
    let rotation = Rotation { yaw: 0.0, pitch: 45.0 };
    let _ = handle.look_at(block_center(base));
    tokio::time::sleep(Duration::from_millis(200)).await;

    let ctx = UseOnContext {
        hand: Hand::Main,
        clicked: base,
        face: BlockFace::Up,
        cursor: Vec3f::new(0.5, 1.0, 0.5),
        inside_block: false,
        rotation,
        sneaking: false,
        has_item_in_hand: true,
        placing: Some(mc("stone")),
        orientation: OrientationKind::Fixed,
    };
    let decision = machine.use_on(&ctx, &world);
    let predicted_pos = match &decision {
        UseOnDecision::Place { prediction, .. } => prediction.pos,
        other => panic!("expected a placement decision on a solid block, got {other:?}"),
    };
    assert_eq!(
        predicted_pos, target,
        "placement must predict the air cell above the clicked block"
    );

    // Retry the placement until the server reflects it (clears the load gate).
    let placed = {
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut ok = false;
        while Instant::now() < deadline {
            let _ = handle.look_at(block_center(base));
            send(&handle, &decision);
            tokio::time::sleep(Duration::from_millis(400)).await;
            if block_is(&mut rcon, target, "minecraft:stone").await {
                ok = true;
                break;
            }
        }
        ok
    };
    assert!(
        placed,
        "stone never appeared at {target:?} — the placement did not reach the server through the \
         real client (alive={})",
        handle.is_alive()
    );
    // The server confirms; reconcile agrees (no correction).
    let stone = mc("stone");
    let recon = machine.reconcile(target, Some(&stone));
    assert!(!recon.corrected, "an accepted placement must not reconcile as corrected");
    checks += 1;
    println!("check 1 OK: stone placed at {target:?} (server truth over RCON)");

    // ---------------------------------------------------------------------
    // Check 2 (NEGATIVE CONTROL): a rejected placement rolls back.
    //
    // The client optimistically predicts placing stone into a cell the server
    // knows is BEDROCK (not replaceable). The server refuses; the cell stays
    // bedrock; reconcile flags the correction. A design that trusted the
    // prediction would render stone that is not there.
    // ---------------------------------------------------------------------
    let reject_base = BlockPos::new(bx - 2, by, bz);
    let reject_target = BlockPos::new(bx - 2, by + 1, bz);
    assert!(
        set_block(&mut rcon, reject_base, "minecraft:stone").await,
        "failed to place the reject base block"
    );
    assert!(
        set_block(&mut rcon, reject_target, "minecraft:bedrock").await,
        "failed to place the bedrock the server will defend"
    );

    // The lie that makes the client predict: claim the bedrock cell is
    // replaceable. The server knows better.
    let lying_world = Fixture {
        replaceable: vec![reject_target],
        ..Fixture::default()
    };
    let mut reject_machine = Placement::new();
    let reject_ctx = UseOnContext {
        hand: Hand::Main,
        clicked: reject_base,
        face: BlockFace::Up,
        cursor: Vec3f::new(0.5, 1.0, 0.5),
        inside_block: false,
        rotation,
        sneaking: false,
        has_item_in_hand: true,
        placing: Some(mc("stone")),
        orientation: OrientationKind::Fixed,
    };
    let reject_decision = reject_machine.use_on(&reject_ctx, &lying_world);
    assert!(
        matches!(reject_decision, UseOnDecision::Place { .. }),
        "the client must optimistically predict this placement"
    );
    // Drive it a few times to be sure the server saw and refused it.
    for _ in 0..6 {
        let _ = handle.look_at(block_center(reject_base));
        send(&handle, &reject_decision);
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    let still_bedrock = block_is(&mut rcon, reject_target, "minecraft:bedrock").await;
    let became_stone = block_is(&mut rcon, reject_target, "minecraft:stone").await;
    println!(
        "  rejected placement: server reports bedrock={still_bedrock} stone={became_stone} \
         (predicted stone)"
    );
    assert!(
        still_bedrock && !became_stone,
        "the server must refuse a placement into bedrock; it reported stone={became_stone}"
    );
    // Reconcile against server truth (bedrock): the rollback is observed.
    let bedrock = mc("bedrock");
    let reject_recon = reject_machine.reconcile(reject_target, Some(&bedrock));
    assert!(
        reject_recon.corrected,
        "a refused placement must reconcile as corrected (the optimistic block snaps back)"
    );
    checks += 1;
    println!("check 2 OK: rejected placement rolled back to bedrock, reconcile flagged corrected");

    // ---------------------------------------------------------------------
    // Check 3: right-clicking a chest OPENS it (no placement).
    // ---------------------------------------------------------------------
    let chest = BlockPos::new(bx, by, bz - 3);
    let chest_top = BlockPos::new(bx, by + 1, bz - 3);
    assert!(
        set_block(&mut rcon, chest, "minecraft:chest").await,
        "failed to place the chest"
    );
    assert!(
        set_block(&mut rcon, chest_top, "minecraft:air").await,
        "failed to clear above the chest"
    );

    let interact_world = Fixture {
        interactable: vec![chest],
        replaceable: vec![chest_top],
    };
    let mut interact_machine = Placement::new();
    let interact_ctx = UseOnContext {
        hand: Hand::Main,
        clicked: chest,
        face: BlockFace::Up,
        cursor: Vec3f::new(0.5, 1.0, 0.5),
        inside_block: false,
        rotation,
        sneaking: false, // not sneaking -> the chest actuates
        has_item_in_hand: true,
        placing: Some(mc("stone")),
        orientation: OrientationKind::Fixed,
    };
    let interact_decision = interact_machine.use_on(&interact_ctx, &interact_world);
    assert!(
        matches!(interact_decision, UseOnDecision::Interact { .. }),
        "an un-sneaked click on a chest must actuate it, not place"
    );
    opened.lock().unwrap().clear();
    let chest_opened = {
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut ok = false;
        while Instant::now() < deadline {
            let _ = handle.look_at(block_center(chest));
            send(&handle, &interact_decision);
            tokio::time::sleep(Duration::from_millis(400)).await;
            if !opened.lock().unwrap().is_empty() {
                ok = true;
                break;
            }
        }
        ok
    };
    assert!(
        chest_opened,
        "the chest never opened — no ScreenOpened event arrived (alive={})",
        handle.is_alive()
    );
    // Nothing was placed on top of the chest.
    assert!(
        block_is(&mut rcon, chest_top, "minecraft:air").await,
        "an interaction must place nothing, but a block appeared above the chest"
    );
    if let Some(&wid) = opened.lock().unwrap().first() {
        let _ = handle.send_action(ClientAction::ContainerClose { window_id: wid });
    }
    checks += 1;
    println!("check 3 OK: chest opened on right-click (ScreenOpened observed), nothing placed");

    // ---------------------------------------------------------------------
    // Check 4: SNEAK + right-click on the same chest PLACES against it.
    // ---------------------------------------------------------------------
    let sneak_world = Fixture {
        interactable: vec![chest],
        replaceable: vec![chest_top],
    };
    let mut sneak_machine = Placement::new();
    let sneak_ctx = UseOnContext {
        hand: Hand::Main,
        clicked: chest,
        face: BlockFace::Up,
        cursor: Vec3f::new(0.5, 1.0, 0.5),
        inside_block: false,
        rotation,
        sneaking: true, // sneaking with an item -> suppress use, place instead
        has_item_in_hand: true,
        placing: Some(mc("stone")),
        orientation: OrientationKind::Fixed,
    };
    let sneak_decision = sneak_machine.use_on(&sneak_ctx, &sneak_world);
    assert!(
        matches!(&sneak_decision, UseOnDecision::Place { prediction, .. } if prediction.pos == chest_top),
        "sneak + item on a chest must place against it (at the cell above)"
    );
    // The wire packet carries the sneaking secondary-action state via the client's
    // own crouch; drive a crouch input so the server treats us as sneaking, then
    // retry the placement until the block lands above the chest.
    let placed_on_chest = {
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut ok = false;
        let crouch = PlayerInput {
            shift: true,
            ..PlayerInput::EMPTY
        };
        while Instant::now() < deadline {
            let _ = handle.look_at(block_center(chest));
            // Drive the real crouch input so the *server* records shift-key-down
            // (ServerGamePacketListenerImpl.handlePlayerInput -> setShiftKeyDown);
            // otherwise the server treats the click as an interaction and re-opens
            // the chest instead of placing.
            let _ = handle.send_action(ClientAction::SetPlayerInput(crouch));
            tokio::time::sleep(Duration::from_millis(150)).await;
            send(&handle, &sneak_decision);
            tokio::time::sleep(Duration::from_millis(400)).await;
            if block_is(&mut rcon, chest_top, "minecraft:stone").await {
                ok = true;
                break;
            }
        }
        let _ = handle.send_action(ClientAction::SetPlayerInput(PlayerInput::EMPTY));
        ok
    };
    assert!(
        placed_on_chest,
        "sneak-placement never landed stone above the chest at {chest_top:?} (alive={})",
        handle.is_alive()
    );
    checks += 1;
    println!("check 4 OK: sneak + right-click placed stone above the chest");

    // ---------------------------------------------------------------------
    // Cleanup + gate the count so a skipped assertion fails loudly.
    // ---------------------------------------------------------------------
    for (cx, cz) in [(bx - 16, bz - 16), (bx + 16, bz + 16)] {
        let _ = rcon.cmd(&format!("forceload remove {cx} {cz}")).await;
    }
    assert_eq!(
        checks, EXPECTED_CHECKS,
        "expected {EXPECTED_CHECKS} live checks, only reached {checks}"
    );
    println!("=== all {checks} live placement checks passed ===");

    handle.shutdown();
    let _ = drain.await;
}
