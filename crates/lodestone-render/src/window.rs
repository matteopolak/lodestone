//! Optional `winit` window integration (feature `window`).
//!
//! Windowing is deliberately isolated behind a feature so the core renderer —
//! and the entire default test suite — builds and runs with no window server.
//! This module only provides the glue to create a [`SurfaceTarget`] from a
//! `winit` window; it does not own the event loop, keeping the render core
//! independent of any particular windowing model.
//!
//! `winit` 0.30 uses the `ApplicationHandler` trait / resumed-driven window
//! creation model, so surface creation must happen after `resumed`; callers own
//! that lifecycle and hand us a live window here.

use std::sync::Arc;

use winit::window::Window;

use crate::device::{GpuContext, GpuError};
use crate::target::{RenderTarget, SurfaceTarget};

/// Errors specific to windowed bring-up.
#[derive(Debug, thiserror::Error)]
pub enum WindowError {
    /// The surface could not be created for the window.
    #[error("failed to create surface: {0}")]
    CreateSurface(#[from] wgpu::CreateSurfaceError),
    /// GPU bring-up failed.
    #[error(transparent)]
    Gpu(#[from] GpuError),
    /// The adapter and surface had no compatible default configuration.
    #[error("no default surface configuration for this adapter/window")]
    NoDefaultConfig,
}

/// Create a GPU context whose adapter is compatible with `window`, plus a
/// [`SurfaceTarget`] sized to the window's current inner size.
///
/// The window is held by an [`Arc`] so the surface can borrow it for `'static`,
/// matching `winit` 0.30's shared-window model.
///
/// # Errors
/// Returns [`WindowError`] if surface creation, adapter/device selection, or
/// swapchain configuration fails.
/// Native-only: it blocks on adapter/device selection. A browser main thread cannot
/// block — see [`attach_window_async`], which this is a thin wrapper over.
#[cfg(not(target_arch = "wasm32"))]
pub fn attach_window(
    window: Arc<Window>,
) -> Result<(GpuContext, SurfaceTarget<'static>), WindowError> {
    pollster::block_on(attach_window_async(window))
}

/// As [`attach_window`], but `await`s adapter/device selection instead of blocking on
/// it.
///
/// **This is the real function and `attach_window` is the wrapper**, which is the right
/// way round: `Instance::request_adapter` and `Adapter::request_device` are genuinely
/// asynchronous, and only the native caller can afford to pretend otherwise.
/// `pollster::block_on` parks the calling thread until the future completes; on a
/// browser main thread there is no other thread to make progress, so the future it is
/// waiting on can never resolve. Blocking there does not merely stall, it cannot
/// finish.
///
/// The browser caller therefore drives this from `wasm_bindgen_futures::spawn_local`
/// and tolerates a frame or two with no GPU — see `lodestone-shell`'s
/// `app::lifecycle`, whose `resumed` is split precisely along this seam.
///
/// # Errors
/// Returns [`WindowError`] if surface creation, adapter/device selection, or
/// swapchain configuration fails.
pub async fn attach_window_async(
    window: Arc<Window>,
) -> Result<(GpuContext, SurfaceTarget<'static>), WindowError> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let surface = instance.create_surface(window.clone())?;
    let ctx = GpuContext::new_for_surface(instance, &surface).await?;

    let size = window.inner_size();
    let target = SurfaceTarget::new(
        surface,
        ctx.adapter(),
        ctx.device(),
        size.width,
        size.height,
    )
    .ok_or(WindowError::NoDefaultConfig)?;

    // Diagnostic: the chosen swapchain format decides whether the surface
    // performs the sRGB encode the terrain shader assumes (it writes linear
    // colour and relies on an `*UnormSrgb` swapchain to do the rest). This
    // crate has no `log`/`tracing` dependency, so `eprintln!` is the
    // available idiom here; keep this line permanently, it is the cheapest
    // possible catch for a whole class of "washed out" colour regressions.
    eprintln!("[lodestone-render] surface format: {:?}", target.format());

    Ok((ctx, target))
}
