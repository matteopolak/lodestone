//! Inspectable GPU capability detection, decoupled from any live device.
//!
//! The whole design goal here is the split between **detect** and **decide**:
//!
//! * [`GpuCapabilities`] is a plain data struct with no `wgpu` handles inside
//!   it. It can be built from a real adapter with [`GpuCapabilities::probe`]
//!   (see [`crate::device`]) *or* fabricated directly in a unit test. That means
//!   every piece of logic that branches on what the GPU can do is testable on a
//!   machine with no GPU and no window server.
//! * Decisions (which draw strategy to use, whether binding arrays are usable,
//!   …) are **pure functions over `GpuCapabilities`**, living in
//!   [`crate::strategy`]. They never touch a device.

/// Which backend the adapter is running on. Mirrors the subset of
/// [`wgpu::Backend`] we care about, but is its own type so capability structs
/// can be constructed in tests without pulling a real backend value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Backend {
    /// Vulkan (Linux/Windows/Android, MoltenVK).
    Vulkan,
    /// Metal (macOS/iOS).
    Metal,
    /// Direct3D 12 (Windows).
    Dx12,
    /// OpenGL / GLES.
    Gl,
    /// WebGPU (browser).
    BrowserWebGpu,
    /// Anything else / not reported.
    Other,
}

/// A GPU-independent, fully-inspectable description of what a single adapter can
/// actually do. Construct real ones with [`GpuCapabilities::probe`]; construct
/// synthetic ones in tests with a struct literal or [`GpuCapabilities::baseline`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpuCapabilities {
    /// Human-readable adapter name (for logs).
    pub adapter_name: String,
    /// Backend the adapter is exposed through.
    pub backend: Backend,

    // --- Features that steer draw submission & meshing ---
    /// `INDIRECT_FIRST_INSTANCE`: indirect draws may set a non-zero base
    /// instance. Required for our instanced indirect paths.
    pub indirect_first_instance: bool,
    /// A single `draw_indexed_indirect` (arguments sourced from a GPU buffer)
    /// works at all. Probed from the `INDIRECT_EXECUTION` *downlevel flag*.
    ///
    /// This is deliberately **not** called `multi_draw_indirect`. wgpu 30 has no
    /// public `Features::MULTI_DRAW_INDIRECT` bit, and a multi-draw call
    /// (`multi_draw_indexed_indirect` with count > 1) is only gated on this same
    /// downlevel flag — yet it is **emulated as a per-draw CPU loop** in wgpu-hal
    /// on Metal, WebGPU and GL, and is a single native command only on Vulkan and
    /// DX12. wgpu exposes no way to tell those two cases apart, so this boolean
    /// means exactly "indirect draws execute", nothing about multi-draw being
    /// cheap. See [`crate::device`] for the probe and [`crate::strategy`] for why
    /// this must not drive strategy selection on its own.
    pub indirect_execution: bool,
    /// GPU-side draw *count* (`MULTI_DRAW_INDIRECT_COUNT`): the draw count comes
    /// from a GPU buffer, so culling never round-trips to the CPU. This is a real
    /// wgpu feature bit and, being the strictly harder superset, is the only
    /// public signal in wgpu 30 that a *native* multi-draw path exists.
    pub multi_draw_indirect_count: bool,
    /// Timestamp queries are supported at all.
    pub timestamp_query: bool,
    /// Timestamp queries may be written *inside* a render/compute pass encoder
    /// (`TIMESTAMP_QUERY_INSIDE_ENCODERS`).
    pub timestamp_inside_encoders: bool,
    /// Sampled-texture binding arrays (`TEXTURE_BINDING_ARRAY`) are supported —
    /// the basis of a bindless texture atlas.
    pub texture_binding_array: bool,
    /// Non-uniform (dynamic) indexing into those binding arrays is supported.
    pub nonuniform_binding_array_indexing: bool,
    /// Subgroup (wave/warp) operations are supported.
    pub subgroup: bool,
    /// 64-bit integers in shaders (`SHADER_INT64`).
    pub shader_int64: bool,
    /// Experimental mesh-shader pipeline is exposed.
    pub experimental_mesh_shader: bool,

    // --- Limits that matter for us ---
    /// Largest single buffer we may create.
    pub max_buffer_size: u64,
    /// Maximum number of bind groups bound at once.
    pub max_bind_groups: u32,
    /// Maximum texture array layers (atlas depth).
    pub max_texture_array_layers: u32,
    /// Largest storage-buffer binding, in bytes.
    pub max_storage_buffer_binding_size: u64,
    /// Storage buffers bindable per shader stage.
    pub max_storage_buffers_per_shader_stage: u32,
}

impl GpuCapabilities {
    /// A deliberately conservative capability set: no optional features, and the
    /// minimum limits guaranteed by `wgpu`'s downlevel defaults. Useful as a
    /// base for tests (`GpuCapabilities { indirect_execution: true,
    /// ..GpuCapabilities::baseline() }`) and as a safe fallback.
    #[must_use]
    pub fn baseline() -> Self {
        Self {
            adapter_name: "synthetic-baseline".to_owned(),
            backend: Backend::Other,
            indirect_first_instance: false,
            indirect_execution: false,
            multi_draw_indirect_count: false,
            timestamp_query: false,
            timestamp_inside_encoders: false,
            texture_binding_array: false,
            nonuniform_binding_array_indexing: false,
            subgroup: false,
            shader_int64: false,
            experimental_mesh_shader: false,
            // wgpu downlevel-webgl2 guaranteed minimums.
            max_buffer_size: 256 << 20,
            max_bind_groups: 4,
            max_texture_array_layers: 256,
            max_storage_buffer_binding_size: 128 << 20,
            max_storage_buffers_per_shader_stage: 0,
        }
    }

    /// Whether a bindless texture atlas (binding array + non-uniform indexing)
    /// is usable. This is a *decision*, kept pure so it is testable without a
    /// GPU — the lesson being that published guidance about binding arrays on
    /// Metal was wrong, so we only ever trust the probed booleans, never a
    /// hardcoded backend assumption.
    #[must_use]
    pub fn supports_bindless_atlas(&self) -> bool {
        self.texture_binding_array && self.nonuniform_binding_array_indexing
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Restored after an over-aggressive trim pass mistook this for a
    /// restated-literal test: `baseline()` is a hand-edited struct literal
    /// that other code (this crate's `strategy` tests, and any caller
    /// wanting a safe fallback) relies on staying conservative, and
    /// `bindless_needs_both_flags` below covers only the bindless flags —
    /// an edit that flipped `indirect_execution`/`multi_draw_indirect_count`
    /// to `true`, or dropped `max_buffer_size` below the WebGPU
    /// downlevel-webgl2 guaranteed minimum, would pass every other test in
    /// this file.
    #[test]
    fn baseline_is_conservative() {
        let c = GpuCapabilities::baseline();
        assert!(!c.indirect_execution);
        assert!(!c.multi_draw_indirect_count);
        assert!(!c.supports_bindless_atlas());
        assert!(c.max_buffer_size >= 256 << 20);
    }

    #[test]
    fn bindless_needs_both_flags() {
        let mut c = GpuCapabilities::baseline();
        c.texture_binding_array = true;
        assert!(!c.supports_bindless_atlas(), "array alone is not enough");
        c.nonuniform_binding_array_indexing = true;
        assert!(c.supports_bindless_atlas());
    }
}
