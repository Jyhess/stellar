@group(0) @binding(0) var<uniform> P: Params;
@group(0) @binding(1) var<storage, read_write> bodies: array<Body>;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if (i >= P.n) {
        return;
    }
    bodies[i].vel = bodies[i].vel + bodies[i].acc * P.half_dt;
}
