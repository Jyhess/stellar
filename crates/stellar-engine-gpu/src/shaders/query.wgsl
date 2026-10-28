@group(0) @binding(0) var<uniform> P: Params;
@group(0) @binding(1) var<uniform> Q: Pass; // n = nombre d'identifiants
@group(0) @binding(2) var<storage, read> ids: array<u32>;
@group(0) @binding(3) var<storage, read> index_of: array<u32>;
@group(0) @binding(4) var<storage, read> bodies: array<Body>;
@group(0) @binding(5) var<storage, read_write> results: array<vec4<f32>>;

// Positions des corps demandés (w = 1 si présent), par la table des indices.
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let k = gid.x;
    if (k >= Q.n) {
        return;
    }
    let id = ids[k];
    var r = vec4<f32>(0.0);
    if (id < arrayLength(&index_of)) {
        let i = index_of[id];
        if (i < P.n && bodies[i].id == id && bodies[i].mass > 0.0) {
            r = vec4<f32>(bodies[i].pos, 1.0);
        }
    }
    results[k] = r;
}
