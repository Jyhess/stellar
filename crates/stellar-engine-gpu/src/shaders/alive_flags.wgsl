@group(0) @binding(0) var<uniform> P: Params;
@group(0) @binding(1) var<storage, read> bodies: array<Body>;
@group(0) @binding(2) var<storage, read_write> flags: array<u32>;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x < P.n) {
        flags[gid.x] = select(0u, 1u, bodies[gid.x].mass > 0.0);
    }
}
