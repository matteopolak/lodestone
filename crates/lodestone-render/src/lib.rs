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

pub mod anim;
pub mod arena;
pub mod block;
pub mod block_models;
pub mod block_resolver;
pub mod blocks_json;
pub mod camera;
pub mod caps;
pub mod crack;
pub mod crack_pipeline;
pub mod device;
pub mod driver;
pub mod entity;
pub mod entity_anim;
pub mod entity_pipeline;
pub mod frame;
pub mod gui_atlas;
pub mod mesh;
pub mod mesher;
pub mod model_pipeline;
pub mod models;
pub mod scene;
pub mod section;
pub mod section_arena;
pub mod strategy;
pub mod suballoc;
pub mod target;
pub mod texture;
pub mod translucency;
pub mod vertex;
pub mod visibility;
pub mod world;

#[cfg(feature = "window")]
pub mod window;

pub use anim::{AnimFrame, AnimSample, AnimSlotUniform, AnimUniform, SpriteAnimation};
pub use arena::{ArenaAllocation, ArenaBuffer, ArenaError};
pub use block::{BlockPipeline, CameraUniform, DEPTH_FORMAT, DepthBuffer, GpuMesh};
pub use block_models::{
    BlockModels, BlockModelsError, CRACK_STAGE_COUNT, FluidCell, FluidKind, FluidSprites,
    StateModel,
};
pub use block_resolver::{BlockAtlas, BlockAtlasError, MAX_SPRITES};
pub use blocks_json::{BlocksJsonError, BlocksJsonRegistry, blocks_json_registry};
pub use camera::{Camera, Frustum, Intersection, Plane};
pub use caps::{Backend, GpuCapabilities};
pub use device::{GpuContext, GpuError};
pub use driver::{InstanceTable, WorldMesher};
pub use entity::{
    EntityBatch, EntityCullStats, EntityFrame, EntityInstance, EntityMesh, EntityModelSet,
    EntitySpawn, MODEL_FEET_OFFSET, PartRange, entity_model_matrix, entity_texture_candidates,
    model_for_type, plan_entities,
};
pub use entity_anim::{AnimFamily, AnimInput, Skeleton};
pub use entity_pipeline::{EntityInstanceRaw, EntityPipeline, GpuEntityModel, upload_instances};
#[cfg(not(target_arch = "wasm32"))]
pub use frame::SystemClock;
pub use frame::{FrameOutcome, FramePacer, FrameTiming, Renderer, TimeSource};
pub use gui_atlas::{GuiAtlas, GuiAtlasError, GuiSpriteQuad};
#[doc(no_inline)]
pub use lodestone_assets::fluid::FluidState;
pub use mesh::{Mesh, MeshStats, face_winding_is_outward, mesh_greedy, mesh_simple};
pub use mesher::{
    BuiltSection, LightGrid, MeshJob, SectionSnapshot, SectionSource, build_batch, column_of,
    dirty_jobs, neighbour_columns, neighbourhood_coords,
};
pub use model_pipeline::{
    GpuModelMesh, ModelPipeline, model_anim_buffer, model_camera_buffer, model_palette_buffer,
    update_model_anim_buffer,
};
pub use models::{
    FluidMeshes, FluidSectionView, ModelMesh, ModelSectionView, ModelVertex, face_of_direction,
    is_full_cube, is_packed_cube, mesh_fluids, mesh_models,
};
pub use scene::{CullStats, FramePlan, WorldScene, section_of};
pub use section::{Cell, Face, SECTION_SIZE, SectionNeighborhood, SectionView, SpriteId, Surface};
pub use section_arena::{INDEX_SIZE, SectionArena, draw_region_for};
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
pub use world::{
    BlockClassifier, ChunkSectionView, SectionLight, SkyDefault, UniformLight, WorldSectionLight,
};
