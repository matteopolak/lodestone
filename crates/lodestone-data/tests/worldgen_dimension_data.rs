//! Nether/End worldgen data: hermetic checks over the committed dimension
//! files, plus an `#[ignore]`d drift guard that re-extracts them from the real
//! 26.2 server jar and asserts byte-for-byte equality.
//!
//! ## Why this lives in `lodestone-data`
//!
//! The files themselves are `lodestone-server`'s embedded asset bundle
//! (`crates/lodestone-server/assets/worldgen/`, see that crate's `build.rs`),
//! but the *provenance* claim — "these bytes came out of the jar unmodified" —
//! is a data-census concern, and this crate is where every other
//! generate-or-assert jar guard lives (`collision_shapes.rs`, `hardness.rs`,
//! `block_states.rs`). Reading across to a sibling crate's assets is
//! established practice here; `lodestone-sound`'s `biome_music_table.rs` does
//! the same with `assets/worldgen/biome/`.
//!
//! ## What each test buys
//!
//! * [`dimension_reference_closure_resolves_in_bundle`] — the defect this phase
//!   fixed was **exactly** a dangling reference: `noise_settings/nether.json`
//!   names `minecraft:nether/temperature` and `minecraft:nether/vegetation`,
//!   and neither noise was bundled. Nothing in the tree noticed, because no
//!   code loaded `nether.json` yet. This test walks the dimension settings,
//!   follows density-function references transitively, and fails if any
//!   referenced document is absent — so the same class of gap cannot land
//!   again ahead of the engine that would consume it.
//! * [`dimension_settings_carry_the_engine_relevant_scalars`] — pins the
//!   handful of scalars the Nether/End engine gap report is built on
//!   (`sea_level`, `aquifers_enabled`, `legacy_random_source`, `default_fluid`,
//!   and the noise cell sizes). Those values are *why* the report says the
//!   engine needs a legacy random source and parameterised cell dimensions; if
//!   a data bump moves them, the report is stale and this fails loudly rather
//!   than the report quietly misleading someone.
//! * [`end_islands_is_the_only_novel_density_function_type`] — pins the
//!   density-function type census across the dimension documents.
//! * [`bundled_dimension_files_match_the_jar`] (`#[ignore]`d) — the provenance
//!   gate. The jar is gitignored (`.gitignore:7 /.cache/`), so this cannot run
//!   in the default suite; it is the same arrangement as
//!   `collision_shapes.rs`'s dump-backed guard.
//!
//! Refresh the committed files from the jar after a data bump with:
//!
//! ```text
//! LODESTONE_REGEN=1 cargo test -p lodestone-data --test worldgen_dimension_data \
//!     bundled_dimension_files_match_the_jar -- --ignored --nocapture
//! ```

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::process::Command;

use serde_json::Value;

/// The four documents this phase added to the bundle, as bundle-relative
/// paths. Each is a byte-for-byte copy of the identically-named entry under
/// `data/minecraft/worldgen/` in the server jar.
///
/// `noise_settings/{amplified,caves,floating_islands,large_biomes}.json` are
/// deliberately **not** here: they are overworld variants / datapack-only
/// samples rather than dimension documents, and were extracted separately.
const DIMENSION_FILES: &[&str] = &[
    "noise_settings/nether.json",
    "noise_settings/end.json",
    "noise/nether/temperature.json",
    "noise/nether/vegetation.json",
];

/// The two dimension `noise_settings` documents whose reference closure this
/// test walks.
const DIMENSION_SETTINGS: &[&str] = &["nether", "end"];

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// `crates/lodestone-server/assets/worldgen/`.
fn bundle_dir() -> PathBuf {
    manifest_dir().join("../lodestone-server/assets/worldgen")
}

/// The real 26.2 server jar inside the bundler wrapper — gitignored, so only
/// the `#[ignore]`d guard may depend on it.
fn jar_path() -> PathBuf {
    manifest_dir().join("../../.cache/mc/26.2/versions/26.2/server-26.2.jar")
}

fn read_json(path: &PathBuf) -> Value {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("reading {path:?}: {e}"));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("parsing {path:?}: {e}"))
}

// ---------------------------------------------------------------------------
// Reference closure
// ---------------------------------------------------------------------------

/// Which registry a `minecraft:`-prefixed string names, decided by the JSON key
/// it sits under.
///
/// The key set is **measured**, not assumed: across `noise_settings/nether.json`,
/// `noise_settings/end.json` and every density function they reach, the only
/// keys carrying a `minecraft:` string are `Name` (block-state names inside
/// `default_block` / `default_fluid` / `result_state`), `random_name` (surface-rule
/// RNG stream names), `type` (the density-function / surface-rule type tag),
/// `noise` (a `noise` registry reference), `biome_is` (biome ids in a surface-rule
/// condition) and `argument2` (a density-function reference).
///
/// Anything else is treated as a density-function reference and *must* resolve,
/// so a future data bump that introduces a reference under a new key fails
/// loudly here instead of silently going unchecked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Registry {
    DensityFunction,
    Noise,
    Biome,
}

impl Registry {
    fn dir(self) -> &'static str {
        match self {
            Registry::DensityFunction => "density_function",
            Registry::Noise => "noise",
            Registry::Biome => "biome",
        }
    }
}

/// Keys whose `minecraft:` values are *not* registry references.
const NON_REFERENCE_KEYS: &[&str] = &["Name", "random_name", "type"];

fn classify(key: &str) -> Option<Registry> {
    if NON_REFERENCE_KEYS.contains(&key) {
        return None;
    }
    match key {
        "noise" => Some(Registry::Noise),
        "biome_is" => Some(Registry::Biome),
        _ => Some(Registry::DensityFunction),
    }
}

/// Strips the `minecraft:` namespace, returning `None` for a foreign namespace
/// (there are none today; a modded id would be a real finding, not a skip).
fn strip_namespace(id: &str) -> Option<&str> {
    id.strip_prefix("minecraft:")
}

#[derive(Default)]
struct Closure {
    density_functions: BTreeSet<String>,
    noises: BTreeSet<String>,
    biomes: BTreeSet<String>,
}

impl Closure {
    fn insert(&mut self, reg: Registry, id: &str) -> bool {
        let set = match reg {
            Registry::DensityFunction => &mut self.density_functions,
            Registry::Noise => &mut self.noises,
            Registry::Biome => &mut self.biomes,
        };
        set.insert(id.to_string())
    }
}

/// Walks one JSON document, recording every registry reference it names.
/// `key` is the JSON key the current node sits under; list elements inherit
/// their list's key, matching how the vanilla codecs read these documents.
fn walk(node: &Value, key: &str, out: &mut Closure, fresh: &mut Vec<String>) {
    match node {
        Value::Object(map) => {
            for (k, v) in map {
                walk(v, k, out, fresh);
            }
        }
        Value::Array(items) => {
            for v in items {
                walk(v, key, out, fresh);
            }
        }
        Value::String(s) => {
            let Some(id) = strip_namespace(s) else {
                return;
            };
            let Some(reg) = classify(key) else {
                return;
            };
            if out.insert(reg, id) && reg == Registry::DensityFunction {
                fresh.push(id.to_string());
            }
        }
        _ => {}
    }
}

/// Builds the transitive closure over the dimension `noise_settings`, following
/// density-function references into their own documents.
fn dimension_closure() -> Closure {
    let bundle = bundle_dir();
    let mut out = Closure::default();
    let mut queue: Vec<String> = Vec::new();

    for setting in DIMENSION_SETTINGS {
        let path = bundle.join(format!("noise_settings/{setting}.json"));
        let doc = read_json(&path);
        walk(&doc, "<root>", &mut out, &mut queue);
    }

    // Transitively resolve density functions. Every reference must exist —
    // that is the assertion this whole test rests on.
    while let Some(id) = queue.pop() {
        let path = bundle.join(format!("density_function/{id}.json"));
        assert!(
            path.is_file(),
            "density_function/{id}.json is referenced by a dimension noise_settings \
             document but is not bundled (looked in {})",
            bundle.display()
        );
        let doc = read_json(&path);
        walk(&doc, "<root>", &mut out, &mut queue);
    }

    out
}

#[test]
fn dimension_reference_closure_resolves_in_bundle() {
    let closure = dimension_closure();

    // Every reference resolves to a bundled document. `dimension_closure`
    // already proved this for density functions (it had to, in order to keep
    // walking); noises and biomes are checked here.
    let bundle = bundle_dir();
    let mut missing: Vec<String> = Vec::new();
    for (reg, ids) in [
        (Registry::Noise, &closure.noises),
        (Registry::Biome, &closure.biomes),
        (Registry::DensityFunction, &closure.density_functions),
    ] {
        for id in ids {
            let rel = format!("{}/{id}.json", reg.dir());
            if !bundle.join(&rel).is_file() {
                missing.push(rel);
            }
        }
    }
    assert!(
        missing.is_empty(),
        "dimension noise_settings reference documents that are not bundled: {missing:#?}"
    );

    // Magnitude, not just sign: a closure that shrank because a reference
    // stopped being followed would still pass the loop above. These counts are
    // measured against the jar (2026-08-07) — nether reaches
    // `nether/base_3d_noise`, end reaches `end/sloped_cheese` which in turn
    // reaches `end/base_3d_noise`.
    assert_eq!(
        closure.density_functions,
        BTreeSet::from([
            "end/base_3d_noise".to_string(),
            "end/sloped_cheese".to_string(),
            "nether/base_3d_noise".to_string(),
        ]),
        "density-function closure changed"
    );
    assert_eq!(
        closure.noises.len(),
        8,
        "noise closure changed: {:#?}",
        closure.noises
    );
    // The two noises this phase added — the whole reason the closure gate
    // exists. Both are Nether-biome noises, wired specially in vanilla's own
    // noise-random-state object (see the engine gap report).
    for required in ["nether/temperature", "nether/vegetation"] {
        assert!(
            closure.noises.contains(required),
            "expected the dimension closure to reach {required}, got {:#?}",
            closure.noises
        );
    }
    assert_eq!(
        closure.biomes,
        BTreeSet::from([
            "basalt_deltas".to_string(),
            "crimson_forest".to_string(),
            "nether_wastes".to_string(),
            "soul_sand_valley".to_string(),
            "warped_forest".to_string(),
        ]),
        "surface-rule biome references changed"
    );
}

#[test]
fn dimension_settings_carry_the_engine_relevant_scalars() {
    let bundle = bundle_dir();
    let read = |name: &str| read_json(&bundle.join(format!("noise_settings/{name}.json")));

    let overworld = read("overworld");
    let nether = read("nether");
    let end = read("end");

    // The Overworld row is the control: it is what the engine implements
    // today, so every difference below is a concrete engine requirement.
    let scalars = |v: &Value| {
        (
            v["sea_level"].as_i64().expect("sea_level"),
            v["aquifers_enabled"].as_bool().expect("aquifers_enabled"),
            v["legacy_random_source"]
                .as_bool()
                .expect("legacy_random_source"),
            v["ore_veins_enabled"].as_bool().expect("ore_veins_enabled"),
            v["default_fluid"]["Name"]
                .as_str()
                .expect("default_fluid.Name")
                .to_string(),
        )
    };

    assert_eq!(
        scalars(&overworld),
        (63, true, false, true, "minecraft:water".to_string()),
        "overworld scalars moved — the Nether/End gap report's control changed"
    );
    assert_eq!(
        scalars(&nether),
        (32, false, true, false, "minecraft:lava".to_string()),
        "nether scalars moved"
    );
    assert_eq!(
        scalars(&end),
        (0, false, true, false, "minecraft:air".to_string()),
        "end scalars moved"
    );

    // Cell dimensions. Vanilla derives cell size as its own quart-to-block
    // conversion (`size * 4`) applied to the noise-settings cell-height/
    // cell-width accessors,
    // so the Overworld's 4x8 cell is (size_horizontal 1, size_vertical 2) and
    // the End's is (2, 1) => an 8-wide, 4-tall cell. The engine's
    // `CELL_WIDTH`/`CELL_HEIGHT` are `const` 4/8, so the End needs these
    // plumbed rather than hardcoded.
    let cell = |v: &Value| {
        (
            v["noise"]["min_y"].as_i64().expect("min_y"),
            v["noise"]["height"].as_i64().expect("height"),
            v["noise"]["size_horizontal"].as_i64().expect("size_horizontal") * 4,
            v["noise"]["size_vertical"].as_i64().expect("size_vertical") * 4,
        )
    };
    assert_eq!(cell(&overworld), (-64, 384, 4, 8), "overworld cell geometry");
    assert_eq!(cell(&nether), (0, 128, 4, 8), "nether cell geometry");
    assert_eq!(
        cell(&end),
        (0, 128, 8, 4),
        "end cell geometry — an 8-wide/4-tall cell is the End's distinguishing \
         engine requirement; if this changed, re-read the gap report"
    );
}

#[test]
fn end_islands_is_the_only_novel_density_function_type() {
    let bundle = bundle_dir();
    let mut types: BTreeMap<String, usize> = BTreeMap::new();

    fn count(node: &Value, types: &mut BTreeMap<String, usize>) {
        match node {
            Value::Object(map) => {
                if let Some(Value::String(t)) = map.get("type") {
                    if let Some(id) = strip_namespace(t) {
                        *types.entry(id.to_string()).or_default() += 1;
                    }
                }
                for v in map.values() {
                    count(v, types);
                }
            }
            Value::Array(items) => {
                for v in items {
                    count(v, types);
                }
            }
            _ => {}
        }
    }

    for setting in DIMENSION_SETTINGS {
        count(
            &read_json(&bundle.join(format!("noise_settings/{setting}.json"))),
            &mut types,
        );
    }
    let closure = dimension_closure();
    for df in &closure.density_functions {
        count(
            &read_json(&bundle.join(format!("density_function/{df}.json"))),
            &mut types,
        );
    }

    // `end_islands` is the one density-function type the engine does not
    // implement, and it appears **twice**, not once: inline in
    // `noise_settings/end.json` and again inside the already-bundled
    // `density_function/end/sloped_cheese.json`. Anyone implementing it must
    // handle both sites.
    assert_eq!(
        types.get("end_islands").copied(),
        Some(2),
        "expected exactly 2 end_islands uses across the dimension documents; \
         full type census: {types:#?}"
    );

    // 26.2 has no `weird_scaled_sampler` — the caves are expressed with
    // `interval_select`. Asserted so nobody ports a type that no longer exists.
    assert!(
        !types.contains_key("weird_scaled_sampler"),
        "weird_scaled_sampler reappeared in 26.2 data: {types:#?}"
    );
}

// ---------------------------------------------------------------------------
// Nether multi-noise biome parameter list
// ---------------------------------------------------------------------------

/// `biome_parameters/nether.json` — the resolved NETHER multi-noise parameter
/// table, dumped by `scripts/worldgen-oracle/NetherParametersOracle.java`.
///
/// It is **not** a jar file copy, which is why it is absent from
/// [`DIMENSION_FILES`]: the jar's
/// `multi_noise_biome_source_parameter_list/nether.json` is 37 bytes of
/// `{"preset": "minecraft:nether"}`, because the table lives in Java
/// (vanilla's own multi-noise-biome-source-parameter-list nether preset) and
/// its codec only ever
/// serialises the preset id. The committed file is the oracle's output.
const NETHER_PARAMETERS: &str = "biome_parameters/nether.json";

/// Parses the 14-column table into `(row, biome)` pairs. Column order is
/// `lodestone_worldgen::biome::parse_table`'s: temperature, humidity,
/// continentalness, erosion, depth, weirdness (each `min,max`), then the scalar
/// `offset`, then the biome id.
fn nether_rows() -> Vec<([i64; 13], String)> {
    let doc = read_json(&bundle_dir().join(NETHER_PARAMETERS));
    doc.as_array()
        .expect("nether parameter table is a JSON array")
        .iter()
        .map(|row| {
            let row = row.as_array().expect("row is an array");
            assert_eq!(row.len(), 14, "row must be 13 numbers + a biome id: {row:?}");
            let mut nums = [0i64; 13];
            for (i, slot) in nums.iter_mut().enumerate() {
                *slot = row[i].as_i64().unwrap_or_else(|| {
                    panic!("column {i} is not an integer (the table stores quantized longs)")
                });
            }
            let biome = row[13].as_str().expect("column 13 is the biome id").to_string();
            (nums, biome)
        })
        .collect()
}

#[test]
fn nether_biome_parameters_agree_with_the_nether_surface_rule_biome_set() {
    // Two independently-extracted documents must name the same five biomes:
    // `noise_settings/nether.json` is a byte copy out of the jar, while
    // `biome_parameters/nether.json` came from a JVM dump of a Java-hardcoded
    // table. Neither was derived from the other, so agreement here is real
    // cross-validation rather than a round trip.
    let closure = dimension_closure();
    let from_params: BTreeSet<String> = nether_rows()
        .into_iter()
        .map(|(_, biome)| {
            strip_namespace(&biome)
                .unwrap_or_else(|| panic!("biome id must be minecraft-namespaced: {biome}"))
                .to_string()
        })
        .collect();

    assert_eq!(
        from_params, closure.biomes,
        "the NETHER parameter table's biome set and nether.json's surface-rule \
         biome set disagree; one of the two extractions is wrong"
    );

    // And every one resolves to a bundled biome document.
    let bundle = bundle_dir();
    for biome in &from_params {
        assert!(
            bundle.join(format!("biome/{biome}.json")).is_file(),
            "biome/{biome}.json named by the NETHER parameter table is not bundled"
        );
    }
}

#[test]
fn nether_biome_parameters_are_degenerate_points_discriminated_by_offset() {
    let rows = nether_rows();

    // Magnitude, not sign: the exact quantized table. Vanilla's own climate
    // parameters'
    // 7-float overload wraps each channel in a degenerate point (= `span(v,v)`)
    // and quantizes by 10000 (vanilla's own quantization-factor constant, applied in
    // vanilla's own coordinate-quantizer), so every bound is an
    // exact multiple of the source constant: crimson_forest's 0.4F -> 4000,
    // warped_forest's 0.375F offset -> 3750, basalt_deltas' 0.175F -> 1750.
    let expected: Vec<([i64; 13], &str)> = vec![
        ([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], "minecraft:nether_wastes"),
        ([0, 0, -5000, -5000, 0, 0, 0, 0, 0, 0, 0, 0, 0], "minecraft:soul_sand_valley"),
        ([4000, 4000, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], "minecraft:crimson_forest"),
        ([0, 0, 5000, 5000, 0, 0, 0, 0, 0, 0, 0, 0, 3750], "minecraft:warped_forest"),
        ([-5000, -5000, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1750], "minecraft:basalt_deltas"),
    ];
    let actual: Vec<([i64; 13], &str)> =
        rows.iter().map(|(n, b)| (*n, b.as_str())).collect();
    assert_eq!(
        actual, expected,
        "NETHER parameter table changed — row order is load-bearing only as a \
         tie-break, but the values are the biome layout itself"
    );

    // Structural properties the engine relies on, asserted rather than assumed:
    for (nums, biome) in &rows {
        // Every channel is a degenerate point (min == max). Unlike the
        // overworld table, the nether list never uses a span parameter.
        for ch in 0..6 {
            assert_eq!(
                nums[ch * 2],
                nums[ch * 2 + 1],
                "{biome} channel {ch} is a span, not a point"
            );
        }
        // continentalness / erosion / depth / weirdness are all zero, because
        // the nether router sets those channels to `zero()`
        // (vanilla's own noise-router-data nether preset). Only temperature, humidity and
        // offset discriminate — which is why the two nether-specific noises
        // matter so much: they ARE the biome layout.
        for ch in 2..6 {
            assert_eq!(nums[ch * 2], 0, "{biome} channel {ch} is nonzero");
        }
    }

    // Exactly two rows carry a nonzero offset. Vanilla's own climate
    // parameter-point "fitness" step adds the offset squared as a flat
    // penalty, so
    // these two are the deliberately-rarer biomes. Our engine reaches the same
    // number via `params[6].distance(0)^2`, which equals `offset^2` for either
    // sign — equivalent, not a gap.
    let with_offset = rows.iter().filter(|(n, _)| n[12] != 0).count();
    assert_eq!(with_offset, 2, "expected warped_forest and basalt_deltas to be offset");
}

// ---------------------------------------------------------------------------
// Provenance guard (jar-backed, `#[ignore]`d)
// ---------------------------------------------------------------------------

/// Reads one entry out of the server jar. Shells to `unzip -p` rather than
/// taking a `zip` dev-dependency: this crate's dependency list is deliberately
/// one entry long, and a new dev-dep would churn the shared `Cargo.lock`.
fn jar_entry(jar: &PathBuf, entry: &str) -> Vec<u8> {
    let out = Command::new("unzip")
        .arg("-p")
        .arg(jar)
        .arg(entry)
        .output()
        .unwrap_or_else(|e| panic!("running unzip -p {jar:?} {entry}: {e}"));
    assert!(
        out.status.success(),
        "unzip -p {} {entry} failed: {}",
        jar.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !out.stdout.is_empty(),
        "jar entry {entry} is empty — the jar layout changed, or the entry is absent"
    );
    out.stdout
}

#[test]
#[ignore = "needs the gitignored 26.2 server jar under .cache/mc"]
fn bundled_dimension_files_match_the_jar() {
    let jar = jar_path();
    assert!(
        jar.is_file(),
        "server jar not found at {} — fetch it before running this guard",
        jar.display()
    );
    let bundle = bundle_dir();
    let regen = std::env::var_os("LODESTONE_REGEN").is_some();

    let mut drifted: Vec<String> = Vec::new();
    for rel in DIMENSION_FILES {
        let jar_bytes = jar_entry(&jar, &format!("data/minecraft/worldgen/{rel}"));
        let dest = bundle.join(rel);

        if regen {
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent).expect("creating destination directory");
            }
            std::fs::write(&dest, &jar_bytes).unwrap_or_else(|e| panic!("writing {dest:?}: {e}"));
            println!("regenerated {rel} ({} bytes)", jar_bytes.len());
            continue;
        }

        let have = std::fs::read(&dest).unwrap_or_else(|e| panic!("reading {dest:?}: {e}"));
        if have == jar_bytes {
            println!("ok {rel} ({} bytes, byte-identical)", jar_bytes.len());
        } else {
            drifted.push(format!(
                "{rel}: bundled {} bytes, jar {} bytes",
                have.len(),
                jar_bytes.len()
            ));
        }
    }

    assert!(
        drifted.is_empty(),
        "bundled dimension files differ from the jar:\n{}\n\nRefresh with \
         LODESTONE_REGEN=1 (see this file's module doc).",
        drifted.join("\n")
    );
}

#[test]
#[ignore = "runs the JVM oracle in a container against the 26.2 server jar"]
fn nether_biome_parameters_match_the_jvm_oracle() {
    let script = manifest_dir().join("../../scripts/worldgen-oracle/run.sh");
    assert!(
        script.is_file(),
        "oracle runner not found at {}",
        script.display()
    );

    let out = Command::new("bash")
        .arg(&script)
        .arg("NetherParametersOracle")
        .output()
        .unwrap_or_else(|e| panic!("running {script:?} NetherParametersOracle: {e}"));
    assert!(
        out.status.success(),
        "oracle failed ({}):\n{}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );

    // The oracle writes nothing but the JSON document to stdout. If a stray log
    // line ever leaks in — log4j over System.err comes back out on stdout once
    // vanilla's own bootstrap step has run, which the oracle's first version did —
    // this parse fails loudly rather than the bytes being written through.
    let dumped = String::from_utf8(out.stdout).expect("oracle stdout is UTF-8");
    let parsed: Value = serde_json::from_str(&dumped).unwrap_or_else(|e| {
        panic!("oracle stdout is not valid JSON ({e}); first 200 bytes:\n{:.200}", dumped)
    });
    assert_eq!(
        parsed.as_array().map(Vec::len),
        Some(5),
        "expected 5 NETHER parameter rows from the oracle"
    );

    let dest = bundle_dir().join(NETHER_PARAMETERS);
    if std::env::var_os("LODESTONE_REGEN").is_some() {
        std::fs::write(&dest, dumped.as_bytes())
            .unwrap_or_else(|e| panic!("writing {dest:?}: {e}"));
        println!("regenerated {NETHER_PARAMETERS} ({} bytes)", dumped.len());
        return;
    }

    let have = std::fs::read_to_string(&dest).unwrap_or_else(|e| panic!("reading {dest:?}: {e}"));
    assert_eq!(
        have, dumped,
        "{NETHER_PARAMETERS} differs from the JVM oracle's output. Refresh with \
         LODESTONE_REGEN=1 (see this file's module doc)."
    );
    println!("ok {NETHER_PARAMETERS} ({} bytes, identical to the oracle)", dumped.len());
}
