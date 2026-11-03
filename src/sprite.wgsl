// Textured sprites (words grabbed from the screen): a rotated quad sampling a
// premultiplied-alpha texture. Coordinates are framebuffer pixels, y down.

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) alpha: f32,
};

@group(0) @binding(0) var tex: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;

@vertex
fn vs_main(
    @builtin(vertex_index) vi: u32,
    @location(0) center: vec2<f32>,
    @location(1) half_size: vec2<f32>,
    // x: rotation (radians), y: opacity
    @location(2) rot_alpha: vec2<f32>,
    @location(3) screen: vec2<f32>,
) -> VsOut {
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 0.0), vec2<f32>(0.0, 1.0),
        vec2<f32>(0.0, 1.0), vec2<f32>(1.0, 0.0), vec2<f32>(1.0, 1.0),
    );
    let uv = corners[vi];
    let local = (uv * 2.0 - 1.0) * half_size;
    let c = cos(rot_alpha.x);
    let s = sin(rot_alpha.x);
    let p = center + vec2<f32>(local.x * c - local.y * s, local.x * s + local.y * c);
    var out: VsOut;
    out.pos = vec4<f32>(p.x / screen.x * 2.0 - 1.0, 1.0 - p.y / screen.y * 2.0, 0.0, 1.0);
    out.uv = uv;
    out.alpha = rot_alpha.y;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    return textureSample(tex, samp, in.uv) * in.alpha;
}
