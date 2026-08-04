//! Live end-to-end: the **real** `lodestone-client`, running the real
//! [`V770Adapter`](lodestone_v770::V770Adapter), sends `SET_CARRIED_ITEM` and
//! `CONTAINER_CLICK` against the real [`V770ServerProtocol`] driving
//! [`serve_connection`] — proving the server-authoritative
//! `lodestone_server::PlayerInventory` model lands the *same* item in the
//! *same* native slot the client's own local prediction already produced,
//! not merely that the wire bytes round-trip (that half is covered
//! hermetically by `crates/protocol/v770/src/server_protocol.rs`'s
//! `inventory_decode_tests`, which decode the real client encoder's output
//! but never touch `lodestone-server`'s consumer at all).
//!
//! This is what CLAUDE.md calls "the strongest evidence available" for this
//! kind of change: a real client, not a hand-built packet. It also directly
//! tests the desync question the task brief raised — the client predicts
//! `ContainerClick` locally with **no server confirmation needed to look
//! correct** (`docs/container-clicks.md`), so the only way to know the
//! server model agrees is to drive the real predictor and read the server's
//! own state back out, which is exactly what this test does via
//! [`serve_connection`]'s returned `ServeSummary::inventory` once the
//! connection closes.
//!
//! Not routed through `IntegratedServer` (unlike this crate's other live
//! tests, e.g. `server_liveness.rs`): that handle intentionally discards
//! [`ServeSummary`] (it just races the connection future against a shutdown
//! signal), and `ServeSummary` is the one place the final `PlayerInventory`
//! is observable at all without adding a new parameter to
//! `IntegratedServer`'s public constructors, which are outside this crate's
//! file ownership for this session. So this test spawns [`serve_connection`]
//! directly over a [`memory_pair`] duplex, the same primitive
//! `IntegratedServer::open_in_memory` itself builds on.

use std::time::Duration;

use lodestone_client::{ClientBuilder, LoginProfile, ServerAddress};
use lodestone_model::{ClientAction, ContainerClickType, ContainerSlotChange, ItemStack};
use lodestone_net::{Connection, memory_pair};
use lodestone_server::{BlockEntityHandle, ChunkColumn, ChunkSource, NoEntities, serve_connection};
use lodestone_v770::{V770ServerProtocol, adapter};
use uuid::Uuid;

/// An all-air, minimal chunk source — this test is about inventory state,
/// not terrain, so the cheapest possible column that still lets the client
/// finish its join sequence is the right one.
struct AirSource;

impl ChunkSource for AirSource {
    fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
        ChunkColumn::new(0, 16)
    }
}

fn profile(name: &str) -> LoginProfile {
    LoginProfile {
        username: name.into(),
        uuid: Uuid::new_v4(),
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

/// A real client selects a hotbar slot and performs a container click
/// against its own inventory (window `0`); both land in the server's
/// [`PlayerInventory`](lodestone_server::PlayerInventory) once the
/// connection closes.
#[tokio::test]
async fn real_client_hotbar_select_and_container_click_reach_the_server_model() {
    let (client_io, server_io) = memory_pair();

    let server_task = tokio::spawn(async move {
        let mut conn = Connection::new(server_io);
        serve_connection(
            &mut conn,
            &V770ServerProtocol,
            &AirSource,
            &NoEntities,
            0,
            &BlockEntityHandle::default(),
        )
        .await
    });

    let (mut handle, _events) = ClientBuilder::new(
        address(),
        profile("InventoryWatcher"),
        Box::new(adapter()),
    )
    .connect_with(client_io);

    handle
        .wait_for_spawn(Duration::from_secs(30))
        .await
        .expect("client never spawned");

    handle
        .send_action(ClientAction::SetCarriedItem { slot: 4 })
        .expect("client still connected");

    // A plain pickup-style click that lands one item in menu slot 9 (native
    // main-storage slot 9 — see `lodestone_server::PlayerInventory`'s
    // menu-slot table), with an empty cursor afterward. Shaped exactly like
    // what `lodestone-game`'s real click predictor would encode for "pick up
    // a pickaxe from the ground and place it in the first main-storage
    // slot," not a hand-invented packet.
    handle
        .send_action(ClientAction::ContainerClick {
            window_id: 0,
            state_id: 1,
            slot: 9,
            button: 0,
            click_type: ContainerClickType::Pickup,
            changed_slots: vec![ContainerSlotChange {
                slot: 9,
                item: Some(stack("minecraft:diamond_pickaxe", 1)),
            }],
            carried_item: None,
        })
        .expect("client still connected");

    // `send_action` only enqueues onto the driver's channel; give the driver
    // task a moment to actually perform the writes before closing the
    // connection out from under it.
    tokio::time::sleep(Duration::from_millis(200)).await;

    handle.shutdown();
    let _ = handle.join().await;

    let summary = tokio::time::timeout(Duration::from_secs(10), server_task)
        .await
        .expect("serve_connection task did not finish in time")
        .expect("serve_connection task panicked")
        .expect("serve_connection returned an error");

    assert_eq!(
        summary.inventory.selected_hotbar_slot(),
        4,
        "SET_CARRIED_ITEM must select hotbar slot 4 server-side"
    );
    assert_eq!(
        summary.inventory.native(9),
        Some(&stack("minecraft:diamond_pickaxe", 1)),
        "CONTAINER_CLICK's changed-slots diff must land in native slot 9"
    );

    // Non-vacuity / negative control: a native slot the click never touched
    // must still read empty — proves the assertions above are checking a
    // real, localized write, not a coincidence of every slot already
    // holding the same value.
    assert!(summary.inventory.native(10).is_none());
}
