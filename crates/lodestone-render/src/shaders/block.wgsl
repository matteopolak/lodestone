
// The **shared** half of the packed path's group-0 uniform (binding 0):
// view-projection plus this frame's fog, identical for every section drawn this
// frame. Byte-for-byte `model.wgsl`'s own `Camera` and the same 112-byte
// `ModelSharedCameraUniform` behind it — one type, one layout, so the packed and
// model paths structurally cannot disagree about fog or the clock.
//
// This used to be `{ view_proj, section_origin }` written **per section, per
// frame** — the exact shape that once profiled at ~4000 `queue.write_buffer`
// calls/frame and 52.9% of main-thread CPU on the model path. The origin is
// constant for a section's life, so it moved to binding 1 behind a dynamic
// offset and is written once, at upload. See `docs/section-camera-uniform.md`.
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

// The factor the *sky* half of the light term is scaled by, so packed terrain
// darkens at night. Rides `fog_end_enabled.z`, the same spare lane `model.wgsl`
// and `entity.wgsl` use, so the packed path, live terrain and mobs cannot
// disagree about what time it is.
//
// `0.0` is the `not wired yet` sentinel and reads as full daylight: every caller
// builds this uniform from a `FogUniform` that zeroes the lane, and taking 0.0
// literally would render all sky-lit terrain pure black. Vanilla's real range is
// [0.24, 1.0], so 0.0 is never legitimate.
//
// Only the sky half is scaled -- block light is a torch and does not dim at dusk.
// Without this term, the demo world and every headless gate would render at a
// fixed permanent noon regardless of the clock.
fn sky_darken() -> f32 {
    let raw = camera.fog_end_enabled.z;
    return select(raw, 1.0, raw <= 0.0);
}

// Byte-for-byte `model.wgsl`'s `linear_fog`/`fog_amount` -- WGSL has no include,
// so the two copies change together or terrain fogs at two different rates
// depending on which mesher produced it. Vanilla's `total_fog_value`
// (`fog.glsl:49-53`): the `max` of two independent linear ramps over two
// different distance metrics from `rel = world - eye`.
fn linear_fog(dist: f32, start: f32, end: f32) -> f32 {
    if (end <= start) {
        return 0.0;
    }
    return clamp((dist - start) / (end - start), 0.0, 1.0);
}

fn fog_amount(rel: vec3<f32>) -> f32 {
    let sph = length(rel);
    let cyl = max(length(rel.xz), abs(rel.y));
    let env = linear_fog(sph, camera.fog_eye.w, camera.fog_end_enabled.w);
    let rd = linear_fog(cyl, camera.fog_color_start.w, camera.fog_end_enabled.x);
    return max(env, rd) * camera.fog_end_enabled.y;
}

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    // Tile coordinate, running 0..w / 0..h across a greedy-merged quad (0..1 for
    // a single-tile reference quad). Interpolated, then wrapped per fragment.
    @location(0) tile: vec2<f32>,
    @location(1) shade: f32,
    // The sprite's atlas sub-rect (min.xy, size.zw). Constant across the quad, so
    // interpolate it flat to avoid drift and let the fragment stage do the wrap.
    @location(2) @interpolate(flat) rect: vec4<f32>,
    // World position, for the fragment stage's per-fragment fog distance —
    // without it, nothing in this path would fade with distance.
    @location(3) world: vec3<f32>,
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
    //
    // `sky * sky_darken()`, not a bare `sky`: `model.wgsl`'s equivalent term
    // scales the sky half by the clock before the `max`, and this path matches
    // it. At midnight `sky_darken` is 0.24, so a fully sky-lit face goes from
    // `0.2 + 0.8*1.00 = 1.000` to `0.2 + 0.8*0.24 = 0.392`.
    //
    // This is still the simple `0.2 + 0.8*l` ramp rather than `model.wgsl`'s full
    // `lightmap.fsh` port -- the packed path meshes full cubes for the demo world
    // and closing that second gap remains out of scope for this path.
    let light_term = 0.2 + 0.8 * max(sky * sky_darken(), block);

    var out: VsOut;
    out.clip = camera.view_proj * vec4<f32>(world, 1.0);
    out.tile = vec2<f32>(tu, tv);
    out.shade = ao * light_term;
    out.rect = sprite_uv[sprite];
    out.world = world;
    return out;
}

// sRGB transfer functions (component-wise), ported straight from
// `model.wgsl` — see that file's copy for the full derivation. Vanilla's
// shade multiply is a non-colour-managed multiply on gamma bytes, not a
// linear-light one; this packed path used to skip the round-trip entirely
// (`tex.rgb * in.shade` in linear space).
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
    // Fade toward the fog colour by view distance, in **gamma** space, folded into
    // the same transfer round-trip -- byte-for-byte `model.wgsl:320-322`. Terrain
    // meshed by either mesher must fog on the same curve, or a demo-world block
    // fades at a different rate from the live-world block beside it.
    let amount = fog_amount(in.world - camera.fog_eye.xyz);
    let fogged_srgb = mix(lit_srgb, linear_to_srgb(camera.fog_color_start.rgb), amount);
    return vec4<f32>(srgb_to_linear(fogged_srgb), tex.a);
}
