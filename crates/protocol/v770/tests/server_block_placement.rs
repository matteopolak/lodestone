//! End-to-end gate for #466: **a real client places a real block, and the
//! server's own world holds the block the player was holding** — not
//! `minecraft:stone`.
//!
//! # Why this reads the server's `ChunkSource` and not the client
//!
//! The client predicts placement locally and predicted it *correctly*
//! throughout the bug, so a gate that reads `handle.block_at(..)` passes
//! against the defect. That is precisely how #466 survived: three things hid
//! it, and one of them is that the wire was fully connected and carrying the
//! wrong value — the failure `cargo xtask connectedness` structurally cannot
//! see. So the assertion here is on the **server's own** `ChunkSource`, held
//! through a shared handle the test keeps its own clone of, read after the
//! connection has closed.
//!
//! # Why per item, and why these items
//!
//! "A block was placed" passes against the bug, since `minecraft:stone` is a
//! block — the *magnitude* species of vacuous test, right subject, predicate
//! too weak. Every assertion here predicts the exact block name.
//!
//! The item list is chosen so it cannot be satisfied by a weaker
//! implementation than the real census:
//!
//! * `dirt`, `oak_planks`, `white_wool`, `glass` — ordinary blocks, the whole
//!   subject of #466. All four placed stone before the fix.
//! * `redstone` — places `minecraft:redstone_wire`, a **different name**. A
//!   name-equality resolver (`item_name == block_name`) fails this row, and
//!   this is also the first half of #465: dust could not be placed at all.
//! * `diamond_sword` — places **nothing**. Both directions matter: the
//!   fallback for a non-placeable item must leave the world untouched, and
//!   must specifically not be stone.
//!
//! # Scope this deliberately does not assert
//!
//! Block *state* is out of scope for #466 (see `apply_use_item_on`'s own doc
//! comment): the server writes each block's bare name, so redstone dust lands
//! without its `power`/connection properties and a log without its `axis`.
//! These assertions are on the block, which is what the issue is about.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestone_client::{BlockPos, ClientBuilder, Hand, LoginProfile, ServerAddress};
use lodestone_model::{BlockFace, ClientAction, ContainerClickType, ContainerSlotChange, GameMode, ItemStack, Vec3f};
use lodestone_net::{Connection, memory_pair};
use lodestone_server::{
    BlockEntityHandle, ChunkColumn, ChunkSource, MobHandle, NoEntities, serve_connection,
};
use lodestone_v770::{V770ServerProtocol, adapter};

/// An all-air column with real edit retention, **shared** with the test.
///
/// The `Arc` is the point: `block_entities_live.rs`'s own `AirSource` is moved
/// wholesale into the server task and is therefore unreadable afterwards, so
/// that file can only assert on the block-entity registry and the client's
/// view. This one hands the test a second handle onto the very same map, which
/// is what makes "assert the server's own state" possible at all.
///
/// The `set_block`/`block_state` overrides are load-bearing, not boilerplate:
/// `ChunkSource::set_block`'s default is a documented no-op, so a unit-struct
/// source silently discards every placement and each assertion below would
/// read air and fail for a reason unrelated to the code under test.
#[derive(Clone, Default)]
struct SharedAirSource {
    edits: Arc<Mutex<HashMap<(i32, i32, i32), String>>>,
}

impl SharedAirSource {
    /// Every edit the server actually wrote, as `(pos, block name)`.
    fn edits(&self) -> HashMap<(i32, i32, i32), String> {
        self.edits.lock().expect("edits lock poisoned").clone()
    }
}

impl ChunkSource for SharedAirSource {
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

fn stack(name: &str) -> ItemStack {
    ItemStack::new(name.parse().expect("valid resource key"), 1)
}

/// What each hotbar slot holds, where it clicks, and what the server must
/// end up with there. `None` means "this item places nothing".
///
/// Positions all sit inside chunk `(0, 0)` (blocks `0..=15` on each axis) and
/// inside the 16-block column, because `view_radius = 0` means no other chunk
/// is ever sent — a negative coordinate here would floor-divide into a
/// never-loaded chunk and the client would never see a confirmation.
const PLACEMENTS: &[(u8, &str, (i32, i32, i32), Option<&str>)] = &[
    (0, "minecraft:dirt", (2, 5, 2), Some("minecraft:dirt")),
    (1, "minecraft:oak_planks", (4, 5, 2), Some("minecraft:oak_planks")),
    // Neither placeable-by-name nor a block entity: the row that fails
    // against a name-equality resolver.
    (2, "minecraft:redstone", (6, 5, 2), Some("minecraft:redstone_wire")),
    // The negative direction, placed in the middle of the run rather than
    // last so the wait below still proves the whole sequence was processed.
    (3, "minecraft:diamond_sword", (8, 5, 2), None),
    (4, "minecraft:white_wool", (10, 5, 2), Some("minecraft:white_wool")),
    (5, "minecraft:glass", (12, 5, 2), Some("minecraft:glass")),
];

#[tokio::test]
async fn every_held_item_places_its_own_block_in_the_servers_own_world() {
    let view_radius = 0;
    let (client_io, server_io) = memory_pair();
    let source = SharedAirSource::default();
    let server_source = source.clone();

    let server_task = tokio::spawn(async move {
        let mut conn = Connection::new(server_io);
        serve_connection(
            &mut conn,
            &V770ServerProtocol,
            &server_source,
            &NoEntities,
            view_radius,
            &BlockEntityHandle::default(),
            &MobHandle::default(),
        )
        .await
    });

    let (mut handle, _events) =
        ClientBuilder::new(address(), profile("BlockPlacer"), Box::new(adapter())).connect_with(client_io);

    handle
        .wait_for_spawn(Duration::from_secs(30))
        .await
        .expect("client never spawned");
    handle
        .wait_for_chunks(1, Duration::from_secs(30))
        .await
        .expect("initial column never arrived");

    // Load the hotbar. Menu slots 36..=44 are native hotbar slots 0..=8 —
    // `PlayerInventory`'s own menu-slot table (`docs/server-inventory.md`).
    handle
        .send_action(ClientAction::ChangeGameMode {
            mode: GameMode::Creative,
        })
        .expect("client still connected");
    for (slot, item, _, _) in PLACEMENTS {
        let menu_slot = 36 + i32::from(*slot);
        handle
            .send_action(ClientAction::SetCreativeModeSlot {
                slot: menu_slot,
                item: Some(stack(item)),
            })
            .expect("client still connected");
    }

    // Select each slot in turn and click. The stream is ordered and the
    // server applies it in order, so slot selection always precedes the
    // placement it governs.
    for (slot, _, (x, y, z), _) in PLACEMENTS {
        handle
            .send_action(ClientAction::SetCarriedItem {
                slot: i32::from(*slot),
            })
            .expect("client still connected");
        handle
            .send_action(ClientAction::UseItemOn {
                hand: Hand::Main,
                pos: BlockPos::new(*x, *y, *z),
                face: BlockFace::Up,
                cursor: Vec3f::new(0.5, 0.0, 0.5),
                inside_block: false,
                sequence: 1,
            })
            .expect("send use item on");
    }

    // Wait for the *last* placement to be confirmed before tearing down.
    // Because the connection is ordered, the last block arriving proves every
    // earlier packet — including the sword, mid-sequence — has been processed,
    // which is what makes the "placed nothing" assertion meaningful rather
    // than a race with an unprocessed packet.
    let (_, _, (lx, ly, lz), last_expected) = PLACEMENTS[PLACEMENTS.len() - 1];
    let last_pos = BlockPos::new(lx, ly, lz);
    let last_block = last_expected.expect("the final row must be a placeable item");
    //
    // The timeout is *recorded* rather than `expect`ed, and asserted last.
    // If the server resolves the wrong block, this wait can only ever time
    // out, and an `expect` here would report "confirmation never arrived" —
    // burying the actual diagnosis. Letting the per-item assertions below run
    // first makes the failure say *which item placed what*, which is the
    // whole point of the gate. Observed while running #466's own negative
    // control: with the pre-fix resolver restored, `expect` reported only
    // `Timeout`, while this ordering reports `holding minecraft:dirt must
    // place minecraft:dirt, server wrote "minecraft:stone"`.
    let confirmed = handle
        .wait_for(Duration::from_secs(30), |h| {
            h.block_at(last_pos).is_some_and(|id| {
                lodestone_data::block_states::block_name(id) == Some(last_block)
            })
        })
        .await
        .is_ok();

    handle.shutdown();
    let _ = handle.join().await;
    let _summary = tokio::time::timeout(Duration::from_secs(10), server_task)
        .await
        .expect("serve_connection task did not finish in time")
        .expect("serve_connection task panicked")
        .expect("serve_connection returned an error");

    // ---- the assertions this test exists for: the SERVER's own world ----
    let edits = source.edits();

    for (_, item, pos, expected) in PLACEMENTS {
        let actual = source.block_state(pos.0, pos.1, pos.2);
        match expected {
            Some(block) => {
                assert_eq!(
                    actual, *block,
                    "holding {item} must place {block} at {pos:?}, server wrote {actual:?}"
                );
            }
            None => {
                assert_eq!(
                    actual, "minecraft:air",
                    "holding {item} must place nothing at {pos:?}, server wrote {actual:?}"
                );
                // Named separately from the air check: "not stone" is the
                // specific regression #466 is about, and an assertion that
                // only said `== air` would not say *why* it matters.
                assert_ne!(
                    actual, "minecraft:stone",
                    "a non-placeable item must never fall back to stone ({item})"
                );
            }
        }
    }

    // No stray writes anywhere: exactly one edit per placeable row, and the
    // sword's cell was never touched at all. Without this, a resolver that
    // additionally wrote stone somewhere would still pass every check above.
    let expected_edits = PLACEMENTS.iter().filter(|(_, _, _, e)| e.is_some()).count();
    assert_eq!(
        edits.len(),
        expected_edits,
        "server wrote {} blocks, expected exactly {expected_edits}: {edits:?}",
        edits.len()
    );
    assert!(
        !edits.values().any(|block| block == "minecraft:stone"),
        "no placement in this test holds a stone item, so the server must never write stone: {edits:?}"
    );

    // Asserted last, for the reason given at the wait itself: the server must
    // also have told the *client* about the final placement. This is the
    // wire-reached-the-player half, kept separate from the server-state half
    // above so a failure names which one broke.
    assert!(
        confirmed,
        "the server never confirmed the final placement ({last_block}) to the client"
    );
}
