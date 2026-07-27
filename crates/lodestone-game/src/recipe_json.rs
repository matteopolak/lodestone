//! JSON loading of recipes and tags from Mojang's generated data.
//!
//! Gated behind the `json` cargo feature so the default build stays free of a
//! JSON dependency. The parser works off `serde_json::Value` rather than a
//! derived schema so that a single unexpected field never fails a whole-corpus
//! load — it tolerates unknown keys and reports only genuinely unparseable
//! recipes.

use lodestone_model::Identifier;
use serde_json::Value;

use crate::item::ItemStack;
use crate::recipe::{
    CookingKind, CookingRecipe, Ingredient, Recipe, ShapedRecipe, ShapelessRecipe, TagEntry,
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
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadError::MissingType => write!(f, "recipe has no `type`"),
            LoadError::BadField(field) => write!(f, "bad or missing field `{field}`"),
            LoadError::BadIdentifier(s) => write!(f, "invalid identifier `{s}`"),
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
    let mut recipe = ShapedRecipe::new(width, height, cells, result);
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
    Ok(ShapelessRecipe::new(ingredients, result))
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
    })
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
