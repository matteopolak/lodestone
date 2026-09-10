//! Typed JSON loading of recipes and tags from generated datapack data.
//!
//! Gated behind the `json` cargo feature so the default build stays free of a
//! JSON dependency. Text first crosses a typed DTO boundary: known recipe
//! shapes, recursive ingredients, result stacks, and tag entries are all
//! represented by Rust types. An unsupported recipe retains its original JSON
//! object in a `RawValue` so a future caller can forward it without losing
//! fields that this client does not understand.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use bon::bon;
use lodestone_model::Identifier;
use serde::de;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::value::RawValue;
use thiserror::Error;

use crate::item::ItemStack;
use crate::recipe::{
    CookingKind, CookingRecipe, Ingredient, Recipe, RecipeBook, RecipeCategory, ShapedRecipe,
    ShapelessRecipe, TagEntry, TagResolver,
};

pub use lodestone_model::ResourceKey;

/// An error loading a recipe or tag document.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum LoadError {
    /// The `type` field was missing or not a string.
    #[error("recipe has no `type`")]
    MissingType,
    /// A required field was absent or malformed after typed decoding.
    #[error("bad or missing field `{0}`")]
    BadField(&'static str),
    /// An identifier failed to parse.
    #[error("invalid identifier `{0}`")]
    BadIdentifier(String),
    /// The bytes were not valid JSON at all.
    #[error("malformed JSON: {0}")]
    Json(String),
}

fn ident(s: &str) -> Result<ResourceKey, LoadError> {
    s.parse()
        .map_err(|_| LoadError::BadIdentifier(s.to_string()))
}

fn serialize_key<S>(key: &ResourceKey, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serializer.serialize_str(&key.to_string())
}

/// The six recipe-book categories accepted by generated recipe documents.
/// Unknown strings remain explicit so decoding a future category does not
/// collapse the document into an untyped JSON value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecipeCategoryDocument {
    /// Building blocks.
    Building,
    /// Redstone components.
    Redstone,
    /// Equipment.
    Equipment,
    /// Miscellaneous recipes.
    Misc,
    /// Food.
    Food,
    /// Furnace blocks.
    Blocks,
    /// A category introduced after this client was built.
    Other(String),
}

impl RecipeCategoryDocument {
    fn from_text(text: String) -> Self {
        match text.as_str() {
            "building" => Self::Building,
            "redstone" => Self::Redstone,
            "equipment" => Self::Equipment,
            "misc" => Self::Misc,
            "food" => Self::Food,
            "blocks" => Self::Blocks,
            _ => Self::Other(text),
        }
    }

    fn as_text(&self) -> &str {
        match self {
            Self::Building => "building",
            Self::Redstone => "redstone",
            Self::Equipment => "equipment",
            Self::Misc => "misc",
            Self::Food => "food",
            Self::Blocks => "blocks",
            Self::Other(text) => text,
        }
    }

    fn to_domain(&self) -> RecipeCategory {
        RecipeCategory::from_json_str(self.as_text())
    }
}

impl Serialize for RecipeCategoryDocument {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_text())
    }
}

impl<'de> Deserialize<'de> for RecipeCategoryDocument {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Ok(Self::from_text(String::deserialize(deserializer)?))
    }
}

/// A recursive typed ingredient. A string beginning with `#` is a tag; a
/// string without it is an item key. Arrays are alternatives and may contain
/// further arrays or object-form item/tag entries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IngredientDocument {
    /// A concrete item key.
    Item(ResourceKey),
    /// An item-tag key.
    Tag(ResourceKey),
    /// Any one of the nested alternatives.
    Any(Vec<IngredientDocument>),
}

impl Serialize for IngredientDocument {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::Item(key) => serialize_key(key, serializer),
            Self::Tag(key) => serializer.serialize_str(&format!("#{key}")),
            Self::Any(options) => options.serialize(serializer),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RawIngredientDocument {
    Text(String),
    Alternatives(Vec<RawIngredientDocument>),
    Object(RawIngredientObject),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawIngredientObject {
    #[serde(default)]
    item: Option<String>,
    #[serde(default)]
    tag: Option<String>,
}

fn ingredient_from_text(text: &str) -> Result<IngredientDocument, LoadError> {
    if let Some(tag) = text.strip_prefix('#') {
        Ok(IngredientDocument::Tag(ident(tag)?))
    } else {
        Ok(IngredientDocument::Item(ident(text)?))
    }
}

fn ingredient_from_raw(raw: RawIngredientDocument) -> Result<IngredientDocument, LoadError> {
    match raw {
        RawIngredientDocument::Text(text) => ingredient_from_text(&text),
        RawIngredientDocument::Alternatives(options) => options
            .into_iter()
            .map(ingredient_from_raw)
            .collect::<Result<Vec<_>, _>>()
            .map(IngredientDocument::Any),
        RawIngredientDocument::Object(object) => match (object.item, object.tag) {
            (Some(item), None) => Ok(IngredientDocument::Item(ident(&item)?)),
            (None, Some(tag)) => Ok(IngredientDocument::Tag(ident(
                tag.strip_prefix('#').unwrap_or(&tag),
            )?)),
            _ => Err(LoadError::BadField("ingredient")),
        },
    }
}

impl<'de> Deserialize<'de> for IngredientDocument {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        ingredient_from_raw(RawIngredientDocument::deserialize(deserializer)?)
            .map_err(de::Error::custom)
    }
}

/// A recipe result, accepted in the compact string form or the object form
/// with an optional output count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResultDocument {
    /// One item with the schema's implicit count of one.
    Item(ResourceKey),
    /// An item and an optional explicit count.
    Stack {
        /// Output item key.
        id: ResourceKey,
        /// Explicit count, or `None` when the JSON omitted it.
        count: Option<i32>,
    },
}

impl ResultDocument {
    /// The output item key.
    #[must_use]
    pub fn id(&self) -> &ResourceKey {
        match self {
            Self::Item(id) | Self::Stack { id, .. } => id,
        }
    }

    /// The effective output count.
    #[must_use]
    pub fn count(&self) -> i32 {
        match self {
            Self::Item(_) => 1,
            Self::Stack { count, .. } => count.unwrap_or(1),
        }
    }
}

#[bon]
impl ResultDocument {
    /// Constructs an object-form result. The named builder keeps the two
    /// same-domain fields explicit for programmatic callers; serde decoding
    /// never uses it.
    #[builder(start_fn = stack_builder)]
    #[must_use]
    pub fn stack(id: ResourceKey, count: Option<i32>) -> Self {
        Self::Stack { id, count }
    }
}

impl Serialize for ResultDocument {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::Item(id) => serialize_key(id, serializer),
            Self::Stack { id, count } => {
                use serde::ser::SerializeStruct;
                let mut object = serializer.serialize_struct("ResultDocument", 2)?;
                object.serialize_field("id", &id.to_string())?;
                if let Some(count) = count {
                    object.serialize_field("count", count)?;
                }
                object.end()
            }
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RawResultDocument {
    Text(String),
    Stack(RawResultStack),
}

#[derive(Debug, Deserialize)]
struct RawResultStack {
    id: String,
    #[serde(default)]
    count: Option<i32>,
}

impl<'de> Deserialize<'de> for ResultDocument {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        match RawResultDocument::deserialize(deserializer)? {
            RawResultDocument::Text(id) => Ok(Self::Item(ident(&id).map_err(de::Error::custom)?)),
            RawResultDocument::Stack(stack) => Ok(Self::Stack {
                id: ident(&stack.id).map_err(de::Error::custom)?,
                count: stack.count,
            }),
        }
    }
}

/// One item-tag value. Object entries with a `required` flag are accepted and
/// canonicalized to the same typed value; this client always treats a tag
/// member as required when resolving it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TagValueDocument {
    /// A concrete item member.
    Item(ResourceKey),
    /// A nested tag member.
    Tag(ResourceKey),
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RawTagValueDocument {
    Text(String),
    Object(RawTagObject),
}

#[derive(Debug, Deserialize)]
struct RawTagObject {
    id: String,
    #[allow(dead_code)]
    required: Option<bool>,
}

impl<'de> Deserialize<'de> for TagValueDocument {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let text = match RawTagValueDocument::deserialize(deserializer)? {
            RawTagValueDocument::Text(text) => text,
            RawTagValueDocument::Object(object) => object.id,
        };
        if let Some(tag) = text.strip_prefix('#') {
            Ok(Self::Tag(ident(tag).map_err(de::Error::custom)?))
        } else {
            Ok(Self::Item(ident(&text).map_err(de::Error::custom)?))
        }
    }
}

impl Serialize for TagValueDocument {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::Item(id) => serialize_key(id, serializer),
            Self::Tag(id) => serializer.serialize_str(&format!("#{id}")),
        }
    }
}

/// A typed item-tag document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TagDocument {
    /// Direct item or nested-tag members.
    pub values: Vec<TagValueDocument>,
    /// The optional pack-layer replacement marker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replace: Option<bool>,
}

/// A shaped recipe's typed body, without its top-level `type` discriminator.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShapedRecipeDocument {
    /// Recipe-book category, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<RecipeCategoryDocument>,
    /// Optional grouping label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// Pattern rows, in row-major order.
    pub pattern: Vec<String>,
    /// Character-to-ingredient mapping.
    pub key: BTreeMap<String, IngredientDocument>,
    /// Output stack.
    pub result: ResultDocument,
    /// Whether mirrored matching is permitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mirror: Option<bool>,
    /// Optional UI notification marker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub show_notification: Option<bool>,
}

/// A shapeless recipe's typed body, without its top-level `type` discriminator.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShapelessRecipeDocument {
    /// Recipe-book category, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<RecipeCategoryDocument>,
    /// Optional grouping label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// Unordered ingredient multiset.
    pub ingredients: Vec<IngredientDocument>,
    /// Output stack.
    pub result: ResultDocument,
}

/// One cooking recipe's typed body, without its top-level `type` discriminator.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CookingRecipeDocument {
    /// Recipe-book category, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<RecipeCategoryDocument>,
    /// Optional grouping label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// The single cooking input.
    pub ingredient: IngredientDocument,
    /// Output stack.
    pub result: ResultDocument,
    /// Experience payout, defaulting to zero.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub experience: Option<f32>,
    /// Cooking duration, defaulting by recipe kind.
    #[serde(
        rename = "cookingtime",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub cooking_time: Option<i32>,
}

/// A stonecutting recipe's typed body.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StonecuttingRecipeDocument {
    /// Input ingredient.
    pub ingredient: IngredientDocument,
    /// Output stack.
    pub result: ResultDocument,
}

/// A smithing transform's typed body.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SmithingTransformRecipeDocument {
    /// Upgrade template.
    pub template: IngredientDocument,
    /// Base item.
    pub base: IngredientDocument,
    /// Addition material.
    pub addition: IngredientDocument,
    /// Output stack.
    pub result: ResultDocument,
}

#[bon]
impl SmithingTransformRecipeDocument {
    /// Constructs a transform body with named setters for its three
    /// same-shaped ingredients. Deserialization uses the derived serde
    /// implementation instead of this builder.
    #[builder(start_fn = builder)]
    #[must_use]
    pub fn new(
        template: IngredientDocument,
        base: IngredientDocument,
        addition: IngredientDocument,
        result: ResultDocument,
    ) -> Self {
        Self {
            template,
            base,
            addition,
            result,
        }
    }
}

/// A smithing trim's typed body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SmithingTrimRecipeDocument {
    /// Trim template.
    pub template: IngredientDocument,
    /// Trimmable base.
    pub base: IngredientDocument,
    /// Trim material.
    pub addition: IngredientDocument,
}

#[bon]
impl SmithingTrimRecipeDocument {
    /// Constructs a trim body with named setters for its three same-shaped
    /// ingredients. Deserialization uses the derived serde implementation.
    #[builder(start_fn = builder)]
    #[must_use]
    pub fn new(
        template: IngredientDocument,
        base: IngredientDocument,
        addition: IngredientDocument,
    ) -> Self {
        Self {
            template,
            base,
            addition,
        }
    }
}

/// A transmutation recipe's typed body.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TransmuteRecipeDocument {
    /// Input item.
    pub input: IngredientDocument,
    /// Material consumed.
    pub material: IngredientDocument,
    /// Output stack.
    pub result: ResultDocument,
}

/// A recipe type this client does not interpret. The complete raw object is
/// retained so callers can forward it without losing future fields.
#[derive(Debug, Clone)]
pub struct UnsupportedRecipeDocument {
    /// The source discriminator, including its namespace when supplied.
    pub recipe_type: String,
    /// The complete source object, retained as raw JSON.
    pub raw: Box<RawValue>,
}

impl PartialEq for UnsupportedRecipeDocument {
    fn eq(&self, other: &Self) -> bool {
        self.recipe_type == other.recipe_type && self.raw.get() == other.raw.get()
    }
}

impl UnsupportedRecipeDocument {
    /// Returns the source JSON object exactly as retained by the decoder.
    #[must_use]
    pub fn raw_json(&self) -> &str {
        self.raw.get()
    }
}

/// A typed recipe document. Known variants carry only their schema fields;
/// unsupported variants retain the complete source object.
#[derive(Debug, Clone, PartialEq)]
pub enum RecipeDocument {
    /// Shaped crafting.
    Shaped(ShapedRecipeDocument),
    /// Shapeless crafting.
    Shapeless(ShapelessRecipeDocument),
    /// Smelting.
    Smelting(CookingRecipeDocument),
    /// Blasting.
    Blasting(CookingRecipeDocument),
    /// Smoking.
    Smoking(CookingRecipeDocument),
    /// Campfire cooking.
    CampfireCooking(CookingRecipeDocument),
    /// Stonecutting.
    Stonecutting(StonecuttingRecipeDocument),
    /// Smithing transform.
    SmithingTransform(SmithingTransformRecipeDocument),
    /// Smithing trim.
    SmithingTrim(SmithingTrimRecipeDocument),
    /// Transmutation.
    Transmute(TransmuteRecipeDocument),
    /// An unhandled recipe kind.
    Unsupported(UnsupportedRecipeDocument),
}

#[derive(Debug, Deserialize)]
struct RecipeTypeProbe {
    #[serde(rename = "type")]
    recipe_type: Option<String>,
}

fn recipe_path(recipe_type: &str) -> &str {
    recipe_type
        .strip_prefix("minecraft:")
        .unwrap_or(recipe_type)
}

impl<'de> Deserialize<'de> for RecipeDocument {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = Box::<RawValue>::deserialize(deserializer)?;
        let probe: RecipeTypeProbe = serde_json::from_str(raw.get()).map_err(de::Error::custom)?;
        let recipe_type = probe
            .recipe_type
            .ok_or_else(|| de::Error::custom("recipe has no `type`"))?;
        let decoded = match recipe_path(&recipe_type) {
            "crafting_shaped" => {
                RecipeDocument::Shaped(serde_json::from_str(raw.get()).map_err(de::Error::custom)?)
            }
            "crafting_shapeless" => RecipeDocument::Shapeless(
                serde_json::from_str(raw.get()).map_err(de::Error::custom)?,
            ),
            "smelting" => RecipeDocument::Smelting(
                serde_json::from_str(raw.get()).map_err(de::Error::custom)?,
            ),
            "blasting" => RecipeDocument::Blasting(
                serde_json::from_str(raw.get()).map_err(de::Error::custom)?,
            ),
            "smoking" => {
                RecipeDocument::Smoking(serde_json::from_str(raw.get()).map_err(de::Error::custom)?)
            }
            "campfire_cooking" => RecipeDocument::CampfireCooking(
                serde_json::from_str(raw.get()).map_err(de::Error::custom)?,
            ),
            "stonecutting" => RecipeDocument::Stonecutting(
                serde_json::from_str(raw.get()).map_err(de::Error::custom)?,
            ),
            "smithing_transform" => RecipeDocument::SmithingTransform(
                serde_json::from_str(raw.get()).map_err(de::Error::custom)?,
            ),
            "smithing_trim" => RecipeDocument::SmithingTrim(
                serde_json::from_str(raw.get()).map_err(de::Error::custom)?,
            ),
            "crafting_transmute" => RecipeDocument::Transmute(
                serde_json::from_str(raw.get()).map_err(de::Error::custom)?,
            ),
            _ => RecipeDocument::Unsupported(UnsupportedRecipeDocument { recipe_type, raw }),
        };
        Ok(decoded)
    }
}

#[derive(Serialize)]
struct TaggedDocument<'a, T: Serialize> {
    #[serde(rename = "type")]
    recipe_type: &'static str,
    #[serde(flatten)]
    body: &'a T,
}

impl Serialize for RecipeDocument {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::Shaped(body) => TaggedDocument {
                recipe_type: "minecraft:crafting_shaped",
                body,
            }
            .serialize(serializer),
            Self::Shapeless(body) => TaggedDocument {
                recipe_type: "minecraft:crafting_shapeless",
                body,
            }
            .serialize(serializer),
            Self::Smelting(body) => TaggedDocument {
                recipe_type: "minecraft:smelting",
                body,
            }
            .serialize(serializer),
            Self::Blasting(body) => TaggedDocument {
                recipe_type: "minecraft:blasting",
                body,
            }
            .serialize(serializer),
            Self::Smoking(body) => TaggedDocument {
                recipe_type: "minecraft:smoking",
                body,
            }
            .serialize(serializer),
            Self::CampfireCooking(body) => TaggedDocument {
                recipe_type: "minecraft:campfire_cooking",
                body,
            }
            .serialize(serializer),
            Self::Stonecutting(body) => TaggedDocument {
                recipe_type: "minecraft:stonecutting",
                body,
            }
            .serialize(serializer),
            Self::SmithingTransform(body) => TaggedDocument {
                recipe_type: "minecraft:smithing_transform",
                body,
            }
            .serialize(serializer),
            Self::SmithingTrim(body) => TaggedDocument {
                recipe_type: "minecraft:smithing_trim",
                body,
            }
            .serialize(serializer),
            Self::Transmute(body) => TaggedDocument {
                recipe_type: "minecraft:crafting_transmute",
                body,
            }
            .serialize(serializer),
            Self::Unsupported(unsupported) => unsupported.raw.serialize(serializer),
        }
    }
}

impl AsRef<RecipeDocument> for RecipeDocument {
    fn as_ref(&self) -> &RecipeDocument {
        self
    }
}

fn ingredient(document: &IngredientDocument) -> Ingredient {
    match document {
        IngredientDocument::Item(id) => Ingredient::Item(id.clone()),
        IngredientDocument::Tag(id) => Ingredient::Tag(id.clone()),
        IngredientDocument::Any(options) => {
            Ingredient::Any(options.iter().map(ingredient).collect())
        }
    }
}

fn result(document: &ResultDocument) -> ItemStack {
    ItemStack::new(document.id().clone(), document.count())
}

fn category(document: Option<&RecipeCategoryDocument>) -> RecipeCategory {
    document.map_or(RecipeCategory::Misc, RecipeCategoryDocument::to_domain)
}

fn shaped(document: &ShapedRecipeDocument) -> Result<ShapedRecipe, LoadError> {
    let height = document.pattern.len();
    let width = document
        .pattern
        .iter()
        .map(|row| row.chars().count())
        .max()
        .unwrap_or(0);
    let mut cells = Vec::with_capacity(width * height);
    for row in &document.pattern {
        let chars: Vec<char> = row.chars().collect();
        for x in 0..width {
            let character = chars.get(x).copied().unwrap_or(' ');
            if character == ' ' {
                cells.push(None);
            } else {
                let key = character.to_string();
                let ingredient_document = document
                    .key
                    .get(&key)
                    .ok_or(LoadError::BadField("key char"))?;
                cells.push(Some(ingredient(ingredient_document)));
            }
        }
    }
    let mut recipe = ShapedRecipe::new(width, height, cells, result(&document.result))
        .with_category(category(document.category.as_ref()));
    if document.mirror == Some(false) {
        recipe = recipe.without_mirror();
    }
    Ok(recipe)
}

fn shapeless(document: &ShapelessRecipeDocument) -> ShapelessRecipe {
    ShapelessRecipe::new(
        document.ingredients.iter().map(ingredient).collect(),
        result(&document.result),
    )
    .with_category(category(document.category.as_ref()))
}

fn cooking(document: &CookingRecipeDocument, kind: CookingKind) -> CookingRecipe {
    let default_time = match kind {
        CookingKind::Smelting => 200,
        CookingKind::Blasting | CookingKind::Smoking => 100,
        CookingKind::CampfireCooking => 600,
    };
    CookingRecipe {
        kind,
        ingredient: ingredient(&document.ingredient),
        result: result(&document.result),
        experience: document.experience.unwrap_or(0.0),
        cooking_time: document.cooking_time.unwrap_or(default_time),
        category: category(document.category.as_ref()),
    }
}

/// Parses a typed recipe document into the version-free recipe model.
///
/// The input is deliberately a DTO rather than an untyped JSON tree; callers
/// that start with text should use [`parse_recipe_text`] or
/// [`parse_recipe_document`].
pub fn parse_recipe(document: impl AsRef<RecipeDocument>) -> Result<Recipe, LoadError> {
    match document.as_ref() {
        RecipeDocument::Shaped(document) => shaped(document).map(Recipe::Shaped),
        RecipeDocument::Shapeless(document) => Ok(Recipe::Shapeless(shapeless(document))),
        RecipeDocument::Smelting(document) => {
            Ok(Recipe::Cooking(cooking(document, CookingKind::Smelting)))
        }
        RecipeDocument::Blasting(document) => {
            Ok(Recipe::Cooking(cooking(document, CookingKind::Blasting)))
        }
        RecipeDocument::Smoking(document) => {
            Ok(Recipe::Cooking(cooking(document, CookingKind::Smoking)))
        }
        RecipeDocument::CampfireCooking(document) => Ok(Recipe::Cooking(cooking(
            document,
            CookingKind::CampfireCooking,
        ))),
        RecipeDocument::Stonecutting(document) => Ok(Recipe::Stonecutting {
            ingredient: ingredient(&document.ingredient),
            result: result(&document.result),
        }),
        RecipeDocument::SmithingTransform(document) => Ok(Recipe::SmithingTransform {
            template: ingredient(&document.template),
            base: ingredient(&document.base),
            addition: ingredient(&document.addition),
            result: result(&document.result),
        }),
        RecipeDocument::SmithingTrim(document) => Ok(Recipe::SmithingTrim {
            template: ingredient(&document.template),
            base: ingredient(&document.base),
            addition: ingredient(&document.addition),
        }),
        RecipeDocument::Transmute(document) => Ok(Recipe::Transmute {
            input: ingredient(&document.input),
            material: ingredient(&document.material),
            result: result(&document.result),
        }),
        RecipeDocument::Unsupported(document) => Ok(Recipe::Special(
            recipe_path(&document.recipe_type).to_string(),
        )),
    }
}

/// Decodes a recipe document from text without exposing an untyped JSON value.
pub fn parse_recipe_document(text: &str) -> Result<RecipeDocument, LoadError> {
    let probe: RecipeTypeProbe =
        serde_json::from_str(text).map_err(|error| LoadError::Json(error.to_string()))?;
    if probe.recipe_type.is_none() {
        return Err(LoadError::MissingType);
    }
    serde_json::from_str(text).map_err(|error| LoadError::Json(error.to_string()))
}

/// Decodes a recipe from text into the version-free recipe model.
pub fn parse_recipe_text(text: &str) -> Result<Recipe, LoadError> {
    parse_recipe_document(text).and_then(parse_recipe)
}

/// Parses a typed tag document into tag resolver entries.
pub fn parse_tag(document: impl AsRef<TagDocument>) -> Result<Vec<TagEntry>, LoadError> {
    Ok(document
        .as_ref()
        .values
        .iter()
        .map(|entry| match entry {
            TagValueDocument::Item(id) => TagEntry::Item(id.clone()),
            TagValueDocument::Tag(id) => TagEntry::Tag(id.clone()),
        })
        .collect())
}

impl AsRef<TagDocument> for TagDocument {
    fn as_ref(&self) -> &TagDocument {
        self
    }
}

/// Decodes a tag document from text.
pub fn parse_tag_document(text: &str) -> Result<TagDocument, LoadError> {
    serde_json::from_str(text).map_err(|error| LoadError::Json(error.to_string()))
}

/// Decodes a tag from text into resolver entries.
pub fn parse_tag_text(text: &str) -> Result<Vec<TagEntry>, LoadError> {
    parse_tag_document(text).and_then(parse_tag)
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
        match parse_recipe_text(json) {
            Ok(recipe) => self.recipes.push((id, recipe)),
            Err(e) => self.failures.push((id.to_string(), e)),
        }
    }

    /// Parses and stages one item-tag document. Errors are recorded, not
    /// returned.
    pub fn push_tag(&mut self, id: Identifier, json: &str) {
        match parse_tag_text(json) {
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
