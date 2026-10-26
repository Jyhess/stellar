@group(0) @binding(0) var<uniform> R: Pass; // n, shift, num_wg
@group(0) @binding(1) var<storage, read> keys: array<u32>;
@group(0) @binding(2) var<storage, read_write> counts: array<u32>;

var<workgroup> h: array<atomic<u32>, 16>;

// Histogramme des chiffres (4 bits) par groupe ; disposition [chiffre][groupe].
@compute @workgroup_size(256)
fn main(
    @builtin(global_invocation_id) gid: vec3<u32>,
    @builtin(local_invocation_id) lid: vec3<u32>,
    @builtin(workgroup_id) wid: vec3<u32>,
) {
    if (lid.x < 16u) {
        atomicStore(&h[lid.x], 0u);
    }
    workgroupBarrier();
    if (gid.x < R.n) {
        let d = (keys[gid.x] >> R.a) & 15u;
        atomicAdd(&h[d], 1u);
    }
    workgroupBarrier();
    if (lid.x < 16u) {
        counts[lid.x * R.b + wid.x] = atomicLoad(&h[lid.x]);
    }
}
