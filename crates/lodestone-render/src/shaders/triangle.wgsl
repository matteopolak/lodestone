
@vertex
fn vs_main(@builtin(vertex_index) idx: u32) -> @builtin(position) vec4<f32> {
    var pts = array<vec2<f32>, 3>(
        vec2<f32>(-0.8, -0.8),
        vec2<f32>( 0.8, -0.8),
        vec2<f32>( 0.0,  0.8),
    );
    return vec4<f32>(pts[idx], 0.0, 1.0);
}

@fragment
fn fs_main() -> @location(0) vec4<f32> {
    return vec4<f32>(1.0, 0.5019608, 0.0, 1.0);
}
