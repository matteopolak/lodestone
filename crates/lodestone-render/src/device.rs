//! Device / adapter bring-up and capability probing.
//!
//! This is the **detect** half of the detect-vs-decide split: it turns a live
//! `wgpu` adapter into an inspectable [`GpuCapabilities`]. Every downstream
//! decision (draw strategy, bindless atlas, …) is a pure function over that
//! struct and lives elsewhere, so it can be tested with no GPU.

use crate::caps::{Backend, GpuCapabilities};

/// Errors from GPU bring-up.
#[derive(Debug, thiserror::Error)]
pub enum GpuError {
    /// No adapter matched the request (e.g. headless CI with no GPU backend).
    #[error("no suitable GPU adapter: {0}")]
    NoAdapter(#[from] wgpu::RequestAdapterError),
    /// The device could not be created from the chosen adapter.
    #[error("failed to create device: {0}")]
    DeviceRequest(#[from] wgpu::RequestDeviceError),
}

/// A live GPU context: instance, adapter, device, queue and the probed
/// capabilities of the adapter.
#[derive(Debug)]
pub struct GpuContext {
    instance: wgpu::Instance,
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
    caps: GpuCapabilities,
}

impl GpuContext {
    /// Bring up a headless context (no surface). Suitable for the bot/library
    /// use case and CI. Requests a high-performance adapter but tolerates
    /// whatever is available.
    ///
    /// # Errors
    /// Returns [`GpuError`] if no adapter or device is available.
    pub async fn new_headless() -> Result<Self, GpuError> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        Self::from_instance(instance, None).await
    }

    /// Blocking convenience wrapper around [`GpuContext::new_headless`].
    ///
    /// # Errors
    /// See [`GpuContext::new_headless`].
    pub fn new_headless_blocking() -> Result<Self, GpuError> {
        pollster::block_on(Self::new_headless())
    }

    /// Bring up a context whose adapter is guaranteed compatible with
    /// `surface`. Used by the windowed path.
    ///
    /// # Errors
    /// Returns [`GpuError`] if no compatible adapter or device is available.
    pub async fn new_for_surface(
        instance: wgpu::Instance,
        surface: &wgpu::Surface<'_>,
    ) -> Result<Self, GpuError> {
        Self::from_instance(instance, Some(surface)).await
    }

    async fn from_instance(
        instance: wgpu::Instance,
        compatible_surface: Option<&wgpu::Surface<'_>>,
    ) -> Result<Self, GpuError> {
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                force_fallback_adapter: false,
                compatible_surface,
                apply_limit_buckets: false,
            })
            .await?;

        let caps = GpuCapabilities::probe(&adapter);

        // We request no optional features yet — the scaffold only needs the
        // core pipeline. Capabilities are probed from the *adapter*, so
        // downstream code still sees everything the hardware can do even though
        // the device is minimal. We take the adapter's limits so large arena
        // buffers are permitted.
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("lodestone-render device"),
                required_features: wgpu::Features::empty(),
                required_limits: adapter.limits(),
                ..Default::default()
            })
            .await?;

        Ok(Self {
            instance,
            adapter,
            device,
            queue,
            caps,
        })
    }

    /// The probed capabilities of this adapter.
    #[must_use]
    pub fn capabilities(&self) -> &GpuCapabilities {
        &self.caps
    }

    /// The `wgpu` instance.
    #[must_use]
    pub fn instance(&self) -> &wgpu::Instance {
        &self.instance
    }

    /// The chosen adapter.
    #[must_use]
    pub fn adapter(&self) -> &wgpu::Adapter {
        &self.adapter
    }

    /// The logical device.
    #[must_use]
    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }

    /// The command queue.
    #[must_use]
    pub fn queue(&self) -> &wgpu::Queue {
        &self.queue
    }
}

impl GpuCapabilities {
    /// Probe a live adapter into a GPU-independent capability struct.
    ///
    /// Crucially, feature booleans are read straight off the adapter — never
    /// inferred from the backend. `indirect_execution` maps to the *downlevel*
    /// `INDIRECT_EXECUTION` flag (meaning only "indirect draws run"; multi-draw
    /// is CPU-emulated on Metal/WebGPU/GL, so this flag must not by itself pick a
    /// multi-draw strategy), while `multi_draw_indirect_count` maps to the real
    /// `MULTI_DRAW_INDIRECT_COUNT` feature — the only public wgpu 30 signal that a
    /// native multi-draw path exists.
    #[must_use]
    pub fn probe(adapter: &wgpu::Adapter) -> Self {
        let f = adapter.features();
        let l = adapter.limits();
        let info = adapter.get_info();
        let downlevel = adapter.get_downlevel_capabilities();

        Self {
            adapter_name: info.name.clone(),
            backend: map_backend(info.backend),
            indirect_first_instance: f.contains(wgpu::Features::INDIRECT_FIRST_INSTANCE),
            indirect_execution: downlevel
                .flags
                .contains(wgpu::DownlevelFlags::INDIRECT_EXECUTION),
            multi_draw_indirect_count: f.contains(wgpu::Features::MULTI_DRAW_INDIRECT_COUNT),
            timestamp_query: f.contains(wgpu::Features::TIMESTAMP_QUERY),
            timestamp_inside_encoders: f.contains(wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS),
            texture_binding_array: f.contains(wgpu::Features::TEXTURE_BINDING_ARRAY),
            nonuniform_binding_array_indexing: f.contains(
                wgpu::Features::SAMPLED_TEXTURE_AND_STORAGE_BUFFER_ARRAY_NON_UNIFORM_INDEXING,
            ),
            subgroup: f.contains(wgpu::Features::SUBGROUP),
            shader_int64: f.contains(wgpu::Features::SHADER_INT64),
            experimental_mesh_shader: f.contains(wgpu::Features::EXPERIMENTAL_MESH_SHADER),
            max_buffer_size: l.max_buffer_size,
            max_bind_groups: l.max_bind_groups,
            max_texture_array_layers: l.max_texture_array_layers,
            max_storage_buffer_binding_size: l.max_storage_buffer_binding_size,
            max_storage_buffers_per_shader_stage: l.max_storage_buffers_per_shader_stage,
        }
    }
}

fn map_backend(b: wgpu::Backend) -> Backend {
    match b {
        wgpu::Backend::Vulkan => Backend::Vulkan,
        wgpu::Backend::Metal => Backend::Metal,
        wgpu::Backend::Dx12 => Backend::Dx12,
        wgpu::Backend::Gl => Backend::Gl,
        wgpu::Backend::BrowserWebGpu => Backend::BrowserWebGpu,
        wgpu::Backend::Noop => Backend::Other,
    }
}
