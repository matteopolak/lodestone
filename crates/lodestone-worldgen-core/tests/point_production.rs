//! Bounded production-data controls for the compiled point evaluator.

use std::path::{Path, PathBuf};

use lodestone_worldgen_core::density::{Builder, Context, NoiseChunkSampler, NoiseParams, Resolver};
use lodestone_worldgen_core::engine::{PointProgram, PointScratch, Program};
use serde_json::Value;

struct FsResolver {
    root: PathBuf,
}

impl FsResolver {
    fn read(&self, kind: &str, id: &str) -> Value {
        let name = id.strip_prefix("minecraft:").unwrap_or(id);
        let path = self.root.join(kind).join(format!("{name}.json"));
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("reading {}: {error}", path.display()));
        serde_json::from_str(&text)
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

fn production_fixture() -> (FsResolver, Value) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../lodestone-worldgen/tests/support/worldgen_data");
    let resolver = FsResolver { root: root.clone() };
    let settings = serde_json::from_str(
        &std::fs::read_to_string(root.join("noise_settings/overworld.json")).unwrap(),
    )
    .unwrap();
    (resolver, settings)
}

#[test]
fn production_preliminary_surface_batch_is_bit_exact() {
    let (resolver, settings) = production_fixture();
    let builder = Builder::new(42, &resolver);
    let tree = builder
        .build(&settings["noise_router"]["preliminary_surface_level"])
        .expect("production preliminary surface tree");
    let program = PointProgram::compile(&tree);

    assert!(program.is_find_top_surface());
    assert!(program.spline_count() > 0, "surface control must reach spline payloads");
    assert!(program.cache_node_count() >= 4);
    assert!(program.shared_nodes() > 0);
    println!(
        "point production graph: nodes={} shared={} cache_nodes={}",
        program.node_count(),
        program.shared_nodes(),
        program.cache_node_count()
    );

    let contexts: Vec<_> = [(-33, -17), (-4, 0), (0, 0), (7, 19), (41, -26), (88, 64)]
        .into_iter()
        .flat_map(|(x, z)| {
            [-65, -64, -63, -40, -32, 0, 240, 256, 320, 321]
                .into_iter()
                .map(move |y| Context::new(x, y, z))
        })
        .collect();
    let mut batch = vec![0.0; contexts.len()];
    let mut scratch = PointScratch::with_capacity(64);
    program.compute_batch(&contexts, &mut batch, &mut scratch);

    for (context, compiled) in contexts.iter().copied().zip(batch) {
        assert_eq!(
            compiled.to_bits(),
            tree.compute(context).to_bits(),
            "production preliminary mismatch at ({}, {}, {})",
            context.x,
            context.y,
            context.z
        );
    }
}

/// Differential control for compiling spline payloads: the bundled climate and
/// surface routes exercise both ordinary and nested spline values at the same
/// positions used by the point and field callers. Raw bits are compared so a
/// changed interval tie, f32 widening, or signed-zero result cannot hide behind
/// approximate equality.
#[test]
fn bundled_spline_point_and_field_paths_are_bit_exact() {
    let (resolver, settings) = production_fixture();
    let router = &settings["noise_router"];
    let contexts = [
        Context::new(-32, 0, -16),
        Context::new(-4, 0, 0),
        Context::new(0, 0, 0),
        Context::new(8, 0, 20),
        Context::new(40, 0, -24),
        Context::new(88, 0, 64),
    ];

    for key in [
        "continents",
        "erosion",
        "ridges",
        "temperature",
        "vegetation",
        "depth",
        "final_density",
    ] {
        let builder = Builder::new(42, &resolver);
        let tree = builder
            .build(&router[key])
            .unwrap_or_else(|error| panic!("bundled {key} density-function: {error}"));
        let program = PointProgram::compile(&tree);
        let field_program = Program::compile(&tree);
        if key == "final_density" {
            assert!(
                field_program.spline_count() > 0,
                "bundled final-density field control must reach spline payloads"
            );
        }
        let sampler = NoiseChunkSampler::from_program(
            field_program,
            builder.slot_count(),
            4,
            8,
            None,
        );
        let mut scratch = PointScratch::with_capacity(128);

        for context in contexts {
            let expected = tree.compute(context);
            let point = program.compute(context, &mut scratch);
            let field = sampler.sample(context.x, context.y, context.z);
            assert_eq!(
                point.to_bits(),
                expected.to_bits(),
                "point spline mismatch in {key} at ({},{},{})",
                context.x,
                context.y,
                context.z
            );
            assert_eq!(
                field.to_bits(),
                expected.to_bits(),
                "field spline mismatch in {key} at ({},{},{})",
                context.x,
                context.y,
                context.z
            );
        }
    }
}
