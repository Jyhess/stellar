// Rendu des corps : billboards circulaires lumineux, mélange additif.

struct Uniforms {
    view: mat4x4<f32>,
    proj: mat4x4<f32>,
    // x : hauteur du viewport (px), y : tan(fov/2), z : taille minimale (px), w : inutilisé
    screen: vec4<f32>,
    // x : luminosité, y : plancher d'intensité des poussières, z, w : inutilisés
    look: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;

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

@vertex
fn vs_main(in: VsIn) -> VsOut {
    var corners = array<vec2<f32>, 4>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(1.0, -1.0),
        vec2<f32>(-1.0, 1.0),
        vec2<f32>(1.0, 1.0),
    );
    let corner = corners[in.vertex];

    let center = u.view * vec4<f32>(in.pos_radius.xyz, 1.0);
    let depth = max(-center.z, 1e-6);
    // Taille d'un pixel dans l'espace vue, à cette profondeur.
    let world_per_px = 2.0 * u.screen.y * depth / u.screen.x;
    let min_radius = 0.5 * u.screen.z * world_per_px;
    let radius = in.pos_radius.w;
    let size = max(radius, min_radius);

    // Un corps plus petit qu'un pixel conserve (à peu près) sa luminosité
    // totale : on diminue son intensité au lieu de l'agrandir.
    var intensity = 1.0;
    if (radius < min_radius) {
        let k = radius / min_radius;
        intensity = max(k * k, u.look.y);
    }

    var out: VsOut;
    let offset = vec4<f32>(corner * size, 0.0, 0.0);
    out.clip = u.proj * (center + offset);
    out.uv = corner;
    // Le canal alpha porte un poids (×16) : 16/255 ↔ 1.
    let weight = in.color.a * (255.0 / 16.0);
    out.color = vec4<f32>(in.color.rgb * intensity * weight * u.look.x, 1.0);
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let d2 = dot(in.uv, in.uv);
    if (d2 > 1.0) {
        discard;
    }
    // Disque plein au cœur, halo doux sur le bord.
    let core = 1.0 - smoothstep(0.55, 1.0, sqrt(d2));
    let glow = exp(-4.0 * d2);
    let f = 0.65 * core + 0.35 * glow;
    return vec4<f32>(in.color.rgb * f, f);
}
