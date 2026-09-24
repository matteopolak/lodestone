//! Focused production-density controls for the pure cell evaluator.

use std::path::{Path, PathBuf};

use lodestone_worldgen_core::density::{
    Builder, Context, Density, NoiseChunkSampler, NoiseParams, Resolver,
};
use lodestone_worldgen_core::engine::{Bounds, PointProgram, PointScratch, Program, XzProductLattice,
    XzRect};
use serde_json::Value;

struct FsResolver {
    root: PathBuf,
}

impl FsResolver {
    fn read(&self, kind: &str, id: &str) -> Value {
        let name = id.strip_prefix("minecraft:").unwrap_or(id);
        let path = self.root.join(kind).join(format!("{name}.json"));
        serde_json::from_str(
            &std::fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("reading {}: {error}", path.display())),
        )
        .unwrap_or_else(|error| panic!("parsing {}: {error}", path.display()))
    }
}

impl Resolver for FsResolver {
    fn density_function(&self, id: &str) -> Value {
        self.read("density_function", id)
    }

    fn noise(&self, id: &str) -> NoiseParams {
        let value = self.read("noise", id);
        NoiseParams {
            first_octave: value["firstOctave"].as_i64().expect("firstOctave") as i32,
            amplitudes: value["amplitudes"]
                .as_array()
                .expect("amplitudes")
                .iter()
                .map(|amplitude| amplitude.as_f64().expect("amplitude"))
                .collect(),
        }
    }
}

fn production_program() -> (Program, usize) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../lodestone-worldgen/tests/support/worldgen_data");
    let resolver = FsResolver { root: root.clone() };
    let settings: Value = serde_json::from_str(
        &std::fs::read_to_string(root.join("noise_settings/overworld.json")).unwrap(),
    )
    .unwrap();
    let builder = Builder::new(42, &resolver);
    let tree = builder
        .build(&settings["noise_router"]["final_density"])
        .expect("bundled final_density density-function");
    let program = Program::compile(&tree);
    (program, builder.slot_count())
}

fn production_program_with_products() -> (Program, Program, usize, std::sync::Arc<XzProductLattice>) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../lodestone-worldgen/tests/support/worldgen_data");
    let resolver = FsResolver { root: root.clone() };
    let settings: Value = serde_json::from_str(
        &std::fs::read_to_string(root.join("noise_settings/overworld.json")).unwrap(),
    )
    .unwrap();
    let builder = Builder::new(42, &resolver);
    let router = &settings["noise_router"];
    let final_density = builder
        .build(&router["final_density"])
        .expect("bundled final_density density-function");
    let preliminary = builder
        .build(&router["preliminary_surface_level"])
        .expect("bundled preliminary_surface_level density-function");
    let factor = builder
        .build(&Value::String("minecraft:overworld/factor".to_owned()))
        .expect("bundled factor density-function");
    let offset = builder
        .build(&Value::String("minecraft:overworld/offset".to_owned()))
        .expect("bundled offset density-function");
    let manifest = lodestone_worldgen_core::engine::XzProductManifest::from_routes(
        42,
        &preliminary,
        &final_density,
        &factor,
        &offset,
    )
    .expect("bundled factor/offset products must be admitted");
    let final_program = Program::compile_with_xz_products(&final_density, manifest.clone());
    let preliminary_program = PointProgram::compile_with_xz_products(&preliminary, manifest);
    let fingerprint = final_program
        .xz_product_fingerprint()
        .expect("bundled final-density graph must retain product identity");
    let rect = XzRect::new(0, 0, 3, 3);
    let mut lattice = XzProductLattice::new(rect, fingerprint);
    let mut values = vec![(0.0, 0.0); 9];
    let contexts: Vec<_> = (0..3)
        .flat_map(|qz| (0..3).map(move |qx| Context::new(qx * 4, 0, qz * 4)))
        .collect();
    assert!(preliminary_program.compute_xz_products(
        &contexts,
        &mut values,
        &mut PointScratch::new(),
    ));
    for (context, &(factor, offset)) in contexts.iter().zip(&values) {
        lattice.insert_pair(context.x, context.z, factor, offset);
    }
    (
        Program::compile(&final_density),
        final_program,
        builder.slot_count(),
        std::sync::Arc::new(lattice),
    )
}

fn digest(sampler: &NoiseChunkSampler, cells: bool) -> u64 {
    let mut digest = 0xcbf29ce484222325_u64;
    for z0 in [0, 4] {
        for x0 in [0, 4] {
            for y0 in [-64, -56] {
                if cells {
                    let mut values = [0.0; 128];
                    sampler.final_density_cell(x0, y0, z0, &mut values);
                    for value in values {
                        digest ^= value.to_bits();
                        digest = digest.wrapping_mul(0x100000001b3);
                    }
                } else {
                    for lz in 0..4 {
                        for lx in 0..4 {
                            for ly in 0..8 {
                                let value = sampler.final_density(x0 + lx, y0 + ly, z0 + lz);
                                digest ^= value.to_bits();
                                digest = digest.wrapping_mul(0x100000001b3);
                            }
                        }
                    }
                }
            }
        }
    }
    digest
}

#[test]
fn bundled_final_density_tile_is_bit_exact_and_admitted() {
    let (program, slots) = production_program();
    assert!(program.node_count() > 100);
    assert!(program.has_overworld_final_density_cell_plan());
    assert!(program.tile_eligible_node_count() > 0);
    assert!(program.tile_plan_count() > 0);
    let nodes = program.node_count();
    let tile_eligible = program.tile_eligible_node_count();
    let bounds = Bounds {
        x: (0, 7),
        y: (-64, -49),
        z: (0, 7),
    };
    let optimized = NoiseChunkSampler::from_program(
        program.clone(),
        slots,
        4,
        8,
        Some(bounds),
    );
    let scalar = NoiseChunkSampler::from_program(program, slots, 4, 8, Some(bounds));
    let expected = digest(&scalar, false);
    let actual = digest(&optimized, true);
    assert_eq!(actual, expected, "tile evaluator changed bundled density bits");
    println!(
        "FIELD_TILE_PRODUCTION nodes={} tile_eligible={} digest={actual:016x}",
        nodes,
        tile_eligible,
    );
}

#[test]
fn bundled_product_density_uses_product_admission_with_scalar_fallback() {
    let (baseline_program, optimized_program, slots, products) = production_program_with_products();
    assert!(optimized_program.has_overworld_final_density_cell_plan());
    let bounds = Bounds {
        x: (0, 7),
        y: (-64, -49),
        z: (0, 7),
    };
    let baseline = NoiseChunkSampler::from_program(
        baseline_program,
        slots,
        4,
        8,
        Some(bounds),
    );
    let optimized = NoiseChunkSampler::from_program_with_xz_products(
        optimized_program,
        slots,
        4,
        8,
        Some(bounds),
        Some(products),
    );
    assert_eq!(digest(&optimized, true), digest(&baseline, false));
}

#[cfg(feature = "gen-counters")]
#[test]
fn bundled_product_density_reduces_field_visits_without_bypassing_slots() {
    let (baseline_program, optimized_program, slots, products) = production_program_with_products();
    let bounds = Bounds {
        x: (0, 7),
        y: (-64, -49),
        z: (0, 7),
    };
    let baseline = NoiseChunkSampler::from_program(
        baseline_program,
        slots,
        4,
        8,
        Some(bounds),
    );
    lodestone_worldgen_core::engine::redundancy_probe::reset();
    lodestone_worldgen_core::engine::redundancy_probe::enable();
    let expected = std::hint::black_box(digest(&baseline, false));
    lodestone_worldgen_core::engine::redundancy_probe::disable();
    let baseline_visits = lodestone_worldgen_core::engine::redundancy_probe::snapshot()
        .field_total();

    let optimized = NoiseChunkSampler::from_program_with_xz_products(
        optimized_program,
        slots,
        4,
        8,
        Some(bounds),
        Some(std::sync::Arc::clone(&products)),
    );
    lodestone_worldgen_core::engine::redundancy_probe::reset();
    lodestone_worldgen_core::engine::redundancy_probe::enable();
    let actual = std::hint::black_box(digest(&optimized, true));
    lodestone_worldgen_core::engine::redundancy_probe::disable();
    let optimized_snapshot = lodestone_worldgen_core::engine::redundancy_probe::snapshot();
    let optimized_visits = optimized_snapshot.field_total();

    assert_eq!(actual, expected);
    assert!(products.hits() > 0, "optimized field path did not read X/Z products");
    assert!(optimized_visits < baseline_visits, "optimized={optimized_visits} baseline={baseline_visits}");
    println!(
        "FIELD_PRODUCT_VISITS baseline={} optimized={} product_hits={} product_misses={} digest={actual:016x}",
        baseline_visits,
        optimized_visits,
        products.hits(),
        products.misses(),
    );
}

#[test]
fn cache_writer_is_a_negative_control_for_tile_admission() {
    let root = Density::FlatCache {
        inner: Box::new(Density::Const(1.0)),
        slot: 0,
        memo: lodestone_worldgen_core::density::XzMemoId::NONE,
    };
    let program = Program::compile(&root);
    assert!(program.tile_eligible_node_count() < program.node_count());
    assert_eq!(program.tile_plan_count(), 0);
    assert!(!program.has_overworld_final_density_cell_plan());
}

#[cfg(feature = "gen-counters")]
#[test]
fn bundled_final_density_tile_reduces_graph_visits() {
    let (program, slots) = production_program();
    let bounds = Bounds {
        x: (0, 7),
        y: (-64, -49),
        z: (0, 7),
    };
    let scalar = NoiseChunkSampler::from_program(
        program.clone(),
        slots,
        4,
        8,
        Some(bounds),
    );
    lodestone_worldgen_core::engine::redundancy_probe::reset();
    lodestone_worldgen_core::engine::redundancy_probe::enable();
    let expected = std::hint::black_box(digest(&scalar, false));
    lodestone_worldgen_core::engine::redundancy_probe::disable();
    let scalar_visits = lodestone_worldgen_core::engine::redundancy_probe::snapshot()
        .field_total();

    let optimized = NoiseChunkSampler::from_program(program, slots, 4, 8, Some(bounds));
    lodestone_worldgen_core::engine::redundancy_probe::reset();
    lodestone_worldgen_core::engine::redundancy_probe::enable();
    let actual = std::hint::black_box(digest(&optimized, true));
    lodestone_worldgen_core::engine::redundancy_probe::disable();
    let tile_visits = lodestone_worldgen_core::engine::redundancy_probe::snapshot().field_total();

    assert_eq!(actual, expected);
    assert!(tile_visits < scalar_visits, "tile={tile_visits} scalar={scalar_visits}");
    println!(
        "FIELD_TILE_VISITS scalar={} tile={} digest={actual:016x}",
        scalar_visits, tile_visits,
    );
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "local retired-instruction and cycle control for production density tiles"]
#[allow(unsafe_code)]
fn bundled_final_density_tile_instruction_cycle_control() {
    use std::hint::black_box;
    use std::mem::size_of;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct RusageInfoV4 {
        ri_uuid: [u8; 16],
        fields: [u64; 36],
    }

    impl Default for RusageInfoV4 {
        fn default() -> Self {
            Self {
                ri_uuid: [0; 16],
                fields: [0; 36],
            }
        }
    }

    unsafe extern "C" {
        fn proc_pid_rusage(pid: i32, flavor: i32, buffer: *mut core::ffi::c_void) -> i32;
    }

    fn usage() -> (u64, u64) {
        assert_eq!(size_of::<RusageInfoV4>(), 16 + 36 * 8);
        let mut info = RusageInfoV4::default();
        let result = unsafe {
            proc_pid_rusage(
                i32::try_from(std::process::id()).unwrap(),
                4,
                (&raw mut info).cast::<core::ffi::c_void>(),
            )
        };
        assert_eq!(result, 0, "proc_pid_rusage failed with {result}");
        (info.fields[29], info.fields[30])
    }

    let (program, slots) = production_program();
    let bounds = Bounds {
        x: (0, 7),
        y: (-64, -49),
        z: (0, 7),
    };
    let scalar_check = NoiseChunkSampler::from_program(
        program.clone(),
        slots,
        4,
        8,
        Some(bounds),
    );
    let optimized_check = NoiseChunkSampler::from_program(
        program.clone(),
        slots,
        4,
        8,
        Some(bounds),
    );
    assert_eq!(digest(&optimized_check, true), digest(&scalar_check, false));
    let scalar = NoiseChunkSampler::from_program(
        program.clone(),
        slots,
        4,
        8,
        Some(bounds),
    );
    let optimized = NoiseChunkSampler::from_program(program, slots, 4, 8, Some(bounds));
    let before_scalar = usage();
    let scalar_digest = black_box(digest(&scalar, false));
    let after_scalar = usage();
    let before_optimized = usage();
    let tile_digest = black_box(digest(&optimized, true));
    let after_optimized = usage();
    assert_eq!(scalar_digest, tile_digest);
    println!(
        "FIELD_TILE_CONTROL scalar_instructions={} tile_instructions={} \
         scalar_cycles={} tile_cycles={} digest={tile_digest:016x}",
        after_scalar.0.saturating_sub(before_scalar.0),
        after_optimized.0.saturating_sub(before_optimized.0),
        after_scalar.1.saturating_sub(before_scalar.1),
        after_optimized.1.saturating_sub(before_optimized.1),
    );
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "local retired-instruction and cycle control for product-admitted cells"]
#[allow(unsafe_code)]
fn bundled_product_density_product_instruction_cycle_control() {
    use std::hint::black_box;
    use std::mem::size_of;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct RusageInfoV4 {
        ri_uuid: [u8; 16],
        fields: [u64; 36],
    }

    impl Default for RusageInfoV4 {
        fn default() -> Self {
            Self {
                ri_uuid: [0; 16],
                fields: [0; 36],
            }
        }
    }

    unsafe extern "C" {
        fn proc_pid_rusage(pid: i32, flavor: i32, buffer: *mut core::ffi::c_void) -> i32;
    }

    fn usage() -> (u64, u64) {
        assert_eq!(size_of::<RusageInfoV4>(), 16 + 36 * 8);
        let mut info = RusageInfoV4::default();
        let result = unsafe {
            proc_pid_rusage(
                i32::try_from(std::process::id()).expect("pid fits in i32"),
                4,
                (&raw mut info).cast::<core::ffi::c_void>(),
            )
        };
        assert_eq!(result, 0, "proc_pid_rusage failed with {result}");
        (info.fields[29], info.fields[30])
    }

    let (baseline_program, optimized_program, slots, products) = production_program_with_products();
    let bounds = Bounds {
        x: (0, 7),
        y: (-64, -49),
        z: (0, 7),
    };
    let baseline = NoiseChunkSampler::from_program(
        baseline_program,
        slots,
        4,
        8,
        Some(bounds),
    );
    let optimized = NoiseChunkSampler::from_program_with_xz_products(
        optimized_program,
        slots,
        4,
        8,
        Some(bounds),
        Some(products),
    );
    let before_baseline = usage();
    let baseline_digest = black_box(digest(&baseline, false));
    let after_baseline = usage();
    let before_optimized = usage();
    let optimized_digest = black_box(digest(&optimized, true));
    let after_optimized = usage();
    assert_eq!(optimized_digest, baseline_digest);
    println!(
        "FIELD_PRODUCT_CONTROL baseline_instructions={} optimized_instructions={} \
         baseline_cycles={} optimized_cycles={} digest={optimized_digest:016x}",
        after_baseline
            .0
            .saturating_sub(before_baseline.0),
        after_optimized.0.saturating_sub(before_optimized.0),
        after_baseline
            .1
            .saturating_sub(before_baseline.1),
        after_optimized.1.saturating_sub(before_optimized.1),
    );
}
