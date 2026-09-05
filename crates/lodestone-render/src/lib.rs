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
pub mod banner_pattern;
pub mod beacon;
pub mod biome_tint;
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
pub mod cull;
pub mod device;
pub mod display;
pub mod distant_terrain;
pub mod driver;
pub mod end_portal;
pub mod entity;
pub mod entity_anim;
pub mod entity_pipeline;
/// Camera-facing sprite billboards for the two entity types whose vanilla
/// renderer builds a quad vertex by vertex, plus the fishing line one of them
/// hangs off and the ominous item spawner's spin/scale.
pub mod entity_sprite;
pub mod fluid_grid;
pub mod fog;
pub mod frame;
pub mod glint;
pub mod gui_atlas;
pub mod gui_entity;
pub mod item_render;
pub mod light;
pub mod lightning_bolt;
pub mod map_item;
pub mod mesh;
pub mod mesher;
pub mod model_arena;
pub mod model_pipeline;
pub mod models;
pub mod painting;
pub mod scene;
pub mod screen_effects;
pub mod section;
pub mod section_arena;
pub mod sign;
pub mod sky;
pub mod sky_pipeline;
pub mod spawner;
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
pub use banner_pattern::{
    DyeColor, MAX_PATTERN_LAYERS, PatternLayer, StoredPatternLayer, banner_pattern_layers,
    gamma_rgb_to_bytes, shield_pattern_layers,
};
pub use beacon::{
    BEAM_GLOW_RADIUS, BEAM_SCALE_THRESHOLD, END_GATEWAY_BEAM_GLOW_RADIUS,
    END_GATEWAY_SOLID_BEAM_RADIUS, BeaconSpawn, BeamSection, BeamVertex, EndGatewayBeamSpawn,
    MAX_RENDER_Y, SOLID_BEAM_RADIUS, average_beam_color, beacon_beam_color, beacon_beam_vertices,
    beam_radius_scale, end_gateway_beam_vertices,
};
pub use block::{
    BlockPipeline, CameraUniform, DEPTH_CLEAR, DEPTH_COMPARE_NEARER,
    DEPTH_COMPARE_NEARER_OR_EQUAL, DEPTH_FORMAT, DepthBuffer, GpuMesh,
};
pub use end_portal::{
    EndGatewaySpawn, EndPortalSpawn, EndPortalVertex, end_gateway_vertices, end_portal_vertices,
};
pub use block_entity::{
    BANNER_BASE_TEXTURE_STEM, BANNER_BODY, BANNER_FLAG, BANNER_WALL_BODY, BANNER_WALL_FLAG, BELL,
    BELL_TEXTURE_STEM, BOOK, BOOK_TEXTURE_STEM, BannerAttachment, BannerInstances,
    BannerLayerDraw, BannerSpawn, BellShakeDirection,
    BellSpawn, BlockEntityBatch, BlockEntityCullStats, BlockEntityFrame, BlockEntityInstance,
    BlockEntityTexture,
    BlockEntityMesh, BlockEntityModelSet, CAMPFIRE_ITEM_LIFT, CAMPFIRE_ITEM_SCALE, CAMPFIRE_SLOTS,
    CHEST_LEFT, CHEST_MATERIALS, CHEST_RIGHT,
    CHEST_SINGLE, CONDUIT_CAGE, CONDUIT_CAGE_TEXTURE_STEM, CONDUIT_CLOSED_EYE_TEXTURE_STEM,
    CONDUIT_EYE, CONDUIT_FRAME_CANDIDATE_COUNT, CONDUIT_OPEN_EYE_TEXTURE_STEM, CONDUIT_SHELL,
    CONDUIT_SHELL_TEXTURE_STEM, CONDUIT_WIND, CONDUIT_WIND_TEXTURE_STEM,
    CONDUIT_WIND_VERTICAL_TEXTURE_STEM, COPPER_GOLEM_POSES, BrushableItemSpawn, CampfireItemSpawn,
    ChestHalf, ChestMaterial, ChestSpawn, ConduitFrame, ConduitSpawn, CopperGolemOxidation,
    CopperGolemPose, CopperGolemStatueSpawn,
    DECORATED_POT_BASE, DECORATED_POT_BASE_TEXTURE_STEM, DECORATED_POT_SIDE_BACK,
    DECORATED_POT_SIDE_DEFAULT_TEXTURE_STEM, DECORATED_POT_SIDE_FRONT, DECORATED_POT_SIDE_LEFT,
    DECORATED_POT_SIDE_RIGHT, DecoratedPotSpawn,
    ENCHANTING_TABLE_BOOK_TILT_DEG, EnchantingTableSpawn, LECTERN_BOOK_OPENNESS,
    LECTERN_BOOK_PAGE_FLIP, LecternSpawn, MovingPistonSpawn, SHELF_SLOTS, ShelfItemSpawn, VaultSpawn, SHIELD, SHIELD_BASE_TEXTURE_STEM,
    SHIELD_BASE_NO_PATTERN_TEXTURE_STEM, SHULKER_BOX, SHULKER_COLOURS,
    SHULKER_DEFAULT_TEXTURE_STEM, SKULL_DRAGON, SKULL_HUMANOID, SKULL_MOB, SKULL_PIGLIN,
    SKULL_RESTING_ANIMATION_POS, SKULL_TYPES, DRAGON_HEAD_JAW_PART, PIGLIN_HEAD_EAR_PARTS,
    dragon_head_jaw_x_rot, piglin_head_ear_z_rots, ShulkerFacing,
    BannerItemRig, ShulkerSpawn, SkullOrientation, SkullSpawn, SkullType, banner_flag_x_rot,
    banner_ground_placement_matrix, banner_item_base_color, banner_item_rig, banner_phase,
    banner_texture_stems,
    shield_has_patterns, shield_item_rig, shield_texture_stems,
    banner_wall_placement_matrix, bell_shake_angle,
    bell_texture_stems, block_entity_placement_matrix, block_entity_texture_stems,
    book_part_poses, book_texture_stems, brushable_item_matrix, campfire_item_matrix, chest_lid_openness, chest_lid_x_rot,
    chest_material_with_season, chest_texture_stem, chest_texture_stems,
    conduit_active_axis_rotation_radians, conduit_active_rotation_value, conduit_advance,
    conduit_anim_time, conduit_animation_phase, conduit_bob, conduit_frame_scan,
    conduit_inactive_y_rot_radians, conduit_texture_stems,
    copper_golem_statue_oxidation_from_item_path, copper_golem_statue_placement_matrix,
    copper_golem_statue_texture_stem, copper_golem_statue_texture_stems,
    DecoratedPotItemRig, decorated_pot_item_rig, decorated_pot_pattern_texture_stem,
    decorated_pot_placement_matrix, decorated_pot_texture_stems,
    enchanting_table_book_hover, enchanting_table_book_openness,
    enchanting_table_book_placement_matrix, enchanting_table_page_flips,
    horizontal_facing_clockwise_yaw, horizontal_facing_yaw, lectern_book_placement_matrix,
    plan_block_entities, shelf_item_offset, shelf_slot_matrix, shulker_lid_pose,
    shulker_placement_matrix, special_item_rig,
    shulker_texture_stem,
    shulker_texture_stems, skull_ground_placement_matrix, skull_texture_stem,
    skull_texture_stems, skull_wall_placement_matrix, trident_item_rig, TRIDENT_ENTITY_MODEL,
};
pub use block_models::{
    BlockModels, BlockModelsError, CRACK_STAGE_COUNT, DRY_FOLIAGE_TINT_SLOT, FOLIAGE_TINT_SLOT,
    FluidCell, FluidKind, FluidSprites, GRASS_TINT_SLOT, ItemGeometry, ItemVariants,
    SpecialItemForm, StateModel, WATER_TINT_SLOT, biome_tint_kind_for_slot, biome_tint_slot,
    stamp_live_item_tint,
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
pub use camera::{
    Camera, Frustum, Intersection, Plane, nausea_portal_warp, spinning_effect_angle_degrees,
    spyglass_fov_modifier,
};
pub use caps::{Backend, GpuCapabilities};
pub use cull::{
    CAMERA_CUBE_BLOCKS, CullVerdict, OcclusionMode, TerrainCull, reachable_from_camera,
    section_coord_of, within_view_distance,
};
pub use device::{GpuContext, GpuError};
pub use distant_terrain::{
    DISTANT_TERRAIN_WGSL, DistantTerrain, HORIZON_CELL_BLOCKS, HORIZON_CELLS_PER_TILE,
    HORIZON_TILE_BLOCKS, HORIZON_TILE_CELLS, HORIZON_TILE_RADIUS, HORIZON_TILES_PER_AXIS,
    HorizonAllocationError, HorizonCell, HorizonTile, HorizonTileCoord,
    horizon_tile_intersects_radius, MAX_HORIZON_BYTES,
    MAX_HORIZON_CELLS, MAX_HORIZON_DISTANCE_CHUNKS, MAX_HORIZON_TILES,
};
pub use driver::{InstanceTable, WorldMesher};
pub use entity::{
    ENTITY_FULLBRIGHT, EXPERIENCE_ORB_ICON_COUNT, EXPERIENCE_ORB_TEXTURE, SHADOW_TEXTURE, ArmourMesh,
    ArmourModelSet, CapeMesh, ELYTRA_ROTATION_LERP, ElytraMesh, ElytraWing, EntityBatch,
    EntityCullStats, EntityFrame, EntityInstance,
    EntityMesh, EntityModelSet, EntitySpawn, MODEL_FEET_OFFSET, PartRange, SheepWoolModelSet,
    WoolMesh, armour_layer_tint, armour_layers, cape_local_rotation, dying_entity_model_matrix,
    elytra_rest_rotations, elytra_target_rotations, elytra_wing_transform, elytra_wing_y,
    entity_model_matrix,
    entity_texture_candidates, entity_variant_sheet, entity_variant_sheet_dirs,
    entity_variant_sheet_for,
    experience_orb_icon, experience_orb_light, experience_orb_matrix, experience_orb_mesh,
    experience_orb_tint, framed_item_matrix, mob_draws_bow_when_aggressive, model_for_type,
    non_living_vehicle_matrix, non_living_vehicle_placement, plan_entities, renderer_is_avatar,
    sheet_reference_of, special_item_hover_lift,
};
pub use entity_anim::{AnimFamily, AnimInput, ArmPose, Skeleton};
pub use entity_pipeline::{
    EntityCameraUniform, EntityInstanceRaw, EntityPipeline, GpuEntityModel,
    HURT_OVERLAY_ALPHA_BYTE, InstanceBufferArena, InstanceTint, NO_TINT, entity_camera_buffer,
    stage_instances_tinted, upload_instances, upload_instances_tinted,
};
#[cfg(not(target_arch = "wasm32"))]
pub use frame::SystemClock;
pub use frame::{FrameOutcome, FramePacer, FrameTiming, Renderer, TimeSource};
pub use gui_atlas::{GuiAtlas, GuiAtlasError, GuiSpriteQuad};
pub use gui_entity::{
    GuiEntityLook, INVENTORY_OFFSET_Y, INVENTORY_RECT_OFFSET, INVENTORY_RECT_SIZE, INVENTORY_SIZE,
    gui_entity_anim, gui_entity_look, gui_entity_pose, gui_entity_view,
};
pub use item_render::{
    CROSSBOW_CHARGE_TICKS, GUI_DEPTH_HALF_RANGE, ItemStateContext, SCALE_LIMIT, TRANSLATION_LIMIT,
    UNITS_PER_BLOCK, compose_special_item_transform, compose_special_node_transform, display_matrix,
    gui_item_pose, gui_ortho, node_transform_matrix,
};
pub use light::{
    BLOCK_FACTOR, BLOCK_LIGHT_TINT, BRIGHTNESS_FACTOR, apply_brightness_option, brightness,
    light_color, light_color_from_levels, light_term, light_term_from_levels, not_gamma,
    not_gamma_vec3, sky_light_color_from_darken,
};
#[doc(no_inline)]
pub use lodestone_assets::fluid::FluidState;
pub use mesh::{Mesh, MeshStats, face_winding_is_outward, mesh_greedy, mesh_simple};
pub use mesher::{
    BuiltSection, LightGrid, MeshJob, SectionSnapshot, SectionSource, build_batch, dirty_jobs,
    neighbour_columns, neighbourhood_coords,
};
pub use map_item::{
    MAP_BRIGHTNESS, MAP_COLOR_BASE, MAP_SIZE as MAP_TEXTURE_SIZE, PackedMapColour, map_color_rgba,
    map_quad_mesh, map_texture_rgba,
};
pub use model_arena::{ArenaMesh, ModelMeshArena};
pub use model_pipeline::{
    CAMERA_DEPTH_BIAS, GpuModelMesh, ModelCameraUniform, ModelPipeline, ModelSharedCameraUniform,
    SECTION_FADE_ALREADY_VISIBLE, SECTION_FADE_DURATION_SECS, SectionOriginUniform,
    model_anim_buffer, model_camera_buffer, model_camera_buffer_with_fog, model_palette_buffer,
    model_shared_camera_buffer, model_shared_camera_buffer_with_fog, section_origin_buffer,
    section_is_nearby, section_visibility, update_model_anim_buffer,
    update_model_shared_camera_buffer, write_section_origin,
};
pub use fluid_grid::{FluidGrid, FluidNeighborCell, PackedCell};
pub use models::{
    FluidMeshes, FluidSectionView, GUI_ITEM_LIGHT, ModelMesh, ModelSectionView, ModelVertex,
    face_of_direction, is_full_cube, is_packed_cube, mesh_fluids, mesh_item_quads, mesh_models,
    mesh_models_layers, mesh_moving_block_quads,
};
pub use scene::{CullStats, FramePlan, WorldScene, section_of};
pub use screen_effects::{
    FIRE_STRIP_TOP, FIRE_TILE_COUNT, FIRE_TINT, ScreenEffectRenderer, ScreenOverlayVertex,
    UNDERWATER_TILE_COUNT, UNDERWATER_TINT_ALPHA, border_warning_overlay_triangles,
    fire_overlay_triangles, underwater_brightness, underwater_overlay_quad,
    underwater_overlay_triangles,
};
pub use section::{Cell, Face, SECTION_SIZE, SectionNeighborhood, SectionView, SpriteId, Surface};
pub use section_arena::{INDEX_SIZE, SectionArena, draw_region_for};
pub use sign::{
    BLACK_TEXT_OUTLINE_RGB, HANGING_TEXT_LINE_HEIGHT, OUTLINE_RENDER_DISTANCE_SQUARED, SignKind,
    SignOrientation, SignSpawn, TEXT_LINE_HEIGHT, dye_text_color_rgb, sign_dark_color_rgb,
    sign_outline_color, sign_side_color, sign_text_transform,
};
pub use spawner::{
    SpawnerMobSpawn, spawner_display_outer_matrix, spawner_display_scale, spawner_spin_degrees,
};
pub use sky::{
    CLOUD_CELL_BLOCKS, CLOUD_FANCY_RADIUS_CELLS, CLOUD_FANCY_THICKNESS, CLOUD_HEIGHT,
    CLOUD_SCROLL_BLOCKS_PER_TICK, CloudStatus, DAY_PERIOD_TICKS, MOON_HEIGHT, MOON_SIZE,
    SKY_DISC_RADIUS, SKY_FOG_END_DISTANCE, STAR_COUNT, STAR_DISTANCE, STAR_FIELD_SEED, SUN_HEIGHT,
    SkyMode,
    SUN_SIZE, SUNRISE_FAN_BOW, SUNRISE_FAN_HEIGHT, SUNRISE_FAN_RADIUS, SUNRISE_FAN_VERTICES,
    SUNRISE_MIN_ALPHA, SUNRISE_STEPS, build_star_field, celestial_angle_for_time_of_day,
    celestial_quad_positions, celestial_quad_uvs, celestial_rotation_matrix, cloud_cell_and_offset,
    cloud_color_for_time_of_day, cloud_color_multiplier_for_time_of_day, cloud_face_vertices,
    cloud_fancy_max_faces, cloud_plane_geometry, cloud_relative_pos_for_camera_y,
    fancy_cloud_geometry, fog_color_for_time_of_day, fog_color_multiplier_for_time_of_day,
    moon_phase_index_for_time_of_day, quad_indices, sky_color_for_time_of_day,
    sky_color_multiplier_for_time_of_day, sky_disc_indices, sky_disc_positions,
    sky_fog_end_for_render_distance, sky_fog_end_for_render_distance_blocks,
    star_brightness_for_time_of_day, sunrise_fan_indices, sunrise_fan_positions,
    sunrise_fan_transform, sunrise_fan_vertex_alphas, sunrise_sunset_color_for_time_of_day,
};
pub use sky_pipeline::{
    CelestialPipeline, CelestialVertex, CloudPipeline, CloudVertex, FancyCloudPipeline,
    SkyDiscPipeline, SkyDiscVertex, SkyFrame, SkyRenderer, SkyVertex, StarPipeline, StarVertex,
    SunrisePipeline, SunriseVertex,
};
pub use strategy::{
    DrawRegion, DrawStrategy, MdiCount, MdiZeroInstance, PerDraw, StrategyError, StrategyKind,
    Submission, build_strategy, select_strategy,
};
pub use suballoc::{AllocStats, Region, SuballocError, Suballocator};
pub use target::{AcquiredFrame, HeadlessTarget, RenderTarget, SurfaceTarget, TargetError};
pub use texture::{
    AtlasBindingModel, AtlasOccupancy, AtlasStats, GUARANTEED_MAX_ARRAY_LAYERS_WEBGPU, GpuAtlas,
    MEASURED_MAX_ARRAY_LAYERS, MipLevel, SpriteRect, TextureLayout, atlas_mip_levels,
    atlas_occupancy, generate_isolated_mips, recommend_layout, select_binding_model,
};
pub use translucency::{RenderLayer, SortViewpoint, TranslucentMesh};
pub use vertex::{BYTES_PER_VERTEX, PackedVertex, VertexFields};
pub use visibility::{
    SectionCoord, SectionVisibility, VisibilityGraph, compute_visibility, compute_visibility_from,
    walk_visible, walk_visible_bounded,
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
