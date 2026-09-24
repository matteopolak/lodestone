
struct Camera {
    view_proj: mat4x4<f32>,
    section_origin: vec4<f32>,
};

@group(0) @binding(0) var<uniform> camera: Camera;
@group(1) @binding(0) var atlas_tex: texture_2d<f32>;
@group(1) @binding(1) var atlas_smp: sampler;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(
    @location(0) position: vec3<f32>,
    @location(1) uv: vec2<f32>,
) -> VsOut {
    let world = position + camera.section_origin.xyz;
    var out: VsOut;
    out.clip = camera.view_proj * vec4<f32>(world, 1.0);
    out.uv = uv;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let color = textureSample(atlas_tex, atlas_smp, in.uv);
    // Ported from vanilla's rendertype_crumbling.fsh: if (color.a < 0.1) discard;
    // This is load-bearing, not cosmetic. The pipeline's doubled-multiply blend
    // uses Dst/Src colour factors, which do not read alpha at all -- every
    // fragment that isn't discarded multiplies the surface regardless of its
    // own alpha. The real destroy_stage_N.png sprites are grayscale+alpha:
    // non-crack texels are white (255) at alpha ~1/255, actual crack marks are
    // dark grays (measured: 61 and 155) at alpha 255 (verified against
    // destroy_stage_0.png). Without this discard, the majority-white area of
    // every sprite would multiply the block by 2 times 1.0 times dst, doubling
    // brightness instead of leaving it untouched -- a much worse defect than
    // the too-white alpha-blend defect this pass replaces.
    if color.a < 0.1 {
        discard;
    }
    return color;
}
