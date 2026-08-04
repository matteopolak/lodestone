//! Hermetic tests for `V770ServerProtocol`'s container-screen encoders
//! (`open_screen`, `container_set_content`, `container_set_slot`,
//! `container_set_data`) — the four packets `docs/block-entities.md` named
//! as its third, previously open gap: "zero hits in
//! `crates/protocol/v770/src/server_protocol.rs`" for any of them.
//!
//! Same shape as `entity_encoders.rs`: decode through the real
//! [`V770Adapter::handle_packet`], not a bespoke reader, so a wrong field
//! order or scale surfaces as a wrong (or failing) decode through *old,
//! independently-verified* code, not just self-consistency
//! (`decode(encode(x)) == x` proves nothing on its own — CLAUDE.md's
//! evidence standard).

use lodestone_core::Nbt;
use lodestone_model::{
    ClientEvent, ConnectionState, Directive, ItemStack, ResourceKey, VersionAdapter,
};
use lodestone_server::{ServerDirective, ServerProtocol};
use lodestone_v770::V770Adapter;
use lodestone_v770::V770ServerProtocol;
use lodestone_world::{
    BiomePatch, BlockEntitySync, ChunkPos, ColumnPatch, LightPatch, LoadedChunk, WorldSink,
};

/// A [`WorldSink`] that ignores every terrain call — these tests only decode
/// container packets, which never touch the world.
#[derive(Default)]
struct NullSink;

impl WorldSink for NullSink {
    fn load(&mut self, _pos: ChunkPos, _chunk: LoadedChunk) {}
    fn merge(&mut self, _pos: ChunkPos, _patch: ColumnPatch) {}
    fn set_block(&mut self, _x: i32, _y: i32, _z: i32, _state: u32) {}
    fn set_blocks(
        &mut self,
        _section_x: i32,
        _section_y: i32,
        _section_z: i32,
        _blocks: &[(u8, u8, u8, u32)],
    ) {
    }
    fn merge_light(&mut self, _pos: ChunkPos, _patch: LightPatch) {}
    fn merge_biomes(&mut self, _pos: ChunkPos, _patch: BiomePatch) {}
    fn unload(&mut self, _pos: ChunkPos) {}
    fn set_block_entity(&mut self, _x: i32, _y: i32, _z: i32, _type_id: u32, _nbt: Nbt) {}
    fn sync_block_entity(
        &mut self,
        _x: i32,
        _y: i32,
        _z: i32,
        _block_entity_type: Option<u32>,
    ) -> BlockEntitySync {
        BlockEntitySync::ChunkAbsent
    }
}

/// Decodes one clientbound packet through the real adapter, returning its
/// emitted [`ClientEvent`]s (panics on anything else, since these packets
/// only ever emit events).
fn decode_events(packet_id: i32, payload: &[u8]) -> Vec<ClientEvent> {
    let adapter = V770Adapter::default();
    let mut sink = NullSink;
    let directives = adapter
        .handle_packet(&mut sink, ConnectionState::Play, packet_id, payload)
        .expect("decodes");
    directives
        .into_iter()
        .map(|d| match d {
            Directive::Emit(event) => event,
            other => panic!("expected only Emit directives, got {other:?}"),
        })
        .collect()
}

fn stack(name: &str, count: u32) -> ItemStack {
    ItemStack::new(
        ResourceKey::new("minecraft", name).expect("static key is valid"),
        count,
    )
}

/// Reduces a decoded stack to `(item key, count)`, dropping components —
/// see `encode_container_content_round_trips_furnace_slots_plus_player_inventory`'s
/// comment for why these tests compare this rather than the full
/// `ItemStack`.
fn id_and_count(item: &Option<ItemStack>) -> Option<(String, u32)> {
    item.as_ref().map(|s| (s.item.to_string(), s.count))
}

/// Shorthand so a call site can write `some("minecraft:iron_ore", 1)`
/// instead of `Some(("minecraft:iron_ore".to_string(), 1))`.
fn some(name: &str, count: u32) -> Option<(String, u32)> {
    Some((name.to_string(), count))
}

#[test]
fn encode_open_screen_round_trips_through_the_real_adapter() {
    let proto = V770ServerProtocol;
    let ServerDirective::Send { packet_id, payload } =
        proto.encode_open_screen(3, "minecraft:furnace", "Furnace")
    else {
        panic!("expected a Send directive");
    };

    let events = decode_events(packet_id, &payload);
    assert_eq!(events.len(), 1);
    let ClientEvent::ScreenOpened {
        window_id,
        menu_type,
        title,
    } = &events[0]
    else {
        panic!("expected ScreenOpened, got {:?}", events[0]);
    };
    assert_eq!(*window_id, 3);
    assert_eq!(menu_type.to_string(), "minecraft:furnace");
    assert_eq!(title.to_plain_string(), "Furnace");
}

/// **Control**: an unrecognised menu key must not produce a packet at all —
/// the malformed-input case, proving the lookup guard is real rather than
/// something that happens to never miss.
#[test]
fn encode_open_screen_emits_nothing_for_an_unknown_menu_key() {
    let proto = V770ServerProtocol;
    assert_eq!(
        proto.encode_open_screen(1, "minecraft:not_a_real_menu", "x"),
        ServerDirective::None
    );
}

/// A furnace-sized (`3` own slots) content payload, with the player's main
/// storage and hotbar rows appended per vanilla's own
/// `addStandardInventorySlots` layout — pins the exact menu-index boundary
/// `crate::inventory::container_menu_slot` (the click-side counterpart, in
/// `lodestone-server`) also assumes.
#[test]
fn encode_container_content_round_trips_furnace_slots_plus_player_inventory() {
    let proto = V770ServerProtocol;
    let mut items: Vec<Option<ItemStack>> = vec![None; 3 + 36];
    items[0] = Some(stack("iron_ore", 1));
    items[1] = Some(stack("coal", 1));
    // Menu slot 12 = container_size(3) + main-storage offset 9 -> player's
    // own native main-storage slot 9.
    items[12] = Some(stack("stone", 5));
    let carried = stack("diamond", 1);

    let ServerDirective::Send { packet_id, payload } =
        proto.encode_container_content(5, 7, &items, Some(&carried))
    else {
        panic!("expected a Send directive");
    };

    let events = decode_events(packet_id, &payload);
    assert_eq!(events.len(), 1);
    let ClientEvent::ContainerContent {
        window_id,
        state_id,
        items: decoded,
        carried_item,
    } = &events[0]
    else {
        panic!("expected ContainerContent, got {:?}", events[0]);
    };
    assert_eq!(*window_id, 5);
    assert_eq!(*state_id, 7);
    assert_eq!(decoded.len(), 39);
    // Compared by (item key, count) only, not full `ItemStack` equality: the
    // real decoder (`read_component_patch`) seeds `max_stack_size`/
    // `max_damage`/`equippable` from the item's *prototype* even for an
    // empty patch (see that function's own doc comment) — a real, deliberate
    // repo behaviour, not something this test is about. Asserting full
    // equality against a bare `ItemStack::new` would fail on that seeded
    // metadata, not on anything this encoder got wrong.
    assert_eq!(id_and_count(&decoded[0]), some("minecraft:iron_ore", 1));
    assert_eq!(id_and_count(&decoded[1]), some("minecraft:coal", 1));
    assert_eq!(id_and_count(&decoded[2]), None, "empty output slot must decode as None, not a default item");
    assert_eq!(id_and_count(&decoded[12]), some("minecraft:stone", 5));
    assert_eq!(id_and_count(carried_item), some("minecraft:diamond", 1));
}

#[test]
fn encode_container_slot_round_trips_one_changed_slot() {
    let proto = V770ServerProtocol;
    let item = stack("iron_ingot", 1);
    let ServerDirective::Send { packet_id, payload } = proto.encode_container_slot(5, 8, 2, Some(&item))
    else {
        panic!("expected a Send directive");
    };

    let events = decode_events(packet_id, &payload);
    assert_eq!(events.len(), 1);
    let ClientEvent::ContainerSlot {
        window_id,
        state_id,
        slot,
        item: decoded,
    } = &events[0]
    else {
        panic!("expected ContainerSlot, got {:?}", events[0]);
    };
    assert_eq!(*window_id, 5);
    assert_eq!(*state_id, 8);
    assert_eq!(*slot, 2);
    // See `encode_container_content_round_trips_furnace_slots_plus_player_inventory`'s
    // comment for why this compares (item, count) rather than the full
    // `ItemStack`.
    assert_eq!(id_and_count(decoded), some("minecraft:iron_ingot", 1));
}

/// **Control**: clearing a slot (the shape a furnace's output emptying out
/// via a real click needs) must decode back to `None`, not some stale or
/// default stack.
#[test]
fn encode_container_slot_can_clear_a_slot() {
    let proto = V770ServerProtocol;
    let ServerDirective::Send { packet_id, payload } = proto.encode_container_slot(5, 9, 0, None) else {
        panic!("expected a Send directive");
    };

    let events = decode_events(packet_id, &payload);
    assert_eq!(events.len(), 1);
    let ClientEvent::ContainerSlot { item: decoded, .. } = &events[0] else {
        panic!("expected ContainerSlot, got {:?}", events[0]);
    };
    assert_eq!(*decoded, None);
}

/// Pins `container_set_data`'s field order against a furnace's own property
/// table (`Furnace::container_data`'s doc comment: index `3` is
/// `cookingTotalTime`, `200` for a smelted iron ore) — the same index the
/// client's `Menus::opened_data()` (issue `3b2bcc5`) stores generically by
/// property id, so a correct index here is what makes a real furnace's
/// progress arrow eventually readable.
#[test]
fn encode_container_data_round_trips_a_furnace_property() {
    let proto = V770ServerProtocol;
    let ServerDirective::Send { packet_id, payload } = proto.encode_container_data(5, 3, 200) else {
        panic!("expected a Send directive");
    };

    let events = decode_events(packet_id, &payload);
    assert_eq!(events.len(), 1);
    let ClientEvent::ContainerData {
        window_id,
        property,
        value,
    } = &events[0]
    else {
        panic!("expected ContainerData, got {:?}", events[0]);
    };
    assert_eq!(*window_id, 5);
    assert_eq!(*property, 3);
    assert_eq!(*value, 200);
}
