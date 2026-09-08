//! External packet controls for unknown positive item ids in recipe sync.
//!
//! These packets deliberately use an item id outside the generated census. A
//! decoder that resolves every recipe item immediately, drops unknown values,
//! or silently substitutes air can still pass the ordinary recipe fixtures,
//! which use only canonical ids.

use lodestone_core::Nbt;
use lodestone_model::event::ClientEvent;
use lodestone_model::{ConnectionState, Directive, ItemId, VersionAdapter};
use lodestone_v26_2::packet_ids::play;
use lodestone_world::{
    BiomePatch, BlockEntitySync, ChunkPos, ColumnPatch, LightPatch, LoadedChunk, WorldSink,
};

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

fn decode(packet_id: i32, payload: &[u8]) -> Vec<ClientEvent> {
    let adapter = lodestone_v26_2::adapter();
    let mut sink = NullSink;
    adapter
        .handle_packet(&mut sink, ConnectionState::Play, packet_id, payload)
        .expect("recipe packet with an unknown positive item id must decode")
        .into_iter()
        .filter_map(|directive| match directive {
            Directive::Emit(event) => Some(event),
            _ => None,
        })
        .collect()
}

fn var_i32(mut value: i32) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let mut byte = (value as u8) & 0x7f;
        value >>= 7;
        if (value != 0) && (value != -1 || (byte & 0x40) == 0) {
            byte |= 0x80;
        }
        out.push(byte);
        if value == 0 || value == -1 && (byte & 0x40) != 0 {
            return out;
        }
    }
}

fn item_display(id: i32) -> Vec<u8> {
    [var_i32(4), var_i32(id)].concat()
}

const EMPTY_DISPLAY: [u8; 1] = [0x00];
const UNKNOWN_ITEM: i32 = i32::MAX;

fn one(packet_id: i32, payload: &[u8]) -> ClientEvent {
    let events = decode(packet_id, payload);
    assert_eq!(events.len(), 1, "expected one event, got {events:?}");
    events.into_iter().next().unwrap()
}

#[test]
fn recipe_book_add_preserves_unknown_positive_result_at_packet_boundary() {
    let payload: Vec<u8> = [
        &[0x01u8][..], // one entry
        &[0x04],       // display id
        &[0x00],       // shapeless display
        &[0x00],       // no ingredients
        &item_display(UNKNOWN_ITEM),
        &EMPTY_DISPLAY, // empty crafting station
        &[0x00],        // absent group
        &[0x03],        // category
        &[0x00],        // no crafting requirements
        &[0x03],        // notification and highlight
        &[0x01],        // replace
    ]
    .concat();

    let ClientEvent::RecipeBookAdded { entries, replace } =
        one(play::clientbound::RECIPE_BOOK_ADD, &payload)
    else {
        panic!("wrong event");
    };
    assert!(replace);
    assert_eq!(
        entries[0].result_items,
        vec![ItemId::protocol_local(UNKNOWN_ITEM as u32)]
    );
}

#[test]
fn ghost_recipe_preserves_unknown_positive_result_at_packet_boundary() {
    let payload: Vec<u8> = [
        &var_i32(3)[..],      // window id
        &var_i32(3)[..],      // stonecutter display: input, result, station
        &item_display(1)[..], // canonical input
        &item_display(UNKNOWN_ITEM)[..],
        &EMPTY_DISPLAY[..],
    ]
    .concat();

    let ClientEvent::GhostRecipeShown { result_items, .. } =
        one(play::clientbound::PLACE_GHOST_RECIPE, &payload)
    else {
        panic!("wrong event");
    };
    assert_eq!(
        result_items,
        vec![ItemId::protocol_local(UNKNOWN_ITEM as u32)]
    );
}

#[test]
fn update_recipes_preserves_unknown_positive_property_and_stonecutter_ids() {
    let key = b"minecraft:furnace_input";
    let payload: Vec<u8> = [
        &[0x01u8][..],
        &[key.len() as u8],
        key,
        &[0x01],
        &var_i32(UNKNOWN_ITEM),
        &[0x01], // one stonecutter row
        &[0x02], // explicit holder set with one id
        &var_i32(UNKNOWN_ITEM),
        &item_display(UNKNOWN_ITEM),
    ]
    .concat();

    let ClientEvent::RecipePropertySetsUpdated {
        item_sets,
        stonecutter_results,
    } = one(play::clientbound::UPDATE_RECIPES, &payload)
    else {
        panic!("wrong event");
    };
    let unknown = ItemId::protocol_local(UNKNOWN_ITEM as u32);
    assert_eq!(item_sets[0].1, vec![unknown]);
    assert_eq!(stonecutter_results, vec![(vec![unknown], vec![unknown])]);
}
