@group(0) @binding(0) var<uniform> P: Params;
@group(0) @binding(1) var<storage, read> keys: array<u32>;
@group(0) @binding(2) var<storage, read_write> children: array<vec2<u32>>;
@group(0) @binding(3) var<storage, read_write> parents: array<u32>;

// Longueur du préfixe commun des clés i et j (indices départageant les
// clés égales) ; −1 hors limites.
fn delta(i: i32, j: i32) -> i32 {
    if (j < 0 || j >= i32(P.n)) {
        return -1;
    }
    let ki = keys[u32(i)];
    let kj = keys[u32(j)];
    if (ki == kj) {
        return 32 + i32(countLeadingZeros(u32(i) ^ u32(j)));
    }
    return i32(countLeadingZeros(ki ^ kj));
}

// Construction parallèle de l'arbre binaire radix (Karras, 2012).
// Nœuds internes 0..n−2 (racine 0) ; feuilles = corps triés (bit LEAF).
// parents : nœuds internes en [0, n−1), feuilles en [n−1, 2n−1).
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let n = P.n;
    if (n < 2u || gid.x >= n - 1u) {
        return;
    }
    let i = i32(gid.x);
    if (i == 0) {
        parents[0] = NONE;
    }
    var d = 1;
    if (delta(i, i + 1) - delta(i, i - 1) < 0) {
        d = -1;
    }
    let dmin = delta(i, i - d);
    var lmax = 2;
    while (delta(i, i + lmax * d) > dmin) {
        lmax = lmax * 2;
    }
    var l = 0;
    var t = lmax / 2;
    while (t >= 1) {
        if (delta(i, i + (l + t) * d) > dmin) {
            l = l + t;
        }
        t = t / 2;
    }
    let j = i + l * d;
    let dnode = delta(i, j);
    var s = 0;
    var step = l;
    loop {
        step = (step + 1) / 2;
        if (delta(i, i + (s + step) * d) > dnode) {
            s = s + step;
        }
        if (step <= 1) {
            break;
        }
    }
    let gamma = i + s * d + min(d, 0);
    var left = u32(gamma);
    var right = u32(gamma + 1);
    if (min(i, j) == gamma) {
        left = left | LEAF;
        parents[n - 1u + u32(gamma)] = u32(i);
    } else {
        parents[u32(gamma)] = u32(i);
    }
    if (max(i, j) == gamma + 1) {
        right = right | LEAF;
        parents[n - 1u + u32(gamma + 1)] = u32(i);
    } else {
        parents[u32(gamma + 1)] = u32(i);
    }
    children[gid.x] = vec2<u32>(left, right);
}
