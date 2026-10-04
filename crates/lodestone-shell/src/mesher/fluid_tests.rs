use super::*;
use crate::blocks::DemoClassifier;
use lodestone_assets::{MemorySource, ResourceManager};
use lodestone_render::BlocksJsonRegistry;
use lodestone_world::PalettedContainer;

fn water_state() -> u32 {
    StateId::from_state_str("minecraft:water[level=0]").unwrap().raw()
}

fn models() -> &'static BlockModels {
    static MODELS: OnceLock<BlockModels> = OnceLock::new();
    MODELS.get_or_init(|| {
        let (manager, registry) = model_resources(false);
        BlockModels::build_with_mip_levels(&manager, &registry, 0).unwrap()
    })
}

fn model_resources(drawable_air: bool) -> (ResourceManager, BlocksJsonRegistry) {
    let mut texture = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut texture, 1, 1);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.write_header().unwrap().write_image_data(&[255; 4]).unwrap();
    }
    let mut source = MemorySource::new("dry-center-fluid-test");
    source.insert("assets/minecraft/textures/block/water_still.png", texture.clone());
    source.insert("assets/minecraft/textures/block/test_solid.png", texture.clone());
    source.insert("assets/minecraft/textures/block/water_flow.png", texture);
    for name in ["solid", "quad"] {
        source.insert(format!("assets/minecraft/blockstates/test_{name}.json"), serde_json::to_vec(
            &serde_json::json!({"variants": {"": {"model": format!("minecraft:block/test_{name}")}}}),
        ).unwrap());
        let mut faces = serde_json::Map::new();
        for face in ["east", "west", "up", "down", "north", "south"] {
            if name == "solid" || face == "east" {
                let mut data = serde_json::json!({"texture": "#all"});
                if name == "solid" { data["cullface"] = face.into(); }
                faces.insert(face.into(), data);
            }
        }
        source.insert(format!("assets/minecraft/models/block/test_{name}.json"), serde_json::to_vec(
            &serde_json::json!({"textures": {"all": "minecraft:block/test_solid"},
                "elements": [{"from": [0, 0, 0], "to": [16, 16, 16], "faces": faces}]}),
        ).unwrap());
    }
    if drawable_air {
        source.insert("assets/minecraft/blockstates/air.json", br#"{"variants":{"":{"model":"minecraft:block/test_quad"}}}"#.to_vec());
    }
    let manager = ResourceManager::new(vec![Box::new(source)]);
    let report = serde_json::json!({
        "minecraft:air": {"states": [{"id": 0, "default": true}]},
        "test:dry_a": {"states": [{"id": 1, "default": true}]},
        "test:dry_b": {"states": [{"id": 2, "default": true}]},
        "test:waterlogged": {"states": [{"id": 3, "default": true, "properties": {"waterlogged": "true"}}]},
        "minecraft:test_solid": {"states": [{"id": 4, "default": true}]},
        "minecraft:test_quad": {"states": [{"id": 5, "default": true}]},
        "minecraft:water": {"states": [{"id": water_state(), "default": true, "properties": {"level": "0"}}]}
    });
    let registry = BlocksJsonRegistry::from_slice(&serde_json::to_vec(&report).unwrap()).unwrap();
    (manager, registry)
}

#[test]
fn light_patch_spatial_proof_tracks_pack_authored_air_geometry() {
    use lodestone_render::BlockAtlas;
    use lodestone_world::{ColumnLight, Heightmaps, LightBoundaryMask, LightSectionChange, LoadedChunk};
    let air = lodestone_data::block::Block::Air.default_state();
    let mut world = World::new();
    let mut column = ChunkColumn::new(0, 1, PaletteKind::block_states(), PaletteKind::biomes(), air.raw(), 0);
    column.set_block(2, 2, 2, 4);
    world.load(ChunkPos::new(0, 0), LoadedChunk::new(column, ColumnLight::new(1), Heightmaps::new(), Vec::new()));
    let write = ChunkWorldWrite::new(world);
    let store = write.read_handle();
    let mut terrain = TerrainMesh::new(MeshScheduler::new(1, ShellClassifier::Demo(DemoClassifier)));
    let change = LightSectionChange { section_index: 1, affected: LightBoundaryMask::for_cell(12, 12, 12) };
    for drawable_air in [false, true, false] {
        let (manager, registry) = model_resources(drawable_air);
        let models = BlockModels::build_with_mip_levels(&manager, &registry, 0).unwrap();
        assert_eq!(models.quads(air).len(), usize::from(drawable_air));
        assert!(models.fluid(air).is_none());
        let atlas = BlockAtlas::build_with_mip_levels(&manager, &registry, 0).unwrap().with_models(models);
        let classifier = ShellClassifier::Vanilla(Arc::new(atlas));
        assert_eq!(spatial_light_air(&classifier), (!drawable_air).then_some(air.raw()));
        terrain.reload_classifier(&store, 1, classifier);
        terrain.forced_columns.clear();
        terrain.light_dirty_sections.clear();
        assert_eq!(terrain.queue_light_changes(&store, 0, 0, &[change]), usize::from(drawable_air));
    }
}

#[test]
fn shared_snapshot_light_preserves_all_slots_and_missing_policy() {
    use lodestone_world::LightData;
    let mut snapshot = mixed_dry_snapshot();
    for i in 0..27 {
        snapshot.lights[i] = Some(SectionLightData {
            sky: LightData::Uniform((i / 16) as u8),
            block: LightData::Uniform((i % 16) as u8),
        });
    }
    for sky_default in [SkyDefault::Full, SkyDefault::None] {
        snapshot.sky_default = sky_default;
        let light = SnapshotLight::new(&snapshot);
        for i in 0..27 {
            let x = (i / 9 - 1) * 16;
            let y = ((i % 9) / 3 - 1) * 16;
            let z = (i % 3 - 1) * 16;
            assert_eq!(light.levels_at(x, y, z), ((i / 16) as u8, (i % 16) as u8));
        }
        assert_eq!(light.levels_at(32, 0, 0), (0, 0));
    }
    snapshot.lights[0] = None;
    snapshot.lights[13] = Some(SectionLightData { sky: LightData::Missing, block: LightData::Missing });
    for (sky_default, expected) in [(SkyDefault::Full, 15), (SkyDefault::None, 0)] {
        snapshot.sky_default = sky_default;
        let light = SnapshotLight::new(&snapshot);
        assert_eq!(light.levels_at(-16, -16, -16), (15, 0));
        assert_eq!(light.levels_at(0, 0, 0), (expected, 0));
    }
}

#[test]
fn shared_model_and_fluid_views_keep_positive_geometry_and_light() {
    let mut center = PalettedContainer::new(PaletteKind::block_states(), 0);
    center.set((8 << 8) | (8 << 4) | 8, 5);
    center.set((10 << 8) | (8 << 4) | 8, water_state());
    let mut snapshot = snapshot(center, 0);
    for i in 0..27 {
        snapshot.lights[i] = Some(SectionLightData {
            sky: lodestone_world::LightData::Uniform((i % 16) as u8),
            block: lodestone_world::LightData::Uniform((15 - i % 16) as u8),
        });
    }
    let light = SnapshotLight::new(&snapshot);
    let expected = mesh_snapshot_models_layers(&snapshot, models(), true, 0);
    let actual = model::mesh_snapshot_models_layers_with_light(&snapshot, models(), true, 0, &light);
    assert_eq!(actual.0.vertices.len(), 4);
    assert_eq!(actual.0.indices.len(), 6);
    assert!(actual.0.vertices.iter().all(|vertex| vertex.light == 0xd2));
    for (actual, expected) in [(&actual.0, &expected.0), (&actual.1, &expected.1)] {
        assert_eq!(bytemuck::cast_slice::<_, u8>(&actual.vertices), bytemuck::cast_slice::<_, u8>(&expected.vertices));
        assert_eq!(actual.indices, expected.indices);
    }
    let actual = mesh_snapshot_fluids_with_light(&snapshot, models(), 0, &light);
    assert_eq!(actual.water.quad_count(), 11);
    assert!(actual.water.vertices.iter().all(|vertex| vertex.light == 0xd2));
    assert_same_geometry(&actual, &mesh_snapshot_fluids_with_grid(&snapshot, models(), 0));
    let dry = mixed_dry_snapshot();
    let empty = mesh_snapshot_models_layers(&dry, models(), true, 0);
    assert_eq!(empty.0.quad_count() + empty.1.quad_count(), 0);

    snapshot.lights[13] = Some(SectionLightData {
        sky: lodestone_world::LightData::Uniform(3),
        block: lodestone_world::LightData::Uniform(7),
    });
    let light = SnapshotLight::new(&snapshot);
    let model = model::mesh_snapshot_models_layers_with_light(&snapshot, models(), true, 0, &light).0;
    let fluid = mesh_snapshot_fluids_with_light(&snapshot, models(), 0, &light);
    assert!(model.vertices.iter().all(|vertex| vertex.light == 0x37));
    assert!(fluid.water.vertices.iter().all(|vertex| vertex.light == 0x37));
}

#[test]
fn visibility_palette_proofs_preserve_sparse_threshold_and_solid_diagonals() {
    let kind = PaletteKind::block_states();
    assert!(models().occludes(StateId::new(4).unwrap()));
    let mut solid_air = snapshot(PalettedContainer::new(kind, 4), 0);
    solid_air.sections[13] = Neighbour::Present(Arc::new(ChunkSection::from_containers(
        PalettedContainer::new(kind, 4), PalettedContainer::new(PaletteKind::biomes(), 0), 4,
    )));
    assert_eq!(solid_air.at(0, 0, 0).non_air_count(), 0);
    assert_eq!(snapshot_visibility(&solid_air, models()), lodestone_render::SectionVisibility::solid());
    for (state, expected) in [(0, lodestone_render::SectionVisibility::all()),
        (4, lodestone_render::SectionVisibility::solid()),
        (u32::MAX, lodestone_render::SectionVisibility::all())]
    {
        assert_eq!(snapshot_visibility(&snapshot(PalettedContainer::new(kind, state), 0), models()), expected);
    }
    for width in [15, 16] {
        let mut center = PalettedContainer::new(kind, 0);
        for y in 0..16 {
            for z in 0..width { center.set(kind.index(8, y, z), 4); }
        }
        let visibility = snapshot_visibility(&snapshot(center, 0), models());
        if width == 15 {
            assert_eq!(visibility, lodestone_render::SectionVisibility::all());
        } else {
            assert!(!visibility.connects(Face::PosX, Face::NegX));
            assert!(visibility.connects(Face::PosX, Face::PosX));
        }
    }
}

fn section(blocks: PalettedContainer) -> Arc<ChunkSection> {
    Arc::new(ChunkSection::from_containers(
        blocks, PalettedContainer::new(PaletteKind::biomes(), 0), 0,
    ))
}

fn snapshot(center: PalettedContainer, neighbor: u32) -> SectionSnapshot {
    let neighbor = section(PalettedContainer::new(PaletteKind::block_states(), neighbor));
    let mut sections: Vec<_> = (0..27)
        .map(|_| Neighbour::Present(Arc::clone(&neighbor))).collect();
    sections[13] = Neighbour::Present(section(center));
    SectionSnapshot {
        key: SectionKey { cx: 0, cz: 0, si: 1, min_y: -64 },
        sections,
        lights: vec![None; 27],
        sky_default: SkyDefault::Full,
        biome_names: Arc::from([]),
        light_revision: None,
    }
}

#[cfg(test)]
fn mixed_dry_snapshot() -> SectionSnapshot {
    let kind = PaletteKind::block_states();
    let values: Vec<u32> = (0..kind.entry_count()).map(|index| (index % 3) as u32).collect();
    snapshot(PalettedContainer::from_values(kind, &values), water_state())
}

#[cfg(test)]
fn assert_same_geometry(actual: &FluidMeshes, expected: &FluidMeshes) {
    for (actual, expected) in [(&actual.water, &expected.water), (&actual.lava, &expected.lava)] {
        assert_eq!(bytemuck::cast_slice::<_, u8>(&actual.vertices), bytemuck::cast_slice::<_, u8>(&expected.vertices));
        assert_eq!(actual.indices, expected.indices);
    }
}

#[test]
fn mixed_dry_palette_skips_fluid_grid_despite_wet_halo() {
    let snapshot = mixed_dry_snapshot();
    assert_eq!(snapshot.at(0, 0, 0).block_states().palette_len(), 3);
    assert_eq!(snapshot.at(0, 0, 0).block_states().single_value(), None);
    assert!(center_is_dry(&snapshot, models()));
    let actual = mesh_snapshot_fluids_at(&snapshot, models(), 0);
    let baseline = mesh_snapshot_fluids_with_grid(&snapshot, models(), 0);
    assert_eq!(actual.water.quad_count() + actual.lava.quad_count(), 0);
    assert_same_geometry(&actual, &baseline);
}

#[test]
fn wet_and_waterlogged_cells_use_the_grid_and_keep_geometry() {
    for state in [water_state(), 3] {
        let mut center = PalettedContainer::new(PaletteKind::block_states(), 0);
        center.set((8 << 8) | (8 << 4) | 8, state);
        let snapshot = snapshot(center, 0);
        assert!(!center_is_dry(&snapshot, models()), "state {state}");
        let actual = mesh_snapshot_fluids_at(&snapshot, models(), 0);
        let baseline = mesh_snapshot_fluids_with_grid(&snapshot, models(), 0);
        assert_same_geometry(&actual, &baseline);
        assert!(actual.water.quad_count() > 0, "state {state} must exercise fluid output");
        if state == water_state() {
            assert_eq!(actual.water.quad_count(), 11, "two top, one bottom, eight side quads");
        }
        assert_eq!(actual.lava.quad_count(), 0);
    }
}

#[test]
fn unused_wet_entries_unknown_ids_and_direct_storage_fall_back() {
    let kind = PaletteKind::block_states();
    let mut unused_wet = PalettedContainer::new(kind, 1);
    unused_wet.set(100, water_state());
    unused_wet.set(100, 1);
    assert_eq!(unused_wet.palette_values(), Some([1, water_state()].as_slice()));
    let unused_snapshot = snapshot(unused_wet, 0);
    assert!(!center_is_dry(&unused_snapshot, models()));
    let actual = mesh_snapshot_fluids_at(&unused_snapshot, models(), 0);
    assert_eq!(actual.water.quad_count() + actual.lava.quad_count(), 0);

    let unknown = snapshot(PalettedContainer::new(kind, u32::MAX), 0);
    assert!(!center_is_dry(&unknown, models()));
    let values: Vec<u32> = (0..kind.entry_count()).map(|index| index as u32 + 100).collect();
    let direct = snapshot(PalettedContainer::from_values(kind, &values), 0);
    assert_eq!(direct.at(0, 0, 0).block_states().palette_values(), None);
    assert!(!center_is_dry(&direct, models()));
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
#[ignore = "release timing diagnostic"]
fn dry_center_fluid_timing_control() {
    assert!(!cfg!(debug_assertions), "run this diagnostic with --release");
    let snapshot = mixed_dry_snapshot();
    let models = models();
    let iterations = 256;
    let measure = |with_grid| {
        let start = crate::platform::Instant::now();
        for _ in 0..iterations {
            let snapshot = std::hint::black_box(&snapshot);
            let models = std::hint::black_box(models);
            let mesh = if with_grid {
                mesh_snapshot_fluids_with_grid(snapshot, models, 0)
            } else {
                mesh_snapshot_fluids_at(snapshot, models, 0)
            };
            assert_eq!(std::hint::black_box(mesh).water.quad_count(), 0);
        }
        start.elapsed()
    };
    let _ = measure(true);
    for round in 0..3 {
        let (grid, dry) = if round % 2 == 0 {
            (measure(true), measure(false))
        } else {
            let dry = measure(false);
            (measure(true), dry)
        };
        eprintln!(
            "dry-center fluid timing: round={round} iterations={iterations} grid_ms={:.3} palette_ms={:.3}",
            grid.as_secs_f64() * 1000.0, dry.as_secs_f64() * 1000.0,
        );
    }
}
