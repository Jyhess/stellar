@group(0) @binding(0) var<uniform> P: Params;
@group(0) @binding(1) var<storage, read> vals: array<u32>;
@group(0) @binding(2) var<storage, read> src: array<Body>;
@group(0) @binding(3) var<storage, read_write> dst: array<Body>;

// Réordonne les corps selon le tri de Morton.
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x < P.n) {
        dst[gid.x] = src[vals[gid.x]];
    }
}
