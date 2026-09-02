// Rain and snow columns: one instanced quad per column, angled to face the
// camera. Vanilla's own weather-effect pass builds the same four
// vertices on the CPU into a `PARTICLE` format buffer; here the CPU emits one
// 48-byte instance and this shader expands it, which is the same trade the
// particle pass already makes.
//
// The quad is NOT a billboard. Its two vertical edges sit at
// `centre ± (half_x, 0, half_z)`, where that offset is the unit vector
// perpendicular to the camera-to-column direction in the XZ plane (see
// `weather::column_offset_table`). So the quad is a vertical ribbon whose
// horizontal extent is exactly 1 block wide, rotated about Y to face the eye —
// a rain streak, not a sprite. Building it from `camera.right`/`camera.up` the
// way the particle pass does would tilt it with the pitch and make the rain lean
// when the player looks up.

struct Camera {
    // Projection * view * translate(camera_position): positions arrive
    // camera-relative and this folds the eye translation back in, matching the
    // particle pass's uniform exactly.
    view_proj: mat4x4<f32>,
};

@group(0) @binding(0) var<uniform> camera: Camera;
@group(1) @binding(0) var weather_tex: texture_2d<f32>;
@group(1) @binding(1) var weather_sampler: sampler;

struct Instance {
    // rel_x, rel_z, y0 (bottom), y1 (top) -- all camera-relative.
    @location(0) base: vec4<f32>,
    // half_x, half_z (the perpendicular half-offset), u_offset, v0.
    @location(1) axis: vec4<f32>,
    // v1, alpha, light, spare.
    @location(2) shade: vec4<f32>,
};

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) alpha: f32,
    @location(2) light: f32,
};

@vertex
fn vs_main(inst: Instance, @builtin(vertex_index) vi: u32) -> VsOut {
    // Triangle-strip corner order, matched to vanilla's quad winding
    // (top-left, top-right, bottom-right, bottom-left reordered into a strip):
    //   0 -> (-half, top)   1 -> (-half, bottom)
    //   2 -> (+half, top)   3 -> (+half, bottom)
    let right = select(-1.0, 1.0, vi >= 2u);
    let top = (vi & 1u) == 0u;

    let half_x = inst.axis.x * right;
    let half_z = inst.axis.y * right;
    let y = select(inst.base.z, inst.base.w, top);

    let world = vec3<f32>(inst.base.x + half_x, y, inst.base.y + half_z);

    var out: VsOut;
    out.clip = camera.view_proj * vec4<f32>(world, 1.0);
    // U runs across the ribbon: u_offset at the -half edge, +1 at the +half.
    // V is precomputed per end of the column, already carrying the scroll, so
    // the animation costs nothing here.
    out.uv = vec2<f32>(
        inst.axis.z + select(0.0, 1.0, right > 0.0),
        select(inst.shade.x, inst.axis.w, top),
    );
    out.alpha = inst.shade.y;
    out.light = inst.shade.z;
    return out;
}

// sRGB transfer, the same pair `model.wgsl` carries and for the same reason:
// vanilla is not colour-managed, so the light term multiplies gamma-encoded
// bytes. Applying it to the linear texel this sampler returns would pull every
// shade factor toward 1.0 and wash the rain out against a dark sky.
fn linear_to_srgb(c: vec3<f32>) -> vec3<f32> {
    let lo = c * 12.92;
    let hi = 1.055 * pow(max(c, vec3<f32>(0.0)), vec3<f32>(1.0 / 2.4)) - 0.055;
    return select(hi, lo, c <= vec3<f32>(0.0031308));
}

fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    let lo = c / 12.92;
    let hi = pow((max(c, vec3<f32>(0.0)) + 0.055) / 1.055, vec3<f32>(2.4));
    return select(hi, lo, c <= vec3<f32>(0.04045));
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // Both textures tile in U and V: rain.png scrolls V past 32 tiles and snow's
    // U is a per-column random walk, so the sampler must repeat rather than
    // clamp. That is the sampler's address mode, set in `weather_pipeline`.
    let texel = textureSample(weather_tex, weather_sampler, in.uv);
    let a = texel.a * in.alpha;
    // Rain texels are near-white with a soft edge; nothing is gained by drawing
    // the transparent tail and it costs blend bandwidth over the whole column.
    if (a < 0.01) {
        discard;
    }
    let lit = srgb_to_linear(linear_to_srgb(texel.rgb) * in.light);
    return vec4<f32>(lit, a);
}
