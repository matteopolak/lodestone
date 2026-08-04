
// The **shared** half of the packed path's group-0 uniform (binding 0):
// view-projection plus this frame's fog, identical for every section drawn this
// frame. Byte-for-byte `model.wgsl`'s own `Camera` and the same 112-byte
// `ModelSharedCameraUniform` behind it — one type, one layout, so the packed and
// model paths structurally cannot disagree about fog or the clock.
//
// This used to be `{ view_proj, section_origin }` written **per section, per
// frame** — the exact shape issue #75 profiled at ~4000 `queue.write_buffer`
// calls/frame and 52.9% of main-thread CPU on the model path. #75 fixed the
// model path and deliberately left this one (issue #76): the origin is constant
// for a section's life, so it moved to binding 1 behind a dynamic offset and is
// written once, at upload. See `docs/section-camera-uniform.md`.
struct Camera {
    view_proj: mat4x4<f32>,
    fog_eye: vec4<f32>,
    fog_color_start: vec4<f32>,
    fog_end_enabled: vec4<f32>,
};

// A section's world-space origin, bound at group 0 binding 1 with a dynamic
// offset: one physically resident arena of these serves every packed section, so
// re-aiming the camera (binding 0, above) never touches this one. Identical to
// `model.wgsl`'s `Origin`, and backed by the same `SectionOriginUniform`.
struct Origin {
    section_origin: vec4<f32>,
};

@group(0) @binding(0) var<uniform> camera: Camera;
@group(0) @binding(1) var<uniform> origin: Origin;
@group(1) @binding(0) var atlas_tex: texture_2d<f32>;
@group(1) @binding(1) var atlas_smp: sampler;
@group(1) @binding(2) var<storage, read> sprite_uv: array<vec4<f32>>;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    // Tile coordinate, running 0..w / 0..h across a greedy-merged quad (0..1 for
    // a single-tile reference quad). Interpolated, then wrapped per fragment.
    @location(0) tile: vec2<f32>,
    @location(1) shade: f32,
    // The sprite's atlas sub-rect (min.xy, size.zw). Constant across the quad, so
    // interpolate it flat to avoid drift and let the fragment stage do the wrap.
    @location(2) @interpolate(flat) rect: vec4<f32>,
};

@vertex
fn vs_main(@location(0) packed: vec3<u32>) -> VsOut {
    let w0 = packed.x;
    let w1 = packed.y;
    let w2 = packed.z;

    let x = f32(w0 & 63u);
    let y = f32((w0 >> 6u) & 63u);
    let z = f32((w0 >> 12u) & 63u);

    let sprite = w1 & 2047u;
    let tu = f32((w1 >> 11u) & 31u);
    let tv = f32((w1 >> 16u) & 31u);

    // Smooth per-corner brightness bytes (0..255).
    let ao = f32(w2 & 255u) / 255.0;
    let sky = f32((w2 >> 8u) & 255u) / 255.0;
    let block = f32((w2 >> 16u) & 255u) / 255.0;

    let world = vec3<f32>(x, y, z) + origin.section_origin.xyz;

    // AO already carries vanilla's 0.4..1.0 range; light lifts a dark floor so
    // unlit faces are dim rather than black.
    let light_term = 0.2 + 0.8 * max(sky, block);

    var out: VsOut;
    out.clip = camera.view_proj * vec4<f32>(world, 1.0);
    out.tile = vec2<f32>(tu, tv);
    out.shade = ao * light_term;
    out.rect = sprite_uv[sprite];
    return out;
}

// sRGB transfer functions (component-wise), ported straight from
// `model.wgsl` — see that file's copy for the full derivation. Vanilla's
// shade multiply is a non-colour-managed multiply on gamma bytes, not a
// linear-light one; this packed path used to skip the round-trip entirely
// (`tex.rgb * in.shade` in linear space), which is issue #400's third
// divergence and was not previously recorded anywhere before that issue.
// Doing the multiply in linear space pulls every shade factor toward 1.0 —
// at midnight (`light_term = 0.392`) a mid-grey 128 texel reads as **82**
// once re-encoded, where the gamma-space round-trip below reads **50**: the
// washed-out look, worst exactly where the scene is darkest.
fn linear_to_srgb(c: vec3<f32>) -> vec3<f32> {
    let lo = c * 12.92;
    let hi = 1.055 * pow(max(c, vec3<f32>(0.0)), vec3<f32>(1.0 / 2.4)) - 0.055;
    return select(hi, lo, c <= vec3<f32>(0.0031308));
}

fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    let lo = c / 12.92;
    let hi = pow((max(c, vec3<f32>(0.0)) + 0.055) / 1.055, vec3<f32>(2.4));
    return select(hi, lo, c <= vec3<f32>(0.04045));
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // Wrap the tile coordinate into [0,1) and map it into the sprite's atlas
    // sub-rect. This tiles a single sprite across every cell of a greedy-merged
    // quad instead of running the UV off the sprite into its atlas neighbours.
    // For a single-tile quad the coordinate is already in [0,1), so this is a
    // no-op — the reference mesher is unchanged.
    let wrapped = fract(in.tile);
    let uv = in.rect.xy + wrapped * in.rect.zw;

    // `fract` is discontinuous at tile seams, which would collapse mip selection
    // to the coarsest level along every seam. Derive the gradient from the
    // *continuous* tile coordinate (scaled into atlas space) so mipmapping stays
    // correct across the merged span.
    let ddx = dpdx(in.tile) * in.rect.zw;
    let ddy = dpdy(in.tile) * in.rect.zw;
    let tex = textureSampleGrad(atlas_tex, atlas_smp, uv, ddx, ddy);
    // Gamma-space shade multiply, see the doc above `linear_to_srgb`.
    let lit_srgb = linear_to_srgb(tex.rgb) * in.shade;
    return vec4<f32>(srgb_to_linear(lit_srgb), tex.a);
}
