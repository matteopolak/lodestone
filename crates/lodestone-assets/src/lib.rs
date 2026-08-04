//! Vanilla resource loading for Lodestone.
//!
//! This crate speaks Mojang's on-disk resource-pack format natively so that any
//! pack that works in the real game works here unmodified. Vanilla's own assets
//! (shipped inside `client.jar`, which is an ordinary zip) are treated as just
//! the lowest-priority pack in the stack, so "use the real textures" and "use a
//! custom pack" are the same code path.
//!
//! # The pack-stack model
//!
//! A [`ResourceSource`] is a single pack: a directory tree or a zip/jar. A
//! [`ResourceManager`] holds an ordered stack of sources, **lowest priority
//! first**. A lookup is served by the highest-priority pack that contains the
//! resource, matching vanilla's override semantics.
//!
//! ```
//! use lodestone_assets::{ResourceLocation, ResourceManager};
//! # use lodestone_assets::MemorySource;
//! let mut vanilla = MemorySource::new("vanilla");
//! vanilla.insert("assets/minecraft/textures/block/stone.png", b"vanilla".to_vec());
//! let mut user = MemorySource::new("user");
//! user.insert("assets/minecraft/textures/block/stone.png", b"custom".to_vec());
//!
//! // Vanilla at the bottom, user pack on top.
//! let manager = ResourceManager::new(vec![Box::new(vanilla), Box::new(user)]);
//! let loc = ResourceLocation::parse("minecraft:block/stone").unwrap();
//! let bytes = manager.read_asset(&loc, "textures", "png").unwrap();
//! assert_eq!(bytes, b"custom"); // highest-priority pack wins
//! ```
//!
//! # Version drift
//!
//! Asset path conventions change across Minecraft versions (for example
//! `textures/blocks/` before 1.13 versus `textures/block/` after). Rather than
//! branching on version inside the loader, that knowledge is captured in an
//! [`AssetProfile`] supplied by a version crate. The loader itself contains no
//! version branching.
//!
//! # From packs to pixels
//!
//! On top of the pack stack sit three layers:
//!
//! - **Blockstates & models.** [`BlockStates`] parses both the `variants` and
//!   `multipart` forms; [`ModelResolver`] flattens a model's parent chain and
//!   resolves its `#texture` variables into a renderer-ready [`ResolvedModel`]
//!   (geometry only — no UV baking).
//! - **Textures.** [`Image::decode_png`](Image) decodes any PNG colour type to
//!   RGBA8, and [`TextureMeta`] parses the sibling `*.png.mcmeta` animation
//!   metadata.
//! - **Atlas.** [`AtlasBuilder`] stitches the decoded textures a model set
//!   references into a single, deterministic, **GPU-free** [`Atlas`] (plain
//!   RGBA8 bytes plus per-sprite UVs and animation info). No `wgpu` dependency
//!   lives here, so the atlas is testable headlessly and reusable by non-visual
//!   consumers.
//!
//! ## Atlas layout: 2D atlas vs. texture array
//!
//! [`AtlasBuilder`] currently emits a single 2D atlas (`layers == 1`) with real
//! UV rectangles, and each [`AtlasSprite`] carries a `layer` field so the same
//! type can describe a texture-array layout later without an API break. For
//! production rendering a **texture array** is the recommended target: in 26.2's
//! `client.jar` the overwhelming majority of block textures are exactly 16×16
//! (the taller ones are 16-wide vertical animation strips of 16×16 frames), so
//! array layers of a common tile size fit vanilla almost perfectly, and giving
//! each sprite its own layer sidesteps the mip-bleed that a packed 2D atlas
//! suffers (a naive box-filter mip pyramid averages across sprite boundaries).
//! Mip generation is deliberately left to the renderer/GPU; this crate only
//! produces the CPU-side pixels and layout.

#![warn(missing_docs)]

mod atlas;
pub mod atlas_source;
mod bake;
pub mod block_entity_models;
mod blockstate;
pub mod entity;
pub mod entity_models;
pub mod equipment;
mod error;
pub mod fluid;
pub mod font;
pub mod gui;
pub mod icon;
pub mod item;
pub mod item_atlas;
pub mod item_model;
pub mod lang;
mod location;
mod manager;
mod meta;
pub mod mipmap;
mod model;
pub mod particle;
pub mod particle_atlas;
mod profile;
pub mod screen_effects;
pub mod sky;
pub mod sound;
mod source;
mod texture;
pub mod tint;

pub use atlas::{
    AnimSlot, AnimSlotFrame, AnimTable, Atlas, AtlasBuilder, AtlasSprite, MipCap, MipLevelRef,
    SpriteFrameSample,
};
pub use atlas_source::{AtlasDefinition, AtlasSource, AtlasSpriteEntry};
pub use bake::{
    BakeOptions, BakedModel, BakedQuad, BlockBaker, FirstWeight, ModelTransform, SeededWeight,
    WeightSelector, bake_model, bake_model_with,
};
pub use blockstate::{
    BlockStateDefinition, BlockStates, ModelRef, MultipartCase, When, parse_variant_key,
};
pub use error::AtlasSourceError;
pub use error::{
    AssetError, AtlasError, BakeError, BlockStateError, FontError, GuiError, IconError,
    ItemAtlasError, ItemModelError, ModelError, ParticleAtlasError, ParticleError,
    ResourceLocationError, ScreenEffectAssetError, SkyAssetError, SoundError, TextureError,
    TintError,
};
pub use icon::{
    DISPLAY_CONTEXT_PROPERTY, DefaultItemContext, DisplayContextItemContext, GuiItemContext,
    IconPart, ItemIcon, ItemIconBuilder, SpriteLayer,
};
pub use item_atlas::{ItemAtlas, ItemAtlasReport};
pub use lang::Language;
pub use item_model::{
    ItemModel, ItemModelNode, ItemModelOutput, ItemPropertyContext, RangeEntry, SelectCase,
    TintSource,
};
pub use location::ResourceLocation;
pub use manager::ResourceManager;
pub use meta::{PackDescription, PackMeta, PackVersion, VersionMeta};
pub use mipmap::{MipStrategy, Transparency, generate_mip_levels, max_mip_level};
pub use model::{
    Axis, Direction, DisplaySlot, DisplayTransform, DisplayTransforms, Element, ElementRotation,
    Face, GuiLight, ModelResolver, RawModel, ResolvedModel, TextureBinding,
};
pub use particle_atlas::{ParticleAtlas, ParticleAtlasReport};
pub use profile::AssetProfile;
pub use screen_effects::{
    FIRE_FRAME_SIZE, fire_frame_count, load_fire_texture, load_pumpkin_overlay_texture,
    load_underwater_texture,
};
pub use sky::{CelestialAtlas, MOON_PHASE_NAMES, SUN_SPRITE_PATH, load_cloud_texture};
#[cfg(not(target_arch = "wasm32"))]
pub use source::DirectorySource;
pub use source::{MemorySource, ResourceSource, ZipSource};
pub use texture::{AnimationFrame, AnimationMeta, Image, TextureMeta};
