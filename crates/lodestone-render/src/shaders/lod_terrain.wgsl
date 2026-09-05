// Coarse distant terrain. The shell does not construct this pipeline yet; the
// standalone shader-validation gate keeps the future vertex-pull contract live.
struct Camera {
    view_proj: mat4x4<f32>,
    fog_eye: vec4<f32>,
    fog_color_start: vec4<f32>,
    fog_end_enabled: vec4<f32>,
};

struct Tile {
    origin_cell: vec2<i32>,
    cell_blocks: u32,
    _padding: u32,
};

@group(0) @binding(0) var<uniform> camera: Camera;
@group(1) @binding(0) var<uniform> tile: Tile;
@group(1) @binding(1) var heights: texture_2d<u32>;
@group(1) @binding(2) var water: texture_2d<u32>;
@group(1) @binding(3) var colours: texture_2d<u32>;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) world: vec3<f32>,
    @location(1) @interpolate(flat) colour: u32,
};

fn fog_amount(rel: vec3<f32>) -> f32 {
    let distance = max(length(rel.xz), abs(rel.y));
    let start = camera.fog_color_start.w;
    let end = camera.fog_end_enabled.x;
    if (end <= start) {
        return 0.0;
    }
    return clamp((distance - start) / (end - start), 0.0, 1.0) * camera.fog_end_enabled.y;
}

@vertex
fn vs_main(@location(0) grid: vec2<u32>) -> VsOut {
    let height = textureLoad(heights, vec2<i32>(grid), 0).x;
    let cell = vec2<f32>(tile.origin_cell + vec2<i32>(grid));
    let world = vec3<f32>(cell.x * f32(tile.cell_blocks), f32(height) - 64.0, cell.y * f32(tile.cell_blocks));
    var out: VsOut;
    out.clip = camera.view_proj * vec4<f32>(world, 1.0);
    out.world = world;
    out.colour = textureLoad(colours, vec2<i32>(grid), 0).x;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let packed = in.colour;
    let r = f32((packed >> 11u) & 31u) / 31.0;
    let g = f32((packed >> 5u) & 63u) / 63.0;
    let b = f32(packed & 31u) / 31.0;
    let fog = fog_amount(in.world - camera.fog_eye.xyz);
    let colour = mix(vec3<f32>(r, g, b), camera.fog_color_start.xyz, fog);
    return vec4<f32>(colour, 1.0);
}
