//! Pluggable draw-submission strategies.
//!
//! Our primary target (Apple M5 / Metal, wgpu 30) does **not** expose
//! `MULTI_DRAW_INDIRECT_COUNT`, so there is no single "best" way to submit the
//! thousands of chunk-section draws a frame. Submission is therefore a strategy
//! chosen by a **pure function of [`GpuCapabilities`]**, so the Vulkan/DX12
//! indirect-count path can be unit-tested against synthetic capability sets long
//! before we ever run on that hardware.
//!
//! # What wgpu 30 actually exposes (measured against the crate source)
//!
//! There is a trap here that cost us a real (latent) bug. wgpu 30 has **no
//! public `Features::MULTI_DRAW_INDIRECT` bit**. `multi_draw_indexed_indirect`
//! (the base multi-draw call) is gated only on the `INDIRECT_EXECUTION`
//! *downlevel flag* — which merely means "indirect draws run at all" — and
//! wgpu-hal **emulates it as a per-draw CPU loop on Metal, WebGPU and GL**
//! (`wgpu-hal` `metal`/`webgpu` backends iterate `0..draw_count`). It is a
//! single native GPU command only on Vulkan (private cap) and DX12
//! (`ExecuteIndirect`). Because the only *public* capability that guarantees a
//! native multi-draw path is `MULTI_DRAW_INDIRECT_COUNT` (the strictly harder
//! superset), that feature is the sole honest signal we can select on.
//!
//! The consequence: on any backend where multi-draw is emulated,
//! [`StrategyKind::MdiZeroInstance`] iterates **every** region (including culled,
//! zeroed draws) as separate CPU-issued indirect draws, which is *strictly worse*
//! than [`StrategyKind::PerDraw`] issuing only the visible ones. So
//! `select_strategy` must **never** pick `MdiZeroInstance` off the back of
//! `INDIRECT_EXECUTION` alone. It selects `MdiCount` when the count feature is
//! present and `PerDraw` otherwise. WebGPU (which advertises
//! `INDIRECT_FIRST_INSTANCE` but no count) correctly lands on `PerDraw`.
//!
//! # The mesh-producer contract
//!
//! A meshing layer (later wired to `lodestone-world`/`lodestone-assets`)
//! produces, each frame, a slice of [`DrawRegion`] plus the vertex/index data
//! already living in suballocated arena buffers (see [`crate::arena`]). Each
//! region names an index range into the shared index arena and a `base_vertex`
//! into the shared vertex arena. Culling flips [`DrawRegion::visible`]. The
//! chosen strategy turns that slice into GPU work; the *result* is identical
//! across strategies (that is what makes [`StrategyKind::PerDraw`] the
//! correctness reference), only the CPU/GPU cost differs.

use crate::caps::GpuCapabilities;

/// One drawable region: an index range into the shared index arena plus the
/// vertex base into the shared vertex arena. This is exactly the shape a mesh
/// producer fills in; strategies never allocate it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DrawRegion {
    /// First index (in elements, not bytes) into the shared index buffer.
    pub first_index: u32,
    /// Number of indices to draw for this region.
    pub index_count: u32,
    /// Value added to every index before it selects a vertex; lets each region
    /// point at its own slice of the shared vertex arena.
    pub base_vertex: i32,
    /// Instance slot for this region (used as `first_instance`), so per-region
    /// data (chunk offset, etc.) can be looked up in an instance buffer.
    pub instance: u32,
    /// Whether the region survived culling this frame.
    pub visible: bool,
}

impl DrawRegion {
    /// Convert to `wgpu`'s indexed-indirect argument layout. Culled regions get
    /// `instance_count = 0` so the GPU skips them without the list changing size
    /// — this is exactly what [`StrategyKind::MdiZeroInstance`] relies on.
    #[must_use]
    pub fn to_indirect_args(&self) -> wgpu::util::DrawIndexedIndirectArgs {
        wgpu::util::DrawIndexedIndirectArgs {
            index_count: self.index_count,
            instance_count: u32::from(self.visible),
            first_index: self.first_index,
            base_vertex: self.base_vertex,
            first_instance: self.instance,
        }
    }
}

/// The three submission strategies, in descending order of preference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StrategyKind {
    /// GPU-side draw *count* via `multi_draw_indexed_indirect_count`.
    ///
    /// Best case: culling runs entirely on the GPU and the CPU issues a single
    /// command regardless of how many regions survive. Requires
    /// `MULTI_DRAW_INDIRECT_COUNT` — **unavailable on our Metal target**.
    ///
    /// * CPU cost: O(1) commands per frame; no read-back of visibility.
    /// * GPU cost: one indirect dispatch; count buffer drives the loop.
    MdiCount,
    /// Multi-draw indirect where culled draws are *zeroed* (instance_count = 0)
    /// rather than removed from the list.
    ///
    /// This is a genuine win **only** where `multi_draw_indexed_indirect` is a
    /// single native GPU command (Vulkan/DX12), so the whole region list submits
    /// in O(1) CPU commands and the GPU skips zeroed draws. It is **not**
    /// auto-selected in wgpu 30: the base multi-draw call is gated on the
    /// `INDIRECT_EXECUTION` downlevel flag but wgpu-hal emulates it as a per-draw
    /// CPU loop on Metal/WebGPU/GL, where it is strictly worse than `PerDraw`, and
    /// wgpu exposes no public flag distinguishing native from emulated. It is
    /// retained as a valid, tested strategy for callers that *know* their adapter
    /// has native base multi-draw (manual override), and for a future wgpu that
    /// surfaces that capability. See [`select_strategy`].
    ///
    /// * CPU cost: O(1) draw commands *iff* native; O(N) emulated (all regions).
    /// * GPU cost: iterates all N regions; zeroed draws are cheap but not free.
    MdiZeroInstance,
    /// One `draw_indexed` per visible region. The universal fallback and the
    /// correctness reference every other strategy is validated against.
    ///
    /// * CPU cost: O(visible) draw commands recorded on the CPU each frame.
    /// * GPU cost: minimal per-draw; no indirect buffer required.
    PerDraw,
}

impl StrategyKind {
    /// Stable identifier for logs/tests.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            StrategyKind::MdiCount => "mdi-count",
            StrategyKind::MdiZeroInstance => "mdi-zero-instance",
            StrategyKind::PerDraw => "per-draw",
        }
    }
}

/// Choose a submission strategy purely from capabilities.
///
/// The selection matrix (see the module docs and crate report for the full
/// reasoning about wgpu 30's emulated multi-draw):
///
/// | backend example        | `indirect_first_instance` | `multi_draw_indirect_count` | result     |
/// |------------------------|---------------------------|-----------------------------|------------|
/// | Vulkan / DX12 desktop  | yes                       | yes                         | `MdiCount` |
/// | Vulkan / DX12, no count| yes                       | no                          | `PerDraw`  |
/// | Metal (Apple M5)       | yes                       | no                          | `PerDraw`  |
/// | WebGPU (Chrome/Metal)  | yes                       | no                          | `PerDraw`  |
/// | WebGL2 / downlevel     | no                        | no                          | `PerDraw`  |
///
/// Only `MdiCount` is selected automatically, and only on the strength of the
/// real `MULTI_DRAW_INDIRECT_COUNT` feature — the one public wgpu 30 signal that
/// a *native* multi-draw path exists. Everything else is `PerDraw`, because the
/// base `multi_draw_indexed_indirect` is CPU-emulated on Metal/WebGPU/GL
/// (strictly worse than per-draw there) and wgpu offers no way to tell that
/// apart from a native path. This is why the WebGPU row resolves to `PerDraw`
/// despite `indirect_first_instance` being present — the earlier code selected
/// `MdiZeroInstance` here, which was the bug.
///
/// `indirect_first_instance` is required for `MdiCount` because each region
/// encodes its instance slot in `first_instance`; without it we cannot index
/// per-region instance data from an indirect draw and fall back to `PerDraw`
/// (which can set the base instance on the CPU side).
#[must_use]
pub fn select_strategy(caps: &GpuCapabilities) -> StrategyKind {
    if caps.indirect_execution && caps.multi_draw_indirect_count && caps.indirect_first_instance {
        StrategyKind::MdiCount
    } else {
        StrategyKind::PerDraw
    }
}

/// Errors raised while recording a submission.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StrategyError {
    /// A strategy that needs an indirect buffer was handed a submission without
    /// one.
    #[error("{strategy} requires a prepared indirect buffer")]
    MissingIndirectBuffer {
        /// Name of the strategy that failed.
        strategy: &'static str,
    },
    /// A strategy that needs a GPU draw-count buffer was handed one without.
    #[error("{strategy} requires a prepared draw-count buffer")]
    MissingCountBuffer {
        /// Name of the strategy that failed.
        strategy: &'static str,
    },
}

/// Everything a strategy needs to record a frame's draws. The mesh producer
/// fills `regions`; the indirect/count buffers are prepared by the strategy's
/// owner when the chosen strategy needs them.
#[derive(Debug, Clone, Copy)]
pub struct Submission<'a> {
    /// Per-region draw descriptions for this frame.
    pub regions: &'a [DrawRegion],
    /// Tightly-packed [`wgpu::util::DrawIndexedIndirectArgs`] for every region,
    /// required by the indirect strategies. `None` for [`PerDraw`].
    pub indirect: Option<&'a wgpu::Buffer>,
    /// A `u32` GPU draw count at offset 0, required by [`MdiCount`].
    pub count: Option<&'a wgpu::Buffer>,
    /// Number of draw slots present in `indirect` (its capacity in draws).
    pub draw_capacity: u32,
}

/// A pluggable way of turning a [`Submission`] into recorded GPU draws.
pub trait DrawStrategy: std::fmt::Debug {
    /// Which strategy this is.
    fn kind(&self) -> StrategyKind;

    /// Record this frame's draws into `pass`. The active pipeline, bind groups,
    /// vertex buffer and index buffer must already be set by the caller.
    ///
    /// # Errors
    /// Returns a [`StrategyError`] if the submission is missing a buffer the
    /// strategy requires.
    fn record(
        &self,
        submission: &Submission<'_>,
        pass: &mut wgpu::RenderPass<'_>,
    ) -> Result<(), StrategyError>;
}

/// One `draw_indexed` per visible region. Correctness reference.
#[derive(Debug, Clone, Copy, Default)]
pub struct PerDraw;

impl DrawStrategy for PerDraw {
    fn kind(&self) -> StrategyKind {
        StrategyKind::PerDraw
    }

    fn record(
        &self,
        submission: &Submission<'_>,
        pass: &mut wgpu::RenderPass<'_>,
    ) -> Result<(), StrategyError> {
        for r in submission.regions {
            if !r.visible {
                continue;
            }
            let base = r.first_index;
            pass.draw_indexed(
                base..base + r.index_count,
                r.base_vertex,
                r.instance..r.instance + 1,
            );
        }
        Ok(())
    }
}

/// Multi-draw indirect with culled draws zeroed out. Metal fallback.
#[derive(Debug, Clone, Copy, Default)]
pub struct MdiZeroInstance;

impl DrawStrategy for MdiZeroInstance {
    fn kind(&self) -> StrategyKind {
        StrategyKind::MdiZeroInstance
    }

    fn record(
        &self,
        submission: &Submission<'_>,
        pass: &mut wgpu::RenderPass<'_>,
    ) -> Result<(), StrategyError> {
        let indirect = submission
            .indirect
            .ok_or(StrategyError::MissingIndirectBuffer {
                strategy: self.kind().name(),
            })?;
        // Every region is submitted; invisible regions were encoded with
        // instance_count = 0 and become GPU no-ops. Needs only
        // DownlevelFlags::INDIRECT_EXECUTION (emulated on Metal).
        pass.multi_draw_indexed_indirect(indirect, 0, submission.draw_capacity);
        Ok(())
    }
}

/// Multi-draw indirect with a GPU-provided draw count. Best; needs
/// `MULTI_DRAW_INDIRECT_COUNT`.
#[derive(Debug, Clone, Copy, Default)]
pub struct MdiCount;

impl DrawStrategy for MdiCount {
    fn kind(&self) -> StrategyKind {
        StrategyKind::MdiCount
    }

    fn record(
        &self,
        submission: &Submission<'_>,
        pass: &mut wgpu::RenderPass<'_>,
    ) -> Result<(), StrategyError> {
        let indirect = submission
            .indirect
            .ok_or(StrategyError::MissingIndirectBuffer {
                strategy: self.kind().name(),
            })?;
        let count = submission.count.ok_or(StrategyError::MissingCountBuffer {
            strategy: self.kind().name(),
        })?;
        pass.multi_draw_indexed_indirect_count(indirect, 0, count, 0, submission.draw_capacity);
        Ok(())
    }
}

/// Build the boxed strategy chosen by [`select_strategy`].
#[must_use]
pub fn build_strategy(kind: StrategyKind) -> Box<dyn DrawStrategy> {
    match kind {
        StrategyKind::MdiCount => Box::new(MdiCount),
        StrategyKind::MdiZeroInstance => Box::new(MdiZeroInstance),
        StrategyKind::PerDraw => Box::new(PerDraw),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::caps::{Backend, GpuCapabilities};

    fn caps(indirect_exec: bool, first_instance: bool, count: bool) -> GpuCapabilities {
        GpuCapabilities {
            indirect_execution: indirect_exec,
            indirect_first_instance: first_instance,
            multi_draw_indirect_count: count,
            ..GpuCapabilities::baseline()
        }
    }

    #[test]
    fn metal_target_selects_per_draw() {
        // Our measured M5/Metal reality: indirect execution + first-instance,
        // but no GPU-side count. Because wgpu-hal *emulates* multi-draw on Metal
        // as a per-draw CPU loop, MdiZeroInstance would be strictly worse than
        // PerDraw, so PerDraw is correct here.
        let m5 = GpuCapabilities {
            backend: Backend::Metal,
            indirect_execution: true,
            indirect_first_instance: true,
            multi_draw_indirect_count: false,
            ..GpuCapabilities::baseline()
        };
        assert_eq!(select_strategy(&m5), StrategyKind::PerDraw);
    }

    #[test]
    fn webgpu_measured_caps_select_per_draw() {
        // Measured directly in Chrome/Apple WebGPU: INDIRECT_FIRST_INSTANCE is
        // present, but there is no multi-draw-indirect of any kind and no COUNT
        // feature. The old selector saw `indirect_execution && first_instance`
        // and picked MdiZeroInstance — a strategy whose only value needs a native
        // multi-draw that WebGPU emulates. This must resolve to PerDraw.
        let webgpu = GpuCapabilities {
            backend: Backend::Other,
            indirect_execution: true,
            indirect_first_instance: true,
            multi_draw_indirect_count: false,
            texture_binding_array: false,
            nonuniform_binding_array_indexing: false,
            ..GpuCapabilities::baseline()
        };
        assert_eq!(select_strategy(&webgpu), StrategyKind::PerDraw);
    }

    #[test]
    fn hypothetical_vulkan_with_count_selects_mdi_count() {
        // We cannot run this locally, but the selection must be right before we
        // ever touch that hardware.
        let vk = caps(true, true, true);
        assert_eq!(select_strategy(&vk), StrategyKind::MdiCount);
    }

    #[test]
    fn no_indirect_falls_back_to_per_draw() {
        assert_eq!(
            select_strategy(&caps(false, false, false)),
            StrategyKind::PerDraw
        );
        // Count without indirect execution is nonsensical: still per-draw.
        assert_eq!(
            select_strategy(&caps(false, true, true)),
            StrategyKind::PerDraw
        );
    }

    #[test]
    fn indirect_without_first_instance_falls_back() {
        // Count present but base instance unusable in indirect draws -> per-draw.
        assert_eq!(
            select_strategy(&caps(true, false, true)),
            StrategyKind::PerDraw
        );
    }

    #[test]
    fn full_selection_matrix() {
        let expect = |ie, fi, cnt, want| {
            assert_eq!(select_strategy(&caps(ie, fi, cnt)), want);
        };
        // Only the count feature + first-instance selects MdiCount; everything
        // else is PerDraw. Notably (true, true, false) — indirect execution but
        // no count, i.e. Metal/WebGPU — is PerDraw, never MdiZeroInstance.
        expect(true, true, true, StrategyKind::MdiCount);
        expect(true, true, false, StrategyKind::PerDraw);
        expect(true, false, true, StrategyKind::PerDraw);
        expect(true, false, false, StrategyKind::PerDraw);
        expect(false, true, true, StrategyKind::PerDraw);
        expect(false, false, false, StrategyKind::PerDraw);
    }

    #[test]
    fn built_strategy_matches_kind() {
        for k in [
            StrategyKind::MdiCount,
            StrategyKind::MdiZeroInstance,
            StrategyKind::PerDraw,
        ] {
            assert_eq!(build_strategy(k).kind(), k);
            assert_eq!(k.name(), build_strategy(k).kind().name());
        }
    }

    #[test]
    fn culled_region_zeroes_instance_count() {
        let r = DrawRegion {
            first_index: 0,
            index_count: 6,
            base_vertex: 0,
            instance: 3,
            visible: false,
        };
        assert_eq!(r.to_indirect_args().instance_count, 0);
        let visible = DrawRegion { visible: true, ..r };
        assert_eq!(visible.to_indirect_args().instance_count, 1);
        assert_eq!(visible.to_indirect_args().first_instance, 3);
    }
}
