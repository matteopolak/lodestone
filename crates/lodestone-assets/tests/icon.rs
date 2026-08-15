//! Hermetic tests for the item -> drawable inventory icon pipeline.
//!
//! Each fixture is a real 26.2 shape (an `items/<id>.json` definition plus the
//! `models/...` it points at) built into an in-memory pack, so the whole pipeline
//! runs without `client.jar`. The cases are chosen so a wrong implementation
//! diverges: a generated item must become a flat *sprite* stack (not geometry), a
//! block item must become a *model* reference under the GUI transform, and a
//! chest must surface as a *special* renderer (the ex-`builtin/entity` seam that
//! only `items/*.json` reveals).

use std::path::PathBuf;

use lodestone_assets::{
    DisplayTransform, GuiItemContext, GuiLight, IconPart, ItemIconBuilder, ResourceLocation,
    ResourceManager, MemorySource, ZipSource,
};

fn loc(s: &str) -> ResourceLocation {
    ResourceLocation::parse(s).unwrap()
}

/// Locates a fetched vanilla `client.jar`, preferring 26.2, matching the
/// convention in `tests/real_jar.rs`.
fn client_jar() -> Option<PathBuf> {
    let cache = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()?
        .parent()?
        .join(".cache/mc");
    let preferred = cache.join("26.2").join("client.jar");
    if preferred.is_file() {
        return Some(preferred);
    }
    let entries = std::fs::read_dir(&cache).ok()?;
    for entry in entries.flatten() {
        let candidate = entry.path().join("client.jar");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Builds a single-pack manager from `(in_pack_path, contents)` pairs.
fn manager(files: &[(&str, &str)]) -> ResourceManager {
    let mut src = MemorySource::new("test");
    for (path, body) in files {
        src.insert((*path).to_string(), body.as_bytes().to_vec());
    }
    ResourceManager::new(vec![Box::new(src)])
}

#[test]
fn generated_item_becomes_a_sprite_stack() {
    let mgr = manager(&[
        (
            "assets/minecraft/items/diamond_sword.json",
            r#"{"model":{"type":"minecraft:model","model":"minecraft:item/diamond_sword"}}"#,
        ),
        (
            "assets/minecraft/models/item/diamond_sword.json",
            r#"{"parent":"minecraft:item/generated","textures":{"layer0":"minecraft:item/diamond_sword"}}"#,
        ),
        (
            "assets/minecraft/models/item/generated.json",
            r#"{"parent":"minecraft:builtin/generated"}"#,
        ),
    ]);
    let builder = ItemIconBuilder::new(&mgr);
    let icon = builder.icon(&loc("minecraft:diamond_sword")).unwrap();

    assert!(icon.is_drawable());
    assert_eq!(icon.parts.len(), 1);
    match &icon.parts[0] {
        IconPart::Sprite { layers } => {
            assert_eq!(layers.len(), 1);
            assert_eq!(layers[0].sprite, loc("minecraft:item/diamond_sword"));
            assert!(layers[0].tint.is_none());
        }
        other => panic!("expected Sprite, got {other:?}"),
    }
}

#[test]
fn generated_item_carries_layers_and_tints_in_order() {
    // A leather-armour-style item: two layers, layer0 dyed (tint index 0),
    // layer1 an untinted overlay. The item definition attaches the tints.
    let mgr = manager(&[
        (
            "assets/minecraft/items/leather_boots.json",
            r#"{"model":{"type":"minecraft:model","model":"minecraft:item/leather_boots",
                "tints":[{"type":"minecraft:dye","default":10511680}]}}"#,
        ),
        (
            "assets/minecraft/models/item/leather_boots.json",
            r#"{"parent":"minecraft:builtin/generated","textures":{
                "layer0":"minecraft:item/leather_boots",
                "layer1":"minecraft:item/leather_boots_overlay"}}"#,
        ),
    ]);
    let builder = ItemIconBuilder::new(&mgr);
    let icon = builder.icon(&loc("minecraft:leather_boots")).unwrap();

    match &icon.parts[0] {
        IconPart::Sprite { layers } => {
            assert_eq!(layers.len(), 2, "both layers present in order");
            assert_eq!(layers[0].sprite, loc("minecraft:item/leather_boots"));
            assert_eq!(layers[1].sprite, loc("minecraft:item/leather_boots_overlay"));
            // Tint index 0 lands on layer0; layer1 is untinted.
            let t0 = layers[0].tint.as_ref().expect("layer0 dyed");
            assert_eq!(t0.kind, "minecraft:dye");
            assert_eq!(t0.default, Some(10511680));
            assert!(layers[1].tint.is_none(), "no tint index for layer1");
        }
        other => panic!("expected Sprite, got {other:?}"),
    }
}

#[test]
fn block_item_becomes_a_model_under_the_gui_transform() {
    let mgr = manager(&[
        (
            "assets/minecraft/items/stone.json",
            r#"{"model":{"type":"minecraft:model","model":"minecraft:block/stone"}}"#,
        ),
        (
            "assets/minecraft/models/block/stone.json",
            r##"{
                "display":{"gui":{"rotation":[30,225,0],"translation":[0,0,0],"scale":[0.625,0.625,0.625]}},
                "textures":{"all":"minecraft:block/stone"},
                "elements":[{"from":[0,0,0],"to":[16,16,16],
                    "faces":{"up":{"texture":"#all"},"north":{"texture":"#all"}}}]
            }"##,
        ),
    ]);
    let builder = ItemIconBuilder::new(&mgr);
    let icon = builder.icon(&loc("minecraft:stone")).unwrap();

    assert!(icon.is_drawable());
    match &icon.parts[0] {
        IconPart::Model {
            model,
            transform,
            gui_light,
        } => {
            assert_eq!(*model, loc("minecraft:block/stone"));
            assert_eq!(*gui_light, GuiLight::Side, "block models are side-lit");
            // The GUI transform comes from the model JSON (the isometric look).
            let expected = DisplayTransform {
                rotation: [30.0, 225.0, 0.0],
                translation: [0.0, 0.0, 0.0],
                scale: [0.625, 0.625, 0.625],
            };
            assert_eq!(*transform, expected);
        }
        other => panic!("expected Model, got {other:?}"),
    }
}

#[test]
fn chest_becomes_a_special_renderer() {
    // The ex-`builtin/entity` seam: chest's `models/item` entry is now an empty
    // shell, and only the `items/*.json` definition reveals the special renderer.
    let mgr = manager(&[(
        "assets/minecraft/items/chest.json",
        r#"{"model":{"type":"minecraft:special","base":"minecraft:item/chest",
            "model":{"type":"minecraft:chest","texture":"minecraft:normal"}}}"#,
    )]);
    let builder = ItemIconBuilder::new(&mgr);
    let icon = builder.icon(&loc("minecraft:chest")).unwrap();

    assert!(icon.is_drawable());
    match &icon.parts[0] {
        IconPart::Special {
            base,
            kind,
            transformation,
        } => {
            assert_eq!(*base, loc("minecraft:item/chest"));
            assert_eq!(kind, "minecraft:chest");
            // chest.json carries no node-level `"transformation"` at all.
            assert_eq!(*transformation, None);
        }
        other => panic!("expected Special, got {other:?}"),
    }
}

#[test]
fn skull_special_renderer_carries_its_own_node_transformation() {
    // The skull family's real gap (issue #645): the node's own
    // `"transformation"` must survive from the parsed tree through
    // `ItemIconBuilder::part_for` onto `IconPart::Special`, not be dropped.
    let mgr = manager(&[(
        "assets/minecraft/items/skeleton_skull.json",
        r#"{"model":{"type":"minecraft:special","base":"minecraft:item/template_skull",
            "model":{"type":"minecraft:head","kind":"skeleton"},
            "transformation":{
                "left_rotation":[1.0,0.0,0.0,-0.0],
                "right_rotation":[0.0,0.0,0.0,1.0],
                "scale":[1.0,1.0,1.0],
                "translation":[0.5,0.0,0.5]
            }}}"#,
    )]);
    let builder = ItemIconBuilder::new(&mgr);
    let icon = builder.icon(&loc("minecraft:skeleton_skull")).unwrap();

    assert!(icon.is_drawable());
    match &icon.parts[0] {
        IconPart::Special {
            kind,
            transformation,
            ..
        } => {
            assert_eq!(kind, "minecraft:head");
            let t = transformation.expect("skull carries its own node transformation");
            assert_eq!(t.translation, [0.5, 0.0, 0.5]);
            assert_eq!(t.left_rotation, [1.0, 0.0, 0.0, -0.0]);
        }
        other => panic!("expected Special, got {other:?}"),
    }
}

#[test]
fn missing_definition_is_a_clear_error_not_a_panic() {
    let mgr = manager(&[]);
    let builder = ItemIconBuilder::new(&mgr);
    let err = builder.icon(&loc("minecraft:nonexistent")).unwrap_err();
    assert!(
        matches!(err, lodestone_assets::IconError::DefinitionMissing(ref s) if s.contains("nonexistent")),
        "got {err:?}"
    );
}

#[test]
fn empty_model_is_not_drawable() {
    // An item whose definition points at `builtin/empty` (vanilla `air`) draws
    // nothing: the icon has no parts and reports itself undrawable.
    let mgr = manager(&[
        (
            "assets/minecraft/items/air.json",
            r#"{"model":{"type":"minecraft:model","model":"minecraft:item/empty"}}"#,
        ),
        (
            "assets/minecraft/models/item/empty.json",
            r#"{"parent":"minecraft:builtin/empty"}"#,
        ),
    ]);
    let builder = ItemIconBuilder::new(&mgr);
    let icon = builder.icon(&loc("minecraft:air")).unwrap();
    assert!(!icon.is_drawable());
    assert!(icon.parts.is_empty());
}

#[test]
fn geometryless_shell_model_is_not_drawable() {
    // A `models/item` template with only a particle texture and no elements (the
    // shape a special item's model entry now takes) renders nothing on the
    // sprite/model path.
    let mgr = manager(&[
        (
            "assets/minecraft/items/decorated_pot.json",
            r#"{"model":{"type":"minecraft:model","model":"minecraft:item/decorated_pot"}}"#,
        ),
        (
            "assets/minecraft/models/item/decorated_pot.json",
            r#"{"textures":{"particle":"minecraft:block/terracotta"}}"#,
        ),
    ]);
    let builder = ItemIconBuilder::new(&mgr);
    let icon = builder.icon(&loc("minecraft:decorated_pot")).unwrap();
    assert!(!icon.is_drawable());
}

#[test]
fn composite_yields_one_part_per_submodel_in_order() {
    let mgr = manager(&[
        (
            "assets/minecraft/items/bundle.json",
            r#"{"model":{"type":"minecraft:composite","models":[
                {"type":"minecraft:model","model":"minecraft:item/bundle_base"},
                {"type":"minecraft:model","model":"minecraft:item/bundle_overlay"}]}}"#,
        ),
        (
            "assets/minecraft/models/item/bundle_base.json",
            r#"{"parent":"minecraft:builtin/generated","textures":{"layer0":"minecraft:item/bundle"}}"#,
        ),
        (
            "assets/minecraft/models/item/bundle_overlay.json",
            r#"{"parent":"minecraft:builtin/generated","textures":{"layer0":"minecraft:item/bundle_overlay"}}"#,
        ),
    ]);
    let builder = ItemIconBuilder::new(&mgr);
    let icon = builder.icon(&loc("minecraft:bundle")).unwrap();
    assert_eq!(icon.parts.len(), 2, "both composite sub-models drawn");
    match (&icon.parts[0], &icon.parts[1]) {
        (IconPart::Sprite { layers: a }, IconPart::Sprite { layers: b }) => {
            assert_eq!(a[0].sprite, loc("minecraft:item/bundle"));
            assert_eq!(b[0].sprite, loc("minecraft:item/bundle_overlay"));
        }
        other => panic!("expected two Sprite parts, got {other:?}"),
    }
}

#[test]
fn default_context_takes_the_select_fallback() {
    // A select with no matching case under the default context falls back to the
    // default appearance — the inventory shows a fresh stack's default model.
    let mgr = manager(&[
        (
            "assets/minecraft/items/compass.json",
            r#"{"model":{"type":"minecraft:select","property":"minecraft:custom_model_data",
                "cases":[{"when":"special","model":{"type":"minecraft:model","model":"minecraft:item/compass_special"}}],
                "fallback":{"type":"minecraft:model","model":"minecraft:item/compass_00"}}}"#,
        ),
        (
            "assets/minecraft/models/item/compass_00.json",
            r#"{"parent":"minecraft:builtin/generated","textures":{"layer0":"minecraft:item/compass_00"}}"#,
        ),
    ]);
    let builder = ItemIconBuilder::new(&mgr);
    let icon = builder.icon(&loc("minecraft:compass")).unwrap();
    match &icon.parts[0] {
        IconPart::Sprite { layers } => {
            assert_eq!(layers[0].sprite, loc("minecraft:item/compass_00"));
        }
        other => panic!("expected fallback Sprite, got {other:?}"),
    }
}

/// Real 26.2 `items/spyglass.json` is a `minecraft:display_context` select with
/// a `gui` case pointing at the flat sprite `minecraft:item/spyglass`, and a
/// *fallback* (no matching case, not a `gui`/held distinction) pointing at the
/// 3-D `minecraft:item/spyglass_in_hand` model. [`GuiItemContext`] must answer
/// `select("minecraft:display_context") == Some("gui")` so this takes the `gui`
/// case; under a context that never answers the select (the old
/// `DefaultItemContext`-only resolution `ItemAtlas` used to use), this silently
/// falls through to the fallback and the inventory icon becomes the in-hand
/// model instead of the flat sprite.
#[test]
#[ignore = "requires a fetched vanilla client.jar in .cache/mc/<version>/"]
fn spyglass_resolves_to_the_flat_gui_sprite_not_the_in_hand_model() {
    let jar = client_jar().expect("no client.jar under .cache/mc/<version>/; fetch it first");
    let source = ZipSource::open(&jar).expect("open client.jar");
    let mgr = ResourceManager::new(vec![Box::new(source)]);

    let builder = ItemIconBuilder::new(&mgr);
    let icon = builder
        .icon_with(&loc("minecraft:spyglass"), &GuiItemContext)
        .expect("spyglass definition resolves");

    assert_eq!(icon.parts.len(), 1, "spyglass GUI icon is a single part");
    match &icon.parts[0] {
        IconPart::Sprite { layers } => {
            assert_eq!(
                layers[0].sprite,
                loc("minecraft:item/spyglass"),
                "GUI icon must be the flat spyglass sprite"
            );
        }
        other => panic!(
            "expected the GUI-case flat Sprite (minecraft:item/spyglass), got {other:?} — \
             this is the `spyglass_in_hand` 3-D model, i.e. the select fell through to its \
             fallback instead of taking the \"gui\" case"
        ),
    }
}
