//! Protocol-5 explosion tests from the independently sourced legacy layout.

use lodestone_core::{Ctx, encode_body};
use lodestone_data::block_states;
use lodestone_model::{ClientEvent, ConnectionState, Directive, Vec3, VersionAdapter};
use lodestone_v1_7::packets::world::Explosion;
use lodestone_world::{
    ChunkColumn, ChunkPos, ColumnLight, Heightmaps, LoadedChunk, PaletteKind, World,
};

const CTX: Ctx = Ctx { version: 5 };

fn fixture() -> Explosion {
    Explosion {
        x: 12.5,
        y: -3.25,
        z: 8.75,
        radius: 2.5,
        affected_block_offsets: vec![[1, -2, 3], [-4, 5, -6]],
        player_motion_x: 0.125,
        player_motion_y: -0.25,
        player_motion_z: 0.5,
    }
}

fn handle(body: &[u8]) -> Result<Vec<Directive>, lodestone_model::AdapterError> {
    let mut world = World::new();
    handle_in_world(&mut world, body)
}

fn handle_in_world(
    world: &mut World,
    body: &[u8],
) -> Result<Vec<Directive>, lodestone_model::AdapterError> {
    let adapter = lodestone_v1_7::adapter_for(lodestone_v1_7::PROTOCOL);
    adapter.handle_packet(
        world,
        ConnectionState::Play,
        lodestone_v1_7::packet_ids::play::clientbound::EXPLOSION,
        body,
    )
}

fn literal_fixture_body() -> Vec<u8> {
    vec![
        0x41, 0x48, 0x00, 0x00, // x = 12.5
        0xC0, 0x50, 0x00, 0x00, // y = -3.25
        0x41, 0x0C, 0x00, 0x00, // z = 8.75
        0x40, 0x20, 0x00, 0x00, // radius = 2.5
        0x00, 0x00, 0x00, 0x02, // two offsets
        0x01, 0xFE, 0x03, // [1, -2, 3]
        0xFC, 0x05, 0xFA, // [-4, 5, -6]
        0x3E, 0x00, 0x00, 0x00, // motion x = 0.125
        0xBE, 0x80, 0x00, 0x00, // motion y = -0.25
        0x3F, 0x00, 0x00, 0x00, // motion z = 0.5
    ]
}

#[test]
fn explosion_encoding_matches_the_literal_wire_layout() {
    let body = encode_body(&fixture(), CTX).expect("explosion fixture encodes");
    assert_eq!(
        body,
        vec![
            0x41, 0x48, 0x00, 0x00, // x = 12.5
            0xC0, 0x50, 0x00, 0x00, // y = -3.25
            0x41, 0x0C, 0x00, 0x00, // z = 8.75
            0x40, 0x20, 0x00, 0x00, // radius = 2.5
            0x00, 0x00, 0x00, 0x02, // two offsets
            0x01, 0xFE, 0x03, // [1, -2, 3]
            0xFC, 0x05, 0xFA, // [-4, 5, -6]
            0x3E, 0x00, 0x00, 0x00, // motion x = 0.125
            0xBE, 0x80, 0x00, 0x00, // motion y = -0.25
            0x3F, 0x00, 0x00, 0x00, // motion z = 0.5
        ]
    );
}

#[test]
fn explosion_emits_exact_center_radius_offsets_and_present_knockback() {
    let body = literal_fixture_body();
    let directives = handle(&body).expect("explosion decodes");
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::Explosion {
            pos: Vec3::new(12.5, -3.25, 8.75),
            radius: 2.5,
            affected_blocks: vec![[1, -2, 3], [-4, 5, -6]],
            knockback: Some(Vec3::new(0.125, -0.25, 0.5)),
        })]
    );
}

#[test]
fn affected_offsets_clear_loaded_blocks_but_leave_neighbors_untouched() {
    let stone = (0..block_states::STATE_COUNT)
        .find(|&id| {
            block_states::block_name(id) == Some("minecraft:stone")
                && block_states::properties(id) == Some(&[])
        })
        .expect("the canonical registry contains property-less stone");
    let mut column = ChunkColumn::new(
        -64,
        24,
        PaletteKind::block_states(),
        PaletteKind::biomes(),
        block_states::air_state_id(),
        0,
    );
    // The packet centre floors to (10, 70, -5). Its [1, 0, 1] offset is the
    // target; [0, 0, 0] is a control neighbour that must remain stone.
    column.set_block(11, 70, 12, stone);
    column.set_block(10, 70, 11, stone);
    let mut world = World::new();
    world.load(
        ChunkPos::new(0, -1),
        LoadedChunk::new(column, ColumnLight::new(24), Heightmaps::new(), Vec::new()),
    );

    let mut packet = fixture();
    packet.x = 10.75;
    packet.y = 70.25;
    packet.z = -4.5;
    packet.affected_block_offsets = vec![[1, 0, 1]];
    let body = encode_body(&packet, CTX).expect("explosion fixture encodes");
    handle_in_world(&mut world, &body).expect("explosion mutates the loaded world");

    assert_eq!(
        world.block_state_at(11, 70, -4),
        Some(block_states::air_state_id()),
        "the authoritative affected offset becomes canonical air"
    );
    assert_eq!(
        world.block_state_at(10, 70, -5),
        Some(stone),
        "an unaffected neighbor remains unchanged"
    );
}

#[test]
fn zero_motion_is_still_an_always_present_knockback() {
    let mut packet = fixture();
    packet.player_motion_x = 0.0;
    packet.player_motion_y = 0.0;
    packet.player_motion_z = 0.0;
    let body = encode_body(&packet, CTX).expect("explosion fixture encodes");
    let directives = handle(&body).expect("zero-motion explosion decodes");
    let [Directive::Emit(ClientEvent::Explosion { knockback, .. })] =
        directives.as_slice()
    else {
        panic!("expected one explosion directive");
    };
    assert_eq!(*knockback, Some(Vec3::new(0.0, 0.0, 0.0)));
}

#[test]
fn negative_count_is_rejected_before_reading_a_motion_tail() {
    let mut body = vec![
        0x41, 0x48, 0x00, 0x00, // x
        0xC0, 0x50, 0x00, 0x00, // y
        0x41, 0x0C, 0x00, 0x00, // z
        0x40, 0x20, 0x00, 0x00, // radius
        0xFF, 0xFF, 0xFF, 0xFF, // count = -1
    ];
    body.extend_from_slice(&[0; 12]);
    assert!(handle(&body).is_err(), "negative offset count must be rejected");
}

#[test]
fn count_that_reaches_into_the_motion_tail_is_rejected() {
    let mut body = vec![
        0x41, 0x48, 0x00, 0x00, // x
        0xC0, 0x50, 0x00, 0x00, // y
        0x41, 0x0C, 0x00, 0x00, // z
        0x40, 0x20, 0x00, 0x00, // radius
        0x00, 0x00, 0x00, 0x02, // count = 2
        0x01, 0xFE, 0x03, // only one offset
    ];
    body.extend_from_slice(&[0; 12]);
    assert!(
        handle(&body).is_err(),
        "a count larger than the available offset bytes must be rejected"
    );
}
