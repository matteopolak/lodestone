// The smallest legal draw, existing only so a timestamp-carrying render pass
// has real vertex and fragment work in it.
//
// `gpu::gpu_timing::GpuQueryTimer::stamp` opens an otherwise-empty pass purely
// to write a timestamp at a pass boundary — the only place a timestamp can be
// written on an adapter without `TIMESTAMP_QUERY_INSIDE_ENCODERS`. An *empty*
// pass turned out not to work for that: `timestamp_writes` samples at **stage**
// boundaries (start of vertex, end of fragment), and a pass with neither stage
// has no such boundary to sample, so the pair it reports is not a duration of
// anything. Measured as an occasional inversion — the bracketing span reading
// shorter than a pass it encloses — which is exactly what an undefined
// timestamp looks like once the `end > begin` filter has thrown away the
// obviously-broken pairs.
//
// So this draws one triangle covering a 1x1 target: three vertices, no
// bindings, no vertex buffer, no interpolation, one fragment. Vertex positions
// come from `vertex_index` rather than a buffer so the pipeline needs no
// layout at all.

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    // A triangle that covers the whole clip rect. Written out rather than
    // computed so it is obvious there is no degenerate case: a zero-area
    // triangle would be culled and produce no fragment stage, which is the
    // exact thing this shader exists to guarantee happens.
    var positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>( 3.0, -1.0),
        vec2<f32>(-1.0,  3.0),
    );
    return vec4<f32>(positions[index], 0.0, 1.0);
}

@fragment
fn fs_main() -> @location(0) vec4<f32> {
    // The value is irrelevant: the target is 1x1 scratch that nothing samples
    // and whose store op is `Discard`. Only the fact that a fragment ran
    // matters.
    return vec4<f32>(1.0, 0.0, 0.0, 1.0);
}
