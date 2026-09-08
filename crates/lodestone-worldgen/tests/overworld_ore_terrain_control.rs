//! Focused external-value control for an ore-placement mismatch.

use lodestone_server::overworld_generator;

#[test]
#[ignore = "diagnostic external-value control"]
fn coal_extra_draw_target_uses_external_terrain_control() {
    // The independent server dump records andesite at this candidate. It is
    // therefore not a coal target and must not consume the exposure draw that
    // follows the first nineteen candidates of the final blob.
    let generator = overworld_generator(42);
    let column = generator.column_shaped(-10, -9);
    let actual = column.block_state(15, 31, 2);
    assert_eq!(actual, "minecraft:andesite", "external control at (-145,31,-142)");
}
