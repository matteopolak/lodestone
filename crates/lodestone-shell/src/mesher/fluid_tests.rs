use super::*;
use lodestone_assets::{MemorySource, ResourceManager};
use lodestone_render::BlocksJsonRegistry;
use lodestone_world::PalettedContainer;

fn water_state() -> u32 {
    StateId::from_state_str("minecraft:water[level=0]").unwrap().raw()
}

fn models() -> &'static BlockModels {
    static MODELS: OnceLock<BlockModels> = OnceLock::new();
    MODELS.get_or_init(|| {
        let mut texture = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut texture, 1, 1);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            encoder.write_header().unwrap().write_image_data(&[255; 4]).unwrap();
        }
        let mut source = MemorySource::new("dry-center-fluid-test");
        source.insert("assets/minecraft/textures/block/water_still.png", texture.clone());
        source.insert("assets/minecraft/textures/block/water_flow.png", texture);
        let manager = ResourceManager::new(vec![Box::new(source)]);
        let report = serde_json::json!({
            "minecraft:air": {"states": [{"id": 0, "default": true}]},
            "test:dry_a": {"states": [{"id": 1, "default": true}]},
            "test:dry_b": {"states": [{"id": 2, "default": true}]},
            "test:waterlogged": {"states": [{"id": 3, "default": true, "properties": {"waterlogged": "true"}}]},
            "minecraft:water": {"states": [{"id": water_state(), "default": true, "properties": {"level": "0"}}]}
        });
        let registry = BlocksJsonRegistry::from_slice(&serde_json::to_vec(&report).unwrap()).unwrap();
        BlockModels::build_with_mip_levels(&manager, &registry, 0).unwrap()
    })
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
    }
}

fn mixed_dry_snapshot() -> SectionSnapshot {
    let kind = PaletteKind::block_states();
    let values: Vec<u32> = (0..kind.entry_count()).map(|index| (index % 3) as u32).collect();
    snapshot(PalettedContainer::from_values(kind, &values), water_state())
}

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
