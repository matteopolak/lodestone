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
    atlas_origin: vec2<i32>,
    cell_blocks: u32,
    _padding0: u32,
    _padding1: vec2<u32>,
};

@group(0) @binding(0) var<uniform> camera: Camera;
@group(1) @binding(0) var<uniform> tile: Tile;
// `terrain_y`/`water_y` and `surface_rgb565`/flags each share one R32Uint
// atlas texel. One 576-by-576 atlas per pair is a fixed 2.53 MiB GPU cost;
// separate per-tile textures would make a camera walk grow resource count.
@group(1) @binding(1) var heights_water: texture_2d<u32>;
@group(1) @binding(2) var colours_flags: texture_2d<u32>;

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

fn grid_corner(vertex_index: u32) -> vec2<u32> {
    let quad = vertex_index / 6u;
    let quad_x = quad % 63u;
    let quad_z = quad / 63u;
    let corner = vertex_index % 6u;
    let offsets = array<vec2<u32>, 6>(
        vec2<u32>(0u, 0u), vec2<u32>(1u, 0u), vec2<u32>(0u, 1u),
        vec2<u32>(1u, 0u), vec2<u32>(1u, 1u), vec2<u32>(0u, 1u),
    );
    return vec2<u32>(quad_x, quad_z) + offsets[corner];
}

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VsOut {
    let grid = grid_corner(vertex_index);
    let atlas = tile.atlas_origin + vec2<i32>(grid);
    let height = textureLoad(heights_water, atlas, 0).x & 65535u;
    let cell = vec2<f32>(tile.origin_cell + vec2<i32>(grid));
    let world = vec3<f32>(cell.x * f32(tile.cell_blocks), f32(height) - 64.0, cell.y * f32(tile.cell_blocks));
    var out: VsOut;
    out.clip = camera.view_proj * vec4<f32>(world, 1.0);
    out.world = world;
    out.colour = textureLoad(colours_flags, atlas, 0).x;
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
