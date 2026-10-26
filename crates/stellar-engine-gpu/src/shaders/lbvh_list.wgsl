@group(0) @binding(0) var<uniform> P: Params;
@group(0) @binding(1) var<storage, read> node_depth: array<u32>;
@group(0) @binding(2) var<storage, read> node_slot: array<u32>;
@group(0) @binding(3) var<storage, read> level_offset: array<u32>;
@group(0) @binding(4) var<storage, read_write> node_list: array<u32>;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (P.n < 2u || gid.x >= P.n - 1u) {
        return;
    }
    node_list[level_offset[node_depth[gid.x]] + node_slot[gid.x]] = gid.x;
}
