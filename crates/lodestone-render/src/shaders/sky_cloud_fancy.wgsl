// FANCY clouds, expanded on the GPU: one instance per face, six vertices each.
//
// The face list changes only when the camera crosses a cell, so it is uploaded
// then and nowhere else; the per-frame state is the uniform below. Each face is
// one packed u32 (see `CloudFace::packed`): a 12-bit signed cell x, a 12-bit
// signed cell z, then the direction in the low three bits of the top byte and
// the face flags above it.

struct Camera {
    view_proj: mat4x4<f32>,
};

struct CloudInfo {
    // The resolved cloud colour: attribute times its timeline track.
    color: vec4<f32>,
    // xyz: camera-relative origin of the camera's own cell, including the
    // sub-cell scroll and the layer's bottom height. w: the distance at which
    // a face has faded out completely.
    offset_and_fade_end: vec4<f32>,
    // xyz: one cell in blocks.
    cell_size: vec4<f32>,
};

@group(0) @binding(0) var<uniform> camera: Camera;
@group(1) @binding(0) var<uniform> cloud: CloudInfo;

const FLAG_MASK_DIR: u32 = 7u;
const FLAG_INSIDE_FACE: u32 = 16u;
const FLAG_USE_TOP_COLOR: u32 = 32u;

// Unit-cube corners per face, four per face in face-direction order
// (down, up, north, south, west, east), one bit per corner.
const PACKED_X: u32 = 0xF03CC3u;
const PACKED_Y: u32 = 0x6666F0u;
const PACKED_Z: u32 = 0xC3F066u;

// Two triangles over the four corners of a face.
const QUAD_CORNER: array<u32, 6> = array<u32, 6>(0u, 1u, 2u, 0u, 2u, 3u);

const FACE_SHADE: array<f32, 6> = array<f32, 6>(0.7, 1.0, 0.8, 0.8, 0.9, 0.9);

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) color: vec4<f32>,
    @location(1) local_pos: vec3<f32>,
};

fn corner(index: u32) -> vec3<f32> {
    return vec3<f32>(
        f32((PACKED_X >> index) & 1u),
        f32((PACKED_Y >> index) & 1u),
        f32((PACKED_Z >> index) & 1u),
    );
}

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32, @location(0) face: u32) -> VsOut {
    let cell_x = bitcast<i32>(face << 20u) >> 20u;
    let cell_z = bitcast<i32>(face << 8u) >> 20u;
    let dir_and_flags = face >> 24u;
    let direction = dir_and_flags & FLAG_MASK_DIR;
    let inside = (dir_and_flags & FLAG_INSIDE_FACE) != 0u;
    let quad_vertex = QUAD_CORNER[vertex_index];
    // Faces built to be seen from inside a cell wind the other way.
    let index = direction * 4u + select(quad_vertex, 3u - quad_vertex, inside);
    let size = cloud.cell_size.xyz;
    let pos = corner(index) * size + vec3<f32>(f32(cell_x), 0.0, f32(cell_z)) * size
        + cloud.offset_and_fade_end.xyz;

    var shade = FACE_SHADE[direction];
    if (dir_and_flags & FLAG_USE_TOP_COLOR) != 0u {
        shade = FACE_SHADE[1];
    }
    var out: VsOut;
    out.clip = camera.view_proj * vec4<f32>(pos, 1.0);
    out.color = vec4<f32>(cloud.color.rgb * shade, cloud.color.a);
    out.local_pos = pos;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // Linear fade to zero over spherical distance, per fragment: per vertex, a
    // cell 12 blocks wide would interpolate the fade across its whole surface.
    let fade_end = cloud.offset_and_fade_end.w;
    let fade = select(clamp(length(in.local_pos) / fade_end, 0.0, 1.0), 0.0, fade_end <= 0.0);
    return vec4<f32>(in.color.rgb, in.color.a * (1.0 - fade));
}
