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
                    &model.water_pipeline,
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

        let mut water = Vec::new();
        let mut blocks = Vec::new();
        for (section, _) in sections {
            let offset = section.origin_alloc.offset() as u32;
            if let Some(mesh) = section.water.as_ref() {
                stats.water_sections_drawn += 1;
                stats.total_quads += section.water_quad_count;
                water.push(TerrainDraw::new(mesh, offset));
            }
            if let Some(mesh) = section.translucent.as_ref() {
                stats.translucent_sections_drawn += 1;
                stats.total_quads += section.translucent_quad_count;
                blocks.push(TerrainDraw::new(mesh, offset));
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
                (&water, &blocks),
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
                (&water, &blocks),
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
                (&water, &blocks),
                stats,
            );
        }
        targets.composite(pipelines, encoder, scene);
        stats.oit_composites += 1;
        stats.draw_calls += water.len() + blocks.len();
    }
}

/// One stage's draws: water through the fluid pipeline, then translucent
/// blocks through theirs. Order within a stage does not matter.
fn draw_stage(
    pass: &mut wgpu::RenderPass<'_>,
    model: &ModelRenderer,
    pipelines: (&wgpu::RenderPipeline, &wgpu::RenderPipeline),
    camera: &wgpu::BindGroup,
    draws: (&[TerrainDraw<'_>], &[TerrainDraw<'_>]),
    stats: &mut RenderStats,
) {
    let mut camera_last = None;
    pass.set_pipeline(pipelines.0);
    pass.set_bind_group(1, &model.atlas_bind_group, &[]);
    pass.set_bind_group(2, &model.water_anim_bind_group, &[]);
    super::frame::emit_terrain_draws(pass, model, camera, draws.0, &mut camera_last, stats);

    pass.set_pipeline(pipelines.1);
    pass.set_bind_group(1, &model.atlas_bind_group, &[]);
    pass.set_bind_group(2, &model.palette_bind_group, &[]);
    pass.set_bind_group(3, &model.anim_bind_group, &[]);
    super::frame::emit_terrain_draws(pass, model, camera, draws.1, &mut camera_last, stats);
}
