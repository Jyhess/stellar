@group(0) @binding(0) var<uniform> P: Params;
@group(0) @binding(1) var<storage, read> parents: array<u32>;
@group(0) @binding(2) var<storage, read_write> level_count: array<atomic<u32>>;
@group(0) @binding(3) var<storage, read_write> node_depth: array<u32>;
@group(0) @binding(4) var<storage, read_write> node_slot: array<u32>;

const LEVELS: u32 = 72u;

// Profondeur de chaque nœud interne et rang dans son niveau.
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (P.n < 2u || gid.x >= P.n - 1u) {
        return;
    }
    var depth = 0u;
    var k = parents[gid.x];
    while (k != NONE && depth < LEVELS - 1u) {
        depth = depth + 1u;
        k = parents[k];
    }
    node_depth[gid.x] = depth;
    node_slot[gid.x] = atomicAdd(&level_count[depth], 1u);
}
