//! Order-independent transparency for the translucent terrain layers.
//!
//! With [`RenderState::set_improved_transparency`] on and a device that
//! supports it, water and translucent blocks skip the sorted blended draws in
//! `frame.rs` and instead go through the stages of `lodestone_render::oit`,
//! each a pass of its own, resolved over the world colour. The world pass is
//! closed before them and resumed after, the way sign text splits it.
//!
//! Entities, particles, clouds and weather keep their ordinary blended draws.
use lodestone_render::oit::{OitPipelines, OitTargets, supported};

use super::terrain::{ModelRenderer, ModelSectionGpu, TerrainDraw};
use super::{RenderState, RenderStats};

/// The lazily built pipelines and the targets they draw into, rebuilt when the
/// frame size changes.
#[derive(Debug)]
pub(super) struct OitState {
    pipelines: OitPipelines,
    targets: OitTargets,
}

impl RenderState {
    /// Select the order-independent path for water and translucent blocks.
    /// Off by default; a device without the needed feature keeps the sorted
    /// path whatever this says.
    pub fn set_improved_transparency(&mut self, enabled: bool) {
        self.improved_transparency = enabled;
    }

    /// Turn per-section back-to-front re-sorting of translucent quads on the
    /// sorted path on or off. On by default; off leaves quads in mesh emission
    /// order, which is the A/B control for gates and frame-time measurements.
    pub fn set_translucent_sorting(&mut self, enabled: bool) {
        self.translucent_sorting = enabled;
    }

    /// Whether [`Self::set_translucent_sorting`] is on.
    #[must_use]
    pub fn translucent_sorting(&self) -> bool {
        self.translucent_sorting
    }

    /// Re-sort the visible sections' translucent quads and rewrite their index
    /// spans, following the reference rule: every nearby section is checked each
    /// frame, the rest round-robin at `max(len / 8, 15)` per frame, and a section
    /// re-sorts when its camera octant changed or the camera crossed a block
    /// while the section is nearby or axis-aligned. A section never sorted
    /// sorts at once so it is never drawn in mesh order. Returns the number of
    /// meshes rewritten.
    pub(super) fn resort_translucent(
        &self,
        queue: &wgpu::Queue,
        visible: &[(&super::terrain::ModelSectionGpu, f32)],
        camera: glam::Vec3,
    ) -> usize {
        use lodestone_render::{SortViewpoint, should_resort};
        /// Squared centre distance under which a section is nearby (32 blocks
        /// reach, matching the reference's nearby list).
        const NEARBY_DISTANCE_SQ: f32 = 32.0 * 32.0;
        let sortable: Vec<&(&super::terrain::ModelSectionGpu, f32)> = visible
            .iter()
            .filter(|(s, _)| s.translucent_sort.borrow().is_some())
            .collect();
        let block = [
            camera.x.floor() as i32,
            camera.y.floor() as i32,
            camera.z.floor() as i32,
        ];
        let block_changed = self.last_sort_camera_block.replace(Some(block)) != Some(block);
        let cam = [f64::from(camera.x), f64::from(camera.y), f64::from(camera.z)];
        let mut resorted = 0;
        let mut visit = |&(section, _): &(&super::terrain::ModelSectionGpu, f32), nearby: bool| {
            let o = section.origin;
            let coord = [o[0].div_euclid(16), o[1].div_euclid(16), o[2].div_euclid(16)];
            let viewpoint = SortViewpoint::new(cam, coord);
            let local = [
                camera.x - o[0] as f32,
                camera.y - o[1] as f32,
                camera.z - o[2] as f32,
            ];
            let mut sort = section.translucent_sort.borrow_mut();
            let (Some(sort), Some(mesh)) = (sort.as_mut(), section.translucent.as_ref()) else {
                return;
            };
            if !should_resort(sort.last_viewpoint(), viewpoint, block_changed, nearby) {
                return;
            }
            let indices = sort.resort(local, viewpoint);
            match (mesh, self.model.as_ref()) {
                (super::terrain::ResidentMesh::Arena(a), Some(model)) => {
                    model.mesh_arena.write_indices(queue, a, indices);
                }
                (super::terrain::ResidentMesh::Dedicated(m), _) => {
                    m.write_indices(queue, indices);
                }
                _ => {}
            }
            resorted += 1;
        };
        let mut far = Vec::new();
        for entry in &sortable {
            // Never-sorted meshes sort immediately; the rest wait for the scan.
            let unsorted = entry.0.translucent.is_some()
                && entry
                    .0
                    .translucent_sort
                    .borrow()
                    .as_ref()
                    .is_some_and(|q| q.last_viewpoint().is_none());
            if unsorted || entry.1 < NEARBY_DISTANCE_SQ {
                visit(*entry, true);
            } else {
                far.push(*entry);
            }
        }
        if !far.is_empty() {
            let budget = (far.len() / 8).max(15);
            let mut cursor = self.sort_cursor.get() % far.len();
            for _ in 0..budget.min(far.len()) {
                visit(far[cursor], false);
                cursor = (cursor + 1) % far.len();
            }
            self.sort_cursor.set(cursor);
        }
        resorted
    }

    /// Whether [`Self::set_improved_transparency`] is on.
    #[must_use]
    pub fn improved_transparency(&self) -> bool {
        self.improved_transparency
    }

    /// Decide whether this frame's translucent terrain takes the
    /// order-independent path, building or resizing its resources if so.
    pub(super) fn prepare_oit(&self, device: &wgpu::Device, has_translucent: bool) -> bool {
        let Some(model) = self.model.as_ref() else {
            return false;
        };
        if !self.improved_transparency || !has_translucent || !supported(device) {
            return false;
        }
        let mut state = self.oit.borrow_mut();
        let (width, height) = (self.depth.width, self.depth.height);
        match state.as_mut() {
            Some(existing) if existing.targets.matches(width, height) => {}
            Some(existing) => {
                existing.targets = OitTargets::new(
                    device,
                    &existing.pipelines,
                    width,
                    height,
                    &model.shared_cam_buffer,
                    model.origin_arena.buffer(),
                );
            }
            None => {
                let stride = model
                    .terrain_pipeline
                    .is_some()
                    .then(|| model.origin_arena.stride());
                let pipelines = OitPipelines::new(
                    device,
                    self.color_format,
                    &model.translucent_pipeline,
                    stride,
                );
                let targets = OitTargets::new(
                    device,
                    &pipelines,
                    width,
                    height,
                    &model.shared_cam_buffer,
                    model.origin_arena.buffer(),
                );
                *state = Some(OitState { pipelines, targets });
            }
        }
        true
    }

    /// Encode the stages and the composite for the visible translucent
    /// sections. Only called when [`Self::prepare_oit`] returned `true`; the
    /// world pass must be closed.
    pub(super) fn encode_oit_terrain(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        scene: &wgpu::TextureView,
        sections: &[(&ModelSectionGpu, f32)],
        stats: &mut RenderStats,
    ) {
        let (Some(model), state) = (self.model.as_ref(), self.oit.borrow()) else {
            return;
        };
        let Some(state) = state.as_ref() else {
            return;
        };

        let mut draws = Vec::new();
        for (section, _) in sections {
            if let Some(mesh) = section.translucent.as_ref() {
                stats.water_sections_drawn += usize::from(section.water_quad_count > 0);
                stats.translucent_sections_drawn += usize::from(section.translucent_quad_count > 0);
                stats.total_quads += section.water_quad_count + section.translucent_quad_count;
                draws.push(TerrainDraw::new(mesh, section.origin_alloc.offset() as u32));
            }
        }

        let depth = &self.depth.view;
        let targets = &state.targets;
        let pipelines = &state.pipelines;
        let mut scratch = RenderStats::default();
        {
            let mut pass = targets.bounds_pass(encoder, depth);
            draw_stage(
                &mut pass,
                model,
                (&pipelines.fluid.bounds, &pipelines.block.bounds),
                &model.cam_bind_group,
                &draws,
                &mut scratch,
            );
        }
        {
            let mut pass = targets.transmittance_pass(encoder, depth);
            draw_stage(
                &mut pass,
                model,
                (&pipelines.fluid.transmittance, &pipelines.block.transmittance),
                targets.transmittance_camera(),
                &draws,
                &mut scratch,
            );
        }
        {
            let mut pass = targets.accumulate_pass(encoder, depth);
            draw_stage(
                &mut pass,
                model,
                (&pipelines.fluid.accumulate, &pipelines.block.accumulate),
                targets.accumulate_camera(),
                &draws,
                stats,
            );
        }
        targets.composite(pipelines, encoder, scene);
        stats.oit_composites += 1;
        stats.draw_calls += draws.len();
    }
}

/// One stage's draws: every section's mixed mesh through the fluid-only
/// pipeline, then through the block-only one. Order within a stage does not
/// matter.
fn draw_stage(
    pass: &mut wgpu::RenderPass<'_>,
    model: &ModelRenderer,
    pipelines: (&wgpu::RenderPipeline, &wgpu::RenderPipeline),
    camera: &wgpu::BindGroup,
    draws: &[TerrainDraw<'_>],
    stats: &mut RenderStats,
) {
    let mut camera_last = None;
    for pipeline in [pipelines.0, pipelines.1] {
        pass.set_pipeline(pipeline);
        pass.set_bind_group(1, &model.atlas_bind_group, &[]);
        pass.set_bind_group(2, &model.palette_bind_group, &[]);
        pass.set_bind_group(3, &model.anim_bind_group, &[]);
        super::frame::emit_terrain_draws(pass, model, camera, draws, &mut camera_last, stats);
    }
}
