//! **The join inventory snapshot**: a rejoining player is told what they are
//! holding, without having to touch a slot first.
//!
//! # The defect this guards
//!
//! Reported as *"when I rejoin the server my inventory is empty, but if I
//! shift-click something then all the items pop in"*. The items were never lost —
//! `PlayerData::to_inventory` restored them before `serve_play`'s first line — and
//! the encoder (`ServerProtocol::encode_container_content`) and this crate's
//! `V770ServerProtocol` implementation of it were both complete and byte-gated in
//! `container_encoders.rs`. What was missing was a **producer on the join path**:
//! every `container_set_content` send in `lodestone-server` was reactive (a menu was
//! opened, a click disagreed, a recipe was placed), so the client kept its
//! fresh-`Menu` default until the first click produced a disagreement and the
//! corrective resync flushed all 46 slots at once. That resync *is* the "pop in".
//!
//! # Where the expected values come from
//!
//! Not from our own encoder, and not from `MenuLayout::player()` — from the 26.2
//! decompile, read as record definitions:
//!
//! | claim | vanilla source |
//! |---|---|
//! | the packet is `container_set_content`, not `set_player_inventory` | `AbstractContainerMenu::sendAllDataToRemote` → `ServerPlayer`'s `ContainerSynchronizer::sendInitialData`, which constructs `ClientboundContainerSetContentPacket` |
//! | window id `0` | `ClientPacketListener.handleContainerContent`'s `containerId == 0` arm routes to `player.inventoryMenu` |
//! | 46 slots, and which index is which | `InventoryMenu`: result `0`, 2×2 grid `1..=4`, armour `5..=8` **head→feet**, main storage `9..=35`, hotbar `36..=44`, off-hand `45` |
//! | state id `1` on the first send | `sendInitialData` passes `container.incrementStateId()`, and `incrementStateId` is `(stateId + 1) & 32767` from a `0` start |
//! | it is sent last on the join | `PlayerList.placeNewPlayer` calls `initInventoryMenu()` after the teleport, the player-info adds and `sendLevelInfo` |
//!
//! `ClientboundSetPlayerInventoryPacket` is the packet this is *not*: it is a
//! single-slot record, `(int slot, ItemStack contents)`, whose only vanilla producer
//! is `Inventory.createInventoryUpdatePacket` acknowledging one pickup. It carries
//! no slot list and no cursor, so it cannot express a snapshot at all.
//!
//! # Why the inventory is seeded through the real player store
//!
//! A gate that asserted only "a `container_set_content` arrived" would pass against
//! one carrying 46 empty slots — which is byte-for-byte the buggy client's own view,
//! and therefore indistinguishable from the defect. So the fixture writes a real
//! player `.dat` through [`lodestone_server::player_data::PlayerDataStore`] before
//! the client logs in, and the assertion is on the **contents**. That also makes the
//! path under test the production one: the store is reached through
//! `ChunkSource::world_registries`, exactly as `serve_play` reaches it.
//!
//! The seeded slots are chosen to be discriminating rather than convenient — see
//! [`SEEDED`].

use std::time::Duration;

use lodestone_core::Writer;
use lodestone_model::{
    ClientEvent, ConnectionState, Directive, ItemStack, ResourceKey, VersionAdapter,
};
use lodestone_net::{Connection, Transport};
use lodestone_server::{ChunkColumn, ChunkSource, IntegratedServer};
use lodestone_v770::packet_ids::{configuration, login, play};
use lodestone_v770::{V770Adapter, V770ServerProtocol};
use lodestone_world::World;
use uuid::Uuid;

mod common;
use common::unique_username;

const MIN_Y: i32 = -64;
const HEIGHT: i32 = 384;

/// `InventoryMenu`'s slot count — result + 2×2 grid + 4 armour + 27 main + 9 hotbar +
/// off-hand. Written as the sum rather than as `46` so the arithmetic is the
/// assertion's own justification.
const MENU_SLOTS: usize = 1 + 4 + 4 + 27 + 9 + 1;

/// The `stateId` the first content packet for a menu carries — `incrementStateId`
/// from a `0` start. **`1`, not `0`**: the increment happens before the send.
const FIRST_STATE_ID: i32 = 1;

/// `(native slot, expected menu slot, item, count)`.
///
/// # Why these slots and these counts
///
/// Every entry exists to make a specific plausible bug fail:
///
/// * **native `0` → menu `36`** and **native `9` → menu `9`** together pin the
///   hotbar's `+36` offset. A fixture using only main storage (where the mapping is
///   the identity) would pass with the offset dropped entirely.
/// * **native `4` → menu `40`** is a second, non-adjacent hotbar entry, so an
///   off-by-one in the offset separates from a wholesale omission.
/// * **native `39` → menu `5`** (head) and **native `36` → menu `8`** (feet) pin the
///   armour block's **reversal**. This is the mapping most likely to be transcribed
///   forwards; with `5..=8` written feet→head instead, the helmet and the boots
///   swap and nothing else in the packet moves.
/// * **native `40` → menu `45`** is the off-hand, the one slot no non-player menu
///   exposes.
///
/// Counts are pairwise distinct (`1, 2, 3, 5, 7, 11, 13`) and so are the items. Two
/// slots holding the same stack transpose byte-perfectly through any encoder, so
/// equal values would make the gate pass under a permutation of the layout — the
/// same trap that made asymmetric arguments necessary in
/// `server_take_item_entity.rs`.
const SEEDED: [(usize, usize, &str, u32); 7] = [
    (0, 36, "minecraft:stone", 1),
    (4, 40, "minecraft:oak_planks", 2),
    (9, 9, "minecraft:diamond", 3),
    (22, 22, "minecraft:iron_ingot", 5),
    (36, 8, "minecraft:diamond_boots", 7),
    (39, 5, "minecraft:iron_helmet", 11),
    (40, 45, "minecraft:shield", 13),
];

/// Flat, cheap, deterministic terrain. The subject is a packet on the join path, so
/// the world only has to exist.
#[derive(Debug)]
struct FlatWorld;

impl ChunkSource for FlatWorld {
    fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
        let mut column = ChunkColumn::new(MIN_Y, HEIGHT);
        for z in 0..16 {
            for x in 0..16 {
                column.set_block(x, 63, z, "minecraft:grass_block[snowy=false]");
            }
        }
        column
    }

    fn block_state(&self, x: i32, y: i32, z: i32) -> String {
        self.column(x.div_euclid(16), y)
            .block_state(x.rem_euclid(16), y, z.rem_euclid(16))
            .to_string()
    }

    fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {}
}

fn key(s: &str) -> ResourceKey {
    s.parse().expect("valid resource key")
}

fn handshake_bytes() -> Vec<u8> {
    let mut w = Writer::default();
    w.var_i32(776);
    w.string("localhost");
    w.u16(25565);
    w.var_i32(2);
    w.into_vec()
}

fn hello_bytes(name: &str, uuid: Uuid) -> Vec<u8> {
    let mut w = Writer::default();
    w.string(name);
    w.uuid(uuid);
    w.into_vec()
}

/// Everything the server says until it goes quiet.
async fn drain<T: Transport>(client: &mut Connection<T>) -> Vec<(i32, Vec<u8>)> {
    const QUIET: Duration = Duration::from_millis(400);
    let mut out = Vec::new();
    while let Ok(Ok(Some(packet))) = tokio::time::timeout(QUIET, client.read_packet()).await {
        out.push(packet);
    }
    out
}

/// Drives login and configuration, returning the whole Play-phase join burst.
async fn join<T: Transport>(
    client: &mut Connection<T>,
    name: &str,
    uuid: Uuid,
) -> Vec<(i32, Vec<u8>)> {
    client.write_packet(0, &handshake_bytes()).await.unwrap();
    client.write_packet(0, &hello_bytes(name, uuid)).await.unwrap();
    let _ = common::read_login_packet(client).await;
    client
        .write_packet(login::serverbound::LOGIN_ACKNOWLEDGED, &[])
        .await
        .unwrap();
    let _ = common::read_login_packet(client).await;
    client
        .write_packet(configuration::serverbound::FINISH_CONFIGURATION, &[])
        .await
        .unwrap();
    drain(client).await
}

/// Decodes one `container_set_content` payload through the **real client adapter**,
/// returning `(window, state, items, carried)`.
///
/// Through `V770Adapter` rather than a hand-rolled reader on purpose: the decoder
/// predates the encoder (it is what the encoder was written against), so this is two
/// independent transcriptions agreeing rather than a self-round-trip. It also proves
/// the packet our server emits is one our own client accepts with zero trailing
/// bytes, which a hand reader would not.
fn decode_content(payload: &[u8]) -> (i32, i32, Vec<Option<ItemStack>>, Option<ItemStack>) {
    let directives = V770Adapter::new()
        .handle_packet(
            &mut World::new(),
            ConnectionState::Play,
            play::clientbound::CONTAINER_SET_CONTENT,
            payload,
        )
        .expect("the server's own container_set_content must decode");
    match directives.as_slice() {
        [
            Directive::Emit(ClientEvent::ContainerContent {
                window_id,
                state_id,
                items,
                carried_item,
            }),
        ] => (*window_id, *state_id, items.clone(), carried_item.clone()),
        other => panic!("expected a ContainerContent event, got {other:?}"),
    }
}

fn tempdir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("lodestone-join-inv-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch world dir");
    dir
}

/// A player whose saved inventory holds seven distinct stacks is sent all of them,
/// in `InventoryMenu` order, as a window-`0` `container_set_content` on join —
/// before sending a single Play packet of their own.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_rejoining_players_saved_inventory_arrives_as_a_window_zero_snapshot() {
    let dir = tempdir("snapshot");
    let (_server, client_io, world) = IntegratedServer::open_persistent_with_mobs(
        V770ServerProtocol,
        &dir,
        FlatWorld,
        MIN_Y,
        HEIGHT,
        (0..=0, 0..=0),
        (0, 0),
        0,
        0,
        Duration::from_secs(3600),
    )
    .expect("open persistent world");

    // The store the *connection* path reaches, not a second one rooted by hand —
    // `serve_play` reads it through this same accessor. A store built separately
    // here could be written to and never consulted, which is the island shape.
    let store = world
        .world_registries()
        .expect("a persistent source answers Some")
        .player_data
        .expect("a persistent world exposes its player store");

    let name = unique_username();
    let uuid = Uuid::new_v4();

    let mut saved = lodestone_server::player_data::PlayerData {
        pos: lodestone_model::Vec3::new(0.5, 64.0, 0.5),
        ..Default::default()
    };
    for (native, _, item, count) in SEEDED {
        saved.inventory[native] = Some(ItemStack::new(key(item), count));
    }
    store.write(uuid, &saved).expect("seed the player file");

    // Nothing but login/configuration is sent — no click, no movement, no slot
    // change. Anything that arrives is unprompted join traffic.
    let joined = join(&mut client_io_conn(client_io), &name, uuid).await;

    let contents: Vec<&Vec<u8>> = joined
        .iter()
        .filter(|(id, _)| *id == play::clientbound::CONTAINER_SET_CONTENT)
        .map(|(_, payload)| payload)
        .collect();
    assert_eq!(
        contents.len(),
        1,
        "the join must carry exactly one container_set_content — none is the \
         reported bug (the client keeps its empty default until a click forces a \
         resync); more than one means a second producer is racing the snapshot"
    );

    let (window, state, items, carried) = decode_content(contents[0]);
    assert_eq!(
        window, 0,
        "the player's own inventory is window 0 — handleContainerContent routes \
         only containerId 0 to player.inventoryMenu"
    );
    assert_eq!(
        items.len(),
        MENU_SLOTS,
        "InventoryMenu has 1 result + 4 grid + 4 armour + 27 main + 9 hotbar + 1 \
         off-hand slots; a short list means the layout dropped a block"
    );
    assert_eq!(
        carried, None,
        "the cursor is empty on join: the container-close path returns a carried \
         stack to the inventory or the floor, so nothing survives a disconnect held"
    );
    assert_eq!(
        state, FIRST_STATE_ID,
        "sendInitialData passes incrementStateId(), which is (0 + 1) & 32767 — the \
         first content packet a real client sees carries 1, never 0"
    );

    // Collected rather than asserted inside the loop: an `assert!` in a `for`
    // proves one arm and leaves the rest as an argument. A wrong armour mapping
    // moves two slots at once, and this reports both.
    let mut wrong = Vec::new();
    for (native, menu, item, count) in SEEDED {
        let got = items[menu].as_ref();
        let ok = got.is_some_and(|s| s.item == key(item) && s.count == count);
        if !ok {
            wrong.push(format!(
                "menu slot {menu} (native {native}) should hold {count}×{item}, got {got:?}"
            ));
        }
    }
    assert!(
        wrong.is_empty(),
        "the snapshot must mirror the saved inventory in InventoryMenu order:\n{}",
        wrong.join("\n")
    );

    // The complement, and it is what makes the assertion above a measurement of
    // *ordering* rather than of mere presence: every slot the fixture did not seed
    // must be empty. Without this a snapshot that filled all 46 slots with the
    // right seven stacks *plus* duplicates elsewhere would pass.
    let seeded_menu: Vec<usize> = SEEDED.iter().map(|(_, menu, _, _)| *menu).collect();
    let unexpected: Vec<usize> = (0..MENU_SLOTS)
        .filter(|slot| !seeded_menu.contains(slot) && items[*slot].is_some())
        .collect();
    assert!(
        unexpected.is_empty(),
        "these menu slots should be empty but are not: {unexpected:?} — a stack in \
         an unseeded slot means the native→menu mapping is off, not that the \
         inventory gained items"
    );
}

/// Wraps the constructor's client end in a `Connection`.
///
/// A free function rather than inline so the test body reads as a sequence of
/// protocol steps; `open_persistent_with_mobs` hands back the raw `DuplexStream`.
fn client_io_conn(io: tokio::io::DuplexStream) -> Connection<tokio::io::DuplexStream> {
    Connection::new(io)
}
