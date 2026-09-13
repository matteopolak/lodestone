// The 2-D GUI enchantment glint: the scrolling foil shimmer over a flat item
// icon in a hotbar cell, an inventory slot or a container slot.
//
// This is a *second* implementation of the glint, and the duplication is forced.
// `lodestone_render::glint`'s pipeline re-rasterises the item's own **3-D**
// geometry: it consumes `ModelVertex`, and its depth compare is `EQUAL`, which
// only works against a depth attachment holding the pass beneath it. A GUI slot's
// flat icon has neither — `hud_sprite.wgsl`'s stream is 8 floats per vertex and
// every GUI sprite pass here runs with `depth_stencil_attachment: None`. So the
// masking has to come from somewhere other than depth.
//
// It comes from the item atlas. The glint quad is the *same* quad the icon drew,
// with the *same* atlas UVs, so sampling the item atlas again gives exactly the
// icon's own alpha — and discarding where that alpha is low confines the shimmer
// to the item's silhouette rather than painting the whole 16x16 cell. That is
// what depth-`EQUAL` buys the 3-D path, obtained differently.
//
// Everything else is shared with the 3-D path and lives on the Rust side in
// `lodestone_render::glint`: the texture matrix (`T(-u_off, +v_off) * Rz(10 deg)
// * S(8.0)`), the `GlintAlpha` strength, and the `GLINT` blend function
// (`dst += src * src`, alpha untouched). The glint sheet is uploaded **non-sRGB**
// for the reason `gpu/glint.rs` documents at length: the blend squares the raw
// byte in gamma space, so the hardware must not decode it to linear on the way in.

struct GuiGlint {
    // `T(-u_off, +v_off) * Rz(10 deg) * S(scale)`, from
    // `lodestone_render::glint::glint_texture_matrix`.
    tex_matrix: mat4x4<f32>,
    // `.x` is `GlintAlpha` (the `glintStrength` option, default 0.75). The rest
    // is padding to the 16-byte uniform alignment.
    fade: vec4<f32>,
}

@group(0) @binding(0) var<uniform> g: GuiGlint;
@group(1) @binding(0) var item_tex: texture_2d<f32>;
@group(1) @binding(1) var item_smp: sampler;
@group(1) @binding(2) var glint_tex: texture_2d<f32>;
@group(1) @binding(3) var glint_smp: sampler;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    // The item atlas UV, for the silhouette mask.
    @location(0) item_uv: vec2<f32>,
    // The same UV through the glint texture matrix.
    @location(1) glint_uv: vec2<f32>,
};

// The vertex buffer is `hud_sprite.wgsl`'s own layout, unchanged, so the glint
// stream can be built by the same `push_sprite_quad`. Location 2 (the tint) is
// declared by that layout and deliberately not consumed here: vanilla's glint
// vertex format is POSITION_TEX only and carries no colour.
@vertex
fn vs_main(
    @location(0) pos: vec2<f32>,
    @location(1) uv: vec2<f32>,
) -> VsOut {
    var out: VsOut;
    out.clip = vec4<f32>(pos, 0.0, 1.0);
    out.item_uv = uv;
    out.glint_uv = (g.tex_matrix * vec4<f32>(uv, 0.0, 1.0)).xy;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // The silhouette mask. Item sprites are cutouts, so this is a hard test at
    // the same 0.1 vanilla's own `glint.fsh` discards at — not a blend, which
    // would let a feathered edge shimmer outside the icon.
    if (textureSample(item_tex, item_smp, in.item_uv).a < 0.1) {
        discard;
    }
    let c = textureSample(glint_tex, glint_smp, in.glint_uv);
    if (c.a < 0.1) {
        discard;
    }
    // `fragColor = vec4(color.rgb * GlintAlpha, color.a)`, exactly `glint.fsh`
    // minus the fog term (a GUI slot has no fog). The alpha is written and then
    // discarded by the `GLINT` blend's `ZERO`/`ONE` alpha equation; only the RGB
    // is observable.
    return vec4<f32>(c.rgb * g.fade.x, c.a);
}
