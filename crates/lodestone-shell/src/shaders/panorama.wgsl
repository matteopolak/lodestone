// The title screen's spinning cubemap panorama.
//
// A port of vanilla's own core panorama shader pair, which
// together are eight lines: transform the cube corner by the combined
// projection/model-view matrix,
// pass the *object-space* position through, and sample a cubemap with it. The
// face is chosen by direction, which is why the cube carries no UVs at all
// (vanilla's own position-only vertex format).
//
// The one addition is `dim`. Vanilla composites a flat dark overlay texture
// over the panorama on every out-of-world screen except the title screen itself
// (vanilla's own background-extraction routine, and the title screen's own empty
// override). That texture was
// decoded out of client.jar and is flat black at alpha 64/255 in every pixel, so
// compositing it is exactly a multiply by `1 - 64/255` — one uniform here instead
// of a second pipeline and a second full-screen quad. See `docs/menu-panorama.md`
// for why the two are algebraically identical on both an sRGB and a linear target.

struct Panorama {
    // Projection * model-view. Object-space positions are what the fragment
    // stage samples with, so this matrix only decides where each face lands.
    view_proj: mat4x4<f32>,
    // .x is the dim factor; .yzw are padding to keep the uniform 16-byte aligned.
    dim: vec4<f32>,
};

@group(0) @binding(0) var<uniform> panorama: Panorama;
@group(0) @binding(1) var cube_tex: texture_cube<f32>;
@group(0) @binding(2) var cube_smp: sampler;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) dir: vec3<f32>,
};

@vertex
fn vs_main(@location(0) pos: vec3<f32>) -> VsOut {
    var out: VsOut;
    out.clip = panorama.view_proj * vec4<f32>(pos, 1.0);
    out.dir = pos;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let texel = textureSample(cube_tex, cube_smp, in.dir);
    // Opaque: the panorama is the backmost thing on the screen and vanilla's own
    // fragment shader writes the sampled alpha, which for every shipped face is 1.
    return vec4<f32>(texel.rgb * (1.0 - panorama.dim.x), 1.0);
}
