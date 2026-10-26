@group(0) @binding(0) var<uniform> S: Pass; // n
@group(0) @binding(1) var<storage, read_write> data: array<u32>;
@group(0) @binding(2) var<storage, read> sums: array<u32>;

@compute @workgroup_size(256)
fn main(
    @builtin(global_invocation_id) gid: vec3<u32>,
    @builtin(workgroup_id) wid: vec3<u32>,
) {
    if (gid.x < S.n) {
        data[gid.x] = data[gid.x] + sums[wid.x];
    }
}
