// The end portal / end gateway star-field effect. Ported from
// `rendertype_end_portal.vsh`/`.fsh` — see `lodestone_render::end_portal`'s
// module doc for what geometry feeds this and `gpu/end_portal.rs`'s doc for
// the bind-group layout.
//
// # The whole trick, in one sentence
//
// Each fragment samples the two textures at *its own screen-space position*
// (derived from clip-space `x`, `y`, `w` — never a per-vertex UV), summed
// across sixteen scrolling, rotating, rescaled copies. That is what makes it
// look like the surface reflects an infinite void rather than a flat quad.
//
// # `v * M` vs `M * v` — the one real porting hazard here
//
// GLSL's `end_portal_layer(layer)` builds a `mat4` and the fragment shader
// evaluates `texProj0 * end_portal_layer(...)` — a **row vector on the
// left**. WGSL has no such operator; `mat4x4 * vec4` is always the
// column-vector convention. Transliterating the GLSL matrix literally and
// then writing `M * v` here would silently transpose every term. Rather than
// carry a hand-transposed matrix (an even easier place to get a sign wrong
// unnoticed), [`end_portal_layer_uv`] below is the fully expanded scalar
// derivation of `v * (R * T * S)` — row-vector matrix product is associative,
// so `v * (R * T * S) == ((v * R) * T) * S`, applied one factor at a time:
//
// 1. **`R`** (`mat4(scale * rotate)`, GLSL's `mat4(mat2)` constructor
//    embeds the 2×2 in the upper-left, identity elsewhere): rotates/scales
//    `(x, y)`, passes `z, w` through unchanged.
// 2. **`T`** (`translate`): adds a `w`-scaled offset to `(x, y)` — the
//    `17.0 / layer` / `(2 + layer/1.5) * (GameTime * 1.5)` terms — leaving
//    `z, w` unchanged. The `w`-scaling is what makes this a *projective*
//    translation rather than a screen-space one.
// 3. **`S`** (`SCALE_TRANSLATE`, a fixed `0.5` scale + `0.25` translate):
//    remaps into the `[0, 1]`-ish range `textureProj`'s divide expects.
//
// `w` itself never changes across any of the three steps (every matrix's
// last column is `(0, 0, 0, 1)` in the *row-vector* layout the GLSL source
// uses), so the final divide is always by the original clip-space `w`.
//
// # No `textureProj`, and no implicit derivatives either
//
// WGSL has no projective-sample built-in. `textureProj(tex, p)` for a 2D
// sampler is defined as `texture(tex, p.xy / p.w)`, so this shader does that
// division explicitly. It then samples with `textureSampleLevel(..., 0.0)`
// rather than plain `textureSample`, and this is not a style choice —
// measured, not assumed: `end_portal_layer_uv`'s **additive** `trans_x_w`/
// `trans_y` terms are per-draw uniforms (they depend on `layer`/`GameTime`,
// never on screen position), so translating by them leaves the UV's
// screen-space *derivative* untouched; only the `k`/rotation terms — large
// for a low layer index — set that derivative's magnitude. Implicit-LOD
// `textureSample` therefore picked the same near-maximum mip (from the
// rotation/scale terms alone) regardless of `GameTime`, and a texture's
// top mip is a single texel — so two renders of the same portal a `GameTime`
// apart came back **byte-identical**, caught by
// `end_portal_pixels.rs::the_swirl_animates_between_two_known_game_times`
// before this fix. `textureSampleLevel(..., 0.0)` sidesteps mip selection
// entirely, at the cost of the swirl's own anti-aliasing — an accepted
// simplification of the same shape `CLAUDE.md`'s rendering-constraints
// section already names for this kind of projective sampling: an exact
// composited byte is not attempted, only that the two arms differ.

struct Camera {
    view_proj: mat4x4<f32>,
    // Ticks-since-login plus the partial tick, the same convention every
    // other GameTime-driven effect in this codebase uses (see
    // `Sim::beacon_source`'s `animation_time`, `vault_spin_degrees`) —
    // vanilla's own `GameTime` uniform's exact scale is not re-derived here.
    game_time: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
};

@group(0) @binding(0) var<uniform> camera: Camera;

@group(1) @binding(0) var sky_tex: texture_2d<f32>;
@group(1) @binding(1) var sky_samp: sampler;

@group(2) @binding(0) var portal_tex: texture_2d<f32>;
@group(2) @binding(1) var portal_samp: sampler;

struct VsIn {
    @location(0) position: vec3<f32>,
    // 1.0 for an end-gateway vertex, 0.0 for an end-portal vertex — see the
    // fragment shader for how this reproduces vanilla's two different
    // `PORTAL_LAYERS` shader-define values (15 vs 16) from one shared,
    // statically-bounded loop instead of two separate pipelines.
    @location(1) is_gateway: f32,
};

struct VsOut {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) tex_proj: vec4<f32>,
    @location(1) is_gateway: f32,
};

@vertex
fn vs_main(in: VsIn) -> VsOut {
    var out: VsOut;
    let clip = camera.view_proj * vec4<f32>(in.position, 1.0);
    out.clip_position = clip;
    // `projection_from_position` (projection.glsl):
    //   projection = position * 0.5;
    //   projection.xy = position.xy * 0.5 + position.w * 0.5;
    //   projection.zw = position.zw;
    out.tex_proj = vec4<f32>(
        clip.x * 0.5 + clip.w * 0.5,
        clip.y * 0.5 + clip.w * 0.5,
        clip.z,
        clip.w,
    );
    out.is_gateway = in.is_gateway;
    return out;
}

// `rendertype_end_portal.fsh`'s `COLORS` array, transcribed verbatim.
const COLORS: array<vec3<f32>, 16> = array<vec3<f32>, 16>(
    vec3<f32>(0.022087, 0.098399, 0.110818),
    vec3<f32>(0.011892, 0.095924, 0.089485),
    vec3<f32>(0.027636, 0.101689, 0.100326),
    vec3<f32>(0.046564, 0.109883, 0.114838),
    vec3<f32>(0.064901, 0.117696, 0.097189),
    vec3<f32>(0.063761, 0.086895, 0.123646),
    vec3<f32>(0.084817, 0.111994, 0.166380),
    vec3<f32>(0.097489, 0.154120, 0.091064),
    vec3<f32>(0.106152, 0.131144, 0.195191),
    vec3<f32>(0.097721, 0.110188, 0.187229),
    vec3<f32>(0.133516, 0.138278, 0.148582),
    vec3<f32>(0.070006, 0.243332, 0.235792),
    vec3<f32>(0.196766, 0.142899, 0.214696),
    vec3<f32>(0.047281, 0.315338, 0.321970),
    vec3<f32>(0.204675, 0.390010, 0.302066),
    vec3<f32>(0.080955, 0.314821, 0.661491),
);

// `end_portal_layer(layer)` applied to `texProj0`, expanded to explicit
// scalar math — see the module doc's derivation. `p` is the interpolated,
// *undivided* clip-space-derived `tex_proj` varying.
fn end_portal_layer_uv(p: vec4<f32>, layer: f32, game_time: f32) -> vec2<f32> {
    let angle = radians((layer * layer * 4321.0 + layer * 9.0) * 2.0);
    let k = (4.5 - layer / 4.0) * 2.0;
    let trans_x_w = 17.0 / layer;
    let trans_y = (2.0 + layer / 1.5) * (game_time * 1.5);

    let c = cos(angle);
    let s = sin(angle);
    // Step 1 (`R`): rotate/scale (x, y).
    let xr = k * (p.x * c + p.y * s);
    let yr = k * (-p.x * s + p.y * c);
    // Step 2 (`T`): w-scaled projective translation.
    let xt = xr + p.w * trans_x_w;
    let yt = yr + p.w * trans_y;
    // Step 3 (`S`, `SCALE_TRANSLATE`).
    let xf = xt * 0.5 + p.w * 0.25;
    let yf = yt * 0.5 + p.w * 0.25;
    return vec2<f32>(xf, yf) / p.w;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let base_uv = in.tex_proj.xy / in.tex_proj.w;
    var color = textureSampleLevel(sky_tex, sky_samp, base_uv, 0.0).rgb * COLORS[0];
    // Vanilla's own loop bound is `PORTAL_LAYERS` (15 for `end_portal`, 16
    // for `end_gateway`), baked in as two separate shader-define values on
    // two separate pipelines. This shader instead always runs the full 16
    // iterations (a compile-time constant, so this loop is statically
    // uniform — no `textureSample` implicit-derivative hazard) and masks the
    // 16th layer's *contribution* to zero for a non-gateway vertex, which is
    // arithmetically identical to vanilla's 15-iteration loop for a portal.
    for (var i = 0u; i < 16u; i = i + 1u) {
        let mask = select(1.0, in.is_gateway, i == 15u);
        let layer = f32(i) + 1.0;
        let uv = end_portal_layer_uv(in.tex_proj, layer, camera.game_time);
        color += textureSampleLevel(portal_tex, portal_samp, uv, 0.0).rgb * COLORS[i] * mask;
    }
    return vec4<f32>(color, 1.0);
}
