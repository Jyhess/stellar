@group(0) @binding(0) var<uniform> P: Params;
@group(0) @binding(1) var<storage, read> ops: array<MergeOp>;
@group(0) @binding(2) var<storage, read_write> bodies: array<Body>;

// Applique les fusions calculées côté CPU : remplacement du corps
// résultant, ou masse nulle pour les corps absorbés (supprimés ensuite).
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= P.n_ops) {
        return;
    }
    let op = ops[gid.x];
    if (op.kind == 0u) {
        bodies[op.index] = op.body;
    } else {
        bodies[op.index].mass = 0.0;
    }
}
