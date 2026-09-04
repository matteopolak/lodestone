//! Operator query fixtures: VarInt correlation id, packed position, nullable NBT.

use lodestone_core::{Nbt, State};
use lodestone_model::BlockPos;
use lodestone_server::{ServerBound, ServerDirective, ServerProtocol};
use lodestone_v26_2::{V770ServerProtocol, packet_ids::play};

#[test]
fn block_entity_query_decodes_position_and_transaction() {
    // x=-3, z=5, y=-17, in the 26/26/12-bit packed position layout.
    let packed = (((-3_i64) & 0x3ff_ffff) << 38) | (5_i64 << 12) | 0xfef;
    let mut body = vec![0xac, 0x02]; // 300
    body.extend_from_slice(&packed.to_be_bytes());
    assert!(matches!(
        V770ServerProtocol.decode(State::Play, play::serverbound::BLOCK_ENTITY_TAG_QUERY, &body),
        ServerBound::BlockEntityTagQuery { transaction_id: 300, pos }
            if pos == BlockPos::new(-3, -17, 5)
    ));
    for length in 0..body.len() {
        assert!(matches!(
            V770ServerProtocol.decode(State::Play, play::serverbound::BLOCK_ENTITY_TAG_QUERY, &body[..length]),
            ServerBound::Ignored
        ));
    }
    body.push(0);
    assert!(matches!(
        V770ServerProtocol.decode(State::Play, play::serverbound::BLOCK_ENTITY_TAG_QUERY, &body),
        ServerBound::Ignored
    ));
}

#[test]
fn block_entity_query_reply_is_nullable_network_nbt_through_boxed_protocol() {
    let proto: Box<dyn ServerProtocol> = Box::new(V770ServerProtocol);
    assert_eq!(proto.encode_tag_query(300, None), ServerDirective::Send {
        packet_id: play::clientbound::TAG_QUERY,
        payload: vec![0xac, 0x02, 0],
    });
    let tag = Nbt::Compound(vec![("v".into(), Nbt::Int(37))]);
    assert_eq!(proto.encode_tag_query(300, Some(&tag)), ServerDirective::Send {
        packet_id: play::clientbound::TAG_QUERY,
        // Unnamed compound, integer named v, value 37, compound terminator.
        payload: vec![0xac, 0x02, 10, 3, 0, 1, b'v', 0, 0, 0, 37, 0],
    });
}
