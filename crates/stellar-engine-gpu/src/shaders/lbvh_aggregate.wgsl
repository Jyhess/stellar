@group(0) @binding(0) var<uniform> P: Params;
@group(0) @binding(1) var<uniform> L: Pass; // a = niveau
@group(0) @binding(2) var<storage, read> bodies: array<Body>;
@group(0) @binding(3) var<storage, read> children: array<vec2<u32>>;
@group(0) @binding(4) var<storage, read> level_count: array<u32>;
@group(0) @binding(5) var<storage, read> level_offset: array<u32>;
@group(0) @binding(6) var<storage, read> node_list: array<u32>;
@group(0) @binding(7) var<storage, read_write> nodes: array<vec4<f32>>;
@group(0) @binding(8) var<storage, read_write> swept: array<vec4<f32>>;

// Nœud k : nodes[3k] = (centre de masse, masse), nodes[3k + 1] = (min, rc²),
// nodes[3k + 2] = (max, 0) ; swept[2k], swept[2k + 1] = boîte balayée
// (trajectoires du pas, rayons compris). rc est le rayon d'ouverture de
// Barnes-Hut : plus grand côté / θ + |centre de masse − centre géométrique|.
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
        let old = b.pos - b.vel * P.dt;
        a.smin = min(old, b.pos) - vec3<f32>(r);
        a.smax = max(old, b.pos) + vec3<f32>(r);
    } else {
        let v0 = nodes[3u * c];
        a.com = v0.xyz;
        a.mass = v0.w;
        a.bmin = nodes[3u * c + 1u].xyz;
        a.bmax = nodes[3u * c + 2u].xyz;
        a.smin = swept[2u * c].xyz;
        a.smax = swept[2u * c + 1u].xyz;
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
    let ext = bmax - bmin;
    let rc = max(ext.x, max(ext.y, ext.z)) * P.inv_theta + length(com - (bmin + bmax) * 0.5);
    nodes[3u * k] = vec4<f32>(com, m);
    nodes[3u * k + 1u] = vec4<f32>(bmin, rc * rc);
    nodes[3u * k + 2u] = vec4<f32>(bmax, 0.0);
    swept[2u * k] = vec4<f32>(min(a.smin, b.smin), 0.0);
    swept[2u * k + 1u] = vec4<f32>(max(a.smax, b.smax), 0.0);
}
