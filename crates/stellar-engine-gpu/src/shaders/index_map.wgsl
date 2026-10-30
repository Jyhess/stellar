@group(0) @binding(0) var<uniform> P: Params;
@group(0) @binding(1) var<storage, read> bodies: array<Body>;
@group(0) @binding(2) var<storage, read_write> index_of: array<u32>;

// Identifier → current index table, rebuilt after each reordering.
// Entries of vanished bodies are not erased: the reader checks that the
// body found carries the requested identifier.
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x < P.n) {
        let id = bodies[gid.x].id;
        if (id < arrayLength(&index_of)) {
            index_of[id] = gid.x;
        }
    }
}
