//! Production chunk-packet regression for sulfur cave decoration.
//!
//! The worldgen feature tests exercise the speleothem algorithms in isolation,
//! but an integrated-server column only reaches a client after the source,
//! protocol encoder, and packet decoder have all agreed. This gate follows
//! that path for the externally captured seed/chunk/cell where the underground
//! biome's sulfur decoration is visible in the served packet.

use lodestone_core::Reader;
use lodestone_data::block_states;
use lodestone_server::dimension::Dimension;
use lodestone_server::{ChunkSource, ServerDirective, ServerProtocol};
use lodestone_v26_2::packets::chunk::{ChunkShape, LevelChunkWithLight};
use lodestone_v26_2::V770ServerProtocol;

const SEED: i64 = 42;
const CHUNK_X: i32 = -500;
const CHUNK_Z: i32 = -500;
const LOCAL_X: usize = 13;
const LOCAL_Y: i32 = -11;
const LOCAL_Z: usize = 5;

#[test]
fn sulfur_cave_decoration_reaches_the_production_chunk_packet() {
    let source = lodestone_server::overworld_chunk_source(SEED);
    let column = source.column(CHUNK_X, CHUNK_Z);
    let ServerDirective::Send { payload, .. } = V770ServerProtocol
        .try_encode_chunk_in_dimension(CHUNK_X, CHUNK_Z, &column, Dimension::Overworld)
        .expect("the generated overworld column must encode")
    else {
        panic!("chunk encoding must produce one send directive");
    };

    let mut reader = Reader::new(&payload);
    let packet = LevelChunkWithLight::decode(&mut reader, &ChunkShape::overworld_1_21())
        .expect("the production chunk packet must decode");
    reader
        .ensure_empty()
        .expect("the production chunk packet must have no trailing bytes");

    assert_eq!((packet.x, packet.z), (CHUNK_X, CHUNK_Z));
    let state = packet.column.get_block(LOCAL_X, LOCAL_Y, LOCAL_Z);
    assert_eq!(
        block_states::block_name(state),
        Some("minecraft:sulfur"),
        "seed {SEED} chunk ({CHUNK_X},{CHUNK_Z}) local ({LOCAL_X},{LOCAL_Y},{LOCAL_Z})"
    );
}
