// Resolves the accumulated translucent colour over the scene. Appended after
// `oit_common.wgsl`, whose bindings 3 and 4 hold the coefficients.
//
// The accumulated alpha is only an estimate of the pixel's coverage, so the
// colour is rescaled to the coverage the transmittance function reports: the
// total absorbance beyond every surface gives the exact amount of light the
// translucent surfaces block.

@group(0) @binding(5) var oit_accumulated: texture_2d<f32>;

@vertex
fn vs_composite(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let uv = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    return vec4<f32>(uv * 2.0 - 1.0, 0.0, 1.0);
}

@fragment
fn fs_composite(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let pos = vec2<i32>(frag.xy);
    let accumulated = textureLoad(oit_accumulated, pos, 0);

    let total = oit_sample_transmittance(pos, 100000.0, 1.0);
    let coverage = 1.0 - select(total, 0.0, total < OIT_FULLY_OPAQUE_TRANSMITTANCE);
    if (coverage < 0.00001 && dot(accumulated.rgb, vec3<f32>(1.0)) < 0.00001) {
        discard;
    }
    let scale = select(1.0, coverage / accumulated.a, accumulated.a > 0.00001);
    return vec4<f32>(accumulated.rgb * scale, coverage);
}
