//! External controls for the configured-carver serde boundary.
//!
//! The valid documents are the same files consumed by the parity harness. The
//! invalid documents live beside this test so a schema regression cannot be
//! hidden by constructing a `serde_json::Value` with the same assumptions as
//! the parser under test.

use lodestone_worldgen::carver::CarverConfig;

#[test]
fn bundled_carvers_decode_through_the_typed_schema() {
    for (name, json) in [
        (
            "cave",
            include_str!("../../lodestone-server/assets/worldgen/configured_carver/cave.json"),
        ),
        (
            "cave_extra_underground",
            include_str!("../../lodestone-server/assets/worldgen/configured_carver/cave_extra_underground.json"),
        ),
        (
            "nether_cave",
            include_str!("../../lodestone-server/assets/worldgen/configured_carver/nether_cave.json"),
        ),
        (
            "canyon",
            include_str!("../../lodestone-server/assets/worldgen/configured_carver/canyon.json"),
        ),
    ] {
        CarverConfig::parse_json(json)
            .unwrap_or_else(|error| panic!("bundled {name} carver must decode: {error}"));
    }
}

#[test]
fn malformed_document_is_rejected_with_a_field_path() {
    let error = CarverConfig::parse_json(include_str!(
        "support/configured_carver_invalid/malformed.json"
    ))
    .expect_err("missing max_inclusive must be rejected");
    let message = error.to_string();
    assert!(
        message.contains("config.floor_level"),
        "missing path in {message}"
    );
}

#[test]
fn unknown_config_field_is_rejected() {
    let error = CarverConfig::parse_json(include_str!(
        "support/configured_carver_invalid/unknown_field.json"
    ))
    .expect_err("unknown fields must not be silently accepted");
    assert!(error.to_string().contains("unexpected"), "error: {error}");
}

#[test]
fn unknown_carver_discriminator_is_rejected() {
    let error = CarverConfig::parse_json(include_str!(
        "support/configured_carver_invalid/unknown_discriminator.json"
    ))
    .expect_err("unknown carver kinds must not degrade to a default");
    assert!(error.to_string().contains("minecraft:spiral"), "error: {error}");
}
