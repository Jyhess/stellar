@group(0) @binding(0) var<uniform> P: Params;
@group(0) @binding(1) var<storage, read_write> bodies: array<Body>;

// Kick ½ dt puis drift dt ; mémorise la position de départ (balayage).
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if (i >= P.n) {
        return;
    }
    var b = bodies[i];
    b.vel = b.vel + b.acc * P.half_dt;
    b.old = b.pos;
    b.pos = b.pos + b.vel * P.dt;
    bodies[i] = b;
}
