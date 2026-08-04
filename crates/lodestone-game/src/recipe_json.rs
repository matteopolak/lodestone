//! JSON loading of recipes and tags from Mojang's generated data.
//!
//! Gated behind the `json` cargo feature so the default build stays free of a
//! JSON dependency. The parser works off `serde_json::Value` rather than a
//! derived schema so that a single unexpected field never fails a whole-corpus
//! load — it tolerates unknown keys and reports only genuinely unparseable
//! recipes.

use std::path::{Path, PathBuf};

use lodestone_model::Identifier;
use serde_json::Value;

use crate::item::ItemStack;
use crate::recipe::{
    CookingKind, CookingRecipe, Ingredient, Recipe, RecipeBook, RecipeCategory, ShapedRecipe,
    ShapelessRecipe, TagEntry, TagResolver,
};

/// An error loading a recipe from JSON.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadError {
    /// The `type` field was missing or not a string.
    MissingType,
    /// A required field was absent or malformed.
    BadField(&'static str),
    /// An identifier failed to parse.
    BadIdentifier(String),
    /// The bytes were not valid JSON at all.
    Json(String),
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadError::MissingType => write!(f, "recipe has no `type`"),
            LoadError::BadField(field) => write!(f, "bad or missing field `{field}`"),
            LoadError::BadIdentifier(s) => write!(f, "invalid identifier `{s}`"),
            LoadError::Json(e) => write!(f, "malformed JSON: {e}"),
        }
    }
}

impl std::error::Error for LoadError {}

fn ident(s: &str) -> Result<Identifier, LoadError> {
    s.parse()
        .map_err(|_| LoadError::BadIdentifier(s.to_string()))
}

fn parse_ingredient(v: &Value) -> Result<Ingredient, LoadError> {
    match v {
        Value::String(s) => Ok(if let Some(tag) = s.strip_prefix('#') {
            Ingredient::Tag(ident(tag)?)
        } else {
            Ingredient::Item(ident(s)?)
        }),
        Value::Array(items) => {
            let opts = items
                .iter()
                .map(parse_ingredient)
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Ingredient::Any(opts))
        }
        // Object form `{ "item": "..." }` (older/alternate schema).
        Value::Object(map) => {
            if let Some(Value::String(s)) = map.get("item") {
                Ok(Ingredient::Item(ident(s)?))
            } else if let Some(Value::String(s)) = map.get("tag") {
                Ok(Ingredient::Tag(ident(s)?))
            } else {
                Err(LoadError::BadField("ingredient"))
            }
        }
        _ => Err(LoadError::BadField("ingredient")),
    }
}

fn parse_result(v: &Value) -> Result<ItemStack, LoadError> {
    match v {
        Value::String(s) => Ok(ItemStack::new(ident(s)?, 1)),
        Value::Object(map) => {
            let id = map
                .get("id")
                .and_then(Value::as_str)
                .ok_or(LoadError::BadField("result.id"))?;
            let count = map.get("count").and_then(Value::as_i64).unwrap_or(1) as i32;
            Ok(ItemStack::new(ident(id)?, count))
        }
        _ => Err(LoadError::BadField("result")),
    }
}

/// Parses a single recipe from its JSON value.
///
/// # Errors
///
/// Returns [`LoadError`] if the recipe type is missing or a required field for
/// the recognised type is malformed.
pub fn parse_recipe(v: &Value) -> Result<Recipe, LoadError> {
    let ty = v
        .get("type")
        .and_then(Value::as_str)
        .ok_or(LoadError::MissingType)?;
    let ty = ty.strip_prefix("minecraft:").unwrap_or(ty);

    match ty {
        "crafting_shaped" => parse_shaped(v).map(Recipe::Shaped),
        "crafting_shapeless" => parse_shapeless(v).map(Recipe::Shapeless),
        "smelting" | "blasting" | "smoking" | "campfire_cooking" => {
            parse_cooking(ty, v).map(Recipe::Cooking)
        }
        "stonecutting" => Ok(Recipe::Stonecutting {
            ingredient: parse_ingredient(field(v, "ingredient")?)?,
            result: parse_result(field(v, "result")?)?,
        }),
        "smithing_transform" => Ok(Recipe::SmithingTransform {
            template: parse_ingredient(field(v, "template")?)?,
            base: parse_ingredient(field(v, "base")?)?,
            addition: parse_ingredient(field(v, "addition")?)?,
            result: parse_result(field(v, "result")?)?,
        }),
        "smithing_trim" => Ok(Recipe::SmithingTrim {
            template: parse_ingredient(field(v, "template")?)?,
            base: parse_ingredient(field(v, "base")?)?,
            addition: parse_ingredient(field(v, "addition")?)?,
        }),
        "crafting_transmute" => Ok(Recipe::Transmute {
            input: parse_ingredient(field(v, "input")?)?,
            material: parse_ingredient(field(v, "material")?)?,
            result: parse_result(field(v, "result")?)?,
        }),
        other => Ok(Recipe::Special(other.to_string())),
    }
}

fn field<'a>(v: &'a Value, name: &'static str) -> Result<&'a Value, LoadError> {
    v.get(name).ok_or(LoadError::BadField(name))
}

fn parse_shaped(v: &Value) -> Result<ShapedRecipe, LoadError> {
    let pattern_rows = field(v, "pattern")?
        .as_array()
        .ok_or(LoadError::BadField("pattern"))?;
    let rows: Vec<&str> = pattern_rows
        .iter()
        .map(|r| r.as_str().ok_or(LoadError::BadField("pattern")))
        .collect::<Result<_, _>>()?;
    let height = rows.len();
    let width = rows.iter().map(|r| r.chars().count()).max().unwrap_or(0);

    let key = field(v, "key")?
        .as_object()
        .ok_or(LoadError::BadField("key"))?;

    let mut cells = Vec::with_capacity(width * height);
    for row in &rows {
        let chars: Vec<char> = row.chars().collect();
        for x in 0..width {
            let c = chars.get(x).copied().unwrap_or(' ');
            if c == ' ' {
                cells.push(None);
            } else {
                let key_str = c.to_string();
                let ing_val = key.get(&key_str).ok_or(LoadError::BadField("key char"))?;
                cells.push(Some(parse_ingredient(ing_val)?));
            }
        }
    }

    let result = parse_result(field(v, "result")?)?;
    let mut recipe = ShapedRecipe::new(width, height, cells, result).with_category(parse_category(v));
    if v.get("show_notification").is_some() {
        // no-op; kept for schema tolerance
    }
    if let Some(false) = v.get("mirror").and_then(Value::as_bool) {
        recipe = recipe.without_mirror();
    }
    Ok(recipe)
}

fn parse_shapeless(v: &Value) -> Result<ShapelessRecipe, LoadError> {
    let ings = field(v, "ingredients")?
        .as_array()
        .ok_or(LoadError::BadField("ingredients"))?;
    let ingredients = ings
        .iter()
        .map(parse_ingredient)
        .collect::<Result<Vec<_>, _>>()?;
    let result = parse_result(field(v, "result")?)?;
    Ok(ShapelessRecipe::new(ingredients, result).with_category(parse_category(v)))
}

fn parse_cooking(ty: &str, v: &Value) -> Result<CookingRecipe, LoadError> {
    let kind = match ty {
        "smelting" => CookingKind::Smelting,
        "blasting" => CookingKind::Blasting,
        "smoking" => CookingKind::Smoking,
        _ => CookingKind::CampfireCooking,
    };
    let default_time = match kind {
        CookingKind::Smelting => 200,
        CookingKind::Blasting | CookingKind::Smoking => 100,
        CookingKind::CampfireCooking => 600,
    };
    Ok(CookingRecipe {
        kind,
        ingredient: parse_ingredient(field(v, "ingredient")?)?,
        result: parse_result(field(v, "result")?)?,
        experience: v.get("experience").and_then(Value::as_f64).unwrap_or(0.0) as f32,
        cooking_time: v
            .get("cookingtime")
            .and_then(Value::as_i64)
            .map_or(default_time, |t| t as i32),
        category: parse_category(v),
    })
}

/// Parses a recipe's optional `"category"` field (present on 694 of 1585
/// recipes in 26.2's own datapack — `dropper.json`'s `"category": "redstone"`
/// is a representative example). Absent entirely defaults to
/// [`RecipeCategory::Misc`], matching vanilla's own default.
fn parse_category(v: &Value) -> RecipeCategory {
    v.get("category")
        .and_then(Value::as_str)
        .map_or(RecipeCategory::Misc, RecipeCategory::from_json_str)
}

/// Parses a tag file's `values` list into [`TagEntry`] items.
///
/// Entries may be bare strings or `{ "id": ..., "required": ... }` objects; a
/// leading `#` marks a nested tag reference.
///
/// # Errors
///
/// Returns [`LoadError`] if `values` is missing or an entry is malformed.
pub fn parse_tag(v: &Value) -> Result<Vec<TagEntry>, LoadError> {
    let values = field(v, "values")?
        .as_array()
        .ok_or(LoadError::BadField("values"))?;
    let mut out = Vec::with_capacity(values.len());
    for entry in values {
        let s = match entry {
            Value::String(s) => s.as_str(),
            Value::Object(map) => map
                .get("id")
                .and_then(Value::as_str)
                .ok_or(LoadError::BadField("tag entry id"))?,
            _ => return Err(LoadError::BadField("tag entry")),
        };
        if let Some(tag) = s.strip_prefix('#') {
            out.push(TagEntry::Tag(ident(tag)?));
        } else {
            out.push(TagEntry::Item(ident(s)?));
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Corpus loading
// ---------------------------------------------------------------------------

/// Accumulates a whole recipe corpus from arbitrarily-sourced JSON documents.
///
/// The builder is deliberately **source-agnostic**: it takes `(Identifier,
/// &str)` pairs and knows nothing about files, jars or the network.
/// [`load_data_root`] layers a filesystem walk on top; a zip-backed source (the
/// same `client.jar` [`lodestone_assets`](https://docs.rs) already reads for
/// models and lang) can feed the same two methods without this crate growing a
/// zip dependency.
///
/// Tags must be registered before matching, not before insertion — the builder
/// collects both and wires them together in [`finish`](Self::finish).
///
/// A malformed document does **not** abort the load. It is recorded in
/// [`failures`](Self::failures) and the rest of the corpus still loads, so one
/// unknown recipe type from a future version cannot leave a client with no
/// recipes at all.
#[derive(Debug, Default)]
pub struct CorpusBuilder {
    recipes: Vec<(Identifier, Recipe)>,
    tags: Vec<(Identifier, Vec<TagEntry>)>,
    failures: Vec<(String, LoadError)>,
}

impl CorpusBuilder {
    /// An empty builder.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Parses and stages one recipe document. Errors are recorded, not returned.
    pub fn push_recipe(&mut self, id: Identifier, json: &str) {
        match parse_json(json).and_then(|v| parse_recipe(&v)) {
            Ok(recipe) => self.recipes.push((id, recipe)),
            Err(e) => self.failures.push((id.to_string(), e)),
        }
    }

    /// Parses and stages one item-tag document. Errors are recorded, not
    /// returned.
    pub fn push_tag(&mut self, id: Identifier, json: &str) {
        match parse_json(json).and_then(|v| parse_tag(&v)) {
            Ok(entries) => self.tags.push((id, entries)),
            Err(e) => self.failures.push((id.to_string(), e)),
        }
    }

    /// Documents that failed to parse, as `(id, error)`.
    #[must_use]
    pub fn failures(&self) -> &[(String, LoadError)] {
        &self.failures
    }

    /// Number of recipes staged so far.
    #[must_use]
    pub fn recipe_count(&self) -> usize {
        self.recipes.len()
    }

    /// Number of item tags staged so far.
    #[must_use]
    pub fn tag_count(&self) -> usize {
        self.tags.len()
    }

    /// Builds the [`RecipeBook`], consuming the builder.
    #[must_use]
    pub fn finish(self) -> RecipeBook {
        let mut resolver = TagResolver::new();
        for (id, entries) in self.tags {
            resolver.insert(id, entries);
        }
        let mut book = RecipeBook::with_tags(resolver);
        for (id, recipe) in self.recipes {
            book.insert(id, recipe);
        }
        book
    }
}

fn parse_json(text: &str) -> Result<Value, LoadError> {
    serde_json::from_str(text).map_err(|e| LoadError::Json(e.to_string()))
}

/// Loads every recipe and item tag under a vanilla **datapack `data/` root**.
///
/// `root` is the directory that contains one subdirectory per namespace, i.e.
/// the `data/` inside `client.jar`:
///
/// ```text
/// data/minecraft/recipe/**/*.json
/// data/minecraft/tags/item/**/*.json
/// ```
///
/// Recursion matters: 26.2 nests item tags one level deep
/// (`tags/item/enchantable/weapon.json` resolves to
/// `minecraft:enchantable/weapon`), so a flat `read_dir` silently drops 33 of
/// the 224 tags. The id is the path relative to `recipe/` or `tags/item/` with
/// the `.json` suffix removed, so subdirectories become part of the path — the
/// same rule vanilla's `FileToIdConverter` uses.
///
/// # Errors
///
/// Returns an [`io::Error`](std::io::Error) only if `root` itself cannot be
/// read. Individual unreadable or malformed documents are recorded in the
/// returned builder's [`failures`](CorpusBuilder::failures).
pub fn load_data_root(root: &Path) -> std::io::Result<CorpusBuilder> {
    let mut builder = CorpusBuilder::new();
    for namespace in read_dir_sorted(root)? {
        let Some(ns) = namespace.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !namespace.is_dir() {
            continue;
        }
        load_tree(&mut builder, ns, &namespace.join("tags").join("item"), true);
        load_tree(&mut builder, ns, &namespace.join("recipe"), false);
    }
    Ok(builder)
}

/// Walks `base` recursively, feeding every `.json` file to the builder with an
/// id derived from its path relative to `base`.
fn load_tree(builder: &mut CorpusBuilder, namespace: &str, base: &Path, is_tag: bool) {
    let mut stack = vec![base.to_path_buf()];
    let mut files = Vec::new();
    while let Some(dir) = stack.pop() {
        let Ok(entries) = read_dir_sorted(&dir) else {
            continue;
        };
        for path in entries {
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|e| e.to_str()) == Some("json") {
                files.push(path);
            }
        }
    }
    files.sort();
    for path in files {
        let Ok(rel) = path.strip_prefix(base) else {
            continue;
        };
        // `/` is a legal identifier path character, and is the separator vanilla
        // itself uses, so nested files keep their subdirectory in the id.
        let mut id_path = rel.with_extension("").to_string_lossy().into_owned();
        if std::path::MAIN_SEPARATOR != '/' {
            id_path = id_path.replace(std::path::MAIN_SEPARATOR, "/");
        }
        let Ok(id) = Identifier::new(namespace, id_path) else {
            continue;
        };
        let Ok(text) = std::fs::read_to_string(&path) else {
            builder.failures.push((
                id.to_string(),
                LoadError::Json("file could not be read".to_string()),
            ));
            continue;
        };
        if is_tag {
            builder.push_tag(id, &text);
        } else {
            builder.push_recipe(id, &text);
        }
    }
}

fn read_dir_sorted(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(dir)?
        .flatten()
        .map(|e| e.path())
        .collect();
    out.sort();
    Ok(out)
}
