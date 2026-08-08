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
    BlockEntity, BlockEntityHandle, ChunkColumn, ChunkSource, Furnace, FurnaceKind, MobHandle,
    NoEntities, serve_connection,
};
use lodestone_v770::{V770ServerProtocol, adapter};

/// An all-air column with real edit retention — this test is about
/// placement resolving the *held item*, not terrain, so the cheapest column
/// that lets the client finish its join sequence (and makes every cell a
/// valid, replaceable placement target) is the right one, the same choice
/// `server_inventory_live.rs` makes for its own `AirSource`. **Unlike**
/// that one, this one needs `set_block`/`block_state` overrides: this test
/// asserts the placement's *effect* (a real furnace block on the wire), and
/// `ChunkSource::set_block` has no default since issue #440 — an implementor
/// must choose explicitly, and a source with no retention cannot persist the
/// edit (the same trap `WorldgenChunkSource`'s `todo!()` documents for the
/// solidity-only transport source). Caught by running this test the first
/// time with a unit-struct `AirSource` and watching `apply_use_item_on`'s own
/// confirming `block_update` re-read the *unedited* default (still air) —
/// the exact silent-discard `docs/block-edit.md` already documents for
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
/// duplicated rather than exposed from `lodestone-v770`'s public API. Needed
/// here because `apply_use_item_on` writes the *bare* `"minecraft:furnace"`
/// string (no `facing`/`lit` properties — a furnace is not one of the families
/// `placed_block_state` orients, see `docs/block-edit.md`), and real furnace
/// states always carry both properties, so the wire id the client actually
/// receives is the block's **default** state, not an exact match.
///
/// # This helper's own history is the reason it now reads the jar column
///
/// It used to compute "the lowest id sharing the block name", the same thing
/// `server_protocol.rs`'s `resolve_state_id` used to do — and it broke, with a
/// 30-second *timeout* rather than a mismatch, the moment issue #546 fixed that
/// fallback to resolve a bare name to the block's real default state. Lowest-id
/// disagrees with the marked default for 661 of 797 multi-state blocks, and
/// `minecraft:furnace` is one of them. So the expectation now comes from
/// `lodestone_data::snow_support::is_default_state` — vanilla's own
/// `state == block.defaultBlockState()`, dumped from the 26.2 server — which is
/// *outside* the resolver under test rather than a second copy of it.
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

    let mut jar_default: Option<u32> = None;
    for id in 0..lodestone_data::block_states::STATE_COUNT {
        if block_name(id) != Some(name) {
            continue;
        }
        if lodestone_data::snow_support::is_default_state(id) == Some(true) {
            jar_default = Some(id);
        }
        let mut have: Vec<(&str, &str)> = properties(id).unwrap_or(&[]).to_vec();
        have.sort_unstable();
        if have == wanted {
            return id;
        }
    }
    jar_default.unwrap_or_else(|| {
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
            &MobHandle::default(),
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

/// **Control**: an empty hand must place **nothing** and must not create a
/// registry entry — proves the furnace test above is exercising a real
/// branch, not a registry that inserts on every placement regardless of what
/// was held.
///
/// Before #466 this asserted the *stone* fallback, which was the pre-existing
/// behaviour: `block_entity_for_item`'s `None` arm wrote `minecraft:stone`
/// for anything it did not recognise. That arm was the path every ordinary
/// block took, so it was removed; a held item that places no block now leaves
/// the world untouched. The control still does its job — it is still the
/// negative arm of the furnace test — it just asserts the corrected
/// behaviour. See `server_block_placement.rs` for the per-item gate.
#[tokio::test]
async fn placing_with_an_empty_hand_places_nothing_and_registers_nothing() {
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
            &MobHandle::default(),
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
    let air_id = resolve_state("minecraft:air");
    assert_ne!(
        stone_id, air_id,
        "the two ids must actually differ for this control to mean anything"
    );

    // No `ContainerClick` for this slot: hotbar slot 0 stays empty, exactly
    // the pre-existing "no inventory model" starting condition.
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

    // An empty hand now changes *nothing*, so there is no post-placement id to
    // wait for — the cell was air before the click and is air after it, and a
    // `wait_for(.. == air)` would resolve before the click was even processed.
    // A second, *real* placement gives the wait something that genuinely
    // transitions; because the stream is ordered, the furnace arriving proves
    // the empty-hand click ahead of it was already processed. Without this the
    // assertions below would race an unprocessed packet and pass vacuously.
    let witness_pos = BlockPos::new(9, 5, 9);
    let furnace_id = resolve_state("minecraft:furnace");
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
    handle
        .send_action(ClientAction::UseItemOn {
            hand: Hand::Main,
            pos: witness_pos,
            face: BlockFace::Up,
            cursor: Vec3f::new(0.5, 0.0, 0.5),
            inside_block: false,
            sequence: 2,
        })
        .expect("send use item on");
    handle
        .wait_for(Duration::from_secs(30), |h| {
            h.block_at(witness_pos) == Some(furnace_id)
        })
        .await
        .expect("witness placement confirmation never arrived");

    assert_eq!(
        handle.block_at(target_pos),
        Some(air_id),
        "an empty hand must place nothing at all (#466)"
    );
    assert_ne!(
        handle.block_at(target_pos),
        Some(stone_id),
        "an empty hand must not fall back to stone (#466)"
    );

    handle.shutdown();
    let _ = handle.join().await;

    let _summary = tokio::time::timeout(Duration::from_secs(10), server_task)
        .await
        .expect("serve_connection task did not finish in time")
        .expect("serve_connection task panicked")
        .expect("serve_connection returned an error");

    assert!(
        block_entities.with(|reg| reg.get(target_pos).is_none()),
        "an empty-hand click must not register any block entity"
    );
}

/// A real client opens a **placed** furnace's screen (`OPEN_SCREEN` +
/// `CONTAINER_SET_CONTENT` + `CONTAINER_SET_DATA`) and loads it with fuel and
/// an ingredient via a real `CONTAINER_CLICK` — closing `docs/block-entities.md`'s
/// third and last named gap. The placement test above deliberately left this
/// open ("nobody can *see* inside a placed furnace... no way to load fuel/
/// ingredient into one at all"); this is the test that closes it, the same
/// way the placement test closed gaps 1 and 2.
#[tokio::test]
async fn real_client_opens_a_placed_furnace_and_loads_it_via_container_click() {
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
            &MobHandle::default(),
        )
        .await
    });

    let (mut handle, _events) = ClientBuilder::new(
        address(),
        profile("FurnaceOpener"),
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

    // Place a furnace at `target_pos`, identical to the placement test above.
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

    handle
        .send_action(ClientAction::UseItemOn {
            hand: Hand::Main,
            pos: target_pos,
            face: BlockFace::Up,
            cursor: Vec3f::new(0.5, 0.0, 0.5),
            inside_block: false,
            sequence: 1,
        })
        .expect("send use item on (place)");
    handle
        .wait_for(Duration::from_secs(30), |h| {
            h.block_at(target_pos) == Some(furnace_id)
        })
        .await
        .expect("placement confirmation never arrived");

    // Right-click the *placed* furnace itself: `apply_use_item_on`'s new
    // branch must recognise the existing block entity's menu and open its
    // screen rather than attempt another placement (nothing about
    // `target_pos`'s block state changes here at all).
    handle
        .send_action(ClientAction::UseItemOn {
            hand: Hand::Main,
            pos: target_pos,
            face: BlockFace::Up,
            cursor: Vec3f::new(0.5, 0.0, 0.5),
            inside_block: false,
            sequence: 2,
        })
        .expect("send use item on (open)");

    handle
        .wait_for(Duration::from_secs(30), |h| h.open_menu().is_some())
        .await
        .expect("furnace screen never opened");

    let opened = handle.open_menu().expect("just waited for Some");
    assert_eq!(
        opened.menu_type.to_string(),
        "minecraft:furnace",
        "the real client must have decoded OPEN_SCREEN's menu id back to minecraft:furnace"
    );
    assert_eq!(
        opened.menu.slot_count(),
        3 + 36,
        "a furnace's own 3 slots plus the standard 27-main + 9-hotbar player tail \
         (CONTAINER_SET_CONTENT's item count is what sizes the client's menu)"
    );

    // Load one iron ore (ingredient, furnace menu slot 0) and one coal
    // (fuel, slot 1) via a real `CONTAINER_CLICK` against the window the
    // server just opened — the same "conjure the exact predicted diff"
    // convention the placement test above already uses for the furnace item
    // itself; there is no "mine ore, collect coal" flow in this test's
    // scope, only the wire path from a client's predicted diff through to
    // the server's own block entity.
    handle
        .send_action(ClientAction::ContainerClick {
            window_id: opened.window_id,
            state_id: 1,
            slot: 0,
            button: 0,
            click_type: ContainerClickType::Pickup,
            changed_slots: vec![
                ContainerSlotChange {
                    slot: 0,
                    item: Some(stack("minecraft:iron_ore", 1)),
                },
                ContainerSlotChange {
                    slot: 1,
                    item: Some(stack("minecraft:coal", 1)),
                },
            ],
            carried_item: None,
        })
        .expect("client still connected");

    // Poll the *server's own* registry, not the client's reconciled menu:
    // `apply_container_clicked` applies the click's diff directly with no
    // confirmation packet sent back (`docs/server-inventory.md`'s
    // established scope, which this landing extends unchanged to non-zero
    // windows), so the server is the authority to observe here.
    let loaded = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let ready = block_entities.with(|reg| match reg.get(target_pos) {
                Some(BlockEntity::Furnace(f)) => {
                    f.input().map(|s| s.item.to_string()) == Some("minecraft:iron_ore".to_string())
                        && f.fuel().map(|s| s.item.to_string()) == Some("minecraft:coal".to_string())
                }
                _ => false,
            });
            if ready {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    assert!(
        loaded.is_ok(),
        "the furnace's own input/fuel slots never reflected the real client's container click"
    );

    let (input, fuel) = block_entities.with(|reg| match reg.get(target_pos) {
        Some(BlockEntity::Furnace(f)) => (f.input().cloned(), f.fuel().cloned()),
        _ => (None, None),
    });
    assert_eq!(input, Some(stack("minecraft:iron_ore", 1)));
    assert_eq!(fuel, Some(stack("minecraft:coal", 1)));

    handle.shutdown();
    let _ = handle.join().await;

    let _summary = tokio::time::timeout(Duration::from_secs(10), server_task)
        .await
        .expect("serve_connection task did not finish in time")
        .expect("serve_connection task panicked")
        .expect("serve_connection returned an error");
}

/// **Control**: right-clicking a placed furnace must not spawn a *second*
/// block entity or overwrite the existing one with a fresh, empty one — the
/// "open, don't place" branch must be a real fork, not a no-op that happens
/// to leave the earlier entry alone by coincidence.
#[tokio::test]
async fn opening_a_furnace_does_not_reset_its_already_loaded_contents() {
    let view_radius = 0;
    let (client_io, server_io) = memory_pair();
    let block_entities = BlockEntityHandle::default();
    let server_block_entities = block_entities.clone();
    let target_pos = BlockPos::new(2, 4, 2);
    server_block_entities.with(|reg| {
        let mut furnace = Furnace::new(FurnaceKind::Furnace);
        furnace.set_input(Some(stack("minecraft:iron_ore", 1)));
        reg.insert(target_pos, BlockEntity::Furnace(furnace));
    });

    let server_task = tokio::spawn(async move {
        let mut conn = Connection::new(server_io);
        serve_connection(
            &mut conn,
            &V770ServerProtocol,
            &AirSource::default(),
            &NoEntities,
            view_radius,
            &server_block_entities,
            &MobHandle::default(),
        )
        .await
    });

    let (mut handle, _events) = ClientBuilder::new(
        address(),
        profile("FurnaceReopener"),
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

    handle
        .send_action(ClientAction::UseItemOn {
            hand: Hand::Main,
            pos: target_pos,
            face: BlockFace::Up,
            cursor: Vec3f::new(0.5, 0.0, 0.5),
            inside_block: false,
            sequence: 1,
        })
        .expect("send use item on (open)");
    handle
        .wait_for(Duration::from_secs(30), |h| h.open_menu().is_some())
        .await
        .expect("furnace screen never opened");

    let opened = handle.open_menu().expect("just waited for Some");
    assert_eq!(
        opened.menu.slot_item(0).map(|item| item.item().to_string()),
        Some("minecraft:iron_ore".to_string()),
        "CONTAINER_SET_CONTENT must report the furnace's real, already-loaded \
         input slot, not an empty freshly-placed one"
    );

    handle.shutdown();
    let _ = handle.join().await;

    let _summary = tokio::time::timeout(Duration::from_secs(10), server_task)
        .await
        .expect("serve_connection task did not finish in time")
        .expect("serve_connection task panicked")
        .expect("serve_connection returned an error");

    let input = block_entities.with(|reg| match reg.get(target_pos) {
        Some(BlockEntity::Furnace(f)) => f.input().cloned(),
        _ => None,
    });
    assert_eq!(
        input,
        Some(stack("minecraft:iron_ore", 1)),
        "opening the furnace must not have reset its input slot"
    );
    assert_eq!(
        block_entities.with(|reg| reg.len()),
        1,
        "opening must not have inserted a second block entity at the same position"
    );
}
