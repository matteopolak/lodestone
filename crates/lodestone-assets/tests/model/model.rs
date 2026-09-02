//! Tests for block model parsing and resolution.

use lodestone_assets::MemorySource;
use lodestone_assets::{
    Axis, Direction, ModelResolver, RawModel, ResourceLocation, ResourceManager, TextureBinding,
};

/// Builds a manager whose single pack holds the given `(model_path, json)` pairs,
/// where `model_path` is like `block/stone` (stored under assets/.../models/).
fn manager_with_models(models: &[(&str, &str)]) -> ResourceManager {
    let mut src = MemorySource::new("test");
    for (path, json) in models {
        src.insert(
            format!("assets/minecraft/models/{path}.json"),
            json.as_bytes().to_vec(),
        );
    }
    ResourceManager::new(vec![Box::new(src)])
}

// --- raw parsing ---

#[test]
fn parses_raw_model_fields() {
    let json = r##"{
        "parent":"minecraft:block/cube_all",
        "ambientocclusion":false,
        "gui_light":"front",
        "texture_size":[32,32],
        "textures":{"all":"block/stone"},
        "elements":[{
            "from":[0,0,0],"to":[16,16,16],
            "rotation":{"origin":[8,8,8],"axis":"y","angle":45,"rescale":true},
            "faces":{
                "up":{"uv":[0,0,16,16],"texture":"#all","cullface":"up","tintindex":0,"rotation":90}
            }
        }]
    }"##;
    let m = RawModel::parse(json.as_bytes()).unwrap();
    assert_eq!(
        m.parent,
        Some(ResourceLocation::parse("minecraft:block/cube_all").unwrap())
    );
    assert_eq!(m.ambient_occlusion, Some(false));
    assert_eq!(m.gui_light.as_deref(), Some("front"));
    assert_eq!(m.texture_size, Some([32, 32]));
    assert_eq!(
        m.textures.get("all").map(String::as_str),
        Some("block/stone")
    );

    let el = &m.elements.as_ref().unwrap()[0];
    assert_eq!(el.from, [0.0, 0.0, 0.0]);
    assert_eq!(el.to, [16.0, 16.0, 16.0]);
    let rot = el.rotation.as_ref().unwrap();
    assert_eq!(rot.origin, [8.0, 8.0, 8.0]);
    assert_eq!(rot.angles, [0.0, 45.0, 0.0]);
    assert_eq!(rot.single_axis(), Some((Axis::Y, 45.0)));
    assert!(rot.rescale);

    let face = el.faces.get(&Direction::Up).unwrap();
    assert_eq!(face.uv, Some([0.0, 0.0, 16.0, 16.0]));
    assert_eq!(face.texture, "#all");
    assert_eq!(face.cullface, Some(Direction::Up));
    assert_eq!(face.tintindex, Some(0));
    assert_eq!(face.rotation, 90);
}

#[test]
fn raw_model_defaults() {
    let m = RawModel::parse(br##"{"textures":{}}"##).unwrap();
    assert!(m.parent.is_none());
    assert!(m.elements.is_none());
    assert_eq!(m.ambient_occlusion, None);
}

#[test]
fn object_form_texture_value_uses_sprite() {
    // 26.2 introduced an object form for translucent textures:
    // `{"sprite": "<loc>", "force_translucent": true}` (glass, ice, redstone,
    // slime, honey, ...). The geometry layer only needs the sprite reference.
    let m = RawModel::parse(
        br##"{"textures":{"all":{"force_translucent":true,"sprite":"minecraft:block/glass"}}}"##,
    )
    .unwrap();
    assert_eq!(
        m.textures.get("all").map(String::as_str),
        Some("minecraft:block/glass")
    );
}

#[test]
fn malformed_model_json_errors() {
    assert!(RawModel::parse(b"not json").is_err());
    assert!(RawModel::parse(br##"{"parent":"BAD NS"}"##).is_err());
}

// --- resolution ---

#[test]
fn resolves_parent_chain_and_texture_variables() {
    // stone -> cube_all -> cube -> block (like real vanilla)
    let manager = manager_with_models(&[
        (
            "block/stone",
            r##"{"parent":"block/cube_all","textures":{"all":"block/stone"}}"##,
        ),
        (
            "block/cube_all",
            r##"{"parent":"block/cube","textures":{"up":"#all","down":"#all","north":"#all"}}"##,
        ),
        (
            "block/cube",
            r##"{"parent":"block/block","elements":[{"from":[0,0,0],"to":[16,16,16],"faces":{"up":{"texture":"#up"},"down":{"texture":"#down"}}}]}"##,
        ),
        ("block/block", r##"{"gui_light":"side"}"##),
    ]);
    let resolver = ModelResolver::new(&manager);
    let resolved = resolver
        .resolve(&ResourceLocation::parse("block/stone").unwrap())
        .unwrap();

    // elements come from block/cube (nearest ancestor that defines them).
    assert_eq!(resolved.elements.len(), 1);
    // #up resolves through cube_all(#all) -> stone(block/stone).
    let up = resolved.resolve_texture("#up").unwrap();
    assert_eq!(up.to_string(), "minecraft:block/stone");
    // face texture references resolve too.
    let face = resolved.elements[0].faces.get(&Direction::Up).unwrap();
    let tex = resolved.resolve_texture(&face.texture).unwrap();
    assert_eq!(tex.to_string(), "minecraft:block/stone");
    assert!(resolved.unresolved_textures().is_empty());
}

#[test]
fn child_elements_override_parent_elements() {
    let manager = manager_with_models(&[
        (
            "block/child",
            r##"{"parent":"block/parent","elements":[{"from":[1,1,1],"to":[2,2,2],"faces":{}}]}"##,
        ),
        (
            "block/parent",
            r##"{"elements":[{"from":[0,0,0],"to":[16,16,16],"faces":{}}]}"##,
        ),
    ]);
    let resolver = ModelResolver::new(&manager);
    let resolved = resolver
        .resolve(&ResourceLocation::parse("block/child").unwrap())
        .unwrap();
    assert_eq!(resolved.elements[0].from, [1.0, 1.0, 1.0]);
}

#[test]
fn inherits_parent_elements_when_child_has_none() {
    let manager = manager_with_models(&[
        (
            "block/child",
            r##"{"parent":"block/parent","textures":{"all":"block/x"}}"##,
        ),
        (
            "block/parent",
            r##"{"elements":[{"from":[0,0,0],"to":[16,16,16],"faces":{"up":{"texture":"#all"}}}]}"##,
        ),
    ]);
    let resolver = ModelResolver::new(&manager);
    let resolved = resolver
        .resolve(&ResourceLocation::parse("block/child").unwrap())
        .unwrap();
    assert_eq!(resolved.elements.len(), 1);
    assert_eq!(
        resolved.resolve_texture("#all").unwrap().to_string(),
        "minecraft:block/x"
    );
}

#[test]
fn detects_parent_cycle() {
    let manager = manager_with_models(&[
        ("block/a", r##"{"parent":"block/b"}"##),
        ("block/b", r##"{"parent":"block/a"}"##),
    ]);
    let resolver = ModelResolver::new(&manager);
    let err = resolver
        .resolve(&ResourceLocation::parse("block/a").unwrap())
        .unwrap_err();
    assert!(format!("{err}").to_lowercase().contains("cycle"));
}

#[test]
fn missing_model_is_error() {
    let manager = manager_with_models(&[("block/a", r##"{"parent":"block/missing"}"##)]);
    let resolver = ModelResolver::new(&manager);
    assert!(
        resolver
            .resolve(&ResourceLocation::parse("block/a").unwrap())
            .is_err()
    );
}

#[test]
fn unresolved_texture_variable_is_detectable() {
    // #missing is never defined; #cyclic points in a loop.
    let manager = manager_with_models(&[(
        "block/x",
        r##"{"textures":{"a":"#b","b":"#a","c":"#missing"},"elements":[{"from":[0,0,0],"to":[1,1,1],"faces":{"up":{"texture":"#a"}}}]}"##,
    )]);
    let resolver = ModelResolver::new(&manager);
    let resolved = resolver
        .resolve(&ResourceLocation::parse("block/x").unwrap())
        .unwrap();
    assert!(resolved.resolve_texture("#a").is_none());
    assert!(resolved.resolve_texture("#c").is_none());
    let unresolved = resolved.unresolved_textures();
    assert!(unresolved.contains(&"a".to_string()));
    assert!(unresolved.contains(&"c".to_string()));
    // A binding can be inspected directly.
    assert!(matches!(
        resolved.textures.get("c"),
        Some(TextureBinding::Unresolved(_))
    ));
}

#[test]
fn euler_element_rotation_is_parsed() {
    // Vanilla 1.21+ hanging-sign models use a Euler x/y/z rotation instead of
    // the classic single-axis form. Both must parse.
    let json = r##"{
        "elements":[{
            "from":[2,11,11],"to":[5,15,11],
            "rotation":{"x":180,"y":-67.5,"z":-180,"origin":[8,0,8]},
            "faces":{"north":{"uv":[0,0,2,2],"texture":"#all"}}
        }]
    }"##;
    let m = RawModel::parse(json.as_bytes()).unwrap();
    let rot = m.elements.as_ref().unwrap()[0].rotation.as_ref().unwrap();
    assert_eq!(rot.origin, [8.0, 0.0, 8.0]);
    assert_eq!(rot.angles, [180.0, -67.5, -180.0]);
    // A general Euler rotation is not reducible to a single axis.
    assert_eq!(rot.single_axis(), None);
    assert!(!rot.rescale);
}
