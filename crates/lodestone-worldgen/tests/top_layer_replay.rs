//! Focused production seam control for source-local top-layer replay.
//!
//! The test uses the embedded generator only to prove that the public replay
//! method honors a resident override.  The independent fixture/oracle controls
//! for the top-layer algorithm remain in `lodestone-server`.

use std::collections::BTreeMap;

use lodestone_server::overworld_generator;
use lodestone_worldgen::feature::top_layer::SNOW_LAYER;
use lodestone_worldgen_parity::lifecycle::top_layer_spills;

const SEED: i64 = 42;

#[test]
#[ignore = "embedded production generation; run explicitly with --ignored"]
fn source_top_layer_replay_reads_the_resident_override() {
    let generator = overworld_generator(SEED);
    let source = (-64..=64)
        .flat_map(|cz| (-64..=64).map(move |cx| (cx, cz)))
        .find(|&(cx, cz)| {
            let biome = generator.biome_at_quart(cx * 4 + 2, 15, cz * 4 + 2);
            biome.contains("snow")
                || biome.contains("windswept_hills")
                || biome == "minecraft:grove"
        })
        .expect("seed 42 must expose a cold source in the search window");

    let cold = top_layer_spills(&generator, source, &BTreeMap::new());
    let witness = cold
        .iter()
        .find(|spill| spill.state == SNOW_LAYER)
        .expect("the selected cold source must produce a snow-layer spill");
    assert!(
        cold.iter().all(|spill| {
            (spill.position.0.div_euclid(16), spill.position.2.div_euclid(16)) == source
        }),
        "source-local top-layer replay must not emit a neighbour spill"
    );

    // Fill the witness column with motion-blocking stone in the resident view.
    // The top-layer body then has no free in-bounds position in that column.
    // If the replay method ignored overrides, the original snow witness would
    // still be emitted.
    let x = witness.position.0;
    let z = witness.position.2;
    let overrides = (generator.min_y()..generator.min_y() + generator.height())
        .map(|y| ((x, y, z), "minecraft:stone".to_owned()))
        .collect::<BTreeMap<_, _>>();
    let blocked = top_layer_spills(&generator, source, &overrides);
    assert!(
        blocked.iter().all(|spill| spill.position != witness.position),
        "resident override must suppress the original snow witness"
    );
}
