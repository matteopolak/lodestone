//! Live **block-breaking through the real client stack**, against the SURVIVAL
//! oracle (`lodestone-survival`, game :25565, RCON :25566).
//!
//! Mining did not exist anywhere in the workspace until `mining.rs`; this gate is
//! what proves the new state machine is not an island. The whole path runs
//! through the public client:
//!   1. `ClientBuilder::connect()` — the real transport + v26-2 adapter (resolved
//!      through the registry; `lodestone-game` still names no version crate).
//!   2. Drive the **actual** [`Mining`] predictor tick-by-tick and lower its
//!      emitted [`ClientAction::BlockAction`] START / STOP onto the wire through
//!      `ClientHandle::send_action` — a real `player_action` encode. Nothing is
//!      hand-constructed, so a divergence between what the machine *predicts* and
//!      what it *sends* cannot hide (same discipline as `live_container.rs`).
//!   3. Assert the **server** reports the block as air, read over RCON. The
//!      expected value originates outside our code entirely — not our optimistic
//!      client-side prediction, which would prove nothing.
//!
//! ## What it gates
//!
//! - **Correctness.** A stone block driven by the real machine actually becomes
//!   air server-side, read back over RCON.
//! - **Timing.** Bare-handed stone (~150 vanilla ticks) takes *materially* longer
//!   to reach air than a hardness-0 instant block (`slime_block`), which completes
//!   on the START tick alone. Without the timing gate a break that always
//!   completes immediately would pass the "became air" assertion.
//! - **Negative control.** A block we place but never dig stays stone.
//!
//! ## The diamond-pickaxe contrast is proven hermetically, not here — a real bug
//!
//! The brief asks to contrast stone bare-handed vs a diamond pickaxe. Doing that
//! *live* is blocked by a genuine v26-2 decode gap that this gate surfaced: on
//! `/item replace ... diamond_pickaxe`, the server syncs the slot with
//! `container_set_slot` (packet 20) carrying the pickaxe *with a data-component
//! patch*, and v26-2's `read_item_stack` refuses any component patch (it needs a
//! bespoke codec per component type). The driver treats that decode error as
//! fatal, so equipping any tool crashes the session:
//!
//! ```text
//! ERROR adapter rejected packet
//!   error=... item id 966 [diamond_pickaxe] carries 1 added and 0 removed
//!           data components; component patches are not yet supported
//!   packet_id=20 [container_set_slot]
//! ```
//!
//! **STALE as of the `ItemStack` components work — corrected 2026-07-28.** The
//! decode is now **fail-open, not fatal**: `read_component_patch` decodes the
//! modeled components, and on the first unmodeled one sets `has_unmodeled`,
//! warns, and returns `complete == false`, dropping the rest of that packet
//! rather than tearing down the connection ("the packet is dropped past this
//! point, not fatal", `v26-2/src/adapter.rs`). **Equipping a tool no longer
//! crashes the session.**
//!
//! This note misled an agent on 2026-07-28 into reporting tool speed as
//! hard-blocked. What *is* still missing is narrower: `minecraft:tool` is not
//! among the modeled components (only `custom_name`, `damage`, `enchantments`),
//! so `BreakInputs::tool_speed` has no source yet. That is a modeling gap, not
//! an outage.
//!
//! The *tool-speed* half of the break formula is therefore proven hermetically
//! in `mining.rs` (diamond pickaxe = 6 ticks vs bare hand = 151 ticks on stone);
//! the live gate proves the half needing no component-bearing item — a real
//! bare-handed break lands, and time depends on the target. **Note the
//! provenance: the 151-tick figure is server-confirmed over RCON; the 6-tick
//! diamond figure is hermetic only.**
//!
//! ## Two server facts this test is built around (26.2 `ServerPlayerGameMode`)
//!
//! - **The load gate.** `handlePlayerAction` is dropped until `hasClientLoaded()`
//!   (`ServerGamePacketListenerImpl:1273`). The real driver never sends
//!   `player_loaded`, so the server auto-loads us only after ~60 ticks (~3s). The
//!   *instant-break* step doubles as the load-clear: it retries START until the
//!   slime block vanishes, and only then are the timed digs attempted. (This is
//!   the same gate the drop test in `live_inventory.rs` waits on, and a different
//!   one from `handleContainerClick`, which is *not* gated — do not harmonise the
//!   waits.)
//! - **Single START + single STOP is enough.** On STOP the server breaks when
//!   `getDestroyProgress * (ticksSpentDestroying + 1) >= 0.7`; if that is not yet
//!   met it sets `hasDelayedDestroy` and finishes the block on its own subsequent
//!   `tick()`s once cumulative progress `>= 1.0`. Either way the block breaks
//!   after one STOP, so we drive the machine to the STOP it emits and then poll.
//!
//! ## Run it
//!
//! ```text
//! cargo test -p lodestone-game --features live-mining \
//!     --test live_mining -- --ignored --nocapture
//! ```
#![cfg(feature = "live-mining")]

use std::time::{Duration, Instant};

use lodestone_client::{ClientBuilder, ClientEvent, ClientHandle, LoginProfile, ServerAddress};
use lodestone_game::item::ItemStack as GameItem;
use lodestone_game::mining::{BreakInputs, Mining};
use lodestone_model::{BlockActionKind, BlockFace, BlockPos, ClientAction};
use lodestone_testsupport::{AsyncRconClient as Rcon, poll_until, unique_username};
use uuid::Uuid;

const HOST: &str = "127.0.0.1";
const PORT: u16 = 25565;
const RCON_PORT: u16 = 25566;
const RCON_PASSWORD: &str = "lodestone";

/// Stone's destroy time (vanilla's own stone block, `.strength(1.5F, 6.0F)`), used to build
/// the `BreakInputs` the machine accumulates. Kept as an *injected* input, the
/// same way `CollisionView` injects geometry — the crate holds no block table, so
/// this test cannot pass by agreeing with a fixture we minted ourselves.
const STONE_HARDNESS: f32 = 1.5;

/// Bare-hand-on-stone inputs: no tool speed bonus, wrong tool (÷100 divider).
fn stone_bare_hand() -> BreakInputs {
    BreakInputs {
        hardness: STONE_HARDNESS,
        is_air: false,
        correct_tool: false,
        tool_speed: 1.0,
        ..BreakInputs::default()
    }
}

/// Hardness-0 inputs: `progress_per_tick` is `+inf >= 1.0`, so the machine's
/// `start()` takes the instant-break branch and never retains a live dig.
fn instant() -> BreakInputs {
    BreakInputs {
        hardness: 0.0,
        is_air: false,
        ..BreakInputs::default()
    }
}

fn is_stop(action: &ClientAction) -> bool {
    matches!(
        action,
        ClientAction::BlockAction {
            action: BlockActionKind::StopDestroy,
            ..
        }
    )
}

/// The server's own truth for a block position: `execute if block ... air`
/// reports "Test passed" only when the block *is* air. This is the authority for
/// the break — not the client's optimistic prediction.
async fn is_air(rcon: &mut Rcon, pos: BlockPos) -> bool {
    let resp = rcon
        .cmd(&format!(
            "execute if block {} {} {} minecraft:air",
            pos.x, pos.y, pos.z
        ))
        .await;
    // Terminal `execute if block` reports "Test passed" on a match and
    // "Test failed" otherwise. Match exactly so we never mis-read either.
    resp.contains("Test passed")
}

/// Places a block server-side and confirms it by reading the state back, so it
/// works even when the target already held that block (in which case `setblock`
/// reports "Could not set the block" but the state we want is nonetheless there).
async fn set_block(rcon: &mut Rcon, pos: BlockPos, block: &str) -> bool {
    rcon.cmd(&format!("setblock {} {} {} {block}", pos.x, pos.y, pos.z))
        .await;
    let check = rcon
        .cmd(&format!(
            "execute if block {} {} {} {block}",
            pos.x, pos.y, pos.z
        ))
        .await;
    check.contains("Test passed")
}

/// Drive the real [`Mining`] machine to completion against `pos`, lowering every
/// emitted action onto the wire, then poll the server until the block is air.
/// Returns the wall-clock from the first START to the observed air, or `None` on
/// timeout. Ticks at ~50ms so the client accumulator and the server's own tick
/// clock stay roughly aligned (STOP then lands past the server's 0.7 threshold).
async fn dig_to_air(
    handle: &ClientHandle,
    rcon: &mut Rcon,
    pos: BlockPos,
    inputs: &BreakInputs,
    tool: Option<GameItem>,
    max_ticks: u32,
) -> Option<Duration> {
    let mut machine = Mining::new();
    let face = BlockFace::West;
    let t0 = Instant::now();
    for action in machine.start(pos, face, inputs, tool.clone()) {
        let _ = handle.send_action(action);
    }
    // Instant break leaves no live dig: START alone broke it, poll straight away.
    if machine.is_destroying() {
        let mut ticks = 0;
        loop {
            tokio::time::sleep(Duration::from_millis(50)).await;
            let mut stopped = false;
            for action in machine.continue_(pos, face, inputs, tool.clone()) {
                if is_stop(&action) {
                    stopped = true;
                }
                let _ = handle.send_action(action);
            }
            ticks += 1;
            if stopped || ticks >= max_ticks {
                println!(
                    "    dig drove {ticks} ticks, STOP emitted={stopped}, elapsed={:?}",
                    t0.elapsed()
                );
                break;
            }
        }
    }
    // Poll server-truth for air. The server breaks on STOP (>=0.7) or finishes on
    // its own ticks via delayed-destroy (>=1.0); both land here.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if is_air(rcon, pos).await {
            return Some(t0.elapsed());
        }
        if Instant::now() >= deadline {
            let actual = rcon
                .cmd(&format!(
                    "execute if block {} {} {} minecraft:stone",
                    pos.x, pos.y, pos.z
                ))
                .await;
            println!(
                "    TIMEOUT: block at ({},{},{}) not air after {:?}; still-stone check={actual:?}, player pos={:?}",
                pos.x,
                pos.y,
                pos.z,
                t0.elapsed(),
                handle.position()
            );
            return None;
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires the lodestone-survival server on 127.0.0.1:25565 (RCON :25566)"]
async fn block_breaking_round_trips_through_client() {
    println!("=== LIVE BLOCK-BREAKING (protocol 776, survival :25565) ===");

    // Surface the driver's internal tracing (notably a fatal "adapter rejected
    // packet" with its packet id) so a decode failure in the v26-2 adapter is
    // visible rather than a silent event-stream close.
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
        .expect("v26-2 family compiled into the registry via lodestone-client/live-v26-2");

    let (mut handle, mut events) = ClientBuilder::new(server, profile, adapter)
        .connect()
        .await
        .expect(
            "connect to lodestone-survival on 127.0.0.1:25565 — recreate it with \
             ./scripts/live-oracles/survival.sh",
        );

    // Drain the event stream so the driver's bounded channel never backpressures.
    let drain = tokio::spawn(async move {
        while let Some(event) = events.recv().await {
            match event {
                ClientEvent::Disconnect { reason } => {
                    eprintln!("!!! driver saw Disconnect: {}", reason.to_plain_string());
                    break;
                }
                ClientEvent::Death { .. } => eprintln!("!!! event: Death"),
                ClientEvent::Respawned { .. } => eprintln!("!!! event: Respawned"),
                _ => {}
            }
        }
        eprintln!("!!! drain loop ended (event stream closed)");
    });

    // Reach Play: the server must know our player before RCON targets it.
    let ready = poll_until(
        Duration::from_secs(30),
        Duration::from_millis(100),
        || async {
            handle
                .players()
                .into_iter()
                .find(|p| p.name.as_deref() == Some(user.as_str()))
        },
    )
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

    // Survival gamemode is required (creative insta-breaks everything, which would
    // make the timing gate vacuous). `op` bypasses spawn protection (default 16
    // blocks around world spawn blocks non-ops from breaking). The effects keep a
    // stray mob, fall, fire, drowning or hunger from killing the player mid-run —
    // a death triggers an auto-respawn/teleport in the driver and the entity
    // briefly vanishes ("No entity was found"), stranding every later command.
    println!("  RCON op       -> {:?}", rcon.cmd(&format!("op {user}")).await);
    println!(
        "  RCON gamemode -> {:?}",
        rcon.cmd(&format!("gamemode survival {user}")).await
    );
    for eff in [
        "minecraft:resistance 999999 255 true",
        "minecraft:regeneration 999999 9 true",
        "minecraft:fire_resistance 999999 0 true",
        "minecraft:water_breathing 999999 0 true",
        "minecraft:saturation 999999 9 true",
    ] {
        let _ = rcon.cmd(&format!("effect give {user} {eff}")).await;
    }
    println!("  RCON effects  -> resistance/regen/fire/water/saturation applied");

    // Settle on a position, then target blocks two east of the player at feet
    // level (dx=2 avoids overlapping the player box; all within the 4.5-block
    // interaction range). Never dig the floor the player stands on (by-1).
    let pos = poll_until(Duration::from_secs(15), Duration::from_millis(200), || async {
        handle.position()
    })
    .await
    .expect("client never reported a position");
    let bx = pos.x.floor() as i32;
    let by = pos.y.floor() as i32;
    let bz = pos.z.floor() as i32;
    println!("  player feet block = ({bx}, {by}, {bz})");

    let stone_pos = BlockPos::new(bx + 2, by, bz);
    let instant_pos = BlockPos::new(bx + 2, by, bz + 2);
    let control_pos = BlockPos::new(bx + 2, by, bz - 2);

    // Clear a small air pocket around every target so each block is mined in the
    // open (reachable, no neighbouring block absorbing the click) and the player
    // never suffocates against terrain we placed next to it.
    for p in [stone_pos, instant_pos, control_pos] {
        for dy in 0..=1 {
            for dz in -1..=1 {
                let _ = rcon
                    .cmd(&format!(
                        "setblock {} {} {} minecraft:air",
                        p.x,
                        p.y + dy,
                        p.z + dz
                    ))
                    .await;
            }
        }
    }

    let mut checked = 0usize;

    // --- Negative control: a block we place and never dig ---
    assert!(
        set_block(&mut rcon, control_pos, "minecraft:stone").await,
        "failed to place the control stone block — is the chunk loaded?"
    );

    // --- Instant break + load-gate clear ---
    //
    // `slime_block` has destroy time 0 (hardness 0), so the machine's `start()`
    // takes the instant-break branch: START only, no STOP. The server insta-mines
    // it on START once loaded, so retrying START until it vanishes both proves the
    // instant-break path AND clears the ~3s `hasClientLoaded()` gate for the timed
    // digs below.
    let mut instant_time = None;
    let mut instant_saw_stop = false;
    let mut instant_attempts = 0u32;
    let instant_deadline = Instant::now() + Duration::from_secs(30);
    loop {
        instant_attempts += 1;
        assert!(
            set_block(&mut rcon, instant_pos, "minecraft:slime_block").await,
            "failed to place the slime block"
        );
        let mut machine = Mining::new();
        let t0 = Instant::now();
        let actions = machine.start(instant_pos, BlockFace::Up, &instant(), None);
        // The instant-break branch must retain no live dig and emit no STOP.
        assert!(
            !machine.is_destroying(),
            "hardness-0 block should instant-break on START, leaving no live dig"
        );
        for action in &actions {
            if is_stop(action) {
                instant_saw_stop = true;
            }
            let _ = handle.send_action(action.clone());
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
        if is_air(&mut rcon, instant_pos).await {
            instant_time = Some(t0.elapsed());
            break;
        }
        if Instant::now() >= instant_deadline {
            break;
        }
    }
    assert!(
        instant_time.is_some(),
        "slime block never broke on START within 30s across {instant_attempts} attempts \
         (load gate never cleared or instant-break path broken; alive={})",
        handle.is_alive()
    );
    assert!(
        !instant_saw_stop,
        "instant break must complete on START alone — the machine must never emit STOP for it"
    );
    checked += 1;
    println!(
        "instant break: slime_block broke on START in {:?} after {instant_attempts} attempt(s) \
         (load gate now clear)",
        instant_time.unwrap()
    );

    // --- Bare hand on stone (slow) ---
    //
    // NOTE ON THE DIAMOND-PICKAXE CONTRAST. The brief asks for stone bare-handed
    // vs a diamond pickaxe. We drive this dig **bare-handed on purpose**: a real
    // server, on `/item replace ... diamond_pickaxe`, immediately syncs the hotbar
    // slot with `container_set_slot` (packet 20) carrying the pickaxe as a full
    // `ItemStack` *with a data-component patch*. v26-2's item decoder refuses any
    // component patch (`adapter.rs` `read_item_stack`: decoding one needs a bespoke
    // codec for each of 111 component types), and the client driver treats an
    // adapter decode error as fatal (`driver.rs`: "adapter rejected packet"), so
    // equipping the pickaxe kills the session before the dig completes. Observed
    // live:
    //
    //   ERROR adapter rejected packet
    //     error=... item id 966 [diamond_pickaxe] carries 1 added and 0 removed
    //             data components; component patches are not yet supported
    //     packet_id=20 [container_set_slot]
    //
    // That is a real v26-2 decode gap (any equipped tool crashes the client), owned
    // by impl-v26-2 — not a mining bug. The *tool-speed* half of the formula is
    // therefore proven **hermetically** in `mining.rs` (diamond pickaxe = 6 ticks
    // vs bare hand = 151 ticks on the same stone), and the live gate proves the
    // half that needs no component-bearing item: a real bare-handed break lands on
    // the server, and break time depends on the target (slow stone vs instant
    // slime), which a "always breaks immediately" bug cannot fake.
    //
    // No tool is equipped (a fresh survival join has an empty hand, whose slot
    // syncs as the empty stack = zero components, which decodes cleanly), so the
    // driver survives the full ~7.5s dig.
    assert!(
        set_block(&mut rcon, stone_pos, "minecraft:stone").await,
        "failed to place the stone block for the bare-hand dig"
    );
    let bare_time = dig_to_air(&handle, &mut rcon, stone_pos, &stone_bare_hand(), None, 400).await;
    assert!(
        bare_time.is_some(),
        "stone never broke bare-handed within the timeout — the dig did not reach the server \
         through the real client (alive={})",
        handle.is_alive()
    );
    checked += 1;
    println!(
        "bare hand: stone -> air in {:?} (server truth over RCON)",
        bare_time.unwrap()
    );

    // --- Timing gate: break time depends on the target ---
    //
    // Stone bare-handed (~150 vanilla ticks) must take materially longer than the
    // hardness-0 instant block (one START tick). If a break "always completes
    // immediately" — the exact bug the timing gate exists to catch — these two
    // collapse to the same duration and this fails.
    let instant_time = instant_time.unwrap();
    let bare_time = bare_time.unwrap();
    assert!(
        bare_time > instant_time * 3,
        "bare-hand stone ({bare_time:?}) must be materially slower than an instant block \
         ({instant_time:?}); if they are close the break is completing immediately regardless of \
         the target (vacuous)"
    );
    checked += 1;
    assert!(
        instant_time < Duration::from_secs(2),
        "an instant-break block should complete on the START tick, got {instant_time:?}"
    );
    checked += 1;
    assert!(
        bare_time > Duration::from_secs(3),
        "bare-handed stone (~7.5s of ticks) should take several seconds, got {bare_time:?}"
    );
    checked += 1;

    // --- Negative control still holds: the un-dug block is not air ---
    let control_is_air = is_air(&mut rcon, control_pos).await;
    assert!(
        !control_is_air,
        "the control block we never dug reported as air — the gate is not discriminating \
         (a break/air-check bug would look exactly like this)"
    );
    checked += 1;
    println!("negative control: un-dug stone is still solid server-side");

    const EXPECTED_CHECKS: usize = 6;
    assert!(
        checked >= EXPECTED_CHECKS,
        "anti-vacuity floor: only {checked} comparisons ran, expected >= {EXPECTED_CHECKS} — \
         an assertion was skipped, the gate is no longer proving what it claims"
    );

    // Best-effort cleanup on the shared server: clear placed blocks, drop the
    // effects, and deop.
    for p in [stone_pos, instant_pos, control_pos] {
        let _ = rcon
            .cmd(&format!("setblock {} {} {} minecraft:air", p.x, p.y, p.z))
            .await;
    }
    let _ = rcon.cmd(&format!("effect clear {user}")).await;
    let _ = rcon.cmd(&format!("deop {user}")).await;

    println!(
        "=== BLOCK-BREAKING ORACLE PASSED: {checked} comparisons — the real Mining machine drove \
         START/STOP through ClientHandle::send_action, and the server confirmed air over RCON for \
         the instant ({instant_time:?}) and bare-hand stone ({bare_time:?}) digs ==="
    );
    handle.shutdown();
    drain.abort();
}
