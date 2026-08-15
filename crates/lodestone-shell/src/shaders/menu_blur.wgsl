// The blur vanilla runs behind an open in-game menu.
//
// Record: `Screen.extractBackground` (`net.minecraft.client.gui.screens.Screen`)
// calls `extractBlurredBackground` for every screen that is not
// `isInGameUi()` (that fork is the flat translucent gradient instead --
// `Container`/sign-edit/command-block-edit screens want no blur, which is why
// this pass is only ever invoked for the overlay screens that set
// `MenuFrame::blur`, not for every `MenuBackdrop::Dim` frame). That method
// calls `GuiRenderState::blurBeforeThisStratum`, which is realised as
// `GameRenderer::processBlurEffect` running the `minecraft:blur` post chain
// (`assets/minecraft/post_effect/blur.json`) over the already-drawn frame,
// before the screen's own widgets are drawn on top -- so the background blurs
// and the menu stays sharp.
//
// The chain is six passes, three horizontal+vertical pairs, each running this
// fragment stage (`assets/minecraft/shaders/post/box_blur.fsh`). The radius
// comes from `Options::menuBackgroundBlurriness` (an accessibility option,
// `0..=10`, default `5` -- `Options.BLURRINESS_DEFAULT_VALUE`) and is passed
// through untransformed as the shader's `MenuBlurRadius` global. This port
// hardcodes that default (see `menu/render/blur.rs::BLUR_RADIUS`) rather than
// wiring a settings row, which is a deliberate, stated scope cut.
//
// The box filter itself is hand-expanded, not transliterated: bilinear
// sampling lets one tap cover two texels, so the loop advances in steps of 2
// starting half a texel off centre, and the leftover odd tap at the far edge
// is folded back in at half weight so the total sample count still reads as
// `radius * 2 + 1`.

struct BlurConfig {
    // Sample step direction in texel units: (1, 0) for the horizontal pass,
    // (0, 1) for the vertical pass -- `BlurDir` in `box_blur.fsh`.
    dir: vec2<f32>,
    // Effective box half-width in texels -- vanilla's `MenuBlurRadius`.
    radius: f32,
    _pad: f32,
}

@group(0) @binding(0) var src_texture: texture_2d<f32>;
@group(0) @binding(1) var src_sampler: sampler;
@group(1) @binding(0) var<uniform> config: BlurConfig;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

// One full-screen triangle from the vertex index alone -- no vertex buffer,
// matching vanilla's own `core/screenquad` vertex stage for every post pass.
@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> VertexOutput {
    let x = f32((index << 1u) & 2u);
    let y = f32(index & 2u);
    var out: VertexOutput;
    out.position = vec4<f32>(x * 2.0 - 1.0, 1.0 - y * 2.0, 0.0, 1.0);
    out.uv = vec2<f32>(x, y);
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let size = vec2<f32>(textureDimensions(src_texture));
    let one_texel = vec2<f32>(1.0, 1.0) / size;
    let step = one_texel * config.dir;

    var accum = vec4<f32>(0.0, 0.0, 0.0, 0.0);
    var a = -config.radius + 0.5;
    loop {
        if a > config.radius {
            break;
        }
        accum += textureSampleLevel(src_texture, src_sampler, in.uv + step * a, 0.0);
        a += 2.0;
    }
    accum += textureSampleLevel(src_texture, src_sampler, in.uv + step * config.radius, 0.0) * 0.5;
    return accum / (config.radius + 0.5);
}
