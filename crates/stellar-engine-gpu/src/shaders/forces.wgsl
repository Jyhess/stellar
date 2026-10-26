@group(0) @binding(0) var<uniform> P: Params;
@group(0) @binding(1) var<storage, read_write> bodies: array<Body>;
@group(0) @binding(2) var<storage, read> nodes: array<vec4<f32>>;
@group(0) @binding(3) var<storage, read> children: array<vec2<u32>>;

const STACK: u32 = 72u;

// Champ de gravité de Barnes-Hut : un nœud est assimilé à un point si
// d > diagonale/θ + |centre de masse − centre géométrique|.
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if (i >= P.n) {
        return;
    }
    let p = bodies[i].pos;
    var acc = vec3<f32>(0.0);
    var phi = 0.0;
    if (P.n >= 2u) {
        var stack: array<u32, 72>;
        var sp = 1u;
        stack[0] = 0u;
        while (sp > 0u) {
            sp = sp - 1u;
            let c = stack[sp];
            if ((c & LEAF) != 0u) {
                let j = c & ~LEAF;
                if (j != i) {
                    let bj = bodies[j];
                    let d = bj.pos - p;
                    let r2 = dot(d, d) + P.eps2;
                    if (r2 > 0.0) {
                        let inv = inverseSqrt(r2);
                        acc = acc + d * (bj.mass * inv * inv * inv);
                        phi = phi - bj.mass * inv;
                    }
                }
                continue;
            }
            let v0 = nodes[4u * c];
            let v1 = nodes[4u * c + 1u];
            let v2 = nodes[4u * c + 2u];
            let bmin = v1.xyz;
            let bmax = vec3<f32>(v1.w, v2.x, v2.y);
            // Taille du nœud : diagonale de sa boîte (plus prudente que le plus
            // grand côté, les boîtes d'un arbre binaire étant souvent allongées).
            let size = length(bmax - bmin);
            let center = (bmin + bmax) * 0.5;
            let rc = size * P.inv_theta + length(v0.xyz - center);
            let d = v0.xyz - p;
            let r2 = dot(d, d);
            if (r2 > rc * rc || sp + 2u > STACK) {
                let inv = inverseSqrt(r2 + P.eps2);
                acc = acc + d * (v0.w * inv * inv * inv);
                phi = phi - v0.w * inv;
            } else {
                let ch = children[c];
                stack[sp] = ch.x;
                stack[sp + 1u] = ch.y;
                sp = sp + 2u;
            }
        }
    }
    bodies[i].acc = acc * P.g;
    bodies[i].pot = phi * P.g;
}
