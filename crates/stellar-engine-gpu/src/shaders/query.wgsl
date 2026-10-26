@group(0) @binding(0) var<uniform> P: Params;
@group(0) @binding(1) var<uniform> Q: array<vec4<u32>, 4>; // jusqu'à 16 identifiants ; Q[0].x = nombre
@group(0) @binding(2) var<storage, read> bodies: array<Body>;
@group(0) @binding(3) var<storage, read_write> results: array<vec4<f32>>;

fn query_id(k: u32) -> u32 {
    let j = k + 1u;
    return Q[j / 4u][j % 4u];
}

// Positions des corps demandés (w = 1 si trouvé).
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= P.n) {
        return;
    }
    let id = bodies[gid.x].id;
    let count = min(Q[0].x, 15u);
    for (var k = 0u; k < count; k = k + 1u) {
        if (query_id(k) == id) {
            results[k] = vec4<f32>(bodies[gid.x].pos, 1.0);
        }
    }
}
