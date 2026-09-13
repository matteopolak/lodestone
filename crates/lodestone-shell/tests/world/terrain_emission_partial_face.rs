//! Production mesher witness for terrain emission and partial-face lighting.
//!
//! The fixture is deliberately external to this source file: state names and
//! properties are resolved through the real block-state registry, while the
//! expected emission and AO decision remain independent values. The test then
//! drives `snapshot_section_in` and `mesh_snapshot_models`, locates individual
//! emitted quads by their translated positions, and checks the bytes written to
//! the actual model vertices.
//!
//! Run explicitly with:
//!
//! ```text
//! cargo test -p lodestone-shell --test world terrain_emission_partial_face -- --ignored --nocapture
//! ```

use std::path::{Path, PathBuf};

use lodestone::mesher::{ColumnSource, SectionKey, mesh_snapshot_models, snapshot_section_in};
use lodestone_assets::{BakedQuad, ResourceManager, ResourceSource, ZipSource};
use lodestone_data::block_states::StateId;
use lodestone_model::BlockStateRegistry;
use lodestone_render::{BlockModels, BlocksJsonRegistry, ModelMesh, ModelVertex, SkyDefault, blocks_json_registry};
use lodestone_world::{
    ChunkColumn, ChunkPos, ColumnLight, Heightmaps, LightData, LoadedChunk, NibbleArray,
    PaletteKind, World,
};

const SECTIONS: usize = 1;
const SUBJECT: (usize, usize, usize) = (8, 8, 8);

fn pack_root() -> PathBuf {
    let cwd = std::env::current_dir().expect("cwd");
    for base in cwd.ancestors() {
        let cache = base.join(".cache/mc");
        let Ok(entries) = std::fs::read_dir(&cache) else {
            continue;
        };
        let mut roots: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.join("client.jar").is_file() && path.join("generated/reports/blocks.json").is_file()
            })
            .collect();
        roots.sort();
        if let Some(root) = roots.pop() {
            return root;
        }
    }
    panic!(
        "no real client pack found under .cache/mc/<version>/ (needs client.jar and blocks.json)"
    );
}

fn registry(root: &Path) -> BlocksJsonRegistry {
    blocks_json_registry(&root.join("generated/reports/blocks.json")).expect("blocks.json")
}

fn load_models(root: &Path, registry: &BlocksJsonRegistry) -> BlockModels {
    let bytes = std::fs::read(root.join("client.jar")).expect("client.jar");
    let zip = ZipSource::from_bytes(bytes).expect("open client.jar");
    let manager = ResourceManager::new(vec![Box::new(zip) as Box<dyn ResourceSource>]);
    BlockModels::build(&manager, registry).expect("bake block models")
}

/// Resolve one fixture selector without ever hand-writing a global state id.
fn state_id(registry: &impl BlockStateRegistry, spec: &str) -> StateId {
    let (name, selectors) = match spec.split_once('[') {
        Some((name, selectors)) => (name, Some(selectors.strip_suffix(']').expect("closed selector"))),
        None => (spec, None),
    };
    let selectors: Vec<(&str, &str)> = selectors
        .into_iter()
        .flat_map(|text| text.split(','))
        .filter(|pair| !pair.is_empty())
        .map(|pair| pair.split_once('=').expect("property=value selector"))
        .collect();
    for raw in 0..registry.state_count() {
        let Some(state) = registry.resolve(raw) else {
            continue;
        };
        if state.block.to_string() != name
            || selectors.iter().any(|(key, value)| {
                state.properties.get(*key).map(String::as_str) != Some(*value)
            })
        {
            continue;
        }
        return StateId::new(raw).expect("registry id belongs to the state census");
    }
    panic!("{spec} present in blocks.json");
}

fn base_light() -> ColumnLight {
    let mut light = ColumnLight::new(SECTIONS);
    *light.sky_mut(1) = LightData::Uniform(15);
    *light.block_mut(1) = LightData::Uniform(0);
    light
}

/// A central light section with four dark ring cells. The top-step quad's
/// centre remains level 15, while its four canonical corner samples resolve to
/// levels 15, 11, 4, and 8. This makes weighted interpolation and nearest-corner
/// selection disagree by a whole nibble at the located vertex.
fn partial_light() -> ColumnLight {
    let mut light = base_light();
    let mut sky = NibbleArray::filled(15);
    for (x, y, z) in [(8, 9, 7), (7, 9, 8), (7, 9, 7), (7, 9, 9)] {
        sky.set(NibbleArray::index(x, y, z), 0);
    }
    *light.sky_mut(1) = LightData::Values(sky);
    light
}

fn column(air: u32, subject: Option<u32>, occluder: Option<u32>, light: ColumnLight) -> LoadedChunk {
    let mut column = ChunkColumn::new(
        0,
        SECTIONS,
        PaletteKind::block_states(),
        PaletteKind::biomes(),
        air,
        0,
    );
    if let Some(state) = subject {
        column.set_block(SUBJECT.0, SUBJECT.1, SUBJECT.2, state);
    }
    if let Some(state) = occluder {
        column.set_block(9, 9, 8, state);
    }
    LoadedChunk::new(column, light, Heightmaps::new(), Vec::new())
}

fn scene(air: u32, subject: u32, occluder: Option<u32>, central_light: ColumnLight) -> World {
    let mut world = World::new();
    for dx in -1..=1i32 {
        for dz in -1..=1i32 {
            let central = dx == 0 && dz == 0;
            world.load(
                ChunkPos::new(dx, dz),
                column(
                    air,
                    central.then_some(subject),
                    if central { occluder } else { None },
                    if central { central_light.clone() } else { base_light() },
                ),
            );
        }
    }
    world
}

fn key() -> SectionKey {
    SectionKey {
        cx: 0,
        cz: 0,
        si: 0,
        min_y: 0,
    }
}

fn emitted_quad<'a>(mesh: &'a ModelMesh, quad: &BakedQuad, origin: [f32; 3]) -> &'a [ModelVertex] {
    let expected = quad.positions.map(|position| {
        [
            position[0] + origin[0],
            position[1] + origin[1],
            position[2] + origin[2],
        ]
    });
    mesh.vertices
        .chunks_exact(4)
        .find(|vertices| {
            vertices.iter().zip(expected.iter()).all(|(vertex, position)| {
                vertex
                    .position
                    .iter()
                    .zip(position)
                    .all(|(actual, expected)| (actual - expected).abs() < 1.0e-5)
            })
        })
        .unwrap_or_else(|| panic!("located quad was not emitted: {:?}", quad.positions))
}

fn top_partial_quad<'a>(models: &'a BlockModels, state: StateId) -> &'a BakedQuad {
    models
        .quads(state)
        .iter()
        .find(|quad| {
            if quad.direction != lodestone_assets::Direction::Up
                || quad.cullface != Some(lodestone_assets::Direction::Up)
            {
                return false;
            }
            let min_x = quad.positions.iter().map(|p| p[0]).fold(f32::INFINITY, f32::min);
            let max_x = quad.positions.iter().map(|p| p[0]).fold(f32::NEG_INFINITY, f32::max);
            let min_z = quad.positions.iter().map(|p| p[2]).fold(f32::INFINITY, f32::min);
            let max_z = quad.positions.iter().map(|p| p[2]).fold(f32::NEG_INFINITY, f32::max);
            min_x >= 1.0e-4 || min_z >= 1.0e-4 || max_x <= 0.9999 || max_z <= 0.9999
        })
        .expect("straight stair has a cullable partial top face")
}

#[test]
#[ignore = "needs client.jar + blocks.json; run explicitly"]
fn production_mesher_connects_emission_and_partial_face_lighting() {
    let root = pack_root();
    let registry = registry(&root);
    let models = load_models(&root, &registry);
    let air = state_id(&registry, "minecraft:air").raw();
    let stone = state_id(&registry, "minecraft:stone");
    let sea_lantern = state_id(&registry, "minecraft:sea_lantern");
    let stair = state_id(
        &registry,
        "minecraft:oak_stairs[facing=south,half=bottom,shape=straight,waterlogged=false]",
    );

    // External fixture: the expected values originate outside the renderer,
    // and the registry selector proves that no test assertion is satisfied by
    // accidentally picking a neighbouring state with the same block name.
    let mut fixture_rows = 0;
    for line in include_str!("../support/terrain_emission_states.txt").lines() {
        let line = line.split('#').next().unwrap_or_default().trim();
        if line.is_empty() {
            continue;
        }
        let mut fields = line.split_whitespace();
        let spec = fields.next().expect("fixture state selector");
        let expected_emission: u8 = fields
            .next()
            .expect("fixture emission")
            .parse()
            .expect("numeric emission");
        let expected_ao: bool = fields
            .next()
            .expect("fixture AO gate")
            .parse()
            .expect("boolean AO gate");
        let state = state_id(&registry, spec);
        assert_eq!(
            lodestone_data::light_props::emission(state),
            expected_emission,
            "external fixture emission for {spec}"
        );
        assert_eq!(models.light_emission(state), expected_emission);
        assert_eq!(models.ambient_occlusion(state), expected_ao);
        fixture_rows += 1;
    }
    assert_eq!(fixture_rows, 4, "the external state fixture must not be empty");

    // Full-cube emitter: the real snapshot view supplies the state emission to
    // the model mesher, which must flatten AO. The identical stone scene is the
    // executed wrong-lighting control; its ring stone must darken two located
    // top-face vertices, proving that the occluder and geometry are observable.
    let emitter_world = scene(air, sea_lantern.raw(), Some(stone.raw()), base_light());
    let emitter_snap = snapshot_section_in(
        &emitter_world,
        key(),
        Some(SECTIONS),
        SkyDefault::Full,
        ColumnSource::Complete,
    )
    .any()
    .expect("emitter scene snapshots");
    let emitter_mesh = mesh_snapshot_models(&emitter_snap, &models, true);
    let emitter_quad = models
        .quads(sea_lantern)
        .iter()
        .find(|quad| {
            quad.direction == lodestone_assets::Direction::Up
                && quad.cullface == Some(lodestone_assets::Direction::Up)
        })
        .expect("sea lantern has an up face");
    let emitter_vertices = emitted_quad(&emitter_mesh, emitter_quad, [8.0, 8.0, 8.0]);
    assert!(
        emitter_vertices.iter().all(|vertex| (vertex.ao - 1.0).abs() < 1.0e-6),
        "emitting full cube must flatten AO at its located up face: {:?}",
        emitter_vertices.iter().map(|vertex| vertex.ao).collect::<Vec<_>>()
    );

    let stone_world = scene(air, stone.raw(), Some(stone.raw()), base_light());
    let stone_snap = snapshot_section_in(
        &stone_world,
        key(),
        Some(SECTIONS),
        SkyDefault::Full,
        ColumnSource::Complete,
    )
    .any()
    .expect("stone scene snapshots");
    let stone_mesh = mesh_snapshot_models(&stone_snap, &models, true);
    let stone_quad = models
        .quads(stone)
        .iter()
        .find(|quad| {
            quad.direction == lodestone_assets::Direction::Up
                && quad.cullface == Some(lodestone_assets::Direction::Up)
        })
        .expect("stone has an up face");
    let stone_vertices = emitted_quad(&stone_mesh, stone_quad, [8.0, 8.0, 8.0]);
    assert!(
        stone_vertices.iter().any(|vertex| vertex.ao < 0.99),
        "stone control must darken a located up-face vertex: {:?}",
        stone_vertices.iter().map(|vertex| vertex.ao).collect::<Vec<_>>()
    );

    // Partial-face witness: a real straight stair's upper step is inset on X.
    // The chosen light fixture predicts 0x60 at its min-X/min-Z vertex after
    // shape weighting; nearest-corner sampling would instead write 0x40.
    let stair_quad = top_partial_quad(&models, stair);
    let vertex_index = stair_quad
        .positions
        .iter()
        .position(|position| position[0] <= 0.0001 && position[2] <= 0.5001)
        .expect("partial top quad has a min-X/min-Z vertex");
    let stair_world = scene(air, stair.raw(), None, partial_light());
    let stair_snap = snapshot_section_in(
        &stair_world,
        key(),
        Some(SECTIONS),
        SkyDefault::Full,
        ColumnSource::Complete,
    )
    .any()
    .expect("stair scene snapshots");
    let stair_mesh = mesh_snapshot_models(&stair_snap, &models, true);
    let stair_vertices = emitted_quad(&stair_mesh, stair_quad, [8.0, 8.0, 8.0]);
    let actual = stair_vertices[vertex_index].light;
    println!(
        "=== PRODUCTION TERRAIN EMISSION/PARTIAL-FACE GATE ===\n  emitter AO: {:?}\n  stone control AO: {:?}\n  stair partial vertex {vertex_index}: actual light {actual:#04x}, weighted prediction 0x60, nearest control 0x40",
        emitter_vertices.iter().map(|vertex| vertex.ao).collect::<Vec<_>>(),
        stone_vertices.iter().map(|vertex| vertex.ao).collect::<Vec<_>>(),
    );
    assert_eq!(
        actual, 0x60,
        "the production stair vertex must use shape-weighted light; 0x40 is the deliberately wrong nearest-corner control"
    );
    assert_ne!(actual, 0x40, "nearest-corner detector control must disagree");
}
