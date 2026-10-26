@group(0) @binding(0) var<uniform> P: Params;
@group(0) @binding(1) var<uniform> L: Pass; // a = niveau
@group(0) @binding(2) var<storage, read> bodies: array<Body>;
@group(0) @binding(3) var<storage, read> children: array<vec2<u32>>;
@group(0) @binding(4) var<storage, read> level_count: array<u32>;
@group(0) @binding(5) var<storage, read> level_offset: array<u32>;
@group(0) @binding(6) var<storage, read> node_list: array<u32>;
@group(0) @binding(7) var<storage, read_write> nodes: array<vec4<f32>>;

// Nœud : v0 = (centre de masse, masse), v1 = (min, max.x),
// v2 = (max.yz, balayé min.xy), v3 = (balayé min.z, balayé max).
struct Agg {
    com: vec3<f32>,
    mass: f32,
    bmin: vec3<f32>,
    bmax: vec3<f32>,
    smin: vec3<f32>,
    smax: vec3<f32>,
};

fn load(c: u32) -> Agg {
    var a: Agg;
    if ((c & LEAF) != 0u) {
        let b = bodies[c & ~LEAF];
        let r = P.r0 * pow(max(b.mass, 1.0e-30), 1.0 / 3.0);
        a.com = b.pos;
        a.mass = b.mass;
        a.bmin = b.pos;
        a.bmax = b.pos;
        a.smin = min(b.old, b.pos) - vec3<f32>(r);
        a.smax = max(b.old, b.pos) + vec3<f32>(r);
    } else {
        let v0 = nodes[4u * c];
        let v1 = nodes[4u * c + 1u];
        let v2 = nodes[4u * c + 2u];
        let v3 = nodes[4u * c + 3u];
        a.com = v0.xyz;
        a.mass = v0.w;
        a.bmin = v1.xyz;
        a.bmax = vec3<f32>(v1.w, v2.x, v2.y);
        a.smin = vec3<f32>(v2.z, v2.w, v3.x);
        a.smax = v3.yzw;
    }
    return a;
}

// Agrège un niveau de l'arbre (les niveaux sont traités du plus profond à
// la racine, chacun dans son propre lancement : aucune course mémoire).
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let level = L.a;
    if (gid.x >= level_count[level]) {
        return;
    }
    let k = node_list[level_offset[level] + gid.x];
    let ch = children[k];
    let a = load(ch.x);
    let b = load(ch.y);
    let m = a.mass + b.mass;
    var com = (a.com + b.com) * 0.5;
    if (m > 0.0) {
        com = (a.com * a.mass + b.com * b.mass) / m;
    }
    let bmin = min(a.bmin, b.bmin);
    let bmax = max(a.bmax, b.bmax);
    let smin = min(a.smin, b.smin);
    let smax = max(a.smax, b.smax);
    nodes[4u * k] = vec4<f32>(com, m);
    nodes[4u * k + 1u] = vec4<f32>(bmin, bmax.x);
    nodes[4u * k + 2u] = vec4<f32>(bmax.y, bmax.z, smin.x, smin.y);
    nodes[4u * k + 3u] = vec4<f32>(smin.z, smax);
}
