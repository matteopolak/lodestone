//! GPU foundation for Lodestone.
//!
//! This crate is the rendering foundation: device/adapter bring-up, inspectable
//! capability detection, pluggable draw-submission strategies, an abstract
//! render target (headless *or* windowed), a frame-loop scaffold, and GPU buffer
//! suballocation for the chunk-mesh workload.
//!
//! # Design spine: detect vs. decide
//!
//! The recurring principle is a hard split between **detecting** what the GPU
//! can do and **deciding** what to do about it:
//!
//! * Detection ([`device`]) turns a live `wgpu` adapter into a plain
//!   [`GpuCapabilities`] struct with no GPU handles inside it.
//! * Decisions ([`strategy::select_strategy`], [`GpuCapabilities`] predicates,
//!   the [`suballoc`] allocator) are **pure functions** over that data, so they
//!   are unit-tested exhaustively with no GPU and no window server. Only the
//!   thin GPU-backed wrappers require real hardware, and those tests are
//!   `#[ignore]`d.
//!
//! Runtime probing is authoritative: we never gate behaviour on a hardcoded
//! backend/platform assumption, because published guidance about (for instance)
//! binding arrays on Metal has been wrong before.
//!
//! # What a mesh producer plugs into
//!
//! A later meshing layer (wired to `lodestone-world` / `lodestone-assets`)
//! allocates vertex/index spans from [`ArenaBuffer`], writes packed geometry,
//! and emits a slice of [`DrawRegion`] each frame. The selected
//! [`DrawStrategy`] turns that slice into GPU work. This crate has **no**
//! dependency on those crates yet by design.

#![warn(missing_docs)]

pub mod air_bubbles;
pub mod anim;
pub mod arena;
pub mod block;
pub mod block_entity;
pub mod block_models;
pub mod block_resolver;
pub mod blocks_json;
pub mod camera;
pub mod caps;
/// Vanilla's **fancy** cloud mesh — `clouds.png` voxelized into cells and the
/// visible faces extruded. The `FANCY` counterpart to
/// [`sky::cloud_plane_geometry`]'s flat `FAST` quad; pure geometry, no GPU.
pub mod cloud_mesh;
pub mod crack;
pub mod crack_pipeline;
pub mod crack_resolver;
pub mod device;
pub mod driver;
pub mod entity;
pub mod entity_anim;
pub mod entity_pipeline;
pub mod fog;
pub mod frame;
pub mod gui_atlas;
pub mod item_render;
pub mod light;
pub mod mesh;
pub mod mesher;
pub mod model_pipeline;
pub mod models;
pub mod scene;
pub mod screen_effects;
pub mod section;
pub mod section_arena;
pub mod sky;
pub mod sky_pipeline;
pub mod strategy;
pub mod suballoc;
pub mod target;
pub mod texture;
pub mod translucency;
pub mod vertex;
pub mod visibility;
pub mod weather;
pub mod weather_pipeline;
pub mod world;

#[cfg(feature = "window")]
pub mod window;

pub use air_bubbles::{
    BUBBLE_COUNT, BUBBLE_SEPARATION, BUBBLE_SIZE, BubbleSlot, bubble_position, bubble_row,
    bubble_row_visible,
};
pub use anim::{AnimFrame, AnimSample, AnimSlotUniform, AnimUniform, SpriteAnimation};
pub use arena::{ArenaAllocation, ArenaBuffer, ArenaError};
pub use block::{BlockPipeline, CameraUniform, DEPTH_FORMAT, DepthBuffer, GpuMesh};
pub use block_entity::{
    BlockEntityBatch, BlockEntityCullStats, BlockEntityFrame, BlockEntityInstance, BlockEntityMesh,
    BlockEntityModelSet, CHEST_LEFT, CHEST_MATERIALS, CHEST_RIGHT, CHEST_SINGLE, ChestHalf,
    ChestMaterial, ChestSpawn, block_entity_placement_matrix, chest_lid_openness, chest_lid_x_rot,
    chest_material_with_season, chest_texture_stem, chest_texture_stems, horizontal_facing_yaw,
    plan_block_entities,
};
pub use block_models::{
    BlockModels, BlockModelsError, CRACK_STAGE_COUNT, FluidCell, FluidKind, FluidSprites,
    ItemGeometry, ItemVariants, StateModel,
};
pub use block_resolver::{BlockAtlas, BlockAtlasError, MAX_SPRITES};
pub use blocks_json::{BlocksJsonError, BlocksJsonRegistry};
// `blocks_json::blocks_json_registry` is itself gated (it re-exports the
// native-only disk loader in `blocks_json_native.rs`, confined there so
// `std::fs` cannot leak onto the wasm path — see `blocks_json.rs`'s `mod
// native`). This re-export has to carry the identical `cfg`, the same way
// `frame::SystemClock` does a few lines down: a blanket `pub use` here would
// try to name an item that does not exist under `--target wasm32-unknown-
// unknown`, which is exactly the `unresolved import` this crate was failing
// wasm-check.sh with. A loader that reads `std::fs` cannot exist on wasm
// regardless, so gating the *symbol* is correct — the alternative, a stub
// that silently returned an empty registry, would render an untextured
// world with no error on the one platform that hit this path.
#[cfg(not(target_arch = "wasm32"))]
pub use blocks_json::blocks_json_registry;
pub use camera::{Camera, Frustum, Intersection, Plane};
pub use caps::{Backend, GpuCapabilities};
pub use device::{GpuContext, GpuError};
pub use driver::{InstanceTable, WorldMesher};
pub use entity::{
    ENTITY_FULLBRIGHT, ArmourMesh, ArmourModelSet, EntityBatch, EntityCullStats, EntityFrame,
    EntityInstance, EntityMesh, EntityModelSet, EntitySpawn, MODEL_FEET_OFFSET, PartRange,
    SheepWoolModelSet, WoolMesh, armour_layer_tint, armour_layers, entity_model_matrix,
    entity_texture_candidates, mob_draws_bow_when_aggressive, model_for_type, plan_entities,
};
pub use entity_anim::{AnimFamily, AnimInput, ArmPose, Skeleton};
pub use entity_pipeline::{
    EntityCameraUniform, EntityInstanceRaw, EntityPipeline, GpuEntityModel,
    HURT_OVERLAY_ALPHA_BYTE, InstanceTint, NO_TINT, entity_camera_buffer, upload_instances,
    upload_instances_tinted,
};
#[cfg(not(target_arch = "wasm32"))]
pub use frame::SystemClock;
pub use frame::{FrameOutcome, FramePacer, FrameTiming, Renderer, TimeSource};
pub use gui_atlas::{GuiAtlas, GuiAtlasError, GuiSpriteQuad};
pub use item_render::{
    CROSSBOW_CHARGE_TICKS, GUI_DEPTH_HALF_RANGE, ItemStateContext, SCALE_LIMIT, TRANSLATION_LIMIT,
    UNITS_PER_BLOCK, display_matrix, gui_item_pose, gui_ortho,
};
pub use light::{
    BRIGHTNESS_FACTOR, apply_brightness_option, brightness, light_term, light_term_from_levels,
    not_gamma,
};
#[doc(no_inline)]
pub use lodestone_assets::fluid::FluidState;
pub use mesh::{Mesh, MeshStats, face_winding_is_outward, mesh_greedy, mesh_simple};
pub use mesher::{
    BuiltSection, LightGrid, MeshJob, SectionSnapshot, SectionSource, build_batch, column_of,
    dirty_jobs, neighbour_columns, neighbourhood_coords,
};
pub use model_pipeline::{
    GpuModelMesh, ModelCameraUniform, ModelPipeline, ModelSharedCameraUniform,
    SectionOriginUniform, model_anim_buffer, model_camera_buffer, model_camera_buffer_with_fog,
    model_palette_buffer, model_shared_camera_buffer, model_shared_camera_buffer_with_fog,
    section_origin_buffer, update_model_anim_buffer, update_model_shared_camera_buffer,
    write_section_origin,
};
pub use models::{
    FluidMeshes, FluidSectionView, GUI_ITEM_LIGHT, ModelMesh, ModelSectionView, ModelVertex,
    face_of_direction, is_full_cube, is_packed_cube, mesh_fluids, mesh_item_quads, mesh_models,
};
pub use scene::{CullStats, FramePlan, WorldScene, section_of};
pub use screen_effects::{
    FIRE_STRIP_TOP, FIRE_TILE_COUNT, FIRE_TINT, ScreenEffectRenderer, ScreenOverlayVertex,
    UNDERWATER_TILE_COUNT, UNDERWATER_TINT_ALPHA, fire_overlay_triangles, underwater_brightness,
    underwater_overlay_quad, underwater_overlay_triangles,
};
pub use section::{Cell, Face, SECTION_SIZE, SectionNeighborhood, SectionView, SpriteId, Surface};
pub use section_arena::{INDEX_SIZE, SectionArena, draw_region_for};
pub use sky::{
    CLOUD_CELL_BLOCKS, CLOUD_HEIGHT, CLOUD_SCROLL_BLOCKS_PER_TICK, DAY_PERIOD_TICKS, MOON_HEIGHT,
    MOON_SIZE, SKY_DISC_RADIUS, SKY_FOG_END_DISTANCE, STAR_COUNT, STAR_DISTANCE, STAR_FIELD_SEED,
    SUN_HEIGHT, SUN_SIZE, SUNRISE_FAN_BOW, SUNRISE_FAN_HEIGHT, SUNRISE_FAN_RADIUS,
    SUNRISE_FAN_VERTICES, SUNRISE_MIN_ALPHA, SUNRISE_STEPS, build_star_field,
    celestial_angle_for_time_of_day, celestial_quad_positions, celestial_quad_uvs,
    celestial_rotation_matrix, cloud_color_for_time_of_day, cloud_color_multiplier_for_time_of_day,
    cloud_plane_geometry, fog_color_for_time_of_day, fog_color_multiplier_for_time_of_day,
    moon_phase_index_for_time_of_day, quad_indices, sky_color_for_time_of_day,
    sky_color_multiplier_for_time_of_day, sky_disc_indices, sky_disc_positions,
    sky_fog_end_for_render_distance, sky_fog_end_for_render_distance_blocks,
    star_brightness_for_time_of_day, sunrise_fan_indices, sunrise_fan_positions,
    sunrise_fan_transform, sunrise_fan_vertex_alphas, sunrise_sunset_color_for_time_of_day,
};
pub use sky_pipeline::{
    CelestialPipeline, CelestialVertex, CloudPipeline, CloudVertex, SkyDiscPipeline,
    SkyDiscVertex, SkyFrame, SkyRenderer, SkyVertex, StarPipeline, StarVertex, SunrisePipeline,
    SunriseVertex,
};
pub use strategy::{
    DrawRegion, DrawStrategy, MdiCount, MdiZeroInstance, PerDraw, StrategyError, StrategyKind,
    Submission, build_strategy, select_strategy,
};
pub use suballoc::{AllocStats, Region, SuballocError, Suballocator};
pub use target::{AcquiredFrame, HeadlessTarget, RenderTarget, SurfaceTarget, TargetError};
pub use texture::{
    AtlasBindingModel, AtlasStats, GUARANTEED_MAX_ARRAY_LAYERS_WEBGPU, GpuAtlas,
    MEASURED_MAX_ARRAY_LAYERS, MipLevel, SpriteRect, TextureLayout, atlas_mip_levels,
    generate_isolated_mips, recommend_layout, select_binding_model,
};
pub use translucency::{RenderLayer, SortViewpoint, TranslucentMesh};
pub use vertex::{BYTES_PER_VERTEX, PackedVertex, VertexFields};
pub use visibility::{
    SectionCoord, SectionVisibility, VisibilityGraph, compute_visibility, walk_visible,
};
pub use weather::{
    DEFAULT_WEATHER_RADIUS, FullBrightRainProbe, LIGHTNING_FLASH_TICKS, Precipitation,
    RainAmbience, RainSound, WeatherColumn, WeatherInstance, WeatherProbe, WeatherState,
    column_instance, column_offset_table, extract_columns, lightning_flash_linear,
    lightning_flash_srgb, precipitation_for_temperature, rain_count, weather_darken_linear,
    weather_darken_srgb, weather_sky_light_factor,
};
pub use weather_pipeline::{
    WeatherAssetError, WeatherRenderer, WeatherTextures, load_weather_textures,
};
pub use world::{
    BlockClassifier, ChunkSectionView, SectionLight, SkyDefault, UniformLight, WorldSectionLight,
};
