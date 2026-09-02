// Enchantment glint: the scrolling foil shimmer over an item's own geometry.
//
// A faithful port of vanilla 26.2's `core/glint` pair plus `RenderPipelines.GLINT`
// (`RenderPipelines.java:419-433`). Vanilla's own vertex shader is four lines and
// the fragment four more; the interesting content is entirely in the constants,
// which live on the Rust side in `crate::glint` so they can be unit-gated against
// the jar.
//
// Two things about this shader are load-bearing and easy to get wrong.
//
// First, it re-rasterises *the item's own quads*, not a flat overlay quad. Vanilla
// runs the glint as a second pass over the same submit list
// (`ItemFeatureRenderer.java:74-84`) and its pipeline uses depth compare EQUAL with
// zero depth bias (`DepthStencilState(CompareOp.EQUAL, false)`), which only works
// if the two passes rasterise byte-identical clip positions. So the vertex stage
// here computes `clip` exactly as `model.wgsl` does — same uniform, same
// `section_origin` add, same order of operations. Any divergence z-fails the whole
// glint and it silently draws nothing.
//
// Second, `EQUAL` is the one ported depth comparison that does **not** flip sign.
// Our depth is reversed-Z [0,1] like vanilla's, so a ported
// `GREATER_THAN_OR_EQUAL` transcribes unflipped and so does a positive depth
// bias -- and equality is orientation-independent either way, so
// `CompareOp.EQUAL` ports across as `CompareFunction::Equal` unchanged.
//
// The UV transform arrives already composed as a mat4 (`glint.tex_matrix`) rather
// than as time + constants, so there is exactly one implementation of the scroll
// maths and it is the testable one.

struct Glint {
    // Same layout and meaning as `model.wgsl`'s `Camera.view_proj`.
    view_proj: mat4x4<f32>,
    // The glint texture matrix: `T(-u_off, +v_off) * Rz(10 deg) * S(scale)`.
    // Built by `crate::glint::glint_texture_matrix`.
    tex_matrix: mat4x4<f32>,
    // `.xyz` is the section origin added to each vertex position, matching
    // `model.wgsl`'s `Origin.section_origin`. `.w` carries `GlintAlpha` — the
    // `glintStrength` option, default 0.75 (`Options.java:867-874`) — folded into
    // the same uniform rather than given a binding of its own.
    origin_and_alpha: vec4<f32>,
}

@group(0) @binding(0) var<uniform> glint: Glint;
@group(1) @binding(0) var glint_tex: texture_2d<f32>;
@group(1) @binding(1) var glint_smp: sampler;

struct VsIn {
    @location(0) position: vec3<f32>,
    @location(1) uv: vec2<f32>,
    // Locations 2 and 3 exist so this shader can consume `ModelVertex`'s buffer
    // layout unchanged, and therefore re-draw the *same* vertex buffer the model
    // pass drew. That identity is what makes depth-EQUAL viable. Neither value is
    // used: vanilla's glint vertex format is POSITION_TEX only
    // (`DefaultVertexFormat.java:55`) and carries no shade, light or colour.
    @location(2) ao: f32,
    @location(3) packed: vec4<u32>,
}

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs_main(in: VsIn) -> VsOut {
    var out: VsOut;
    let world = in.position + glint.origin_and_alpha.xyz;
    out.clip = glint.view_proj * vec4<f32>(world, 1.0);
    // `texCoord0 = (TextureMat * vec4(UV0, 0.0, 1.0)).xy`, the last line of
    // vanilla's `glint.vsh`. The incoming UV is the *item atlas sprite* UV for
    // `FoilType.STANDARD`, fed straight in.
    out.uv = (glint.tex_matrix * vec4<f32>(in.uv, 0.0, 1.0)).xy;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(glint_tex, glint_smp, in.uv);
    // `glint.fsh`: a hard discard at alpha < 0.1. Note vanilla's
    // `enchanted_glint_item.png` is 8-bit RGB with no alpha channel, so its
    // sampled alpha is always 1.0 and this branch never fires for the item glint;
    // it is kept because `enchanted_glint_armor.png` is palettised and can carry
    // alpha, and because removing a discard is not the kind of thing to do on the
    // strength of one texture's current encoding.
    if (c.a < 0.1) {
        discard;
    }
    // `fragColor = vec4(color.rgb * fade, color.a)` where
    // `fade = (1 - total_fog_value(...)) * GlintAlpha`. Fog is omitted here
    // deliberately: an item glint is drawn at the item, and the model pass has
    // already applied fog to the surface underneath it, so a second fog term
    // would double-count. `GlintAlpha` is not omitted — it is the user-facing
    // `glintStrength` and at its default of 0.75 it is a visible 25% reduction,
    // i.e. exactly the kind of magnitude that a direction-only gate would miss.
    let fade = glint.origin_and_alpha.w;
    // RGB fades, alpha passes through untouched, matching vanilla. That asymmetry
    // matters: the GLINT blend function leaves the destination alpha alone
    // (`ZERO`/`ONE`), so the alpha written here never reaches the framebuffer and
    // only the RGB term is observable.
    return vec4<f32>(c.rgb * fade, c.a);
}
