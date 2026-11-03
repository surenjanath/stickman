// Stick figure renderer: one instanced quad per bone, shaded with a signed
// distance field (capsule for lines, ring for the head) for crisp anti-aliased
// strokes at any Retina scale. All coordinates are framebuffer pixels, y down.
// Output is premultiplied alpha over a transparent clear.

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) a: vec2<f32>,
    @location(1) b: vec2<f32>,
    // x: half stroke width / disc radius / corner radius (px), y: kind (0 capsule, 1 ring, 2 disc, 3 rect)
    @location(2) params: vec2<f32>,
    @location(3) color: vec4<f32>,
};

@vertex
fn vs_main(
    @builtin(vertex_index) vi: u32,
    @location(0) a: vec2<f32>,
    @location(1) b: vec2<f32>,
    @location(2) screen: vec2<f32>,
    @location(3) params: vec2<f32>,
    @location(4) color: vec4<f32>,
) -> VsOut {
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 0.0), vec2<f32>(0.0, 1.0),
        vec2<f32>(0.0, 1.0), vec2<f32>(1.0, 0.0), vec2<f32>(1.0, 1.0),
    );
    let pad = params.x + 2.0; // stroke + 2 px for the AA fringe
    var lo: vec2<f32>;
    var hi: vec2<f32>;
    if (params.y > 2.5) {
        // Rounded rect: a = centre, b = half extents.
        lo = a - b - vec2<f32>(2.0, 2.0);
        hi = a + b + vec2<f32>(2.0, 2.0);
    } else if (params.y > 1.5) {
        lo = a - vec2<f32>(pad, pad);
        hi = a + vec2<f32>(pad, pad);
    } else if (params.y > 0.5) {
        let c = (a + b) * 0.5;
        let r = length(b - a) * 0.5 + pad;
        lo = c - vec2<f32>(r, r);
        hi = c + vec2<f32>(r, r);
    } else {
        lo = min(a, b) - vec2<f32>(pad, pad);
        hi = max(a, b) + vec2<f32>(pad, pad);
    }
    let p = mix(lo, hi, corners[vi]);

    var out: VsOut;
    out.pos = vec4<f32>(p.x / screen.x * 2.0 - 1.0, 1.0 - p.y / screen.y * 2.0, 0.0, 1.0);
    out.a = a;
    out.b = b;
    out.params = params;
    out.color = color;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let p = in.pos.xy; // pixel centre, framebuffer space (top-left origin)
    var d: f32;
    if (in.params.y > 2.5) {
        let q = abs(p - in.a) - in.b + vec2<f32>(in.params.x, in.params.x);
        d = length(max(q, vec2<f32>(0.0, 0.0))) + min(max(q.x, q.y), 0.0) - in.params.x;
    } else if (in.params.y > 1.5) {
        d = length(p - in.a) - in.params.x;
    } else if (in.params.y > 0.5) {
        let c = (in.a + in.b) * 0.5;
        let r = length(in.b - in.a) * 0.5;
        d = abs(length(p - c) - r) - in.params.x;
    } else {
        let pa = p - in.a;
        let ba = in.b - in.a;
        let h = clamp(dot(pa, ba) / max(dot(ba, ba), 1e-6), 0.0, 1.0);
        d = length(pa - ba * h) - in.params.x;
    }
    let coverage = clamp(0.5 - d, 0.0, 1.0);
    let alpha = in.color.a * coverage;
    if (alpha <= 0.0) {
        discard;
    }
    return vec4<f32>(in.color.rgb * alpha, alpha);
}
