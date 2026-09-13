struct Uniform {
    view_proj: mat4x4<f32>,
    // x = viewport width (px), y = viewport height (px), z = half line
    // width (px), w unused. Same screen-space-ribbon uniform shape as
    // `outline.wgsl`'s — see `DebugLineRenderer`'s module doc for why a
    // `LineList` segment (the previous version of this shader) is not this
    // repo's fix.
    viewport: vec4<f32>,
};
@group(0) @binding(0) var<uniform> u: Uniform;

struct VertexIn {
    @location(0) position: vec3<f32>,
    @location(1) other: vec3<f32>,
    @location(2) side: f32,
    @location(3) color: vec4<f32>,
};

struct VOut {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0) color: vec4<f32>,
};

// Expands a line-segment vertex into a screen-space-thickened quad — the same
// technique `outline.wgsl` uses for the block-highlight box, and for the same
// reason: `PrimitiveTopology::LineList` rasterizes at exactly one *physical*
// pixel regardless of resolution or DPI scale, which is why this pass used to
// read as "too thin" (and, at real gameplay resolution, close to invisible).
// Depth is preserved exactly from this vertex's own clip-space z/w (only x/y
// move), so the thickened line still depth-tests as if it were the original
// thin one.
@vertex
fn vs_main(in: VertexIn) -> VOut {
    let clip_this = u.view_proj * vec4<f32>(in.position, 1.0);
    let clip_other = u.view_proj * vec4<f32>(in.other, 1.0);

    let w_this = select(clip_this.w, 1e-5, abs(clip_this.w) < 1e-5);
    let w_other = select(clip_other.w, 1e-5, abs(clip_other.w) < 1e-5);

    let ndc_this = clip_this.xy / w_this;
    let ndc_other = clip_other.xy / w_other;

    let viewport = u.viewport.xy;
    let screen_this = (ndc_this * vec2<f32>(0.5, -0.5) + vec2<f32>(0.5, 0.5)) * viewport;
    let screen_other = (ndc_other * vec2<f32>(0.5, -0.5) + vec2<f32>(0.5, 0.5)) * viewport;

    var dir = screen_other - screen_this;
    let len = length(dir);
    if len > 1e-5 {
        dir = dir / len;
    } else {
        dir = vec2<f32>(1.0, 0.0);
    }
    let normal = vec2<f32>(-dir.y, dir.x);

    let half_width_px = u.viewport.z;
    let new_screen = screen_this + normal * (half_width_px * in.side);

    let new_ndc = (new_screen / viewport - vec2<f32>(0.5, 0.5)) * vec2<f32>(2.0, -2.0);

    var out: VOut;
    out.clip_pos = vec4<f32>(new_ndc * w_this, clip_this.z, w_this);
    out.color = in.color;
    return out;
}

@fragment
fn fs_main(in: VOut) -> @location(0) vec4<f32> {
    return in.color;
}
