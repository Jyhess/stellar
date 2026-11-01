// Rendering of the bodies: glowing circular billboards, additive blending.

struct Uniforms {
    view: mat4x4<f32>,
    proj: mat4x4<f32>,
    // x: viewport height (px), y: tan(fov/2), z: minimum size (px), w: unused
    screen: vec4<f32>,
    // x: brightness, y: intensity floor of the dust, z, w: unused
    look: vec4<f32>,
    // Direct source: anchor (xyz) and base radius r0 (w)
    anchor: vec4<f32>,
    // Direct source: x = ln(maximum mass), 0 if it is at most 1
    mass: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;

// Direct source, camera tracking a body: its position at the step being drawn,
// copied from the engine on the GPU. The published position, used for the
// anchor, lags a few steps behind the bodies' buffers.
const MAX_FOLLOW: u32 = 32u;
struct Follow {
    // x: number of entries (one per subsystem that may hold the body or count
    // in the barycenter); y: 0 = a body, 1 = a barycenter
    count: vec4<u32>,
    // Per entry: position of the body in the subsystem's frame (xyz, w = 1 if
    // present), or the subsystem's moments (x: mass, yzw: Σ m·position, in its
    // frame); then the subsystem's origin minus the tracked point used for
    // the anchor (xyz).
    entries: array<vec4<f32>, 64>,
};

@group(0) @binding(1) var<uniform> follow: Follow;

// Displacement of the tracked body since the position the anchor was built
// from (zero if it is not found).
fn follow_shift() -> vec3<f32> {
    let count = min(follow.count.x, MAX_FOLLOW);
    if (follow.count.y == 1u) {
        // Barycenter of the subsystems' barycenters, weighted by their masses.
        var mass = 0.0;
        var sum = vec3<f32>(0.0);
        for (var k = 0u; k < count; k++) {
            let m = follow.entries[2u * k];
            if (m.x > 0.0) {
                mass += m.x;
                sum += m.x * (m.yzw / m.x + follow.entries[2u * k + 1u].xyz);
            }
        }
        if (mass > 0.0) {
            return sum / mass;
        }
        return vec3<f32>(0.0);
    }
    for (var k = 0u; k < count; k++) {
        let p = follow.entries[2u * k];
        if (p.w > 0.5) {
            return p.xyz + follow.entries[2u * k + 1u].xyz;
        }
    }
    return vec3<f32>(0.0);
}

struct VsIn {
    @builtin(vertex_index) vertex: u32,
    @location(0) pos_radius: vec4<f32>,
    @location(1) color: vec4<f32>,
};

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
};

// Billboard of a body: position relative to the anchor, radius, colour and
// weight (1 for an ordinary body).
fn billboard(vertex: u32, rel: vec3<f32>, radius: f32, rgb: vec3<f32>, weight: f32) -> VsOut {
    var corners = array<vec2<f32>, 4>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(1.0, -1.0),
        vec2<f32>(-1.0, 1.0),
        vec2<f32>(1.0, 1.0),
    );
    let corner = corners[vertex];

    let center = u.view * vec4<f32>(rel, 1.0);
    let depth = max(-center.z, 1e-6);
    // Size of a pixel in view space, at this depth.
    let world_per_px = 2.0 * u.screen.y * depth / u.screen.x;
    let min_radius = 0.5 * u.screen.z * world_per_px;
    let size = max(radius, min_radius);

    // A body smaller than a pixel keeps (roughly) its total brightness: its
    // intensity is reduced instead of enlarging it.
    var intensity = 1.0;
    if (radius < min_radius) {
        let k = radius / min_radius;
        intensity = max(k * k, u.look.y);
    }

    var out: VsOut;
    let offset = vec4<f32>(corner * size, 0.0, 0.0);
    out.clip = u.proj * (center + offset);
    out.uv = corner;
    out.color = vec4<f32>(rgb * intensity * weight * u.look.x, 1.0);
    return out;
}

@vertex
fn vs_main(in: VsIn) -> VsOut {
    // The alpha channel carries a weight (×16): 16/255 ↔ 1.
    let weight = in.color.a * (255.0 / 16.0);
    return billboard(in.vertex, in.pos_radius.xyz, in.pos_radius.w, in.color.rgb, weight);
}

// Colour by mass (logarithmic scale), like `mass_color` on the CPU side:
// pale blue → white → yellow → orange.
fn mass_color(m: f32) -> vec3<f32> {
    var t = 0.0;
    if (u.mass.x > 0.0) {
        t = clamp(log(max(m, 1.0)) / u.mass.x, 0.0, 1.0);
    }
    let c0 = vec3<f32>(0.55, 0.68, 1.0);
    let c1 = vec3<f32>(0.95, 0.95, 1.0);
    let c2 = vec3<f32>(1.0, 0.85, 0.45);
    let c3 = vec3<f32>(1.0, 0.45, 0.15);
    if (t <= 0.35) {
        return mix(c0, c1, t / 0.35);
    }
    if (t <= 0.7) {
        return mix(c1, c2, (t - 0.35) / 0.35);
    }
    return mix(c2, c3, (t - 0.7) / 0.3);
}

struct BodyIn {
    @builtin(vertex_index) vertex: u32,
    @location(0) pos: vec3<f32>,
    @location(1) mass: f32,
};

// Bodies read directly from the GPU engine's buffers.
@vertex
fn vs_bodies(in: BodyIn) -> VsOut {
    if (in.mass <= 0.0) {
        // Dead body (merged or removed, waiting for compaction): vertex outside
        // the view volume, nothing is drawn.
        var out: VsOut;
        out.clip = vec4<f32>(0.0, 0.0, -2.0, 1.0);
        out.uv = vec2<f32>(0.0);
        out.color = vec4<f32>(0.0);
        return out;
    }
    let radius = u.anchor.w * pow(in.mass, 1.0 / 3.0);
    let anchor = u.anchor.xyz + follow_shift();
    return billboard(in.vertex, in.pos - anchor, radius, mass_color(in.mass), 1.0);
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let d2 = dot(in.uv, in.uv);
    if (d2 > 1.0) {
        discard;
    }
    // Solid disc in the core, soft halo at the edge.
    let core = 1.0 - smoothstep(0.55, 1.0, sqrt(d2));
    let glow = exp(-4.0 * d2);
    let f = 0.65 * core + 0.35 * glow;
    return vec4<f32>(in.color.rgb * f, f);
}
