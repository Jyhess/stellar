// Rendu des corps : billboards circulaires lumineux, mélange additif.

struct Uniforms {
    view: mat4x4<f32>,
    proj: mat4x4<f32>,
    // x : hauteur du viewport (px), y : tan(fov/2), z : taille minimale (px), w : inutilisé
    screen: vec4<f32>,
    // x : luminosité, y : plancher d'intensité des poussières, z, w : inutilisés
    look: vec4<f32>,
    // Source directe : ancre (xyz) et rayon de base r0 (w)
    anchor: vec4<f32>,
    // Source directe : x = ln(masse maximale), 0 si elle vaut au plus 1
    mass: vec4<f32>,
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

// Billboard d'un corps : position relative à l'ancre, rayon, couleur et
// poids (1 pour un corps ordinaire).
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
    // Taille d'un pixel dans l'espace vue, à cette profondeur.
    let world_per_px = 2.0 * u.screen.y * depth / u.screen.x;
    let min_radius = 0.5 * u.screen.z * world_per_px;
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
    out.color = vec4<f32>(rgb * intensity * weight * u.look.x, 1.0);
    return out;
}

@vertex
fn vs_main(in: VsIn) -> VsOut {
    // Le canal alpha porte un poids (×16) : 16/255 ↔ 1.
    let weight = in.color.a * (255.0 / 16.0);
    return billboard(in.vertex, in.pos_radius.xyz, in.pos_radius.w, in.color.rgb, weight);
}

// Couleur selon la masse (échelle logarithmique), comme `mass_color` côté
// CPU : bleu pâle → blanc → jaune → orangé.
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

// Corps lus directement dans les tampons du moteur GPU.
@vertex
fn vs_bodies(in: BodyIn) -> VsOut {
    if (in.mass <= 0.0) {
        // Corps mort (fusionné ou éliminé, en attente de compaction) :
        // sommet hors du volume de vue, rien n'est dessiné.
        var out: VsOut;
        out.clip = vec4<f32>(0.0, 0.0, -2.0, 1.0);
        out.uv = vec2<f32>(0.0);
        out.color = vec4<f32>(0.0);
        return out;
    }
    let radius = u.anchor.w * pow(in.mass, 1.0 / 3.0);
    return billboard(in.vertex, in.pos - u.anchor.xyz, radius, mass_color(in.mass), 1.0);
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
