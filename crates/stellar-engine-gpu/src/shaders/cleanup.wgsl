@group(0) @binding(0) var<uniform> P: Params;
@group(0) @binding(1) var<storage, read> bodies: array<Body>;
@group(0) @binding(2) var<storage, read> moments: array<vec4<f32>>;
@group(0) @binding(3) var<storage, read_write> far_index: array<u32>;
@group(0) @binding(4) var<storage, read_write> far: array<Body>;
@group(0) @binding(5) var<storage, read_write> counter: array<atomic<u32>>;

// Repère les corps au-delà de la distance limite du barycentre : leur indice,
// et leur enregistrement tant que le tampon le permet. Le tri (corps non
// liés éliminés, amas liés conservés) est fait sur le CPU.
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
    let b = bodies[i];
    let dp = b.pos - cm;
    if (b.mass > 0.0 && dot(dp, dp) > P.limit2) {
        let slot = atomicAdd(&counter[0], 1u);
        far_index[slot] = i;
        if (slot < arrayLength(&far)) {
            far[slot] = b;
        }
    }
}
