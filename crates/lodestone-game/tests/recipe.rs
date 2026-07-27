//! Hermetic recipe matching tests (no external data).

use lodestone_game::item::ItemStack;
use lodestone_game::recipe::{
    CraftingGrid, Ingredient, ShapedRecipe, ShapelessRecipe, TagEntry, TagResolver,
};

fn id(s: &str) -> lodestone_model::Identifier {
    s.parse().unwrap()
}

fn item(s: &str) -> Ingredient {
    Ingredient::Item(id(s))
}

fn grid3(cells: [&str; 9]) -> CraftingGrid {
    let v = cells
        .iter()
        .map(|c| if c.is_empty() { None } else { Some(id(c)) })
        .collect();
    CraftingGrid::new(3, 3, v)
}

#[test]
fn shaped_matches_at_any_offset() {
    // 2x1 pattern: two planks side by side -> should match anywhere in 3x3.
    let recipe = ShapedRecipe::new(
        2,
        1,
        vec![
            Some(item("minecraft:oak_planks")),
            Some(item("minecraft:oak_planks")),
        ],
        ItemStack::new(id("minecraft:stick"), 4),
    );
    let tags = TagResolver::new();

    // top-left
    let g = grid3([
        "minecraft:oak_planks",
        "minecraft:oak_planks",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
    ]);
    assert!(recipe.matches(&g, &tags));
    // bottom-right
    let g = grid3([
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "minecraft:oak_planks",
        "minecraft:oak_planks",
    ]);
    assert!(recipe.matches(&g, &tags));
    // stray extra item breaks it (uncovered cell must be empty)
    let g = grid3([
        "minecraft:oak_planks",
        "minecraft:oak_planks",
        "minecraft:stone",
        "",
        "",
        "",
        "",
        "",
        "",
    ]);
    assert!(!recipe.matches(&g, &tags));
}

#[test]
fn shaped_mirror_default_on_and_can_disable() {
    // Asymmetric L: planks at (0,0) and (0,1) and (1,1).
    let pattern = vec![
        Some(item("minecraft:iron_ingot")),
        None,
        Some(item("minecraft:iron_ingot")),
        Some(item("minecraft:iron_ingot")),
    ];
    let recipe = ShapedRecipe::new(
        2,
        2,
        pattern.clone(),
        ItemStack::new(id("minecraft:bucket"), 1),
    );
    let tags = TagResolver::new();

    // Mirrored placement: iron at (1,0),(0,1),(1,1)
    let g = grid3([
        "",
        "minecraft:iron_ingot",
        "",
        "minecraft:iron_ingot",
        "minecraft:iron_ingot",
        "",
        "",
        "",
        "",
    ]);
    assert!(recipe.matches(&g, &tags), "mirror should match by default");

    let no_mirror = ShapedRecipe::new(2, 2, pattern, ItemStack::new(id("minecraft:bucket"), 1))
        .without_mirror();
    assert!(
        !no_mirror.matches(&g, &tags),
        "mirror disabled should reject"
    );
}

#[test]
fn shapeless_is_multiset_not_reused() {
    // Two distinct ingredients; a grid with two of the SAME item must fail.
    let recipe = ShapelessRecipe::new(
        vec![item("minecraft:sugar"), item("minecraft:paper")],
        ItemStack::new(id("minecraft:book"), 1),
    );
    let tags = TagResolver::new();

    let ok = grid3([
        "minecraft:sugar",
        "minecraft:paper",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
    ]);
    assert!(recipe.matches(&ok, &tags));

    // Only one paper present but two ingredients — reusing paper is forbidden.
    let bad = grid3([
        "minecraft:paper",
        "minecraft:paper",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
    ]);
    assert!(!recipe.matches(&bad, &tags));

    // Wrong count (extra item) fails.
    let extra = grid3([
        "minecraft:sugar",
        "minecraft:paper",
        "minecraft:paper",
        "",
        "",
        "",
        "",
        "",
        "",
    ]);
    assert!(!recipe.matches(&extra, &tags));
}

#[test]
fn tag_ingredient_resolves_including_nested() {
    let mut tags = TagResolver::new();
    tags.insert(
        id("minecraft:planks"),
        vec![
            TagEntry::Item(id("minecraft:oak_planks")),
            TagEntry::Tag(id("minecraft:special_planks")),
        ],
    );
    tags.insert(
        id("minecraft:special_planks"),
        vec![TagEntry::Item(id("minecraft:crimson_planks"))],
    );

    let ing = Ingredient::Tag(id("minecraft:planks"));
    assert!(ing.matches(&id("minecraft:oak_planks"), &tags));
    assert!(
        ing.matches(&id("minecraft:crimson_planks"), &tags),
        "nested tag member"
    );
    assert!(!ing.matches(&id("minecraft:stone"), &tags));
}

#[test]
fn tag_resolution_survives_cycles() {
    let mut tags = TagResolver::new();
    tags.insert(
        id("minecraft:a"),
        vec![
            TagEntry::Tag(id("minecraft:b")),
            TagEntry::Item(id("minecraft:x")),
        ],
    );
    tags.insert(
        id("minecraft:b"),
        vec![
            TagEntry::Tag(id("minecraft:a")),
            TagEntry::Item(id("minecraft:y")),
        ],
    );
    let set = tags.resolve(&id("minecraft:a"));
    assert!(set.contains(&id("minecraft:x")));
    assert!(set.contains(&id("minecraft:y")));
}

#[test]
fn any_option_ingredient_matches_either() {
    let ing = Ingredient::Any(vec![item("minecraft:white_bed"), item("minecraft:red_bed")]);
    let tags = TagResolver::new();
    assert!(ing.matches(&id("minecraft:red_bed"), &tags));
    assert!(!ing.matches(&id("minecraft:blue_bed"), &tags));
}
