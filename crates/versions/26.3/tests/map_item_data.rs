//! `map_item_data` on the 26.3 dialect: every decoration type id the release's
//! own registry report lists reaches the client as its registry key, including
//! the five types 26.2 does not have.
//!
//! The ids are from the release's generated registry report
//! (`minecraft:map_decoration_type`, `protocol_id`), not from the tables the
//! decoder uses: 24 is `banner_red` (as in 26.2) and 35, 36, 37, 38, 39 are
//! `abandoned_camp`, `ancient_city`, `desert_pyramid`, `mineshaft` and
//! `ocean_ruin_warm`.

use lodestone_model::{ClientEvent, ConnectionState, Directive, VersionAdapter};
use lodestone_v26_2::V770Adapter;
use lodestone_v26_3::{connection_dialect, packet_ids};
use lodestone_world::World;

fn var_i32(value: i32) -> Vec<u8> {
    let mut out = Vec::new();
    let mut v = value as u32;
    loop {
        let byte = (v & 0x7F) as u8;
        v >>= 7;
        if v == 0 {
            out.push(byte);
            break;
        }
        out.push(byte | 0x80);
    }
    out
}

#[test]
fn decoration_ids_added_in_26_3_decode_to_their_registry_keys() {
    let adapter = V770Adapter::with_connection_dialect(connection_dialect());
    let ids = [(24, "minecraft:banner_red"), (35, "minecraft:abandoned_camp"), (36, "minecraft:ancient_city"),
        (37, "minecraft:desert_pyramid"), (38, "minecraft:mineshaft"), (39, "minecraft:ocean_ruin_warm")];
    let mut payload = var_i32(5); // map id
    payload.push(0); // scale
    payload.push(0); // unlocked
    payload.push(1); // decorations present
    payload.extend(var_i32(ids.len() as i32));
    for (index, (id, _)) in ids.iter().enumerate() {
        payload.extend(var_i32(*id));
        payload.push(index as u8); // x
        payload.push(0); // y
        payload.push(index as u8 + 16); // rotation byte, masked to 0..=15
        payload.push(0); // no name
    }
    payload.push(0); // no colour patch
    let directives = adapter
        .handle_packet(&mut World::new(), ConnectionState::Play, packet_ids::play::clientbound::MAP_ITEM_DATA, &payload)
        .expect("a 26.3 map_item_data decodes");
    let [Directive::Emit(ClientEvent::MapItemData { decorations: Some(decorations), .. })] = directives.as_slice() else {
        panic!("expected one MapItemData with decorations, got {directives:?}");
    };
    let kinds: Vec<String> = decorations.iter().map(|decoration| decoration.kind.to_string()).collect();
    assert_eq!(kinds, ids.iter().map(|(_, key)| (*key).to_owned()).collect::<Vec<_>>());
    assert_eq!(decorations[5].rotation, 5);
}
