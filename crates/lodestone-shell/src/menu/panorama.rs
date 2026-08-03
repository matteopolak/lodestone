//! The title screen's spinning cubemap panorama.
//!
//! ## What it is
//!
//! Vanilla's title screen background is **not** a world render and not a scrolling
//! image: it is a unit cube, textured with a six-face cubemap, viewed from the
//! inside through an 85° perspective, tilted 10° down and yawed slowly. The whole
//! thing is `CubeMap.java` + `Panorama.java` + `panorama.{vsh,fsh}`, all four of
//! which are short, and this module is their port.
//!
//! [`PanoramaFaces`] is the CPU half (decode + stack the six PNGs) and
//! [`PanoramaRenderer`] the GPU half (cube texture, pipeline, 36-vertex buffer,
//! spin state). `menu/render.rs` owns exactly three lines of it: a field, a lazy
//! attach, and a draw before the menu's own quads.
//!
//! ## How it works
//!
//! Six things fix the image, and five of them are easy to get plausibly wrong —
//! a scrambled sky still looks like a sky. Each is a named constant below with
//! its source line:
//!
//! 1. **Face order.** `CubeMapTexture.SUFFIXES` is `_1, _3, _5, _4, _0, _2`
//!    ([`FACE_SUFFIXES`]) — **not** `0..5`. Layer `n` of a cubemap is
//!    `+X, -X, +Y, -Y, +Z, -Z`, so `panorama_1` is +X and `panorama_0` is +Z.
//! 2. **Each face is flipped vertically** as it is stacked
//!    (`copyRect(…, swapX = false, swapY = true)`, `CubeMapTexture.java:28,49`;
//!    `swapY` writes source row `y` to target row `h-1-y`).
//! 3. **The sampler is Linear**, from `TextureMetadataSection(blur = true, …)`
//!    (`CubeMapTexture.java:53`). Almost every other menu texture in this repo is
//!    Nearest; this one is deliberately not.
//! 4. **The geometry carries no UVs** — `DefaultVertexFormat.POSITION`, 24
//!    vertices, 6 quads ([`CUBE_QUADS`]). The fragment stage samples by
//!    *direction*, using the object-space position verbatim.
//! 5. **The projection is perspective**, FOV 85° vertical, near 0.05, far 10.0
//!    (`CubeMap.java:29-31`; `Projection` feeds `fov` to JOML's `setPerspective`,
//!    whose first argument is `fovy`).
//! 6. **The model-view is `rotationX(PI)` then `rotateX(10°)` then
//!    `rotateY(spin)`** (`CubeMap.java:57-59`), where the 10° comes from
//!    `GuiRenderer.java:120`'s `cubeMap.render(10.0F, spin)` and `spin` is the
//!    **negated** accumulator (`Panorama.java:30` passes `-this.spin`).
//!
//! Spin accumulates as `wrapDegrees(spin + realtimeDeltaTicks * panoramaSpeed *
//! 0.1)` (`Panorama.java:24-28`). Note *realtime* delta ticks: the title screen
//! has no world clock, so [`PanoramaRenderer::advance`] measures wall time and
//! converts at [`TICKS_PER_SECOND`]. At the default `panoramaSpeed` of 1.0
//! (`Options.java:313-320`) that is 2°/s — a three-minute revolution, which is
//! slow enough that "it looks static" is not evidence of a bug. (It *was* also
//! genuinely invisible for one commit, for a different reason: the faces were
//! being read from the jar's 1×1 stubs, and a solid-coloured cube looks identical
//! at every yaw. Both explanations are live; check
//! [`PanoramaFaces::from_object_store`] before believing either.)
//!
//! ## Where the faces come from — not the jar
//!
//! **`client.jar` ships a 69-byte 1×1 grey stub for all six faces.** The real
//! 1024×1024 art is delivered through the launcher's asset-object store, and
//! [`load`] prefers that store per face, falling back to the stub so a checkout
//! with an unpopulated store still runs. [`crate::asset_objects`] holds the
//! measurement and the reason this is only eight names in the whole game;
//! [`PanoramaFaces::from_object_store`] is how a caller tells the two apart,
//! because a flat-grey cubemap renders perfectly and is not the game.
//!
//! ## What this deliberately does not do
//!
//! **`panorama_overlay.png` is not drawn — and this is now measured on the real
//! object, not on the jar's copy.** Vanilla blits it over the panorama at texture
//! size 16×128, tiled to the full screen (`Panorama.java:31`). The asset-store
//! object (hash `9dd32387…`, 86 bytes) decodes to **1×1 RGBA, one distinct value,
//! `(255, 255, 255, 0)`, alpha extrema `(0, 0)`** — confirmed by hexdump: the
//! IHDR is `1×1`, colour type 6, and the whole IDAT is `ff ff ff 00`. The 86 vs
//! the jar stub's 68 bytes is a `gAMA` chunk, not content. Blitting a fully
//! transparent texture cannot change a pixel, so drawing it would be provable
//! dead code. Adding it if a future pack makes it real means one more textured
//! quad on the existing menu-sprite pipeline, not another pass.
//!
//! **There is no blur.** `Screen.extractBlurredBackground` blurs whatever is
//! behind a menu when `menuBackgroundBlurriness >= 1`; at the option's 0 this is
//! exactly vanilla, above it vanilla reads calmer. Same gap `OVERLAY_BG` already
//! documents in `menu/render.rs`.
//!
//! ## How to change it
//!
//! Every vanilla number is a `pub const` here with its source line, and the
//! matrices are built by [`view_projection`], which is a free function precisely
//! so a test can assert against it without a GPU. The riskiest edit is
//! [`FACE_SUFFIXES`]: reorder it and the sky still renders, just wrong.
//! [`assemble`]'s test pins both the order and the flip against synthetic faces
//! whose rows encode their own index.

use std::time::Instant;

use glam::Mat4;
use lodestone_assets::Image;

/// In-pack path prefix of the cubemap — `GuiRenderer.java:89`'s
/// `Identifier.withDefaultNamespace("textures/gui/title/background/panorama")`,
/// which `CubeMapTexture` then suffixes.
pub const PANORAMA_BASE: &str = "assets/minecraft/textures/gui/title/background/panorama";

/// The per-face suffixes **in cubemap layer order** — `CubeMapTexture.SUFFIXES`
/// (`CubeMapTexture.java:14`).
///
/// Layer order for a cubemap is `+X, -X, +Y, -Y, +Z, -Z`, so this reads: `+X` is
/// `panorama_1`, `-X` is `panorama_3`, `+Y` (up) is `panorama_5`, `-Y` (down) is
/// `panorama_4`, `+Z` is `panorama_0`, `-Z` is `panorama_2`. It is **not** `0..5`,
/// and a wrong order yields a plausible-looking scrambled sky rather than an
/// obvious failure.
pub const FACE_SUFFIXES: [&str; 6] = ["_1", "_3", "_5", "_4", "_0", "_2"];

/// The overlay vanilla blits over the panorama. Not drawn — see the module docs
/// for the measurement that makes it a provable no-op in 26.2.
pub const PANORAMA_OVERLAY_PATH: &str =
    "assets/minecraft/textures/gui/title/background/panorama_overlay.png";

/// Vertical field of view, in degrees — `CubeMap.PROJECTION_FOV`
/// (`CubeMap.java:31`).
pub const FOV_DEGREES: f32 = 85.0;
/// Near plane — `CubeMap.PROJECTION_Z_NEAR` (`CubeMap.java:29`).
pub const Z_NEAR: f32 = 0.05;
/// Far plane — `CubeMap.PROJECTION_Z_FAR` (`CubeMap.java:30`).
pub const Z_FAR: f32 = 10.0;
/// Downward tilt applied before the yaw — `GuiRenderer.java:120` passes `10.0F`
/// as `CubeMap.render`'s `rotXInDegrees`.
pub const TILT_DEGREES: f32 = 10.0;
/// Degrees of yaw per tick at speed 1.0 — `Panorama.java:27`'s `delta * 0.1F`.
pub const SPIN_DEGREES_PER_TICK: f32 = 0.1;
/// `panoramaSpeed`'s default (`Options.java:313-320`). The option itself is not
/// wired into this repo's settings screen yet; [`PanoramaRenderer::set_speed`] is
/// the seam for when it is.
pub const DEFAULT_SPIN_SPEED: f32 = 1.0;
/// Ticks per real second, for turning `Instant` deltas into vanilla's
/// `getRealtimeDeltaTicks()`.
pub const TICKS_PER_SECOND: f32 = 20.0;

/// How much `textures/gui/menu_background.png` darkens whatever is behind it.
///
/// The file is 16×16 and **every pixel is grey 0, alpha 64** (measured out of
/// 26.2's `client.jar`; `inworld_menu_background.png` is byte-identical), so
/// vanilla's tiled 32 px blit is a flat 25 %-black wash and compositing it is a
/// multiply by `1 - 64/255`. Vanilla applies it to every out-of-world screen
/// **except** the title screen, whose `extractBackground` override is empty
/// (`TitleScreen.java:330`).
pub const MENU_BACKGROUND_DIM: f32 = 64.0 / 255.0;

/// How much to darken the panorama on a given screen.
///
/// Vanilla's `Screen.extractBackground` draws, out of world: the panorama, then
/// the blur, then `menu_background.png` — so every out-of-world screen wears the
/// [`MENU_BACKGROUND_DIM`] wash. `TitleScreen` is the exception, and it is an
/// *override* rather than a special case in the base class: `TitleScreen`'s
/// `extractBackground` is empty (`TitleScreen.java:330`) and it draws the
/// panorama itself from `extractRenderState` (`:307`), so the title screen gets
/// the raw cubemap with nothing over it.
///
/// This is the constant most likely to be "corrected" by someone comparing the
/// title screen to a screenshot of the server list and concluding they should
/// match. They should not.
#[must_use]
pub fn dim_for_screen(is_title_screen: bool) -> f32 {
    if is_title_screen {
        0.0
    } else {
        MENU_BACKGROUND_DIM
    }
}

/// The unit cube, as vanilla lists it: six quads of four corners, verbatim from
/// `CubeMap.initializeVertices` (`CubeMap.java:80-103`), in that order.
///
/// Quads rather than triangles so the transcription can be diffed against the
/// Java line-for-line; [`cube_vertices`] expands each into vanilla's own two
/// triangles.
pub const CUBE_QUADS: [[[f32; 3]; 4]; 6] = [
    // +Z: the face you are looking at when spin = 0.
    [
        [-1.0, -1.0, 1.0],
        [-1.0, 1.0, 1.0],
        [1.0, 1.0, 1.0],
        [1.0, -1.0, 1.0],
    ],
    // +X
    [
        [1.0, -1.0, 1.0],
        [1.0, 1.0, 1.0],
        [1.0, 1.0, -1.0],
        [1.0, -1.0, -1.0],
    ],
    // -Z
    [
        [1.0, -1.0, -1.0],
        [1.0, 1.0, -1.0],
        [-1.0, 1.0, -1.0],
        [-1.0, -1.0, -1.0],
    ],
    // -X
    [
        [-1.0, -1.0, -1.0],
        [-1.0, 1.0, -1.0],
        [-1.0, 1.0, 1.0],
        [-1.0, -1.0, 1.0],
    ],
    // -Y (down)
    [
        [-1.0, -1.0, -1.0],
        [-1.0, -1.0, 1.0],
        [1.0, -1.0, 1.0],
        [1.0, -1.0, -1.0],
    ],
    // +Y (up)
    [
        [-1.0, 1.0, 1.0],
        [-1.0, 1.0, -1.0],
        [1.0, 1.0, -1.0],
        [1.0, 1.0, 1.0],
    ],
];

/// Number of vertices [`cube_vertices`] emits: 6 quads × 2 triangles × 3.
pub const CUBE_VERTEX_COUNT: usize = 36;

/// The cube as a triangle list, using vanilla's own quad→triangle split.
///
/// `RenderSystem.getSequentialBuffer(PrimitiveTopology.QUADS)` emits
/// `0, 1, 2, 2, 3, 0` per quad, and `CubeMap.render` draws 36 indices over the 24
/// vertices. Expanding to 36 vertices here costs 144 bytes and removes the index
/// buffer entirely.
#[must_use]
pub fn cube_vertices() -> Vec<f32> {
    let mut out = Vec::with_capacity(CUBE_VERTEX_COUNT * 3);
    for quad in CUBE_QUADS {
        for i in [0usize, 1, 2, 2, 3, 0] {
            out.extend_from_slice(&quad[i]);
        }
    }
    out
}

/// Vanilla's `Mth.wrapDegrees(float)` (`Mth.java:216-224`): reduce to
/// `[-180, 180)`.
///
/// Ported rather than replaced with a `rem_euclid` because the accumulator is
/// *observable* — [`PanoramaRenderer::spin_degrees`] is what a test asserts on,
/// and a different-but-equivalent range would make those numbers disagree with
/// vanilla's for no reason.
#[must_use]
pub fn wrap_degrees(angle: f32) -> f32 {
    let mut normalized = angle % 360.0;
    if normalized >= 180.0 {
        normalized -= 360.0;
    }
    if normalized < -180.0 {
        normalized += 360.0;
    }
    normalized
}

/// The combined projection × model-view for a given canvas and spin.
///
/// `CubeMap.render` sets the model-view stack to `rotationX(PI)`, then
/// `rotateX(10°)`, then `rotateY(rotY)`. JOML's `rotationX` *sets* the matrix
/// (it is not a multiply) and `rotateX`/`rotateY` post-multiply, in the same
/// column-major, column-vector convention `glam` uses — so this is a direct
/// transcription, not a re-derivation.
///
/// `spin_degrees` is the value handed to `CubeMap.render`, i.e. already negated
/// relative to [`PanoramaRenderer::spin_degrees`] (`Panorama.java:30`).
#[must_use]
pub fn view_projection(width: u32, height: u32, spin_degrees: f32) -> Mat4 {
    let aspect = if height == 0 {
        1.0
    } else {
        width as f32 / height as f32
    };
    // glam's `perspective_rh` is [0,1] depth where JOML's may be [-1,1]; the
    // difference is confined to the z row and this pass has no depth attachment
    // at all, so it cannot matter here. x/y are identical between the two.
    let projection = Mat4::perspective_rh(FOV_DEGREES.to_radians(), aspect, Z_NEAR, Z_FAR);
    let model_view = Mat4::from_rotation_x(std::f32::consts::PI)
        * Mat4::from_rotation_x(TILT_DEGREES.to_radians())
        * Mat4::from_rotation_y(spin_degrees.to_radians());
    projection * model_view
}

/// The six panorama faces, decoded, flipped and stacked into cubemap layer order
/// — ready to upload as a single `depth_or_array_layers = 6` texture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PanoramaFaces {
    /// Side length of one face. Cube textures must be square.
    pub size: u32,
    /// `size * size * 6 * 4` bytes: layer 0 first, each layer a top-down RGBA8
    /// image of the (already vertically flipped) source face.
    ///
    /// At vanilla's real 1024×1024 this is **25 MB**, held only until
    /// [`PanoramaRenderer::new`] has uploaded it (`ensure_panorama` drops the
    /// `Arc` at the end of its block). Do not cache it.
    pub rgba: Vec<u8>,
    /// How many of the six faces came from the launcher's asset-object store
    /// rather than from `client.jar`.
    ///
    /// **6 is the real art; 0 means every face is a jar stub.** This is not
    /// diagnostic decoration — the jar ships 1×1 grey stubs for all six faces
    /// (see [`crate::asset_objects`]), so a panorama built entirely from the jar
    /// renders a flat colour and *looks* like a working-but-boring sky. A gate
    /// that means to measure the real cubemap must assert this is 6.
    ///
    /// [`assemble`] leaves this 0 because it does not know where its `Image`s came
    /// from; [`load`] sets it.
    pub from_object_store: usize,
}

impl PanoramaFaces {
    /// Bytes per layer.
    #[must_use]
    pub fn layer_bytes(&self) -> usize {
        (self.size as usize) * (self.size as usize) * 4
    }

    /// Whether all six faces came from the object store, i.e. whether this is
    /// vanilla's real panorama rather than the jar's stubs.
    #[must_use]
    pub fn is_real_art(&self) -> bool {
        self.from_object_store == 6
    }
}

/// Stack six already-decoded faces into [`PanoramaFaces`].
///
/// `faces` must already be in [`FACE_SUFFIXES`] order — that is, `faces[0]` is
/// `panorama_1`. Each is flipped vertically on the way in, mirroring
/// `CubeMapTexture.loadContents`'s `copyRect(…, swapY = true)`.
///
/// # Errors
///
/// Returns a message naming the offender when the faces disagree in size, when a
/// face is not square (a cube texture cannot be), or when a face's byte count
/// does not match its declared dimensions.
pub fn assemble(faces: &[Image; 6]) -> Result<PanoramaFaces, String> {
    let size = faces[0].width;
    if size == 0 {
        return Err(format!(
            "panorama face {} is zero-sized",
            FACE_SUFFIXES[0]
        ));
    }
    if faces[0].height != size {
        return Err(format!(
            "panorama face {} is {}x{}, but a cubemap face must be square",
            FACE_SUFFIXES[0], faces[0].width, faces[0].height
        ));
    }
    let stride = (size as usize) * 4;
    let layer = stride * (size as usize);
    let mut rgba = vec![0u8; layer * 6];

    for (index, face) in faces.iter().enumerate() {
        if face.width != size || face.height != size {
            return Err(format!(
                "panorama face {} is {}x{} but face {} is {size}x{size}; \
                 vanilla requires every side to match (CubeMapTexture.java:32-46)",
                FACE_SUFFIXES[index], face.width, face.height, FACE_SUFFIXES[0]
            ));
        }
        if face.rgba.len() < layer {
            return Err(format!(
                "panorama face {} declares {size}x{size} ({layer} bytes) but carries {}",
                FACE_SUFFIXES[index],
                face.rgba.len()
            ));
        }
        let base = index * layer;
        for y in 0..size as usize {
            // `swapY`: source row `y` lands in target row `size - 1 - y`.
            let dst_row = size as usize - 1 - y;
            let src = &face.rgba[y * stride..y * stride + stride];
            rgba[base + dst_row * stride..base + dst_row * stride + stride].copy_from_slice(src);
        }
    }

    Ok(PanoramaFaces {
        size,
        rgba,
        from_object_store: 0,
    })
}

/// The asset-index key for face `layer`, i.e. the jar path minus its `assets/`
/// prefix. See [`crate::asset_objects`] on why the two differ.
#[must_use]
pub fn face_index_key(layer: usize) -> String {
    let path = face_jar_path(layer);
    path.strip_prefix("assets/")
        .unwrap_or(&path)
        .to_string()
}

/// The `client.jar` path for face `layer`.
#[must_use]
pub fn face_jar_path(layer: usize) -> String {
    format!("{PANORAMA_BASE}{}.png", FACE_SUFFIXES[layer % 6])
}

/// Read and assemble the cubemap, **preferring the asset-object store over the
/// jar** for every face.
///
/// That preference is the whole point of this function rather than a detail.
/// `client.jar` ships a 69-byte 1×1 grey stub for all six faces and the real
/// 1024×1024 art is delivered through the asset index; reading the jar gives you
/// a flat colour that renders perfectly and is not the game. See
/// [`crate::asset_objects`] for the measurement and the eight-name scope.
///
/// The jar is still the fallback, per face, so a checkout with no populated
/// object store keeps a working (if flat) title screen instead of failing. What
/// came from where is reported in [`PanoramaFaces::from_object_store`] — a caller
/// that cares must read it, because the two are visually indistinguishable from
/// "it drew something".
///
/// # Errors
///
/// Returns a message naming the face that is in neither source or fails to
/// decode, or [`assemble`]'s error.
pub fn load(
    manager: &lodestone_assets::ResourceManager,
    objects: Option<&crate::asset_objects::AssetObjectStore>,
) -> Result<PanoramaFaces, String> {
    let mut decoded = Vec::with_capacity(6);
    let mut from_store = 0usize;
    for layer in 0..6 {
        let jar_path = face_jar_path(layer);
        let key = face_index_key(layer);
        // Object store first; the jar entry is a stub whenever both exist.
        let (bytes, whence) = match objects.and_then(|store| store.object_bytes(&key)) {
            Some(bytes) => {
                from_store += 1;
                (bytes, "object store")
            }
            None => match manager.read(&jar_path) {
                Some(bytes) => (bytes, "client.jar (stub)"),
                None => {
                    return Err(format!(
                        "panorama face {} is in neither the asset-object store \
                         (key {key}) nor client.jar ({jar_path})",
                        FACE_SUFFIXES[layer]
                    ));
                }
            },
        };
        let image = Image::decode_png(&bytes)
            .map_err(|e| format!("decode panorama face {} from {whence}: {e}", FACE_SUFFIXES[layer]))?;
        decoded.push(image);
    }
    let faces: [Image; 6] = decoded
        .try_into()
        .map_err(|_| "expected exactly six panorama faces".to_string())?;
    let mut stacked = assemble(&faces)?;
    stacked.from_object_store = from_store;
    Ok(stacked)
}

/// GPU renderer for the panorama: cube texture, its own pipeline and bind group,
/// a static 36-vertex buffer, and the spin accumulator.
///
/// Drawn **into the menu's existing render pass**, first, before any menu quad —
/// so it needs no pass of its own and no change to `app.rs`'s frame loop. It has
/// no depth attachment and `cull_mode: None`: from a point inside a convex box
/// every ray exits through exactly one face, and everything on the near side of
/// the camera is removed by the near plane, so no pixel is covered twice and
/// there is nothing for a depth test or a winding rule to arbitrate. That is a
/// deliberate divergence from vanilla's `RenderPipelines.PANORAMA`, which leaves
/// the builder's `withCull(true)` default on — it produces the same image without
/// depending on a screen-space winding polarity, which this repo has got backwards
/// before (see `CLAUDE.md`, "the GUI winding invariant is negative").
#[derive(Debug)]
pub struct PanoramaRenderer {
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    uniform: wgpu::Buffer,
    verts: wgpu::Buffer,
    /// Kept alive because the bind group's view is derived from it.
    #[allow(dead_code)]
    texture: wgpu::Texture,
    /// Side length of one face, for diagnostics and gates.
    size: u32,
    /// [`PanoramaFaces::from_object_store`], carried through so a gate can assert
    /// it is bound to the real art rather than the jar's flat stubs.
    from_object_store: usize,
    /// The accumulator, in vanilla's sign. Negated on the way to the matrix.
    spin: f32,
    /// `panoramaSpeed`.
    speed: f32,
    /// When [`Self::advance`] last ran. `None` until the first frame, whose delta
    /// is therefore zero rather than "however long the process has been up".
    last: Option<Instant>,
}

/// The uniform block `panorama.wgsl` declares: a matrix plus a padded scalar.
#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct PanoramaUniform {
    view_proj: [f32; 16],
    /// `.x` is the dim factor; the rest is padding to a 16-byte boundary.
    dim: [f32; 4],
}

impl PanoramaRenderer {
    /// Upload `faces` and build the pipeline for a target of `color_format`.
    #[must_use]
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        color_format: wgpu::TextureFormat,
        faces: &PanoramaFaces,
    ) -> Self {
        // `assemble` rejects a zero-sized or short face set, but `PanoramaFaces`
        // is publicly constructible, so clamp rather than index out of bounds: a
        // 1×1 black cubemap is a visible-but-harmless result where a panic in a
        // draw path is not.
        let size = faces.size.max(1);
        let needed = (size as usize) * (size as usize) * 4 * 6;
        let fallback = vec![0u8; needed];
        let upload: &[u8] = if faces.rgba.len() >= needed {
            &faces.rgba[..needed]
        } else {
            tracing::warn!(
                target: "assets",
                have = faces.rgba.len(),
                need = needed,
                "panorama cubemap is short; uploading black"
            );
            &fallback
        };
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("menu-panorama-cubemap"),
            size: wgpu::Extent3d {
                width: size,
                height: size,
                depth_or_array_layers: 6,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            // sRGB, matching `GpuAtlas`: every other texture on the menu pass
            // samples through an `*UnormSrgb` view, and the live swapchain is
            // sRGB too, so the sample -> write round trip is byte-exact.
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            upload,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(size * 4),
                rows_per_image: Some(size),
            },
            wgpu::Extent3d {
                width: size,
                height: size,
                depth_or_array_layers: 6,
            },
        );
        let view = texture.create_view(&wgpu::TextureViewDescriptor {
            label: Some("menu-panorama-cubemap-view"),
            dimension: Some(wgpu::TextureViewDimension::Cube),
            array_layer_count: Some(6),
            ..Default::default()
        });
        // blur = true -> Linear, unlike the Nearest every other menu texture
        // uses. Address modes are irrelevant for a cubemap (the coordinate is a
        // direction, and face selection is not an address wrap), so vanilla's
        // clamp = false is not reproduced as Repeat: ClampToEdge is what keeps
        // the filter from reaching past a face edge.
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("menu-panorama-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("menu-panorama-shader"),
            source: wgpu::ShaderSource::Wgsl(PANORAMA_WGSL.into()),
        });
        let bind_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("menu-panorama-bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::Cube,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        // One bind group, well under wgpu's `max_bind_groups` floor of 4 — see
        // `CLAUDE.md` on the model shader, which spends all four.
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("menu-panorama-layout"),
            bind_group_layouts: &[Some(&bind_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("menu-panorama-pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: 3 * 4,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &[wgpu::VertexAttribute {
                        format: wgpu::VertexFormat::Float32x3,
                        offset: 0,
                        shader_location: 0,
                    }],
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: color_format,
                    // Opaque: the panorama replaces the backdrop rather than
                    // tinting it, so there is nothing to blend with.
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
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let verts_data = cube_vertices();
        let verts = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("menu-panorama-verts"),
            size: (verts_data.len() * 4) as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&verts, 0, bytemuck::cast_slice(&verts_data));

        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("menu-panorama-uniform"),
            size: std::mem::size_of::<PanoramaUniform>() as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("menu-panorama-bind"),
            layout: &bind_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });

        Self {
            pipeline,
            bind_group,
            uniform,
            verts,
            texture,
            size,
            from_object_store: faces.from_object_store,
            spin: 0.0,
            speed: DEFAULT_SPIN_SPEED,
            last: None,
        }
    }

    /// Side length of one cubemap face, as loaded.
    #[must_use]
    pub fn face_size(&self) -> u32 {
        self.size
    }

    /// How many of the six bound faces came from the asset-object store —
    /// [`PanoramaFaces::from_object_store`], carried through.
    ///
    /// A gate measuring the real sky must assert this is 6: with the jar's stubs
    /// the cubemap is a single flat colour, and every "the panorama drew" test
    /// still passes.
    #[must_use]
    pub fn faces_from_object_store(&self) -> usize {
        self.from_object_store
    }

    /// The spin accumulator, in vanilla's sign and range (`[-180, 180)`).
    #[must_use]
    pub fn spin_degrees(&self) -> f32 {
        self.spin
    }

    /// Override `panoramaSpeed` (1.0 is vanilla's default; 0.0 holds the spin,
    /// which is what `Panorama.holdSpin` achieves).
    pub fn set_speed(&mut self, speed: f32) {
        self.speed = speed;
    }

    /// Advance the spin by the wall-clock time since the previous call.
    ///
    /// The first call establishes the baseline and advances nothing.
    pub fn advance(&mut self, now: Instant) {
        let dt = match self.last {
            Some(previous) => now.saturating_duration_since(previous).as_secs_f32(),
            None => 0.0,
        };
        self.last = Some(now);
        self.advance_seconds(dt);
    }

    /// Advance the spin by `seconds` of real time. Split out from
    /// [`Self::advance`] so the accumulation is testable without sleeping.
    pub fn advance_seconds(&mut self, seconds: f32) {
        let delta_ticks = seconds * TICKS_PER_SECOND;
        self.spin = wrap_degrees(self.spin + delta_ticks * self.speed * SPIN_DEGREES_PER_TICK);
    }

    /// Write the uniform for this frame. Must run before the pass begins, since
    /// a queue write cannot be recorded into an open render pass.
    ///
    /// `dim` is [`MENU_BACKGROUND_DIM`] on the out-of-world screens vanilla
    /// composites `menu_background.png` over, and `0.0` on the title screen,
    /// whose `extractBackground` override is empty.
    pub fn prepare(&self, queue: &wgpu::Queue, width: u32, height: u32, dim: f32) {
        // `-self.spin`: `Panorama.java:30` hands `CubeMap.render` the negated
        // accumulator, and `view_projection` takes the value at that call site.
        let vp = view_projection(width, height, -self.spin);
        let data = PanoramaUniform {
            view_proj: vp.to_cols_array(),
            dim: [dim, 0.0, 0.0, 0.0],
        };
        queue.write_buffer(&self.uniform, 0, bytemuck::bytes_of(&data));
    }

    /// Draw the cube into an already-open pass. Pair with [`Self::prepare`].
    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>) {
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.set_vertex_buffer(0, self.verts.slice(..));
        pass.draw(0..CUBE_VERTEX_COUNT as u32, 0..1);
    }
}

const PANORAMA_WGSL: &str = include_str!("../shaders/panorama.wgsl");

#[cfg(test)]
mod tests {
    use super::*;

    /// A face whose every pixel encodes its row: red = row index, green = the
    /// face's own index. That makes both the stacking order and the flip visible
    /// in the output bytes.
    fn marked_face(index: u8, size: u32) -> Image {
        let mut rgba = Vec::with_capacity((size * size * 4) as usize);
        for y in 0..size {
            for _ in 0..size {
                rgba.extend_from_slice(&[y as u8, index, 0, 255]);
            }
        }
        Image {
            width: size,
            height: size,
            rgba,
        }
    }

    fn six_marked_faces(size: u32) -> [Image; 6] {
        [
            marked_face(0, size),
            marked_face(1, size),
            marked_face(2, size),
            marked_face(3, size),
            marked_face(4, size),
            marked_face(5, size),
        ]
    }

    #[test]
    fn the_face_order_is_vanillas_suffix_table_not_zero_through_five() {
        // `CubeMapTexture.SUFFIXES`, transcribed. The point of the assertion is
        // that a "tidying" edit to `FACE_SUFFIXES` fails here rather than
        // shipping a scrambled sky.
        assert_eq!(FACE_SUFFIXES, ["_1", "_3", "_5", "_4", "_0", "_2"]);
        // And the naive order a reader would guess must *not* be what we ship.
        assert_ne!(FACE_SUFFIXES, ["_0", "_1", "_2", "_3", "_4", "_5"]);
    }

    #[test]
    fn the_index_key_drops_the_assets_prefix_the_jar_path_keeps() {
        // The asset index names objects *without* the leading `assets/`; the jar
        // names entries *with* it. Using the jar path as an index key resolves
        // nothing, silently, and you get the stub — which is the exact mistake
        // that made the panorama a flat grey for a commit. `audio.rs` documents
        // the same trap for sounds.
        let jar = face_jar_path(4);
        let key = face_index_key(4);
        assert!(
            jar.starts_with("assets/"),
            "the jar path must be pack-absolute, got {jar}"
        );
        assert!(
            !key.starts_with("assets/"),
            "an asset-index key must not carry the assets/ prefix, got {key}"
        );
        assert_eq!(key, jar.strip_prefix("assets/").expect("checked above"));
        // Layer 4 is +Z, which vanilla's suffix table fills from `panorama_0`.
        assert_eq!(
            key,
            "minecraft/textures/gui/title/background/panorama_0.png",
            "layer 4 must be panorama_0 — see FACE_SUFFIXES"
        );
    }

    #[test]
    fn every_layer_has_a_distinct_key_across_all_six() {
        // A modulo or index slip would give two layers the same face, which
        // renders as a plausible sky with a duplicated wall.
        let keys: Vec<String> = (0..6).map(face_index_key).collect();
        let mut unique = keys.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), 6, "duplicate panorama face keys: {keys:?}");
    }

    #[test]
    fn assemble_reports_no_object_store_faces_because_it_cannot_know() {
        // `assemble` takes decoded images and has no idea where they came from;
        // `load` is what sets the count. If this ever reported 6 by default, a
        // gate asserting "real art" would pass against the jar's stubs.
        let stacked = assemble(&six_marked_faces(2)).expect("assemble");
        assert_eq!(stacked.from_object_store, 0);
        assert!(!stacked.is_real_art());
    }

    #[test]
    fn assemble_stacks_faces_in_order_and_flips_each_vertically() {
        let size = 4;
        let faces = six_marked_faces(size);
        let stacked = assemble(&faces).expect("six equal square faces assemble");
        assert_eq!(stacked.size, size);
        assert_eq!(stacked.rgba.len(), stacked.layer_bytes() * 6);

        let stride = (size * 4) as usize;
        for layer in 0..6usize {
            let base = layer * stacked.layer_bytes();
            // Green carries the *source* face index, so layer n must hold face n:
            // this is the stacking order.
            assert_eq!(
                stacked.rgba[base + 1], layer as u8,
                "layer {layer} does not hold source face {layer}"
            );
            // Red carries the source row. `swapY` puts source row `size-1` at
            // target row 0, so the first row of the layer must be `size - 1`.
            assert_eq!(
                stacked.rgba[base], (size - 1) as u8,
                "layer {layer} row 0 is source row {} — the vertical flip is missing",
                stacked.rgba[base]
            );
            // And the last row must be source row 0.
            let last = base + stride * (size as usize - 1);
            assert_eq!(
                stacked.rgba[last], 0,
                "layer {layer} last row is source row {}, expected 0",
                stacked.rgba[last]
            );
        }
    }

    /// Control for the test above: without the flip, row 0 would be source row 0.
    /// This asserts the detector can tell the two apart, so a green
    /// `assemble_stacks_faces_in_order_and_flips_each_vertically` means something.
    #[test]
    fn an_unflipped_stack_would_fail_the_flip_assertion() {
        let size = 4;
        let face = marked_face(0, size);
        // The unflipped copy the port could easily have written instead.
        let unflipped_first_row_red = face.rgba[0];
        let stacked = assemble(&six_marked_faces(size)).expect("assemble");
        assert_eq!(unflipped_first_row_red, 0, "source row 0 is marked 0");
        assert_ne!(
            stacked.rgba[0], unflipped_first_row_red,
            "the flip must actually change which row lands first"
        );
    }

    #[test]
    fn a_non_square_face_is_rejected_rather_than_uploaded() {
        let mut faces = six_marked_faces(4);
        faces[0] = Image {
            width: 4,
            height: 2,
            rgba: vec![0; 4 * 2 * 4],
        };
        let err = assemble(&faces).expect_err("a cubemap face must be square");
        assert!(err.contains("square"), "unexpected message: {err}");
    }

    #[test]
    fn mismatched_face_sizes_name_the_offending_suffix() {
        let mut faces = six_marked_faces(4);
        faces[3] = marked_face(3, 8);
        let err = assemble(&faces).expect_err("faces must all match");
        assert!(
            err.contains(FACE_SUFFIXES[3]),
            "the message must name the offender, got: {err}"
        );
    }

    #[test]
    fn the_cube_is_thirty_six_vertices_of_vanillas_twenty_four_corners() {
        let verts = cube_vertices();
        assert_eq!(verts.len(), CUBE_VERTEX_COUNT * 3);
        // Vanilla's first quad, expanded as `0,1,2, 2,3,0`.
        let first = &verts[0..18];
        assert_eq!(
            first,
            &[
                -1.0, -1.0, 1.0, // 0
                -1.0, 1.0, 1.0, // 1
                1.0, 1.0, 1.0, // 2
                1.0, 1.0, 1.0, // 2
                1.0, -1.0, 1.0, // 3
                -1.0, -1.0, 1.0, // 0
            ]
        );
        // Every corner is a unit-cube corner: no vertex may drift off ±1.
        for v in &verts {
            assert!(
                (v.abs() - 1.0).abs() < 1e-6,
                "a panorama cube corner is {v}, not ±1 — the cube is not a unit cube"
            );
        }
    }

    #[test]
    fn wrap_degrees_matches_vanillas_range() {
        assert!((wrap_degrees(0.0) - 0.0).abs() < 1e-6);
        assert!((wrap_degrees(179.0) - 179.0).abs() < 1e-6);
        // 180 is *not* in range: vanilla's `>= 180` subtracts a turn.
        assert!((wrap_degrees(180.0) - -180.0).abs() < 1e-6);
        assert!((wrap_degrees(370.0) - 10.0).abs() < 1e-6);
        assert!((wrap_degrees(-190.0) - 170.0).abs() < 1e-6);
    }

    #[test]
    fn the_spin_rate_is_two_degrees_per_second_at_vanillas_default_speed() {
        // 0.1 deg/tick x 20 ticks/s x speed 1.0. Predicted from the constants,
        // not read off the implementation: a full revolution takes three minutes,
        // so "it looks static" is expected, not a bug.
        let expected_per_second = SPIN_DEGREES_PER_TICK * TICKS_PER_SECOND * DEFAULT_SPIN_SPEED;
        assert!((expected_per_second - 2.0).abs() < 1e-6);

        let mut spin = 0.0f32;
        for _ in 0..10 {
            spin = wrap_degrees(spin + 1.0 * TICKS_PER_SECOND * DEFAULT_SPIN_SPEED * SPIN_DEGREES_PER_TICK);
        }
        assert!(
            (spin - 20.0).abs() < 1e-4,
            "ten seconds of spin is {spin} degrees, expected 20"
        );
    }

    #[test]
    fn the_projection_puts_the_plus_z_face_in_front_of_the_camera() {
        // The one geometric claim worth pinning: `rotationX(PI)` is what turns
        // the cube's +Z face (vanilla's first quad) into the thing you see, by
        // mapping it to negative view-space z, which is where a right-handed
        // camera looks. Checked through the real matrix rather than asserted as
        // a polarity.
        let vp = view_projection(1920, 1080, 0.0);
        let centre_of_plus_z = vp * glam::Vec4::new(0.0, 0.0, 1.0, 1.0);
        assert!(
            centre_of_plus_z.w > 0.0,
            "the +Z face centre is behind the camera (w = {}) — the 180 degree \
             X rotation is missing or doubled",
            centre_of_plus_z.w
        );
        // And it lands at the middle of the screen.
        let ndc_x = centre_of_plus_z.x / centre_of_plus_z.w;
        let ndc_y = centre_of_plus_z.y / centre_of_plus_z.w;
        assert!(
            ndc_x.abs() < 1e-5,
            "the +Z face centre is off-axis horizontally at spin 0: x = {ndc_x}"
        );
        // 10 degrees of tilt moves it off the vertical centre, but not far.
        assert!(
            ndc_y.abs() < 0.5,
            "the +Z face centre is {ndc_y} off vertical centre; 10 degrees of \
             tilt cannot account for that"
        );
        // The -Z face centre must be behind the camera, which is the other half
        // of the same claim.
        let centre_of_minus_z = vp * glam::Vec4::new(0.0, 0.0, -1.0, 1.0);
        assert!(
            centre_of_minus_z.w < 0.0,
            "the -Z face centre is in front of the camera (w = {}) — the cube is \
             inside out",
            centre_of_minus_z.w
        );
    }

    #[test]
    fn only_the_title_screen_escapes_the_menu_background_wash() {
        assert!(
            (dim_for_screen(true) - 0.0).abs() < 1e-9,
            "the title screen's `extractBackground` override is empty, so nothing \
             may be composited over its panorama"
        );
        assert!(
            (dim_for_screen(false) - MENU_BACKGROUND_DIM).abs() < 1e-9,
            "every other out-of-world screen wears `menu_background.png`"
        );
        // The two must actually differ, or this function is decorative.
        assert!((dim_for_screen(true) - dim_for_screen(false)).abs() > 0.2);
    }

    #[test]
    fn the_menu_background_dim_is_the_measured_alpha_of_vanillas_texture() {
        // `menu_background.png` decoded out of 26.2's client.jar: 16x16, one
        // distinct RGBA, (0, 0, 0, 64). So the composite is a multiply by
        // 1 - 64/255 = 0.749.
        assert!((MENU_BACKGROUND_DIM - 64.0 / 255.0).abs() < 1e-9);
        assert!((1.0 - MENU_BACKGROUND_DIM - 0.749_019_6).abs() < 1e-6);
    }
}
