//! End-to-end: a **real** `lodestone-client`, running the real
//! [`V770Adapter`](lodestone_v770::adapter), places a furnace against the
//! real [`V770ServerProtocol`] over the in-memory transport — closing
//! `docs/block-entities.md`'s first two named gaps together (the
//! `BlockPos`-keyed registry, and placement honouring the held item) with
//! the strongest evidence this repo has: a real client observing a real
//! server's own state, not a hermetic round trip through our own encoder.
//!
//! Unlike [`crate::block_edit`]'s placement gate, this drives
//! [`serve_connection`] **directly** rather than through
//! [`IntegratedServer`](lodestone_server::IntegratedServer) — the same
//! choice `server_inventory_live.rs` already made for exactly the same
//! reason: the test needs to hold its own clone of the
//! [`BlockEntityHandle`] so it can inspect the server's own registry after
//! the connection closes, and `IntegratedServer`'s public constructors build
//! and own that handle internally with no accessor. `serve_connection` is
//! the same driver `IntegratedServer::open_in_memory` wraps byte-for-byte
//! (see `integrated.rs`'s own module doc comment), so this is not a
//! reduced-fidelity path — it is the identical wire/codec/protocol
//! machinery with one extra observation point.

use std::time::Duration;

use lodestone_client::{BlockPos, ClientBuilder, Hand, LoginProfile, ServerAddress};
use lodestone_data::block_states::{block_name, properties};
use lodestone_model::{BlockFace, ClientAction, ContainerClickType, ContainerSlotChange, ItemStack, Vec3f};
use lodestone_net::{Connection, memory_pair};
use lodestone_server::{
    BlockEntity, BlockEntityHandle, ChunkColumn, ChunkSource, FurnaceKind, NoEntities,
    serve_connection,
};
use lodestone_v770::{V770ServerProtocol, adapter};

/// An all-air column with real edit retention — this test is about
/// placement resolving the *held item*, not terrain, so the cheapest column
/// that lets the client finish its join sequence (and makes every cell a
/// valid, replaceable placement target) is the right one, the same choice
/// `server_inventory_live.rs` makes for its own `AirSource`. **Unlike**
/// that one, this one needs `set_block`/`block_state` overrides: this test
/// asserts the placement's *effect* (a real furnace block on the wire), and
/// `ChunkSource`'s default `set_block` is a documented no-op
/// (`crate::chunk`'s own module doc comment — the same trap
/// `WorldgenChunkSource` deliberately leaves unfixed for the transport-only
/// tests it exists for). Caught by running this test the first time with a
/// unit-struct `AirSource` and watching `apply_use_item_on`'s own confirming
/// `block_update` re-read the *unedited* default (still air) — the exact
/// silent-discard `docs/block-edit.md` already documents for
/// `WorldgenChunkSource`.
#[derive(Default)]
struct AirSource {
    edits: std::sync::Mutex<std::collections::HashMap<(i32, i32, i32), String>>,
}

impl ChunkSource for AirSource {
    fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
        ChunkColumn::new(0, 16)
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

fn stack(name: &str, count: u32) -> ItemStack {
    ItemStack::new(name.parse().expect("valid resource key"), count)
}

/// Resolves a **full** block-state string to its protocol-776 registry id —
/// duplicated from `block_edit.rs`'s own `resolve_state` (same three-tier
/// algorithm `server_protocol.rs`'s private `resolve_state_id` uses: exact
/// match, then the lowest-id state sharing the block name — its default —
/// then air) rather than exposed from `lodestone-v770`'s public API. Needed
/// here because `apply_use_item_on` writes the *bare* `"minecraft:furnace"`
/// string (no `facing`/`lit` properties — this crate's placement has no
/// per-block orientation rules, see `docs/block-edit.md`), and real furnace
/// states always carry both properties, so the wire id the client actually
/// receives is the block's **default** state (tier two), not an exact match
/// (tier one) — this helper has to compute the same fallback the server
/// does, not just look up an exact string.
fn resolve_state(state: &str) -> u32 {
    let (name, raw_props) = match state.split_once('[') {
        Some((name, rest)) => (name, rest.strip_suffix(']').unwrap_or(rest)),
        None => (state, ""),
    };
    let mut wanted: Vec<(&str, &str)> = if raw_props.is_empty() {
        Vec::new()
    } else {
        raw_props
            .split(',')
            .filter_map(|pair| pair.split_once('='))
            .collect()
    };
    wanted.sort_unstable();

    let mut same_name_default: Option<u32> = None;
    for id in 0..lodestone_data::block_states::STATE_COUNT {
        if block_name(id) != Some(name) {
            continue;
        }
        if same_name_default.is_none() {
            same_name_default = Some(id);
        }
        let mut have: Vec<(&str, &str)> = properties(id).unwrap_or(&[]).to_vec();
        have.sort_unstable();
        if have == wanted {
            return id;
        }
    }
    same_name_default.unwrap_or_else(|| {
        (0..)
            .find(|&id| block_name(id) == Some("minecraft:air"))
            .expect("generated table has an air entry")
    })
}

/// A real client selects a furnace from its hotbar (via a
/// `CONTAINER_CLICK` landing the item in native slot 0, hotbar's first slot
/// — the same "shaped like a real click predictor's diff" pattern
/// `server_inventory_live.rs` already established) and right-clicks an air
/// cell. The server must: place the real `minecraft:furnace` block (not
/// `minecraft:stone`, the pre-existing always-stone fallback), *and* insert
/// a fresh [`BlockEntity::Furnace`] into the registry at that position —
/// `docs/block-entities.md`'s first two named gaps, proven together because
/// the second gap's whole point is giving the first gap's registry
/// something real to hold.
#[tokio::test]
async fn real_client_places_a_furnace_and_the_server_registers_it() {
    let view_radius = 0;
    let (client_io, server_io) = memory_pair();
    let block_entities = BlockEntityHandle::default();
    let server_block_entities = block_entities.clone();

    let server_task = tokio::spawn(async move {
        let mut conn = Connection::new(server_io);
        serve_connection(
            &mut conn,
            &V770ServerProtocol,
            &AirSource::default(),
            &NoEntities,
            view_radius,
            &server_block_entities,
        )
        .await
    });

    let (mut handle, _events) = ClientBuilder::new(
        address(),
        profile("FurnacePlacer"),
        Box::new(adapter()),
    )
    .connect_with(client_io);

    handle
        .wait_for_spawn(Duration::from_secs(30))
        .await
        .expect("client never spawned");
    handle
        .wait_for_chunks(1, Duration::from_secs(30))
        .await
        .expect("initial column never arrived");

    // Land `minecraft:furnace` in native slot 0 (menu slot 36, hotbar's
    // first slot — `PlayerInventory`'s own menu-slot table,
    // `docs/server-inventory.md`) — the default selected hotbar slot, so no
    // `SetCarriedItem` is needed first.
    handle
        .send_action(ClientAction::ContainerClick {
            window_id: 0,
            state_id: 1,
            slot: 36,
            button: 0,
            click_type: ContainerClickType::Pickup,
            changed_slots: vec![ContainerSlotChange {
                slot: 36,
                item: Some(stack("minecraft:furnace", 1)),
            }],
            carried_item: None,
        })
        .expect("client still connected");

    let target_pos = BlockPos::new(3, 5, 3);
    let furnace_id = resolve_state("minecraft:furnace");
    let stone_id = resolve_state("minecraft:stone");
    assert_ne!(
        furnace_id, stone_id,
        "the two ids must actually differ for this test to mean anything"
    );

    handle
        .send_action(ClientAction::UseItemOn {
            hand: Hand::Main,
            pos: target_pos,
            face: BlockFace::Up,
            cursor: Vec3f::new(0.5, 0.0, 0.5),
            inside_block: false,
            sequence: 1,
        })
        .expect("send use item on");

    // Wait for the *specific* post-placement id, not merely "some id" — the
    // AirSource column is already fully loaded (every cell reads as air, a
    // real id) before the placement even lands, so `.is_some()` alone would
    // resolve immediately against the pre-placement state and prove nothing.
    handle
        .wait_for(Duration::from_secs(30), |h| {
            h.block_at(target_pos) == Some(furnace_id)
        })
        .await
        .expect("placement confirmation never arrived");

    assert_eq!(
        handle.block_at(target_pos),
        Some(furnace_id),
        "a furnace item in hand must place a real furnace block, not the stone fallback"
    );

    handle.shutdown();
    let _ = handle.join().await;

    let _summary = tokio::time::timeout(Duration::from_secs(10), server_task)
        .await
        .expect("serve_connection task did not finish in time")
        .expect("serve_connection task panicked")
        .expect("serve_connection returned an error");

    // The real proof this test exists for: the server's own registry, not
    // just the wire confirmation, holds a real `Furnace` at the placed
    // position.
    let registered = block_entities.with(|reg| reg.get(target_pos).cloned());
    assert!(
        matches!(
            &registered,
            Some(BlockEntity::Furnace(f)) if f.kind() == FurnaceKind::Furnace
        ),
        "expected a registered plain furnace at {target_pos:?}, got {registered:?}"
    );

    // **Control**: a position nobody clicked must not have gained an entry —
    // proves the insert is keyed to the actual placement position, not a
    // side effect that touches the whole registry.
    let untouched = BlockPos::new(target_pos.x + 5, target_pos.y, target_pos.z);
    assert!(
        block_entities.with(|reg| reg.get(untouched).is_none()),
        "an unrelated position must not have gained a block entity"
    );
    assert_eq!(
        block_entities.with(|reg| reg.len()),
        1,
        "exactly one block entity must be registered — the placed furnace, nothing extra"
    );
}

/// **Control**: an item that is *not* one of the four block-entity blocks
/// must still fall back to the pre-existing plain-stone placement and must
/// **not** create a registry entry — proves the furnace test above is
/// exercising a real branch, not a registry that inserts on every
/// placement regardless of what was held.
#[tokio::test]
async fn placing_with_an_empty_hand_still_falls_back_to_stone_and_registers_nothing() {
    let view_radius = 0;
    let (client_io, server_io) = memory_pair();
    let block_entities = BlockEntityHandle::default();
    let server_block_entities = block_entities.clone();

    let server_task = tokio::spawn(async move {
        let mut conn = Connection::new(server_io);
        serve_connection(
            &mut conn,
            &V770ServerProtocol,
            &AirSource::default(),
            &NoEntities,
            view_radius,
            &server_block_entities,
        )
        .await
    });

    let (mut handle, _events) = ClientBuilder::new(
        address(),
        profile("EmptyHandPlacer"),
        Box::new(adapter()),
    )
    .connect_with(client_io);

    handle
        .wait_for_spawn(Duration::from_secs(30))
        .await
        .expect("client never spawned");
    handle
        .wait_for_chunks(1, Duration::from_secs(30))
        .await
        .expect("initial column never arrived");

    // `(5, 64, 5)`, not a negative coordinate: with `view_radius = 0` only
    // chunk `(0, 0)` (blocks `0..=15` on each axis) is ever loaded, and a
    // negative x/z here would floor-divide into a neighbouring, never-sent
    // chunk — `handle.block_at` would then read `None` forever and the
    // `wait_for` below would time out for a reason that has nothing to do
    // with placement (this was caught by running this exact control and
    // watching it fail: the first draft used `(-3, 64, -3)` and timed out).
    let target_pos = BlockPos::new(5, 5, 5);
    let stone_id = resolve_state("minecraft:stone");

    // No `ContainerClick` at all: hotbar slot 0 stays empty, exactly the
    // pre-existing "no inventory model" starting condition this landing
    // extends rather than replaces.
    handle
        .send_action(ClientAction::UseItemOn {
            hand: Hand::Main,
            pos: target_pos,
            face: BlockFace::Up,
            cursor: Vec3f::new(0.5, 0.0, 0.5),
            inside_block: false,
            sequence: 1,
        })
        .expect("send use item on");

    // Same reasoning as the furnace test above: wait for the *specific*
    // post-placement id, since the pre-loaded air column already makes
    // `.is_some()` true before anything happens.
    handle
        .wait_for(Duration::from_secs(30), |h| {
            h.block_at(target_pos) == Some(stone_id)
        })
        .await
        .expect("placement confirmation never arrived");

    assert_eq!(
        handle.block_at(target_pos),
        Some(stone_id),
        "an empty hand must still place stone, the pre-existing fallback"
    );

    handle.shutdown();
    let _ = handle.join().await;

    let _summary = tokio::time::timeout(Duration::from_secs(10), server_task)
        .await
        .expect("serve_connection task did not finish in time")
        .expect("serve_connection task panicked")
        .expect("serve_connection returned an error");

    assert!(
        block_entities.with(|reg| reg.is_empty()),
        "a stone placement must not register any block entity"
    );
}
