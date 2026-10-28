@group(0) @binding(0) var<uniform> P: Params;
@group(0) @binding(1) var<storage, read_write> bodies: array<Body>;
@group(0) @binding(2) var<storage, read> moments: array<vec4<f32>>;
@group(0) @binding(3) var<storage, read_write> removed: array<u32>;
@group(0) @binding(4) var<storage, read_write> counter: array<atomic<u32>>;

// Élimine les corps non liés ET au-delà de la distance limite du barycentre.
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if (i >= P.n) {
        return;
    }
    let m = moments[0].x;
    if (m <= 0.0) {
        return;
    }
    let cm = moments[0].yzw / m;
    let vcm = moments[1].xyz / m;
    let b = bodies[i];
    let dp = b.pos - cm;
    let dv = b.vel - vcm;
    if (b.mass > 0.0 && dot(dp, dp) > P.limit2 && 0.5 * dot(dv, dv) + b.pot > 0.0) {
        let slot = atomicAdd(&counter[0], 1u);
        removed[slot] = b.id;
        bodies[i].mass = 0.0;
    }
}
