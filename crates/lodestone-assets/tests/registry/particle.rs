//! Particle definition parsing (`particles/*.json`).

use lodestone_assets::ParticleError;
use lodestone_assets::particle::ParticleDefinition;

fn parse(json: &str) -> Result<ParticleDefinition, ParticleError> {
    ParticleDefinition::parse(json.as_bytes())
}

#[test]
fn parses_texture_list_in_order() {
    let def = parse(r#"{"textures":["minecraft:effect_1","minecraft:effect_0"]}"#).unwrap();
    assert_eq!(def.textures.len(), 2);
    assert_eq!(def.textures[0].to_string(), "minecraft:effect_1");
    assert_eq!(def.textures[1].to_string(), "minecraft:effect_0");
}

#[test]
fn default_namespace_is_applied() {
    let def = parse(r#"{"textures":["angry"]}"#).unwrap();
    assert_eq!(def.textures[0].to_string(), "minecraft:angry");
}

#[test]
fn missing_textures_key_is_tolerated() {
    let def = parse(r#"{}"#).unwrap();
    assert!(def.textures.is_empty());
    // Explicit null is also tolerated.
    let def = parse(r#"{"textures":null}"#).unwrap();
    assert!(def.textures.is_empty());
}

#[test]
fn textures_not_an_array_is_rejected() {
    assert!(matches!(
        parse(r#"{"textures":"nope"}"#),
        Err(ParticleError::Json(_))
    ));
}

#[test]
fn malformed_json_is_rejected() {
    assert!(matches!(parse("not json"), Err(ParticleError::Json(_))));
}

#[test]
fn texture_paths_use_particle_prefix() {
    let def = parse(r#"{"textures":["minecraft:effect_7","mypack:spark"]}"#).unwrap();
    assert_eq!(
        def.texture_paths(),
        vec![
            "assets/minecraft/textures/particle/effect_7.png".to_string(),
            "assets/mypack/textures/particle/spark.png".to_string(),
        ]
    );
}
