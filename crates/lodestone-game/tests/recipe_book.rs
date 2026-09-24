//! The loaded recipe corpus, checked against Mojang's own datapack JSON.
//!
//! `#[ignore]`d and gated behind the `json` feature because it reads the
//! gitignored jar cache. Run with:
//!
//! ```text
//! cargo test -p lodestone-game --features json --test recipe_book -- --ignored --nocapture
//! ```
//!
//! Where [`recipe_corpus`](../recipe_corpus.rs) proves the loader is
//! self-consistent over the whole corpus, this file proves it is *correct*: the
//! expected values below were transcribed **by hand** from the real files under
//! `.cache/mc/26.2/client-src/data/minecraft/recipe/`, so nothing here can be
//! satisfied by a loader that is wrong in the same way twice.
#![cfg(feature = "json")]

use std::path::{Path, PathBuf};

use lodestone_game::recipe::{CraftingGrid, Recipe, RecipeBook};
use lodestone_game::recipe_json::load_data_root;
use lodestone_model::Identifier;

/// The `data/` root inside the extracted 26.2 client jar.
fn data_root() -> Option<PathBuf> {
    let manifest = env!("CARGO_MANIFEST_DIR");
    for up in ["..", "../..", "../../.."] {
        let root = Path::new(manifest)
            .join(up)
            .join(".cache/mc/26.2/client-src/data");
        if root.join("minecraft/recipe").is_dir() {
            return Some(root);
        }
    }
    None
}

fn book() -> RecipeBook {
    let root = data_root().expect("recipe corpus not found under .cache; extract the client jar");
    let builder = load_data_root(&root).expect("read data root");
    assert!(
        builder.failures().is_empty(),
        "{} documents failed to load: {:?}",
        builder.failures().len(),
        &builder.failures()[..builder.failures().len().min(10)]
    );
    builder.finish()
}

fn id(s: &str) -> Identifier {
    s.parse().expect("valid identifier")
}

/// Builds a 3×3 grid from row-major cell names; `""` is an empty cell.
fn grid(cells: [&str; 9]) -> CraftingGrid {
    CraftingGrid::new(
        3,
        3,
        cells
            .iter()
            .map(|c| if c.is_empty() { None } else { Some(id(c)) })
            .collect(),
    )
}

fn assert_crafts(book: &RecipeBook, cells: [&str; 9], recipe: &str, result: &str, count: i32) {
    let g = grid(cells);
    let (matched_id, stack) = book
        .match_grid_entry(&g)
        .unwrap_or_else(|| panic!("grid crafted nothing, expected {recipe}"));
    assert_eq!(matched_id, &id(recipe), "matched the wrong recipe");
    assert_eq!(stack.item(), &id(result), "wrong result item");
    assert_eq!(stack.count(), count, "wrong result count");
}

#[test]
#[ignore = "reads gitignored jar cache; run with --features json --ignored"]
fn loads_the_whole_vanilla_corpus() {
    let book = book();
    // 26.2 ships 1585 recipe files and 224 item tags. The tag count is the one
    // that catches a non-recursive walk: 33 of them are nested one level deep
    // under `tags/item/enchantable/` and `tags/item/sulfur_cube_archetype/`.
    assert_eq!(book.len(), 1585, "recipe count");
    assert_eq!(book.tags().len(), 224, "item tag count");

    let mut shaped = 0;
    let mut shapeless = 0;
    let mut cooking = 0;
    for (_, recipe) in book.iter() {
        match recipe {
            Recipe::Shaped(_) => shaped += 1,
            Recipe::Shapeless(_) => shapeless += 1,
            Recipe::Cooking(_) => cooking += 1,
            _ => {}
        }
    }
    eprintln!("shaped {shaped}, shapeless {shapeless}, cooking {cooking}");
    assert_eq!(shaped, 733);
    assert_eq!(shapeless, 323);
    assert_eq!(cooking, 116);

    // The two grid kinds are exactly the ones that can match a CraftingGrid.
    let grid_recipes = book.iter().filter(|(_, r)| r.is_grid_recipe()).count();
    assert_eq!(grid_recipes, shaped + shapeless);
}

#[test]
#[ignore = "reads gitignored jar cache; run with --features json --ignored"]
fn nested_item_tags_keep_their_subdirectory_in_the_id() {
    let book = book();
    // `tags/item/enchantable/weapon.json` -> `minecraft:enchantable/weapon`, the
    // same id vanilla's FileToIdConverter produces. A flat read_dir would have
    // called this `minecraft:weapon` (or dropped it).
    let weapon = id("minecraft:enchantable/weapon");
    // Directly listed in that file.
    assert!(
        book.tags().contains(&weapon, &id("minecraft:mace")),
        "nested tag minecraft:enchantable/weapon did not resolve its direct member"
    );
    // Reached through two levels of `#tag` reference:
    // enchantable/weapon -> #enchantable/sharp_weapon -> #axes -> diamond_axe.
    // The middle hop is itself a nested-path tag, so this fails unless *both*
    // the walk and the id derivation handle subdirectories.
    assert!(
        book.tags().contains(&weapon, &id("minecraft:diamond_axe")),
        "transitive resolution through nested-path tags failed"
    );
}

/// `recipe/crafting_table.json`: shaped, 2×2 of `#minecraft:planks`, result
/// `minecraft:crafting_table` ×1 (no `count` key, so the default 1).
///
/// Deliberately fed *spruce* planks: an ingredient matcher that compared item
/// ids instead of resolving the tag would pass with oak and fail here.
#[test]
#[ignore = "reads gitignored jar cache; run with --features json --ignored"]
fn crafting_table_is_a_2x2_of_any_planks() {
    let book = book();
    assert_crafts(
        &book,
        [
            "minecraft:spruce_planks",
            "minecraft:spruce_planks",
            "",
            "minecraft:spruce_planks",
            "minecraft:spruce_planks",
            "",
            "",
            "",
            "",
        ],
        "minecraft:crafting_table",
        "minecraft:crafting_table",
        1,
    );
    // A 2×2 pattern must match anywhere in the 3×3 grid, not just top-left.
    assert_crafts(
        &book,
        [
            "",
            "",
            "",
            "",
            "minecraft:bamboo_planks",
            "minecraft:bamboo_planks",
            "",
            "minecraft:bamboo_planks",
            "minecraft:bamboo_planks",
        ],
        "minecraft:crafting_table",
        "minecraft:crafting_table",
        1,
    );
}

/// `recipe/stick.json`: shaped `["#", "#"]` with `# = #minecraft:planks`,
/// result `minecraft:stick` ×4.
#[test]
#[ignore = "reads gitignored jar cache; run with --features json --ignored"]
fn sticks_are_two_stacked_planks_and_yield_four() {
    let book = book();
    assert_crafts(
        &book,
        [
            "minecraft:cherry_planks",
            "",
            "",
            "minecraft:cherry_planks",
            "",
            "",
            "",
            "",
            "",
        ],
        "minecraft:stick",
        "minecraft:stick",
        4,
    );
    // Side by side is a *different* shape. It is not nothing — vanilla crafts a
    // pressure plate from exactly that — which is the sharper assertion: the
    // orientation has to pick the *other* recipe, not merely fail to match.
    assert_crafts(
        &book,
        [
            "minecraft:cherry_planks",
            "minecraft:cherry_planks",
            "",
            "",
            "",
            "",
            "",
            "",
            "",
        ],
        "minecraft:cherry_pressure_plate",
        "minecraft:cherry_pressure_plate",
        1,
    );
}

/// `recipe/oak_planks.json`: **shapeless**, one `#minecraft:oak_logs`, result
/// `minecraft:oak_planks` ×4. Fed `oak_wood`, a tag member that is not the
/// obvious `oak_log`.
#[test]
#[ignore = "reads gitignored jar cache; run with --features json --ignored"]
fn oak_planks_is_shapeless_from_any_oak_log() {
    let book = book();
    for cell in 0..9 {
        let mut cells = [""; 9];
        cells[cell] = "minecraft:oak_wood";
        assert_crafts(
            &book,
            cells,
            "minecraft:oak_planks",
            "minecraft:oak_planks",
            4,
        );
    }
}

/// `recipe/torch.json`: shaped `["X", "#"]` with `# = minecraft:stick` and
/// `X = ["minecraft:coal", "minecraft:charcoal"]`, result `minecraft:torch` ×4.
///
/// The array form is [`Ingredient::Any`]; charcoal is the *second* option, so a
/// loader that kept only the first would fail this.
#[test]
#[ignore = "reads gitignored jar cache; run with --features json --ignored"]
fn torch_accepts_either_coal_option() {
    let book = book();
    for fuel in ["minecraft:coal", "minecraft:charcoal"] {
        assert_crafts(
            &book,
            [fuel, "", "", "minecraft:stick", "", "", "", "", ""],
            "minecraft:torch",
            "minecraft:torch",
            4,
        );
    }
    // Upside down is a different shape (mirroring is left-to-right only).
    assert!(
        book.match_grid(&grid([
            "minecraft:stick",
            "",
            "",
            "minecraft:coal",
            "",
            "",
            "",
            "",
            "",
        ]))
        .is_none(),
        "stick above coal should not craft a torch"
    );
}

/// `recipe/chest.json`: shaped `["###", "# #", "###"]`, `# = #minecraft:planks`,
/// result `minecraft:chest` ×1. The hole in the middle is load-bearing: a
/// pattern cell that is `None` means *must be empty*.
#[test]
#[ignore = "reads gitignored jar cache; run with --features json --ignored"]
fn chest_is_a_ring_of_planks_with_an_empty_centre() {
    let book = book();
    let p = "minecraft:jungle_planks";
    assert_crafts(
        &book,
        [p, p, p, p, "", p, p, p, p],
        "minecraft:chest",
        "minecraft:chest",
        1,
    );
    // Filling the centre must break it, not silently craft a chest.
    assert!(
        book.match_grid(&grid([p, p, p, p, p, p, p, p, p]))
            .is_none(),
        "a full 3x3 of planks must not craft a chest"
    );
}

/// `recipe/bookshelf.json`: `["###", "XXX", "###"]` with `# = #minecraft:planks`
/// and `X = minecraft:book`, result `minecraft:bookshelf` ×1. Two distinct keys
/// in one pattern, so a transposed row/column walk shows up immediately.
#[test]
#[ignore = "reads gitignored jar cache; run with --features json --ignored"]
fn bookshelf_has_a_row_of_books_not_a_column() {
    let book = book();
    let p = "minecraft:dark_oak_planks";
    let b = "minecraft:book";
    assert_crafts(
        &book,
        [p, p, p, b, b, b, p, p, p],
        "minecraft:bookshelf",
        "minecraft:bookshelf",
        1,
    );
    // The transpose — books down the middle *column* — is not a bookshelf.
    assert!(
        book.match_grid(&grid([p, b, p, p, b, p, p, b, p]))
            .is_none(),
        "a column of books must not craft a bookshelf"
    );
}

#[test]
#[ignore = "reads gitignored jar cache; run with --features json --ignored"]
fn an_empty_or_nonsense_grid_crafts_nothing() {
    let book = book();
    assert!(book.match_grid(&grid([""; 9])).is_none(), "empty grid");
    assert!(
        book.match_grid(&grid([
            "minecraft:bedrock",
            "minecraft:dragon_egg",
            "",
            "",
            "",
            "",
            "",
            "",
            "",
        ]))
        .is_none(),
        "nonsense grid"
    );
}
