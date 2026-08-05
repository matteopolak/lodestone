//! Issue #465: placing a block must trigger the same neighbour-update
//! fan-out every other mutation already gets, and the result must reach the
//! player.
//!
//! # Why this test is here and not in `lodestone-server`
//!
//! `lodestone-server`'s own `serve_play.rs` drives a file-local
//! `FakeProtocol` stand-in. The thing under test is a *path* — a real
//! `UseItemOn` packet reaching `apply_use_item_on`, and the resulting block
//! changes reaching a client — so a stand-in `ServerProtocol` is exactly the
//! **world**-species vacuous test CLAUDE.md names: the transport resolves to
//! an implementation production never uses, and the gate passes either way.
//! `lodestone-server` cannot dev-depend on `lodestone-v770` (that is the
//! dependency edge, reversed), so the real-protocol gate lives here, beside
//! `server_block_placement.rs`, which established this harness.
//!
//! # The rig
//!
//! Copied from `lodestone_server::redstone_oracle_gate`, whose expected
//! profile was measured against a **live vanilla 26.2 server**, so the
//! expected values below originate entirely outside the code under test:
//!
//! ```text
//!   y=1:  T  .  w  w  w  w  w  w  w  w  w  w  w  w  w  w
//!   y=0: [======== stone floor, z = 7..=9 ========]
//!   x =   0  1  2  3  4  5  6  7  8  9 10 11 12 13 14 15
//! ```
//!
//! `T` is a lit standing redstone torch, `w` is dust seeded at `power=0`, and
//! `.` at `x = 1` is the **gap** — air. With the gap open nothing carries a
//! signal: the torch only powers what is adjacent to it, and every dust cell
//! sees only unpowered neighbours. The player then places one redstone dust
//! into the gap, which is the single mutation this whole test is about.
//!
//! After that placement the run must light up to the live server's own
//! attenuation profile, `power = 16 - x`, at **every** coordinate.
//!
//! # The computed-vs-delivered pair
//!
//! Two independent measurements of the same event, which is what separates a
//! signal that is *computed* from one that reaches a player:
//!
//! - **computed** — `SharedWorld`, the server's own world, read back through
//!   the second `Arc` handle the test keeps.
//! - **delivered** — `handle.block_at`, the client's decoded world, which can
//!   only change because a `BLOCK_UPDATE` packet actually arrived.
//!
//! Dropping the delivery half while keeping the column write must leave every
//! computed assertion passing and fail only the delivered ones. The cascade
//! cells (`x >= 2`) are the honest half of that measurement: the client
//! predicts its *own* placement at `x = 1`, so that cell alone would appear
//! delivered even with no packet at all.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestone_client::{BlockPos, ClientBuilder, Hand, LoginProfile, ServerAddress};
use lodestone_model::{
    BlockFace, ClientAction, ContainerClickType, ContainerSlotChange, ItemStack, Vec3f,
};
use lodestone_net::{Connection, memory_pair};
use lodestone_server::{
    BlockEntityHandle, ChunkColumn, ChunkSource, MobHandle, NoEntities, serve_connection,
};
use lodestone_v770::{V770ServerProtocol, adapter};

const ROW_Z: i32 = 8;
const FLOOR_Y: i32 = 0;
const DUST_Y: i32 = 1;
/// The gap the player fills — the one mutation under test.
const PLACE_X: i32 = 1;
const RUN_END_X: i32 = 15;

const WIRE: &str = "minecraft:redstone_wire";
const TORCH_LIT: &str = "minecraft:redstone_torch[lit=true]";
const UNPOWERED_DUST: &str = "minecraft:redstone_wire[power=0]";
const FLOOR: &str = "minecraft:stone";

/// Measured against a live vanilla 26.2 server (see
/// `lodestone_server::redstone_oracle_gate`'s own `ORACLE_DUST_ATTENUATION`,
/// where this table is the recorded oracle): dust `d` blocks from the torch
/// carries `16 - d`, reaching 0 at 16. Written out literally rather than as a
/// formula so a shared arithmetic mistake cannot make expectation and
/// measurement agree.
const ORACLE_DUST_ATTENUATION: &[(i32, u8)] = &[
    (1, 15),
    (2, 14),
    (3, 13),
    (4, 12),
    (5, 11),
    (6, 10),
    (7, 9),
    (8, 8),
    (9, 7),
    (10, 6),
    (11, 5),
    (12, 4),
    (13, 3),
    (14, 2),
    (15, 1),
];

/// An edit-retaining world shared between the server task and the test.
///
/// The `column` override is **load-bearing and is the reason this type exists
/// rather than reusing `server_block_placement.rs`'s `SharedAirSource`**: that
/// one returns a fresh all-air `ChunkColumn` and ignores its own edit map, so
/// the neighbour fan-out — which reads a whole column, not single blocks —
/// would see an empty world, find no torch and no dust, and compute nothing.
/// The gate would then fail for a reason that has nothing to do with #465,
/// and (worse) a *passing* variant of it would prove nothing at all.
#[derive(Clone, Default)]
struct SharedWorld {
    edits: Arc<Mutex<HashMap<(i32, i32, i32), String>>>,
}

impl SharedWorld {
    fn seed(&self, x: i32, y: i32, z: i32, name: &str) {
        self.set_block(x, y, z, name);
    }
}

impl ChunkSource for SharedWorld {
    fn column(&self, cx: i32, cz: i32) -> ChunkColumn {
        let mut column = ChunkColumn::new(0, 16);
        for (&(x, y, z), name) in self.edits.lock().expect("edits lock poisoned").iter() {
            if x.div_euclid(16) == cx && z.div_euclid(16) == cz && (0..16).contains(&y) {
                column.set_block(x.rem_euclid(16), y, z.rem_euclid(16), name);
            }
        }
        column
    }

    fn block_state(&self, x: i32, y: i32, z: i32) -> String {
        self.edits
            .lock()
            .expect("edits lock poisoned")
            .get(&(x, y, z))
            .cloned()
            .unwrap_or_else(|| "minecraft:air".to_string())
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

/// Reads the `power` property out of a block-state string like
/// `minecraft:redstone_wire[power=12]`. Returns `None` for anything that is
/// not dust, and `Some(0)` for a bare `minecraft:redstone_wire` — which is
/// what placement writes, and is also the correct initial power for freshly
/// placed dust.
fn dust_power_from_state(state: &str) -> Option<u8> {
    let (base, rest) = match state.split_once('[') {
        Some((base, rest)) => (base, Some(rest.trim_end_matches(']'))),
        None => (state, None),
    };
    if base != WIRE {
        return None;
    }
    let Some(rest) = rest else { return Some(0) };
    for pair in rest.split(',') {
        if let Some(("power", value)) = pair.split_once('=') {
            return value.parse().ok();
        }
    }
    Some(0)
}

/// The `power` the *client* believes a cell has, decoded from the block-state
/// id its own world holds. `None` means the client has no block there at all.
fn client_dust_power(handle: &lodestone_client::ClientHandle, pos: BlockPos) -> Option<u8> {
    let id = handle.block_at(pos)?;
    if lodestone_data::block_states::block_name(id) != Some(WIRE) {
        return None;
    }
    lodestone_data::block_states::properties(id)?
        .iter()
        .find(|(key, _)| *key == "power")
        .and_then(|(_, value)| value.parse().ok())
}

/// Seeds the rig of the module doc into `world`.
fn seed_rig(world: &SharedWorld) {
    for x in 0..16 {
        for z in (ROW_Z - 1)..=(ROW_Z + 1) {
            world.seed(x, FLOOR_Y, z, FLOOR);
        }
    }
    world.seed(0, DUST_Y, ROW_Z, TORCH_LIT);
    // The gap at PLACE_X is deliberately left as air.
    for x in (PLACE_X + 1)..=RUN_END_X {
        world.seed(x, DUST_Y, ROW_Z, UNPOWERED_DUST);
    }
}
/// One run of the whole scenario: seed the rig, connect a real client, place
/// one dust into the gap, and report both measurements.
///
/// Returns `(computed, delivered)` — the server's own world and the client's
/// decoded world, each as `(x, power)` along the run.
async fn place_one_dust_into_the_gap() -> (Vec<(i32, Option<u8>)>, Vec<(i32, Option<u8>)>) {
    let view_radius = 0;
    let (client_io, server_io) = memory_pair();
    let world = SharedWorld::default();
    seed_rig(&world);
    let server_world = world.clone();

    let server_task = tokio::spawn(async move {
        let mut conn = Connection::new(server_io);
        serve_connection(
            &mut conn,
            &V770ServerProtocol,
            &server_world,
            &NoEntities,
            view_radius,
            &BlockEntityHandle::default(),
            &MobHandle::default(),
        )
        .await
    });

    let (mut handle, _events) =
        ClientBuilder::new(address(), profile("RedstonePlacer"), Box::new(adapter()))
            .connect_with(client_io);

    handle
        .wait_for_spawn(Duration::from_secs(30))
        .await
        .expect("client never spawned");
    handle
        .wait_for_chunks(1, Duration::from_secs(30))
        .await
        .expect("initial column never arrived");

    // Precondition, asserted rather than assumed: with the gap open the run
    // must be dark. If the rig were already lit, the gate below would pass
    // without the placement doing anything — the precondition species of
    // vacuous test.
    for &(x, _) in ORACLE_DUST_ATTENUATION.iter().skip(1) {
        let state = world.block_state(x, DUST_Y, ROW_Z);
        assert_eq!(
            dust_power_from_state(&state),
            Some(0),
            "precondition: dust at (x={x}, y={DUST_Y}, z={ROW_Z}) must be unpowered before \
             the placement, but the rig seeded it as {state:?}"
        );
    }
    assert_eq!(
        world.block_state(PLACE_X, DUST_Y, ROW_Z),
        "minecraft:air",
        "precondition: the gap at (x={PLACE_X}, y={DUST_Y}, z={ROW_Z}) must be open"
    );

    // Hotbar slot 0 (menu slot 36) is selected by default, so no
    // `SetCarriedItem` is needed. `minecraft:redstone` is the *item*; the
    // server's own census resolves it to `minecraft:redstone_wire`.
    handle
        .send_action(ClientAction::ContainerClick {
            window_id: 0,
            state_id: 1,
            slot: 36,
            button: 0,
            click_type: ContainerClickType::Pickup,
            changed_slots: vec![ContainerSlotChange {
                slot: 36,
                item: Some(ItemStack::new(
                    "minecraft:redstone".parse().expect("valid resource key"),
                    1,
                )),
            }],
            carried_item: None,
        })
        .expect("client still connected");

    // Click the floor beneath the gap, face up: `apply_use_item_on` resolves a
    // non-air clicked block to its face neighbour, so the target is the gap.
    handle
        .send_action(ClientAction::UseItemOn {
            hand: Hand::Main,
            pos: BlockPos::new(PLACE_X, FLOOR_Y, ROW_Z),
            face: BlockFace::Up,
            cursor: Vec3f::new(0.5, 1.0, 0.5),
            inside_block: false,
            sequence: 1,
        })
        .expect("send use item on");

    // Wait for the placement to round-trip. Deliberately waits on the block
    // *name* rather than its power: the power is exactly what the encoder
    // cannot currently carry (see `the_powered_run_reaches_the_client`), so a
    // power-valued wait here would turn every failure in this file into a bare
    // 30-second `Timeout` and bury the diagnosis.
    let placed = BlockPos::new(PLACE_X, DUST_Y, ROW_Z);
    handle
        .wait_for(Duration::from_secs(30), |h| {
            h.block_at(placed)
                .and_then(lodestone_data::block_states::block_name)
                == Some(WIRE)
        })
        .await
        .expect("the placed dust was never confirmed to the client at all");

    // Read the client's world BEFORE teardown — `join` consumes the handle.
    let delivered: Vec<(i32, Option<u8>)> = ORACLE_DUST_ATTENUATION
        .iter()
        .map(|&(x, _)| (x, client_dust_power(&handle, BlockPos::new(x, DUST_Y, ROW_Z))))
        .collect();

    handle.shutdown();
    let _ = handle.join().await;
    let _summary = tokio::time::timeout(Duration::from_secs(10), server_task)
        .await
        .expect("serve_connection task did not finish in time")
        .expect("serve_connection task panicked")
        .expect("serve_connection returned an error");

    let computed: Vec<(i32, Option<u8>)> = ORACLE_DUST_ATTENUATION
        .iter()
        .map(|&(x, _)| {
            (
                x,
                dust_power_from_state(&world.block_state(x, DUST_Y, ROW_Z)),
            )
        })
        .collect();

    (computed, delivered)
}

/// **The #465 gate.** A player placing one redstone dust into the gap must
/// light the whole run, in the server's own world, to the live server's own
/// attenuation profile at every coordinate.
///
/// # Predicting the value, not the sign
///
/// Three hypotheses, all computed from constants outside the code under test,
/// and the measurement must land on exactly one:
///
/// | hypothesis | power at `x` | at `x = 1` | at `x = 15` |
/// |---|---|---|---|
/// | oracle (live 26.2) | `16 - x` | 15 | 1 |
/// | **#465 unfixed: no neighbour update** | `0` everywhere | 0 | 0 |
/// | wrong: dust carries the source undecayed | `15` | 15 | 15 |
///
/// The unfixed-#465 hypothesis differs from the oracle at all 15 coordinates,
/// and the no-decay hypothesis at 14 of them. A gate asserting only "the run
/// has some power somewhere" would pass under the no-decay model; a gate
/// asserting only that the placed cell *is* dust would pass under the unfixed
/// model, because placement has written the right block since `3b71a0b` — it
/// simply never told anything about it.
#[tokio::test]
async fn placing_dust_into_a_gap_powers_the_whole_run_in_the_servers_own_world() {
    let (computed, _delivered) = place_one_dust_into_the_gap().await;

    // Report by location, never as an aggregate: name every coordinate that
    // disagrees and what was there.
    let mut wrong: Vec<String> = Vec::new();
    for (&(x, oracle_power), &(cx, measured)) in ORACLE_DUST_ATTENUATION.iter().zip(computed.iter())
    {
        assert_eq!(x, cx, "rig misalignment between the oracle table and the readback");
        if measured != Some(oracle_power) {
            wrong.push(format!(
                "dust at (x={x}, y={DUST_Y}, z={ROW_Z}), {x} block(s) from the torch: \
                 our model says {measured:?}, the live 26.2 server measured power={oracle_power}"
            ));
        }
    }
    assert!(
        wrong.is_empty(),
        "the server's own world disagrees with the live-26.2 oracle at {} of {} coordinates \
         (#465: placement ran no neighbour update):\n  {}",
        wrong.len(),
        ORACLE_DUST_ATTENUATION.len(),
        wrong.join("\n  ")
    );

    // Separate the oracle from each wrong hypothesis, so this gate is known to
    // be able to tell them apart rather than merely agreeing with one.
    let agreements_with_unfixed = ORACLE_DUST_ATTENUATION.iter().filter(|(_, p)| *p == 0).count();
    let agreements_with_no_decay = ORACLE_DUST_ATTENUATION.iter().filter(|(_, p)| *p == 15).count();
    assert_eq!(
        agreements_with_unfixed, 0,
        "the unfixed-#465 hypothesis (every cell 0) must differ from the oracle at every \
         coordinate, otherwise this gate cannot separate them"
    );
    assert_eq!(
        agreements_with_no_decay, 1,
        "the no-decay hypothesis (every cell 15) must agree with the oracle at exactly one \
         coordinate, otherwise this gate cannot separate them"
    );
}

/// The **delivered** half of the computed-vs-delivered pair, and the reason
/// #465 cannot be closed as "a player can see redstone work".
///
/// # Why this is ignored, and what unblocks it
///
/// It fails for a reason that is neither of #465's two named causes, is
/// **pre-existing**, and lives outside `lodestone-server` entirely:
/// [`resolve_state_id`] in `crates/protocol/v770/src/server_protocol.rs`
/// matches a state string against the block-state table by **exact property
/// set**:
///
/// ```text
/// let mut have: Vec<(&str, &str)> = properties(id).unwrap_or(&[]).to_vec();
/// have.sort_unstable();
/// if have == wanted { return id; }
/// ```
///
/// `minecraft:redstone_wire` has **1296** states carrying five properties
/// (`north`/`east`/`south`/`west`/`power`), while this server's canonical dust
/// string carries exactly one — `redstone_wire::set_power` emits
/// `minecraft:redstone_wire[power=N]` and deliberately models no connection
/// graph at all (see that module's own doc comment). `have == wanted` is
/// therefore **never** true for any dust state, and the function falls through
/// to `same_name_default`, the lowest id with that name: `4011`, whose
/// properties are `east=up, north=up, power=0, south=up, west=up`.
///
/// So every dust update this server has ever sent — from a random tick, from a
/// drained scheduled tick, and now from a placement — is delivered to the
/// client as **`power=0`**, whatever the server computed. Measured: the server
/// holds the full oracle profile 15..1 while the client holds 0 at all 14
/// cascade coordinates. That is a fully-connected wire carrying the wrong
/// value, which `cargo xtask connectedness` structurally cannot see.
///
/// **That blocker is fixed and this gate is live.** `resolve_state_id` now has
/// a subset tier — the lowest-id state agreeing on every property the caller
/// *did* specify, tried before the same-name default. This test was written
/// `#[ignore]`d and named rather than deleted, which is what made un-ignoring
/// it the one-line proof that the fix reaches a player rather than only the
/// server's own state.
///
/// Note what remains cosmetically wrong and is deliberately not asserted here:
/// lowest-id is not the marked default for 661 of 797 multi-state blocks, so
/// dust resolves with its four connection properties `up` rather than `none`
/// and renders climbing rather than flat. `power` — the load-bearing half, and
/// the only thing this gate reads — is exact.
///
/// `x = PLACE_X` is skipped: the client predicts its own placement there, so
/// that one cell cannot distinguish "a packet arrived" from "nothing arrived".
#[tokio::test]
async fn the_powered_run_reaches_the_client() {
    let (computed, delivered) = place_one_dust_into_the_gap().await;

    let mut undelivered: Vec<String> = Vec::new();
    for ((&(x, oracle_power), &(_, was_computed)), &(dx, seen)) in ORACLE_DUST_ATTENUATION
        .iter()
        .zip(computed.iter())
        .zip(delivered.iter())
        .skip(1)
    {
        assert_eq!(x, dx, "rig misalignment between the oracle table and the readback");
        if seen != Some(oracle_power) {
            undelivered.push(format!(
                "dust at (x={x}, y={DUST_Y}, z={ROW_Z}): the client was told {seen:?}, \
                 the server computed {was_computed:?}, the live 26.2 oracle says {oracle_power}"
            ));
        }
    }
    assert!(
        undelivered.is_empty(),
        "the signal is COMPUTED but not DELIVERED at {} of {} cascade coordinates:\n  {}",
        undelivered.len(),
        ORACLE_DUST_ATTENUATION.len() - 1,
        undelivered.join("\n  ")
    );
}
