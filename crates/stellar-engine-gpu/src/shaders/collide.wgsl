@group(0) @binding(0) var<uniform> P: Params;
@group(0) @binding(1) var<storage, read> bodies: array<Body>;
@group(0) @binding(2) var<storage, read> nodes: array<vec4<f32>>;
@group(0) @binding(3) var<storage, read> children: array<vec2<u32>>;
@group(0) @binding(4) var<storage, read_write> pairs: array<PairRec>;
@group(0) @binding(5) var<storage, read_write> counter: array<atomic<u32>>;

fn radius(m: f32) -> f32 {
    return P.r0 * pow(max(m, 1.0e-30), 1.0 / 3.0);
}

// Distance minimale entre deux trajectoires rectilignes sur le pas.
fn swept_min_distance(a0: vec3<f32>, a1: vec3<f32>, b0: vec3<f32>, b1: vec3<f32>) -> f32 {
    let d0 = b0 - a0;
    let dv = (b1 - b0) - (a1 - a0);
    let dv2 = dot(dv, dv);
    var t = 0.0;
    if (dv2 > 0.0) {
        t = clamp(-dot(d0, dv) / dv2, 0.0, 1.0);
    }
    return length(d0 + dv * t);
}

fn overlaps(amin: vec3<f32>, amax: vec3<f32>, bmin: vec3<f32>, bmax: vec3<f32>) -> bool {
    return all(amin <= bmax) && all(bmin <= amax);
}

// Contacts balayés : chaque corps parcourt l'arbre des boîtes balayées et
// enregistre les paires (i < j) dont les trajectoires se rapprochent à
// moins de la somme des rayons.
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if (i >= P.n || P.n < 2u || P.r0 <= 0.0) {
        return;
    }
    let bi = bodies[i];
    let ri = radius(bi.mass);
    let qmin = min(bi.old, bi.pos) - vec3<f32>(ri);
    let qmax = max(bi.old, bi.pos) + vec3<f32>(ri);
    var stack: array<u32, 72>;
    var sp = 1u;
    stack[0] = 0u;
    while (sp > 0u) {
        sp = sp - 1u;
        let c = stack[sp];
        if ((c & LEAF) != 0u) {
            let j = c & ~LEAF;
            if (j > i) {
                let bj = bodies[j];
                let rj = radius(bj.mass);
                let jmin = min(bj.old, bj.pos) - vec3<f32>(rj);
                let jmax = max(bj.old, bj.pos) + vec3<f32>(rj);
                if (overlaps(qmin, qmax, jmin, jmax)
                    && swept_min_distance(bi.old, bi.pos, bj.old, bj.pos) <= ri + rj) {
                    let slot = atomicAdd(&counter[0], 1u);
                    if (slot < P.cap) {
                        pairs[slot] = PairRec(bi, bj, i, j, 0u, 0u);
                    }
                }
            }
            continue;
        }
        let v2 = nodes[4u * c + 2u];
        let v3 = nodes[4u * c + 3u];
        let smin = vec3<f32>(v2.z, v2.w, v3.x);
        let smax = v3.yzw;
        if (overlaps(qmin, qmax, smin, smax) && sp + 2u <= 72u) {
            let ch = children[c];
            stack[sp] = ch.x;
            stack[sp + 1u] = ch.y;
            sp = sp + 2u;
        }
    }
}
