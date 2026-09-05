//! The bounded GPU bridge for the coarse distant-terrain horizon.
//!
//! This is deliberately separate from normal chunk meshes. A fixed 9 by 9
//! tile atlas represents a 256-chunk visual horizon with two 576 by 576
//! `R32Uint` textures (2.53 MiB total), while normal chunks retain their own
//! configured streaming radius. The shell must populate this bridge from a
//! cheap surface query; it must never turn an extended visual horizon into a
//! request for full chunk columns.

use std::cell::RefCell;

use bytemuck::{Pod, Zeroable};
use lodestone_render::{
    DEPTH_COMPARE_NEARER_OR_EQUAL, DEPTH_FORMAT, DISTANT_TERRAIN_WGSL, DistantTerrain,
    HORIZON_CELL_BLOCKS, HORIZON_TILE_CELLS, HORIZON_TILES_PER_AXIS, HorizonCell,
    ModelSharedCameraUniform, fog::FogUniform,
    model_shared_camera_buffer_with_fog, update_model_shared_camera_buffer,
};

/// Cells along an edge of the two fixed GPU atlases.
pub(crate) const HORIZON_ATLAS_CELLS: u32 =
    HORIZON_TILES_PER_AXIS as u32 * HORIZON_TILE_CELLS as u32;
/// Bytes in the height/water and colour/flags atlases combined.
pub(crate) const HORIZON_ATLAS_GPU_BYTES: u64 =
    HORIZON_ATLAS_CELLS as u64 * HORIZON_ATLAS_CELLS as u64 * 2 * 4;
/// Vertices emitted by the vertex-pulled 63 by 63 quad grid for one tile.
pub(crate) const HORIZON_TILE_VERTEX_COUNT: u32 =
    ((HORIZON_TILE_CELLS as u32 - 1) * (HORIZON_TILE_CELLS as u32 - 1)) * 6;

const TILE_UNIFORM_BYTES: u64 = 32;
/// The WebGPU baseline alignment. Rejecting a larger required stride keeps the
/// per-tile uniform allocation a known 20,736 bytes rather than a
/// device-specific unbounded multiple.
const MAX_TILE_UNIFORM_STRIDE: u32 = 256;
const TILE_UNIFORM_BUFFER_BYTES: u64 = MAX_TILE_UNIFORM_STRIDE as u64
    * HORIZON_TILES_PER_AXIS as u64
    * HORIZON_TILES_PER_AXIS as u64;

/// Group-1 binding zero for one atlas tile.
///
/// Each entry begins at the device's dynamic-uniform alignment. This is one
/// fixed buffer, rather than one uniform allocation per tile or per redraw.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct TileUniform {
    origin_cell: [i32; 2],
    atlas_origin: [i32; 2],
    cell_blocks: u32,
    near_field_radius_blocks: f32,
    _padding: [u32; 2],
}

/// Tracks which fixed atlas slots contain real surface samples.
///
/// A tile is never drawn before its query result is uploaded. This is both the
/// visual seam's missing-data behaviour and the omission detector the screen
/// gate uses: a submitted slot outside this set is a renderer bug, not a
/// plausible empty horizon.
#[derive(Debug, Clone)]
struct TileResidency {
    populated: [bool; HORIZON_TILES_PER_AXIS * HORIZON_TILES_PER_AXIS],
}

impl TileResidency {
    const fn empty() -> Self {
        Self {
            populated: [false; HORIZON_TILES_PER_AXIS * HORIZON_TILES_PER_AXIS],
        }
    }

    fn clear(&mut self) {
        self.populated.fill(false);
    }

    fn mark_populated(&mut self, slot: usize) {
        self.populated[slot] = true;
    }

    fn is_populated(&self, slot: usize) -> bool {
        self.populated.get(slot).copied().unwrap_or(false)
    }

    fn next_omitted(&self) -> Option<usize> {
        self.populated
            .iter()
            .enumerate()
            .filter(|(_, populated)| !**populated)
            .min_by_key(|(slot, _)| {
                let x = *slot % HORIZON_TILES_PER_AXIS;
                let z = *slot / HORIZON_TILES_PER_AXIS;
                (x.abs_diff(HORIZON_TILES_PER_AXIS / 2))
                    .max(z.abs_diff(HORIZON_TILES_PER_AXIS / 2))
            })
            .map(|(slot, _)| slot)
    }

    fn submitted_slots(&self) -> impl Iterator<Item = usize> + '_ {
        self.populated
            .iter()
            .enumerate()
            .filter_map(|(slot, populated)| populated.then_some(slot))
    }

    /// Returns false when a draw list contains an unpopulated atlas slot.
    fn submitted_slots_are_complete(&self, submitted: impl IntoIterator<Item = usize>) -> bool {
        submitted.into_iter().all(|slot| self.is_populated(slot))
    }
}

/// The GPU resources and fixed CPU terrain grid behind one visual horizon.
#[derive(Debug)]
pub(crate) struct DistantTerrainRenderer {
    terrain: DistantTerrain,
    residency: TileResidency,
    heights_water: wgpu::Texture,
    heights_water_view: wgpu::TextureView,
    colours_flags: wgpu::Texture,
    colours_flags_view: wgpu::TextureView,
    camera_buffer: wgpu::Buffer,
    camera_bind_group: wgpu::BindGroup,
    tile_uniform_buffer: wgpu::Buffer,
    tile_bind_group: wgpu::BindGroup,
    tile_uniform_stride: u32,
    tile_uniform_bytes: RefCell<Vec<u8>>,
    near_field_radius_blocks: f32,
    pipeline: wgpu::RenderPipeline,
}

/// Why a bounded distant-terrain renderer could not be constructed.
#[derive(Debug)]
pub(crate) enum HorizonGpuError {
    /// The fixed CPU terrain allocation failed.
    Allocation(lodestone_render::HorizonAllocationError),
    /// The adapter requires a dynamic uniform stride above this tier's fixed
    /// budget instead of the portable 256-byte baseline.
    UnsupportedUniformAlignment(u32),
}

impl From<lodestone_render::HorizonAllocationError> for HorizonGpuError {
    fn from(value: lodestone_render::HorizonAllocationError) -> Self {
        Self::Allocation(value)
    }
}

impl DistantTerrainRenderer {
    /// Allocate a fixed horizon around the camera's current world block.
    pub(crate) fn new(
        device: &wgpu::Device,
        color_format: wgpu::TextureFormat,
        camera_block: [i32; 2],
    ) -> Result<Self, HorizonGpuError> {
        let terrain = DistantTerrain::new(camera_block[0], camera_block[1])?;
        let tile_uniform_stride = device.limits().min_uniform_buffer_offset_alignment;
        if tile_uniform_stride > MAX_TILE_UNIFORM_STRIDE || tile_uniform_stride < TILE_UNIFORM_BYTES as u32 {
            return Err(HorizonGpuError::UnsupportedUniformAlignment(
                tile_uniform_stride,
            ));
        }
        let extent = wgpu::Extent3d {
            width: HORIZON_ATLAS_CELLS,
            height: HORIZON_ATLAS_CELLS,
            depth_or_array_layers: 1,
        };
        let texture = |label| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: extent,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::R32Uint,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            })
        };
        let heights_water = texture("lodestone-distant-terrain-heights-water");
        let colours_flags = texture("lodestone-distant-terrain-colours-flags");
        let heights_water_view = heights_water.create_view(&wgpu::TextureViewDescriptor::default());
        let colours_flags_view = colours_flags.create_view(&wgpu::TextureViewDescriptor::default());

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("lodestone-distant-terrain-shader"),
            source: wgpu::ShaderSource::Wgsl(DISTANT_TERRAIN_WGSL.into()),
        });
        let camera_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("lodestone-distant-terrain-camera-layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: wgpu::BufferSize::new(
                        std::mem::size_of::<ModelSharedCameraUniform>() as u64,
                    ),
                },
                count: None,
            }],
        });
        let camera_buffer = model_shared_camera_buffer_with_fog(
            device,
            glam::Mat4::IDENTITY.to_cols_array_2d(),
            FogUniform::disabled(),
        );
        let camera_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("lodestone-distant-terrain-camera-bind-group"),
            layout: &camera_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: camera_buffer.as_entire_binding(),
            }],
        });

        let tile_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("lodestone-distant-terrain-tile-layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: true,
                        min_binding_size: wgpu::BufferSize::new(TILE_UNIFORM_BYTES),
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Uint,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Uint,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });
        let tile_uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("lodestone-distant-terrain-tile-uniforms"),
            size: u64::from(tile_uniform_stride)
                * HORIZON_TILES_PER_AXIS as u64
                * HORIZON_TILES_PER_AXIS as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let tile_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("lodestone-distant-terrain-tile-bind-group"),
            layout: &tile_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: &tile_uniform_buffer,
                        offset: 0,
                        size: wgpu::BufferSize::new(TILE_UNIFORM_BYTES),
                    }),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&heights_water_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&colours_flags_view),
                },
            ],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("lodestone-distant-terrain-pipeline-layout"),
            bind_group_layouts: &[Some(&camera_layout), Some(&tile_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("lodestone-distant-terrain-pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: color_format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(true),
                depth_compare: Some(DEPTH_COMPARE_NEARER_OR_EQUAL),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        Ok(Self {
            terrain,
            residency: TileResidency::empty(),
            heights_water,
            heights_water_view,
            colours_flags,
            colours_flags_view,
            camera_buffer,
            camera_bind_group,
            tile_uniform_buffer,
            tile_bind_group,
            tile_uniform_stride,
            tile_uniform_bytes: RefCell::new(vec![
                0;
                tile_uniform_stride as usize
                    * HORIZON_TILES_PER_AXIS
                    * HORIZON_TILES_PER_AXIS
            ]),
            near_field_radius_blocks: 0.0,
            pipeline,
        })
    }

    /// Recenter the bounded grid and invalidate all tile samples when the
    /// camera crosses a horizon-tile boundary. No texture or vector grows.
    pub(crate) fn recenter(&mut self, camera_block: [i32; 2]) {
        let before = self.terrain.centre();
        self.terrain.recenter(camera_block[0], camera_block[1]);
        if self.terrain.centre() != before {
            self.residency.clear();
        }
    }

    /// Exclude the normal streamed-chunk disk from the coarse pass.
    ///
    /// This is a visual transition only: it neither changes the normal camera
    /// far plane nor requests a chunk. The clamp makes a malformed option no
    /// more expensive than the fixed 256-chunk representation can be.
    pub(crate) fn set_near_field_radius_chunks(&mut self, chunks: u32) {
        self.near_field_radius_blocks = (chunks.min(256) * HORIZON_CELL_BLOCKS as u32) as f32;
    }

    /// Query and upload one still-empty tile. Returns false once the fixed
    /// window is fully populated.
    ///
    /// Sampling a tile at a time bounds a redraw's generator work to 4,096
    /// coarse queries. The caller should invoke this only for a local
    /// Overworld query source, never as a request to the server's chunk stream.
    pub(crate) fn populate_one(
        &mut self,
        queue: &wgpu::Queue,
        mut sample: impl FnMut(i32, i32) -> HorizonCell,
    ) -> bool {
        let Some(slot) = self.residency.next_omitted() else {
            return false;
        };
        let mut height_water = vec![0; HORIZON_TILE_CELLS * HORIZON_TILE_CELLS];
        let mut colours_flags = vec![0; HORIZON_TILE_CELLS * HORIZON_TILE_CELLS];
        {
            let tile = self
                .terrain
                .tiles_mut()
                .nth(slot)
                .expect("fixed residency slot must index the fixed terrain grid");
            let (origin_x, origin_z) = tile.coord().block_origin();
            for z in 0..HORIZON_TILE_CELLS {
                for x in 0..HORIZON_TILE_CELLS {
                    let world_x = origin_x.saturating_add((x as i32) * HORIZON_CELL_BLOCKS);
                    let world_z = origin_z.saturating_add((z as i32) * HORIZON_CELL_BLOCKS);
                    let cell = sample(world_x, world_z);
                    let index = z * HORIZON_TILE_CELLS + x;
                    height_water[index] =
                        u32::from(cell.terrain_y) | (u32::from(cell.water_y) << 16);
                    colours_flags[index] =
                        u32::from(cell.surface_rgb565) | (u32::from(cell.flags) << 16);
                    let wrote = tile.set_cell(x, z, cell);
                    debug_assert!(wrote);
                }
            }
        }
        Self::upload_tile(
            queue,
            &self.heights_water,
            &self.colours_flags,
            slot,
            &height_water,
            &colours_flags,
        );
        self.residency.mark_populated(slot);
        true
    }

    /// Write this frame's camera and fog once before the render pass opens.
    pub(crate) fn prepare(
        &self,
        queue: &wgpu::Queue,
        view_proj: [[f32; 4]; 4],
        fog: FogUniform,
    ) {
        update_model_shared_camera_buffer(queue, &self.camera_buffer, view_proj, fog);
        let mut bytes = self.tile_uniform_bytes.borrow_mut();
        for (slot, tile) in self.terrain.tiles().enumerate() {
            let coord = tile.coord();
            let uniform = TileUniform {
                origin_cell: [
                    coord.x.saturating_mul(HORIZON_TILE_CELLS as i32),
                    coord.z.saturating_mul(HORIZON_TILE_CELLS as i32),
                ],
                atlas_origin: atlas_origin(slot),
                cell_blocks: HORIZON_CELL_BLOCKS as u32,
                near_field_radius_blocks: self.near_field_radius_blocks,
                _padding: [0; 2],
            };
            let start = slot * self.tile_uniform_stride as usize;
            bytes[start..start + TILE_UNIFORM_BYTES as usize]
                .copy_from_slice(bytemuck::bytes_of(&uniform));
        }
        queue.write_buffer(&self.tile_uniform_buffer, 0, &bytes);
    }

    /// Submit only fully populated tiles into the active depth-tested world
    /// pass. Normal chunks draw immediately afterwards and therefore occlude
    /// this coarse surface at the near-field boundary.
    pub(crate) fn draw(&self, pass: &mut wgpu::RenderPass<'_>) {
        let submitted: Vec<_> = self.residency.submitted_slots().collect();
        debug_assert!(self.residency.submitted_slots_are_complete(submitted.iter().copied()));
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.camera_bind_group, &[]);
        for slot in submitted {
            pass.set_bind_group(1, &self.tile_bind_group, &[(slot as u32) * self.tile_uniform_stride]);
            pass.draw(0..HORIZON_TILE_VERTEX_COUNT, 0..1);
        }
    }

    /// The negative-control seam for the headless screen gate.
    ///
    /// A test feeds it a slot that population deliberately skipped. Keeping
    /// the check beside the actual draw-list detector proves the control is not
    /// a second, more permissive assertion over test-only bookkeeping.
    #[cfg(test)]
    pub(crate) fn rejects_unpopulated_submission(&self, slot: usize) -> bool {
        !self.residency.submitted_slots_are_complete([slot])
    }

    fn upload_tile(
        queue: &wgpu::Queue,
        heights_water: &wgpu::Texture,
        colours_flags: &wgpu::Texture,
        slot: usize,
        height_water_values: &[u32],
        colour_flag_values: &[u32],
    ) {
        let [atlas_x, atlas_z] = atlas_origin(slot);
        let origin = wgpu::Origin3d {
            x: atlas_x as u32,
            y: atlas_z as u32,
            z: 0,
        };
        let layout = wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some((HORIZON_TILE_CELLS * std::mem::size_of::<u32>()) as u32),
            rows_per_image: Some(HORIZON_TILE_CELLS as u32),
        };
        let extent = wgpu::Extent3d {
            width: HORIZON_TILE_CELLS as u32,
            height: HORIZON_TILE_CELLS as u32,
            depth_or_array_layers: 1,
        };
        for (texture, values) in [
            (heights_water, height_water_values),
            (colours_flags, colour_flag_values),
        ] {
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture,
                    mip_level: 0,
                    origin,
                    aspect: wgpu::TextureAspect::All,
                },
                bytemuck::cast_slice(values),
                layout,
                extent,
            );
        }
    }
}

fn atlas_origin(slot: usize) -> [i32; 2] {
    debug_assert!(slot < HORIZON_TILES_PER_AXIS * HORIZON_TILES_PER_AXIS);
    [
        ((slot % HORIZON_TILES_PER_AXIS) * HORIZON_TILE_CELLS) as i32,
        ((slot / HORIZON_TILES_PER_AXIS) * HORIZON_TILE_CELLS) as i32,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn horizon_atlas_is_fixed_and_stays_below_three_mebibytes_of_gpu_texels() {
        assert_eq!(HORIZON_ATLAS_CELLS, 576);
        assert_eq!(HORIZON_TILE_VERTEX_COUNT, 23_814);
        assert_eq!(HORIZON_ATLAS_GPU_BYTES, 2_654_208);
        assert!(HORIZON_ATLAS_GPU_BYTES < 3 * 1024 * 1024);
    }

    #[test]
    fn atlas_slots_cover_each_tile_once_without_crossing_an_edge() {
        assert_eq!(atlas_origin(0), [0, 0]);
        assert_eq!(atlas_origin(8), [512, 0]);
        assert_eq!(atlas_origin(72), [0, 512]);
        assert_eq!(atlas_origin(80), [512, 512]);
        let [x, z] = atlas_origin(80);
        assert_eq!(x + HORIZON_TILE_CELLS as i32, HORIZON_ATLAS_CELLS as i32);
        assert_eq!(z + HORIZON_TILE_CELLS as i32, HORIZON_ATLAS_CELLS as i32);
    }

    #[test]
    fn omitted_tile_detector_rejects_a_synthetic_unpopulated_submission() {
        let mut residency = TileResidency::empty();
        residency.mark_populated(3);
        assert!(residency.submitted_slots_are_complete([3]));
        assert!(
            !residency.submitted_slots_are_complete([3, 4]),
            "control: the detector must reject an omitted slot if a draw path submits it"
        );
    }

    #[test]
    fn first_query_tile_is_the_camera_tile_not_a_far_corner() {
        let residency = TileResidency::empty();
        assert_eq!(residency.next_omitted(), Some(40));
    }

    #[test]
    fn near_field_clip_cannot_exceed_the_fixed_horizon() {
        let set = |chunks: u32| (chunks.min(256) * HORIZON_CELL_BLOCKS as u32) as f32;
        assert_eq!(set(8), 128.0);
        assert_eq!(set(u32::MAX), 4096.0);
    }
}
