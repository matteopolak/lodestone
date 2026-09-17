//! Bounded production-data controls for the compiled point evaluator.

use std::path::{Path, PathBuf};

use lodestone_worldgen_core::density::{Builder, Context, NoiseParams, Resolver};
use lodestone_worldgen_core::engine::{PointProgram, PointScratch};
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
