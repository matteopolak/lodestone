use lodestone_model::{ClientEvent, ConnectionState, Directive, VersionAdapter, WorldSink};
use lodestone_world::{BiomePatch, BlockEntitySync, ChunkPos, ColumnPatch, LightPatch, LoadedChunk};
use lodestone_v1_21_11::{packet_ids, V774Adapter};

#[derive(Default)]
struct Sink;

impl WorldSink for Sink {
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
    fn set_block_entity(
        &mut self,
        _x: i32,
        _y: i32,
        _z: i32,
        _type_id: u32,
        _nbt: lodestone_core::Nbt,
    ) {
    }
    fn sync_block_entity(
        &mut self,
        _x: i32,
        _y: i32,
        _z: i32,
        _block_entity_type: Option<u32>,
    ) -> BlockEntitySync {
        BlockEntitySync::ChunkAbsent
    }
    fn merge_light(&mut self, _pos: ChunkPos, _patch: LightPatch) {}
    fn merge_biomes(&mut self, _pos: ChunkPos, _patch: BiomePatch) {}
    fn unload(&mut self, _pos: ChunkPos) {}
}

fn packet(adapter: &V774Adapter, packet_id: i32, payload: &[u8]) -> Vec<Directive> {
    let mut sink = Sink;
    adapter
        .handle_packet(&mut sink, ConnectionState::Play, packet_id, payload)
        .expect("packet should decode")
}

fn var_i32(value: i32) -> Vec<u8> {
    let mut out = Vec::new();
    let mut value = value as u32;
    loop {
        let mut byte = (value & 0x7f) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if value == 0 {
            return out;
        }
    }
}

fn explode_prefix() -> Vec<u8> {
    let mut body = Vec::new();
    for value in [12.25_f64, 64.5, -3.75] {
        body.extend_from_slice(&value.to_be_bytes());
    }
    body.extend_from_slice(&2.5_f32.to_be_bytes());
    body.extend_from_slice(&17_i32.to_be_bytes());
    body.push(0); // Optional knockback absent.
    body
}

fn add_explosion_particle_entry(body: &mut Vec<u8>, particle_id: i32, options: &[u8]) {
    body.extend(var_i32(particle_id));
    body.extend_from_slice(options);
    body.extend_from_slice(&1.25_f32.to_be_bytes());
    body.extend_from_slice(&(-0.75_f32).to_be_bytes());
    body.extend(var_i32(3));
}

#[test]
fn block_destruction_774_is_byte_exact_and_reaches_overlay_event() {
    // Entity id 300, packed BlockPos (3, 70, -5), clear-stage byte 10.
    let packed = (3_i64 << 38) | ((-5_i64 & 0x3ffffff) << 12) | 70;
    let mut body = var_i32(300);
    body.extend_from_slice(&packed.to_be_bytes());
    body.push(10);
    let events = packet(
        &V774Adapter::default(),
        packet_ids::play::clientbound::BLOCK_DESTRUCTION,
        &body,
    );
    assert_eq!(
        events,
        vec![Directive::Emit(ClientEvent::BlockDestruction {
            entity_id: 300,
            pos: lodestone_model::BlockPos::new(3, 70, -5),
            progress: 10,
        })]
    );
}

#[test]
fn explode_774_reads_f64_center_i32_count_optional_f64_knockback_and_tail() {
    let mut body = Vec::new();
    for value in [12.25_f64, 64.5, -3.75] {
        body.extend_from_slice(&value.to_be_bytes());
    }
    body.extend_from_slice(&2.5_f32.to_be_bytes());
    body.extend_from_slice(&17_i32.to_be_bytes());
    body.push(1);
    for value in [0.125_f64, -0.25, 0.5] {
        body.extend_from_slice(&value.to_be_bytes());
    }
    // 774's generated particle report assigns explosion_emitter id 22.
    body.push(22);
    // Positive holder reference (registry id 0 encoded as id + 1), then an
    // empty weighted block-particle list.
    body.push(1);
    body.push(0);

    let events = packet(
        &V774Adapter::default(),
        packet_ids::play::clientbound::EXPLODE,
        &body,
    );
    assert_eq!(
        events,
        vec![Directive::Emit(ClientEvent::Explosion {
            pos: lodestone_model::Vec3::new(12.25, 64.5, -3.75),
            radius: 2.5,
            affected_blocks: Vec::new(),
            knockback: Some(lodestone_model::Vec3::new(0.125, -0.25, 0.5)),
        })]
    );

    let mut trailing = body;
    trailing.push(0xff);
    let mut sink = Sink;
    assert!(V774Adapter::default()
        .handle_packet(
            &mut sink,
            ConnectionState::Play,
            packet_ids::play::clientbound::EXPLODE,
            &trailing,
        )
        .is_err());
}

#[test]
fn explode_774_consumes_all_fixed_parameterized_particle_schemas() {
    let mut body = explode_prefix();
    body.push(8); // dragon_breath: f32 power
    body.extend_from_slice(&0.375_f32.to_be_bytes());
    body.push(1); // Positive sound-holder reference.
    body.push(7); // Seven weighted debris entries.

    let mut options = 0.625_f32.to_be_bytes().to_vec();
    add_explosion_particle_entry(&mut body, 8, &options);

    options.clear();
    options.extend_from_slice(&0x0012_3456_i32.to_be_bytes());
    options.extend_from_slice(&1.5_f32.to_be_bytes());
    add_explosion_particle_entry(&mut body, 16, &options);

    options.clear();
    options.extend_from_slice(&0x8012_3456_u32.to_be_bytes());
    add_explosion_particle_entry(&mut body, 21, &options);

    options.clear();
    options.extend_from_slice(&0x4012_3456_u32.to_be_bytes());
    add_explosion_particle_entry(&mut body, 36, &options);

    options.clear();
    options.extend_from_slice(&0x2012_3456_u32.to_be_bytes());
    add_explosion_particle_entry(&mut body, 42, &options);

    options.clear();
    options.extend_from_slice(&0x0012_3456_i32.to_be_bytes());
    options.extend_from_slice(&2.25_f32.to_be_bytes());
    add_explosion_particle_entry(&mut body, 46, &options);

    options.clear();
    options.extend(var_i32(300)); // Dust-pillar block-state id.
    add_explosion_particle_entry(&mut body, 109, &options);

    let events = packet(
        &V774Adapter::default(),
        packet_ids::play::clientbound::EXPLODE,
        &body,
    );
    assert_eq!(events.len(), 1, "all seven option bodies must leave the tail aligned");
    assert!(matches!(events[0], Directive::Emit(ClientEvent::Explosion { .. })));
}

#[test]
fn explode_774_consumes_nested_item_vibration_and_trail_options() {
    let mut body = explode_prefix();
    body.push(22); // Bare explosion-emitter particle.
    body.push(1); // Positive sound-holder reference.
    body.push(3);

    // Item particle: an empty slot is the complete Slot codec (one VarInt 0).
    add_explosion_particle_entry(&mut body, 47, &[0]);

    // Vibration particle: block source, packed Position (1, 2, 3), arrival.
    let packed_position = (1_i64 << 38) | ((3_i64 & 0x3ffffff) << 12) | 2;
    let mut vibration = vec![0];
    vibration.extend_from_slice(&packed_position.to_be_bytes());
    vibration.extend(var_i32(20));
    add_explosion_particle_entry(&mut body, 48, &vibration);

    // Trail particle: target center followed by the schema's one raw colour
    // byte (not a packed i32 or VarInt).
    let mut trail = Vec::new();
    for value in [1.25_f64, 2.5, -4.75] {
        trail.extend_from_slice(&value.to_be_bytes());
    }
    trail.push(0x7f);
    add_explosion_particle_entry(&mut body, 49, &trail);

    let events = packet(
        &V774Adapter::default(),
        packet_ids::play::clientbound::EXPLODE,
        &body,
    );
    assert_eq!(events.len(), 1);

    // A vibration source discriminator outside the local schema is not
    // allowed to consume the following bytes as if they were a position.
    let mut malformed = explode_prefix();
    malformed.push(48);
    malformed.extend([2, 0, 1, 2]);
    let mut sink = Sink;
    assert!(V774Adapter::default()
        .handle_packet(
            &mut sink,
            ConnectionState::Play,
            packet_ids::play::clientbound::EXPLODE,
            &malformed,
        )
        .is_err());

    // A fixed-width option truncated before its float must fail, rather than
    // letting the next field manufacture a false sound-holder value.
    let mut truncated = explode_prefix();
    truncated.push(8);
    truncated.push(0);
    assert!(V774Adapter::default()
        .handle_packet(
            &mut sink,
            ConnectionState::Play,
            packet_ids::play::clientbound::EXPLODE,
            &truncated,
        )
        .is_err());

    let mut trailing = body;
    trailing.push(0xff);
    assert!(V774Adapter::default()
        .handle_packet(
            &mut sink,
            ConnectionState::Play,
            packet_ids::play::clientbound::EXPLODE,
            &trailing,
        )
        .is_err());
}
