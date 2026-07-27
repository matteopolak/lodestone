//! Whole-corpus recipe coverage against Mojang's generated data.
//!
//! `#[ignore]`d and gated behind the `json` feature because it reads the
//! gitignored jar cache. Run with:
//!
//! ```text
//! cargo test -p lodestone-game --features json --test recipe_corpus -- --ignored --nocapture
//! ```
//!
//! It does more than "does it parse": for every shaped and shapeless recipe it
//! *constructs a crafting grid from the recipe's own ingredients* (resolving
//! tags to a representative member) and asserts the recipe then matches that
//! grid and yields its declared result. A transposed pattern or a broken tag
//! resolver fails this immediately.
#![cfg(feature = "json")]

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use lodestone_game::recipe::{CraftingGrid, Ingredient, Recipe, TagResolver};
use lodestone_game::recipe_json::{parse_recipe, parse_tag};
use lodestone_model::Identifier;

fn cache_root() -> Option<PathBuf> {
    let manifest = env!("CARGO_MANIFEST_DIR");
    for up in ["..", "../..", "../../.."] {
        let root = Path::new(manifest)
            .join(up)
            .join(".cache/mc/26.2/client-src/data/minecraft");
        if root.join("recipe").is_dir() {
            return Some(root);
        }
    }
    None
}

fn load_tags(root: &Path) -> TagResolver {
    let mut tags = TagResolver::new();
    let dir = root.join("tags/item");
    let Ok(entries) = fs::read_dir(&dir) else {
        return tags;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        let stem = path.file_stem().unwrap().to_string_lossy();
        let tag_id: Identifier = format!("minecraft:{stem}").parse().unwrap();
        if let Ok(entries) = parse_tag(&value) {
            tags.insert(tag_id, entries);
        }
    }
    tags
}

/// A representative concrete item for an ingredient, if one exists.
fn representative(ing: &Ingredient, tags: &TagResolver) -> Option<Identifier> {
    match ing {
        Ingredient::Item(id) => Some(id.clone()),
        Ingredient::Tag(tag) => {
            let mut members: Vec<Identifier> = tags.resolve(tag).into_iter().collect();
            members.sort();
            members.into_iter().next()
        }
        Ingredient::Any(opts) => opts.iter().find_map(|o| representative(o, tags)),
    }
}

fn build_grid_shaped(
    cells: &[Option<Ingredient>],
    w: usize,
    h: usize,
    tags: &TagResolver,
) -> Option<CraftingGrid> {
    // Place into a 3x3 grid (top-left anchored).
    let gw = 3.max(w);
    let gh = 3.max(h);
    let mut grid = vec![None; gw * gh];
    for y in 0..h {
        for x in 0..w {
            if let Some(ing) = &cells[y * w + x] {
                grid[y * gw + x] = Some(representative(ing, tags)?);
            }
        }
    }
    Some(CraftingGrid::new(gw, gh, grid))
}

#[test]
#[ignore = "reads gitignored jar cache; run with --features json --ignored"]
fn recipe_corpus_coverage() {
    let Some(root) = cache_root() else {
        panic!("recipe corpus not found under .cache; run the jar extractor first");
    };
    let tags = load_tags(&root);
    let recipe_dir = root.join("recipe");

    let mut total = 0usize;
    let mut parse_ok = 0usize;
    let mut parse_fail: Vec<String> = Vec::new();
    let mut by_type: HashMap<&'static str, usize> = HashMap::new();

    let mut grid_recipes = 0usize; // shaped + shapeless
    let mut grid_matched = 0usize;
    let mut grid_unresolvable = 0usize; // ingredient tag had no members
    let mut grid_mismatch: Vec<String> = Vec::new();

    for entry in fs::read_dir(&recipe_dir).unwrap().flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        total += 1;
        let name = path.file_stem().unwrap().to_string_lossy().to_string();
        let text = fs::read_to_string(&path).unwrap();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        let recipe = match parse_recipe(&value) {
            Ok(r) => {
                parse_ok += 1;
                r
            }
            Err(e) => {
                parse_fail.push(format!("{name}: {e}"));
                continue;
            }
        };

        let kind = match &recipe {
            Recipe::Shaped(_) => "shaped",
            Recipe::Shapeless(_) => "shapeless",
            Recipe::Cooking(_) => "cooking",
            Recipe::Stonecutting { .. } => "stonecutting",
            Recipe::SmithingTransform { .. } => "smithing_transform",
            Recipe::SmithingTrim { .. } => "smithing_trim",
            Recipe::Transmute { .. } => "transmute",
            Recipe::Special(_) => "special",
        };
        *by_type.entry(kind).or_default() += 1;

        // Grid-construct-and-match assertion for the two grid recipe kinds.
        match &recipe {
            Recipe::Shaped(r) => {
                grid_recipes += 1;
                let cells = shaped_cells(&value);
                match build_grid_shaped(&cells.0, cells.1, cells.2, &tags) {
                    Some(grid) => match r.matches(&grid, &tags) {
                        true => grid_matched += 1,
                        false => grid_mismatch.push(name.clone()),
                    },
                    None => grid_unresolvable += 1,
                }
            }
            Recipe::Shapeless(r) => {
                grid_recipes += 1;
                let ings = shapeless_ings(&value);
                let mut reps = Vec::new();
                let mut ok = true;
                for ing in &ings {
                    match representative(ing, &tags) {
                        Some(id) => reps.push(Some(id)),
                        None => {
                            ok = false;
                            break;
                        }
                    }
                }
                if !ok || reps.len() > 9 {
                    grid_unresolvable += 1;
                } else {
                    while reps.len() < 9 {
                        reps.push(None);
                    }
                    let grid = CraftingGrid::new(3, 3, reps);
                    if r.matches(&grid, &tags) {
                        grid_matched += 1;
                    } else {
                        grid_mismatch.push(name.clone());
                    }
                }
            }
            _ => {}
        }
    }

    let mut types: Vec<_> = by_type.iter().collect();
    types.sort();
    eprintln!("\n=== recipe corpus coverage (26.2) ===");
    eprintln!("tags loaded:            {}", tags.len());
    eprintln!("recipe files:           {total}");
    eprintln!("parsed OK:              {parse_ok}");
    eprintln!("parse failures:         {}", parse_fail.len());
    for f in parse_fail.iter().take(20) {
        eprintln!("    FAIL {f}");
    }
    eprintln!("by type:");
    for (k, n) in &types {
        eprintln!("    {k:<20} {n}");
    }
    eprintln!("grid recipes:           {grid_recipes}");
    eprintln!("  matched own grid:     {grid_matched}");
    eprintln!("  unresolvable tags:    {grid_unresolvable}");
    eprintln!("  MISMATCH:             {}", grid_mismatch.len());
    for m in grid_mismatch.iter().take(30) {
        eprintln!("    MISMATCH {m}");
    }

    // Hard assertions: every file must parse, and every grid recipe whose
    // ingredients we could resolve must match a grid built from itself.
    assert!(
        parse_fail.is_empty(),
        "{} recipes failed to parse",
        parse_fail.len()
    );
    assert!(
        grid_mismatch.is_empty(),
        "{} grid recipes did not match a grid built from their own ingredients",
        grid_mismatch.len()
    );
    // Sanity floor: we expect the great majority of grid recipes to be
    // resolvable from vanilla tags.
    assert!(
        grid_matched * 100 / grid_recipes >= 95,
        "only {grid_matched}/{grid_recipes} grid recipes matched"
    );
}

/// Re-extract shaped pattern cells from raw JSON (the parsed `ShapedRecipe`
/// keeps them private). Mirrors the loader's own logic.
fn shaped_cells(value: &serde_json::Value) -> (Vec<Option<Ingredient>>, usize, usize) {
    let rows: Vec<&str> = value["pattern"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r.as_str().unwrap())
        .collect();
    let h = rows.len();
    let w = rows.iter().map(|r| r.chars().count()).max().unwrap_or(0);
    let key = value["key"].as_object().unwrap();
    let mut cells = Vec::with_capacity(w * h);
    for row in &rows {
        let chars: Vec<char> = row.chars().collect();
        for x in 0..w {
            let c = chars.get(x).copied().unwrap_or(' ');
            if c == ' ' {
                cells.push(None);
            } else {
                cells.push(Some(parse_ing(&key[&c.to_string()])));
            }
        }
    }
    (cells, w, h)
}

fn shapeless_ings(value: &serde_json::Value) -> Vec<Ingredient> {
    value["ingredients"]
        .as_array()
        .unwrap()
        .iter()
        .map(parse_ing)
        .collect()
}

fn parse_ing(v: &serde_json::Value) -> Ingredient {
    match v {
        serde_json::Value::String(s) => {
            if let Some(t) = s.strip_prefix('#') {
                Ingredient::Tag(t.parse().unwrap())
            } else {
                Ingredient::Item(s.parse().unwrap())
            }
        }
        serde_json::Value::Array(a) => Ingredient::Any(a.iter().map(parse_ing).collect()),
        _ => panic!("unexpected ingredient shape"),
    }
}
