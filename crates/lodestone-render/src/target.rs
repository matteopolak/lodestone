//! Abstract render targets: the core renderer draws to a `TextureView` and does
//! not care whether it is an offscreen texture (headless / CI / bots) or a
//! window swapchain. Windowing is strictly additive on top of this.

/// Why a frame could not be acquired from a target. Mirrors wgpu 30's
/// [`wgpu::CurrentSurfaceTexture`] non-success variants (wgpu 30 replaced the
/// old `Result<SurfaceTexture, SurfaceError>` with this enum), so the frame
/// loop can react to each case explicitly instead of unwrapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TargetError {
    /// Acquiring the next frame timed out; skip this frame and retry.
    #[error("surface acquire timed out")]
    Timeout,
    /// The window is occluded (minimized / hidden); skip until visible.
    #[error("surface is occluded")]
    Occluded,
    /// The configuration is outdated (usually a resize); reconfigure and retry.
    #[error("surface configuration is outdated")]
    Outdated,
    /// The surface was lost; reconfigure (or recreate) and retry.
    #[error("surface was lost")]
    Lost,
    /// A validation error was raised while acquiring; attend to it and retry.
    #[error("surface acquire raised a validation error")]
    Validation,
}

impl TargetError {
    /// Whether reconfiguring the target is expected to fix this.
    #[must_use]
    pub const fn needs_reconfigure(self) -> bool {
        matches!(self, TargetError::Outdated | TargetError::Lost)
    }

    /// Whether this is a transient condition to simply wait out.
    #[must_use]
    pub const fn is_transient(self) -> bool {
        matches!(self, TargetError::Timeout | TargetError::Occluded)
    }
}

/// A frame acquired from a target: a view to render into plus, for a swapchain,
/// the texture that must be [`present`](AcquiredFrame::present)ed afterwards.
#[derive(Debug)]
pub struct AcquiredFrame {
    view: wgpu::TextureView,
    /// `true` if the surface reported the frame as suboptimal (e.g. mid-resize).
    pub suboptimal: bool,
    surface_texture: Option<wgpu::SurfaceTexture>,
    /// The texture backing [`Self::view`], for **every** target kind — unlike
    /// [`Self::texture`], which is deliberately `None` for a [`HeadlessTarget`]
    /// (see that method's own doc). This exists for a consumer that needs
    /// *some* texture to `copy_texture_to_texture` the already-rendered frame
    /// out of regardless of which target produced it — the menu background
    /// blur, whose GPU-gated tests run against a [`HeadlessTarget`] and would
    /// otherwise have no source texture to blur at all.
    colour_texture: wgpu::Texture,
}

impl AcquiredFrame {
    /// The view to attach as a colour target.
    #[must_use]
    pub fn view(&self) -> &wgpu::TextureView {
        &self.view
    }

    /// The backing swapchain texture, when this frame came from a window.
    ///
    /// `None` for a [`HeadlessTarget`], which owns its texture for the whole of
    /// its life and exposes it as [`HeadlessTarget::texture`] instead — there is
    /// no per-frame texture to hand back.
    ///
    /// This exists so a caller can `copy_texture_to_buffer` out of the window's
    /// own frame (`key.screenshot`, issue #16). [`Self::view`] cannot serve:
    /// a [`wgpu::TextureView`] is not a valid copy source. Reading it is only
    /// legal because [`SurfaceTarget::new`] ORs [`wgpu::TextureUsages::COPY_SRC`]
    /// into the swapchain config — without that flag the copy is a validation
    /// error, so the two changes are one change.
    ///
    /// **The content is undefined until something has rendered into the view.**
    /// Call this immediately before [`Self::present`], never straight after
    /// `acquire`.
    #[must_use]
    pub fn texture(&self) -> Option<&wgpu::Texture> {
        self.surface_texture.as_ref().map(|t| &t.texture)
    }

    /// The texture backing [`Self::view`] — see [`Self::colour_texture`]
    /// field doc for why this differs from [`Self::texture`].
    #[must_use]
    pub fn colour_texture(&self) -> &wgpu::Texture {
        &self.colour_texture
    }

    /// Present the frame. A no-op for headless targets; schedules presentation
    /// of a swapchain frame on `queue` (wgpu 30 presents via the queue).
    pub fn present(self, queue: &wgpu::Queue) {
        if let Some(t) = self.surface_texture {
            queue.present(t);
        }
    }
}

/// A surface the renderer can draw to.
pub trait RenderTarget {
    /// Colour format of the target.
    fn format(&self) -> wgpu::TextureFormat;
    /// Current `(width, height)` in pixels.
    fn size(&self) -> (u32, u32);
    /// Resize / reconfigure the target. Ignores zero dimensions.
    fn resize(&mut self, device: &wgpu::Device, width: u32, height: u32);
    /// Re-apply the target's current configuration, used to recover from a
    /// lost/outdated surface. Default is a no-op (headless never needs it).
    fn reconfigure(&mut self, _device: &wgpu::Device) {}
    /// Acquire the next frame to render into.
    ///
    /// # Errors
    /// Returns a [`TargetError`] describing why acquisition failed.
    fn acquire(&mut self) -> Result<AcquiredFrame, TargetError>;
}

/// An offscreen texture target. The default, so the renderer runs with no
/// window server. Rendered pixels can be read back for verification.
#[derive(Debug)]
pub struct HeadlessTarget {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    format: wgpu::TextureFormat,
    width: u32,
    height: u32,
}

impl HeadlessTarget {
    /// Usage flags on the backing texture: renderable, copyable out (for
    /// read-back verification) and copyable in — `COPY_DST` lets a test seed
    /// this target with known content (`queue.write_texture`, or a
    /// `copy_texture_to_texture` from another texture) before exercising a
    /// pass that reads "whatever was already drawn", such as the menu
    /// background blur (`menu::render::blur`), which otherwise has no
    /// headless-target-compatible way to set up its own precondition.
    pub const USAGE: wgpu::TextureUsages = wgpu::TextureUsages::RENDER_ATTACHMENT
        .union(wgpu::TextureUsages::COPY_SRC)
        .union(wgpu::TextureUsages::COPY_DST);

    /// Create an offscreen target of the given size and format.
    #[must_use]
    pub fn new(
        device: &wgpu::Device,
        width: u32,
        height: u32,
        format: wgpu::TextureFormat,
    ) -> Self {
        let (texture, view) = Self::make_texture(device, width.max(1), height.max(1), format);
        Self {
            texture,
            view,
            format,
            width: width.max(1),
            height: height.max(1),
        }
    }

    fn make_texture(
        device: &wgpu::Device,
        width: u32,
        height: u32,
        format: wgpu::TextureFormat,
    ) -> (wgpu::Texture, wgpu::TextureView) {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("lodestone-render headless target"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: Self::USAGE,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        (texture, view)
    }

    /// The backing texture (e.g. to copy out for read-back).
    #[must_use]
    pub fn texture(&self) -> &wgpu::Texture {
        &self.texture
    }

    /// Read the rendered pixels back as tightly-packed rows of 4-byte texels
    /// (row padding removed). Intended for headless verification; the target
    /// format must be 4 bytes per texel.
    #[must_use]
    pub fn read_texels(&self, device: &wgpu::Device, queue: &wgpu::Queue) -> Vec<u8> {
        const BPP: u32 = 4;
        let unpadded = self.width * BPP;
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let padded = unpadded.div_ceil(align) * align;

        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("lodestone-render readback"),
            size: u64::from(padded) * u64::from(self.height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("lodestone-render readback encoder"),
        });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(self.height),
                },
            },
            wgpu::Extent3d {
                width: self.width,
                height: self.height,
                depth_or_array_layers: 1,
            },
        );
        queue.submit(std::iter::once(encoder.finish()));

        readback.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        let _ = device.poll(wgpu::PollType::wait_indefinitely());

        let mut out = Vec::with_capacity((unpadded * self.height) as usize);
        {
            let view = readback.slice(..).get_mapped_range();
            let view = view.expect("mapped range");
            for row in 0..self.height {
                let start = (row * padded) as usize;
                out.extend_from_slice(&view[start..start + unpadded as usize]);
            }
        }
        readback.unmap();
        out
    }
}

impl RenderTarget for HeadlessTarget {
    fn format(&self) -> wgpu::TextureFormat {
        self.format
    }

    fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    fn resize(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        if width == 0 || height == 0 || (width == self.width && height == self.height) {
            return;
        }
        let (texture, view) = Self::make_texture(device, width, height, self.format);
        self.texture = texture;
        self.view = view;
        self.width = width;
        self.height = height;
    }

    fn acquire(&mut self) -> Result<AcquiredFrame, TargetError> {
        Ok(AcquiredFrame {
            view: self
                .texture
                .create_view(&wgpu::TextureViewDescriptor::default()),
            suboptimal: false,
            surface_texture: None,
            colour_texture: self.texture.clone(),
        })
    }
}

/// A windowed swapchain target. Kept backend-agnostic; created from an already
/// configured [`wgpu::Surface`] plus its configuration.
#[derive(Debug)]
pub struct SurfaceTarget<'window> {
    surface: wgpu::Surface<'window>,
    config: wgpu::SurfaceConfiguration,
    /// The format every acquired [`AcquiredFrame::view`] is created with, and
    /// the format [`RenderTarget::format`] reports for pipeline construction.
    ///
    /// Equal to `config.format` on native (Metal/Vulkan/DX12): wgpu-core's
    /// `surface_get_capabilities` sorts sRGB formats first
    /// (`sort_by_key(|fc| !fc.format.is_srgb())`), so `get_default_config`'s
    /// `formats[0]` is already an `*UnormSrgb` variant there.
    ///
    /// On the WebGPU backend this **must** differ from `config.format`: the
    /// browser canvas API only ever accepts `Rgba8Unorm`/`Bgra8Unorm`/
    /// `Rgba16Float` as a canvas format (see `wgpu`'s `WebSurface::get_capabilities`,
    /// which never lists an `*UnormSrgb` entry at all — there is structurally no
    /// sRGB canvas format to pick), so `config.format` is always non-sRGB there.
    /// [`SurfaceTarget::new`] compensates by reinterpreting the swapchain
    /// texture through an sRGB *view* (via `config.view_formats`), which is the
    /// mechanism the WebGPU spec provides for exactly this. Without it, the
    /// terrain/GUI shaders' linear output reaches the browser's compositor with
    /// no EOTF applied, and the whole presentation — world and every menu quad
    /// alike, since both go through this one swapchain — comes out uniformly
    /// darker than native.
    view_format: wgpu::TextureFormat,
    /// The present mode `get_default_config` chose for this adapter/surface,
    /// kept so [`Self::set_present_mode`] can restore *exactly* what we started
    /// with rather than a mode that merely sounds equivalent.
    ///
    /// [`wgpu::PresentMode::AutoVsync`] is **not** that equivalent: it resolves
    /// to `FifoRelaxed` wherever that exists, which permits tearing on a late
    /// frame, whereas wgpu's default config picks plain `Fifo`. Restoring by
    /// name instead of by remembered value would quietly change the default
    /// presentation of every platform that has `FifoRelaxed`.
    default_present_mode: wgpu::PresentMode,
}

/// Decide the view format an acquired swapchain frame should be created with,
/// given the format `get_default_config` chose for `configure`.
///
/// Returns `None` when `configured` is already sRGB (the native case — no
/// override needed, `config.view_formats` stays empty and the view is created
/// with `configured` directly). Returns `Some(srgb)` when `configured` has a
/// distinct sRGB counterpart (the WebGPU case — `configured` is always
/// `Rgba8Unorm`/`Bgra8Unorm`, since the browser canvas API has no sRGB format
/// to offer `get_default_config` in the first place).
///
/// Pure and wgpu-device-free on purpose: this is the seam a test can drive
/// with a capability list shaped like each backend's, without standing up a
/// real surface.
#[must_use]
fn choose_view_format(configured: wgpu::TextureFormat) -> Option<wgpu::TextureFormat> {
    if configured.is_srgb() {
        return None;
    }
    let srgb = configured.add_srgb_suffix();
    (srgb != configured).then_some(srgb)
}

impl<'window> SurfaceTarget<'window> {
    /// Build a target from a surface, using the adapter's default swapchain
    /// configuration for `width`x`height`. Returns `None` if the surface and
    /// adapter are incompatible (no default config).
    #[must_use]
    pub fn new(
        surface: wgpu::Surface<'window>,
        adapter: &wgpu::Adapter,
        device: &wgpu::Device,
        width: u32,
        height: u32,
    ) -> Option<Self> {
        let mut config = surface.get_default_config(adapter, width.max(1), height.max(1))?;
        // `get_default_config` returns `RENDER_ATTACHMENT` alone. `COPY_SRC` is
        // what makes `AcquiredFrame::texture` usable as a `copy_texture_to_buffer`
        // source, which is the whole of `key.screenshot`'s read-back (issue #16);
        // without it the copy is a validation error rather than a black image.
        // OR rather than assign, so a backend that already asked for more keeps it.
        //
        // Every other `configure` call site (`reconfigure`, `set_present_mode`,
        // `resize`) re-applies *this* `config`, so the flag survives a resize and
        // a surface-lost recovery without a second edit.
        config.usage |= wgpu::TextureUsages::COPY_SRC;
        // See `Self::view_format`'s doc: on the WebGPU backend `config.format`
        // is always non-sRGB, so ask the browser to let us reinterpret the
        // swapchain texture through an sRGB view. `view_formats` must be
        // declared up front for `configure` to permit it at `create_view` time.
        let view_format = choose_view_format(config.format);
        if let Some(srgb) = view_format {
            config.view_formats = vec![srgb];
        }
        surface.configure(device, &config);
        let default_present_mode = config.present_mode;
        Some(Self {
            surface,
            view_format: view_format.unwrap_or(config.format),
            config,
            default_present_mode,
        })
    }

    /// Re-apply the current configuration (surface-lost / outdated recovery).
    pub fn reconfigure(&self, device: &wgpu::Device) {
        self.surface.configure(device, &self.config);
    }

    /// The present mode currently in force.
    #[must_use]
    pub const fn present_mode(&self) -> wgpu::PresentMode {
        self.config.present_mode
    }

    /// The present mode the adapter itself chose at bring-up — pass this back to
    /// [`Self::set_present_mode`] to undo an override. See
    /// [`Self::default_present_mode`]'s field docs for why this is remembered
    /// rather than reconstructed.
    #[must_use]
    pub const fn default_present_mode(&self) -> wgpu::PresentMode {
        self.default_present_mode
    }

    /// Switch the swapchain's present mode — the vsync knob.
    ///
    /// A no-op when the mode already matches, which is what makes this safe to
    /// call every frame: `surface.configure` **recreates the swapchain**, so an
    /// unconditional version would rebuild it 60+ times a second and stutter.
    /// The guard is the feature, not an optimisation.
    ///
    /// `mode` is passed to wgpu as given, so prefer the `Auto*` variants: a
    /// concrete `Immediate`/`Mailbox` that the adapter does not advertise is a
    /// validation error, whereas [`wgpu::PresentMode::AutoNoVsync`] degrades to
    /// `Fifo` and simply stays capped.
    pub fn set_present_mode(&mut self, device: &wgpu::Device, mode: wgpu::PresentMode) {
        if self.config.present_mode == mode {
            return;
        }
        self.config.present_mode = mode;
        self.surface.configure(device, &self.config);
    }
}

impl RenderTarget for SurfaceTarget<'_> {
    fn format(&self) -> wgpu::TextureFormat {
        self.view_format
    }

    fn size(&self) -> (u32, u32) {
        (self.config.width, self.config.height)
    }

    fn resize(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(device, &self.config);
    }

    fn reconfigure(&mut self, device: &wgpu::Device) {
        self.surface.configure(device, &self.config);
    }

    fn acquire(&mut self) -> Result<AcquiredFrame, TargetError> {
        match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t) => Ok(Self::frame(t, false, self.view_format)),
            wgpu::CurrentSurfaceTexture::Suboptimal(t) => Ok(Self::frame(t, true, self.view_format)),
            wgpu::CurrentSurfaceTexture::Timeout => Err(TargetError::Timeout),
            wgpu::CurrentSurfaceTexture::Occluded => Err(TargetError::Occluded),
            wgpu::CurrentSurfaceTexture::Outdated => Err(TargetError::Outdated),
            wgpu::CurrentSurfaceTexture::Lost => Err(TargetError::Lost),
            wgpu::CurrentSurfaceTexture::Validation => Err(TargetError::Validation),
        }
    }
}

impl SurfaceTarget<'_> {
    fn frame(
        texture: wgpu::SurfaceTexture,
        suboptimal: bool,
        view_format: wgpu::TextureFormat,
    ) -> AcquiredFrame {
        let view = texture.texture.create_view(&wgpu::TextureViewDescriptor {
            format: Some(view_format),
            ..Default::default()
        });
        let colour_texture = texture.texture.clone();
        AcquiredFrame {
            view,
            suboptimal,
            colour_texture,
            surface_texture: Some(texture),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Format-decision gate for the "everything is darker on wasm" defect.
    ///
    /// `wgpu`'s `WebSurface::get_capabilities` (the WebGPU backend, used on
    /// wasm32) never lists an `*UnormSrgb` format at all -- the browser canvas
    /// API only accepts `Rgba8Unorm`/`Bgra8Unorm`/`Rgba16Float`, so
    /// `get_default_config`'s `formats[0]` is unconditionally non-sRGB there.
    /// This asserts the decision `SurfaceTarget::new` makes given that input:
    /// it must select an sRGB *view* format rather than silently presenting
    /// linear colour, which is what made the terrain/GUI shaders' linear
    /// output reach the compositor with no EOTF applied and the whole frame
    /// -- menus included, since every menu quad goes through this same
    /// swapchain -- come out darker than native.
    #[test]
    fn web_backend_non_srgb_swapchain_gets_an_srgb_view_format() {
        for configured in [wgpu::TextureFormat::Rgba8Unorm, wgpu::TextureFormat::Bgra8Unorm] {
            let got = choose_view_format(configured);
            let want = Some(configured.add_srgb_suffix());
            assert_eq!(
                got, want,
                "web-shaped input {configured:?}: expected view format override {want:?}, got {got:?}"
            );
            assert!(
                got.is_some_and(|f| f.is_srgb()),
                "view format override for {configured:?} must actually be sRGB, got {got:?}"
            );
        }
    }

    /// Companion arm: on native (Metal/Vulkan/DX12), wgpu-core's
    /// `surface_get_capabilities` sorts sRGB formats first
    /// (`sort_by_key(|fc| !fc.format.is_srgb())`), so `get_default_config`
    /// already hands `SurfaceTarget::new` an sRGB format. No view override is
    /// needed or should be applied -- confirming the fix is a wasm-only
    /// correction, not a blanket behaviour change that could regress native.
    #[test]
    fn native_srgb_swapchain_needs_no_view_format_override() {
        for configured in [
            wgpu::TextureFormat::Bgra8UnormSrgb,
            wgpu::TextureFormat::Rgba8UnormSrgb,
        ] {
            assert_eq!(
                choose_view_format(configured),
                None,
                "native-shaped input {configured:?} must not get a view format override"
            );
        }
    }

    #[test]
    fn target_error_classification() {
        assert!(TargetError::Outdated.needs_reconfigure());
        assert!(TargetError::Lost.needs_reconfigure());
        assert!(!TargetError::Timeout.needs_reconfigure());
        assert!(TargetError::Timeout.is_transient());
        assert!(TargetError::Occluded.is_transient());
        assert!(!TargetError::Validation.is_transient());
        assert!(!TargetError::Validation.needs_reconfigure());
    }
}
