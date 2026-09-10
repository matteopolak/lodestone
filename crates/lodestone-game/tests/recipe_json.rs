//! Typed recipe and tag document boundary tests.

#![cfg(feature = "json")]

use lodestone_game::recipe::{Ingredient, Recipe, TagEntry};
use lodestone_game::recipe_json::{
    IngredientDocument, LoadError, RecipeDocument, ResultDocument, TagValueDocument, parse_recipe,
    parse_recipe_document, parse_recipe_text, parse_tag, parse_tag_document, parse_tag_text,
};

fn key(raw: &str) -> lodestone_model::ResourceKey {
    raw.parse().expect("test resource key")
}

#[test]
fn shaped_document_golden_decodes_to_typed_fields_and_model() {
    let text = r##"{
        "type": "minecraft:crafting_shaped",
        "category": "redstone",
        "group": "lamp",
        "pattern": ["#R", " R"],
        "key": {"#": "minecraft:stone", "R": "#minecraft:dusts"},
        "result": {"id": "minecraft:redstone_lamp", "count": 2},
        "mirror": false
    }"##;

    let document = parse_recipe_document(text).expect("typed shaped document");
    let RecipeDocument::Shaped(shaped) = &document else {
        panic!("expected shaped document");
    };
    assert_eq!(shaped.pattern, ["#R", " R"]);
    assert_eq!(shaped.group.as_deref(), Some("lamp"));
    assert_eq!(shaped.result.id(), &key("minecraft:redstone_lamp"));
    assert_eq!(shaped.result.count(), 2);
    assert_eq!(
        shaped.key.get("R"),
        Some(&IngredientDocument::Tag(key("minecraft:dusts")))
    );

    let Recipe::Shaped(recipe) = parse_recipe(&document).expect("model recipe") else {
        panic!("expected shaped model");
    };
    assert_eq!(recipe.width(), 2);
    assert_eq!(recipe.height(), 2);
    assert_eq!(recipe.result().item(), &key("minecraft:redstone_lamp"));
    assert_eq!(recipe.result().count(), 2);
    assert_eq!(
        recipe.pattern()[0],
        Some(Ingredient::Item(key("minecraft:stone")))
    );
}

#[test]
fn recursive_ingredient_and_result_forms_round_trip() {
    let text = r##"{
        "type": "minecraft:crafting_shapeless",
        "ingredients": [["#minecraft:planks", {"item": "minecraft:bamboo"}]],
        "result": "minecraft:stick"
    }"##;
    let document = parse_recipe_document(text).expect("typed recursive document");
    let RecipeDocument::Shapeless(shapeless) = &document else {
        panic!("expected shapeless document");
    };
    assert_eq!(
        shapeless.ingredients,
        [IngredientDocument::Any(vec![
            IngredientDocument::Tag(key("minecraft:planks")),
            IngredientDocument::Item(key("minecraft:bamboo")),
        ])]
    );
    assert_eq!(
        shapeless.result,
        ResultDocument::Item(key("minecraft:stick"))
    );

    let encoded = serde_json::to_string(&document).expect("serialize typed document");
    let decoded = parse_recipe_document(&encoded).expect("decode round trip");
    assert_eq!(decoded, document);
}

#[test]
fn tags_decode_bare_and_object_entries_without_untyped_values() {
    let text = r##"{
        "replace": true,
        "values": [
            "minecraft:oak_planks",
            "#minecraft:planks",
            {"id": "minecraft:bamboo", "required": false}
        ]
    }"##;
    let document = parse_tag_document(text).expect("typed tag document");
    assert_eq!(document.replace, Some(true));
    assert_eq!(
        document.values,
        [
            TagValueDocument::Item(key("minecraft:oak_planks")),
            TagValueDocument::Tag(key("minecraft:planks")),
            TagValueDocument::Item(key("minecraft:bamboo")),
        ]
    );
    assert_eq!(
        parse_tag(&document).expect("tag entries"),
        vec![
            TagEntry::Item(key("minecraft:oak_planks")),
            TagEntry::Tag(key("minecraft:planks")),
            TagEntry::Item(key("minecraft:bamboo")),
        ]
    );
    let encoded = serde_json::to_string(&document).expect("serialize typed tag");
    assert_eq!(
        parse_tag_text(&encoded).expect("round-trip tag"),
        parse_tag(&document).unwrap()
    );
}

#[test]
fn unsupported_recipe_is_explicit_and_lossless() {
    let text = r#"{
        "type": "future:quantum_crafting",
        "result": {"id": "future:thing", "count": 4},
        "new_field": {"nested": [1, true, null]}
    }"#;
    let document = parse_recipe_document(text).expect("unsupported document");
    let RecipeDocument::Unsupported(unsupported) = &document else {
        panic!("expected explicit unsupported document");
    };
    assert_eq!(unsupported.recipe_type, "future:quantum_crafting");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(unsupported.raw_json()).expect("raw object"),
        serde_json::from_str::<serde_json::Value>(text).expect("source object")
    );
    assert_eq!(
        parse_recipe(&document).expect("special model"),
        Recipe::Special("future:quantum_crafting".to_string())
    );
    let encoded = serde_json::to_string(&document).expect("forward raw object");
    let decoded = parse_recipe_document(&encoded).expect("decode forwarded object");
    assert_eq!(decoded, document);
}

#[test]
fn malformed_documents_fail_at_typed_boundary() {
    assert!(matches!(
        parse_recipe_document(r#"{"pattern": [], "key": {}, "result": "minecraft:stone"}"#),
        Err(LoadError::MissingType)
    ));
    assert!(parse_recipe_text(
        r##"{"type":"minecraft:crafting_shaped","pattern":["#"],"key":{"#":"not valid"},"result":"minecraft:stone"}"##
    )
    .is_err());
    assert!(parse_tag_text(r#"{"values":[{"id": 7}]}"#).is_err());
    assert!(
        parse_recipe_document(
            r#"{"type":"minecraft:crafting_shapeless","ingredients":[],"result":{"count":2}}"#
        )
        .is_err()
    );
}
