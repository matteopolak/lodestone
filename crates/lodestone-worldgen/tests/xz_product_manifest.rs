//! Production construction control for the shared pure-X/Z product plan.

use std::path::{Path, PathBuf};

use lodestone_worldgen::density::{Builder, NoiseParams, Resolver};
use lodestone_worldgen::overworld::GeneratedColumn;
use lodestone_worldgen::engine::XzProductManifest;
use serde_json::Value;
use sha2::{Digest, Sha256};

fn has_wrapped_signature(density: &lodestone_worldgen::density::Density, target: &[u64]) -> bool {
    use lodestone_worldgen::density::Density;

    let signature = |density: &Density| {
        let mut out = Vec::new();
        density.write_signature(&mut out);
        out
    };
    match density {
        Density::FlatCache { inner, .. } | Density::Cache2D { inner, .. } => {
            (inner.is_xz_pure() && signature(inner) == target)
                || has_wrapped_signature(inner, target)
        }
        Density::Add(a, b)
        | Density::Mul(a, b)
        | Density::Min(a, b)
        | Density::Max(a, b) => {
            has_wrapped_signature(a, target) || has_wrapped_signature(b, target)
        }
        Density::Abs(a)
        | Density::Square(a)
        | Density::Cube(a)
        | Density::HalfNegative(a)
        | Density::QuarterNegative(a)
        | Density::Squeeze(a)
        | Density::Invert(a)
        | Density::Marker(a)
        | Density::Interpolated { inner: a, .. } => has_wrapped_signature(a, target),
        Density::Clamp { input, .. } => has_wrapped_signature(input, target),
        Density::RangeChoice {
            input,
            when_in_range,
            when_out_of_range,
            ..
        } => {
            has_wrapped_signature(input, target)
                || has_wrapped_signature(when_in_range, target)
                || has_wrapped_signature(when_out_of_range, target)
        }
        Density::IntervalSelect {
            input, functions, ..
        } => {
            has_wrapped_signature(input, target)
                || functions
                    .iter()
                    .any(|function| has_wrapped_signature(function, target))
        }
        Density::FindTopSurface {
            density,
            upper_bound,
            ..
        } => {
            has_wrapped_signature(density, target) || has_wrapped_signature(upper_bound, target)
        }
        Density::ShiftedNoise {
            shift_x,
            shift_y,
            shift_z,
            ..
        } => {
            has_wrapped_signature(shift_x, target)
                || has_wrapped_signature(shift_y, target)
                || has_wrapped_signature(shift_z, target)
        }
        Density::Const(_)
        | Density::BlendAlpha
        | Density::BlendOffset
        | Density::Beardifier
        | Density::YClampedGradient { .. }
        | Density::Noise { .. }
        | Density::ShiftA(_)
        | Density::ShiftB(_)
        | Density::Shift(_)
        | Density::Spline(_)
        | Density::Blended(_)
        | Density::EndIslands(_) => false,
    }
}

fn product_signature(density: &lodestone_worldgen::density::Density) -> Vec<u64> {
    use lodestone_worldgen::density::Density;

    match density {
        Density::FlatCache { inner, .. } | Density::Cache2D { inner, .. } => {
            let mut out = Vec::new();
            inner.write_signature(&mut out);
            out
        }
        _ => {
            let mut out = Vec::new();
            density.write_signature(&mut out);
            out
        }
    }
}

struct FixtureResolver {
    root: PathBuf,
}

impl FixtureResolver {
    fn read(&self, kind: &str, id: &str) -> Value {
        let name = id.strip_prefix("minecraft:").unwrap_or(id);
        let path = self.root.join(kind).join(format!("{name}.json"));
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("reading {}: {error}", path.display()));
        serde_json::from_str(&text)
            .unwrap_or_else(|error| panic!("parsing {}: {error}", path.display()))
    }
}

impl Resolver for FixtureResolver {
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

fn digest(column: &GeneratedColumn) -> [u8; 32] {
    let mut hasher = Sha256::new();
    for y in column.min_y()..column.min_y() + column.height() {
        for z in 0..16 {
            for x in 0..16 {
                hasher.update(column.block_state_id(x, y, z).raw().to_le_bytes());
                hasher.update([0]);
            }
        }
    }
    hasher.finalize().into()
}

#[test]
fn fixture_routes_admit_one_matching_manifest() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/worldgen_data");
    let resolver = FixtureResolver { root: root.clone() };
    let settings: Value = serde_json::from_str(
        &std::fs::read_to_string(root.join("noise_settings/overworld.json"))
            .expect("read overworld settings"),
    )
    .expect("parse overworld settings");
    let router = &settings["noise_router"];
    let builder = Builder::new(42, &resolver);
    let final_density = builder.build(&router["final_density"]).expect("final density");
    let preliminary = builder
        .build(&router["preliminary_surface_level"])
        .expect("preliminary surface level");
    let factor = builder
        .build(&Value::String("minecraft:overworld/factor".to_owned()))
        .expect("factor");
    let offset = builder
        .build(&Value::String("minecraft:overworld/offset".to_owned()))
        .expect("offset");
    assert!(factor.is_xz_pure(), "factor route must be X/Z pure");
    assert!(offset.is_xz_pure(), "offset route must be X/Z pure");
    let factor_signature = product_signature(&factor);
    let offset_signature = product_signature(&offset);
    assert_ne!(factor_signature, offset_signature, "factor and offset must differ");
    assert!(
        has_wrapped_signature(&preliminary, &factor_signature),
        "preliminary route must wrap factor"
    );
    assert!(
        has_wrapped_signature(&preliminary, &offset_signature),
        "preliminary route must wrap offset"
    );
    assert!(
        has_wrapped_signature(&final_density, &factor_signature),
        "final route must wrap factor"
    );
    let _manifest = XzProductManifest::from_routes(
        42,
        &preliminary,
        &final_density,
        &factor,
        &offset,
    )
    .expect("fixture routes must admit factor/offset products");
}

#[test]
#[ignore = "embedded production generation; run explicitly with --ignored"]
fn embedded_overworld_uses_one_matching_xz_manifest_and_stays_bit_identical() {
    let first = lodestone_server::overworld_generator(42);
    let second = lodestone_server::overworld_generator(42);
    let fingerprint = first
        .xz_product_fingerprint()
        .expect("embedded overworld routes must admit factor/offset products");
    assert_eq!(second.xz_product_fingerprint(), Some(fingerprint));

    first.with_batch_lease(&[(0, 0)], |lease| {
        assert_eq!(
            lease.prepare_pre_ore_targets_with_radius(&[(0, 0)], 0),
            1,
            "the production region prefix must prepare its admitted target"
        );
    });

    let first_column = first.column(0, 0);
    let second_column = second.column(0, 0);
    assert!(first_column.non_air_count() > 0);
    assert_eq!(digest(&first_column), digest(&second_column));
}
