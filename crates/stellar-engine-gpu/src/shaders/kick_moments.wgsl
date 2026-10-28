@group(0) @binding(0) var<uniform> P: Params;
@group(0) @binding(1) var<storage, read_write> bodies: array<Body>;
@group(0) @binding(2) var<storage, read_write> partial: array<vec4<f32>>;

// Kick ½ dt, puis contribution du corps aux moments globaux (même réduction
// que moments_partial) : une seule lecture des corps pour les deux.
var<workgroup> sa: array<vec4<f32>, 256>;
var<workgroup> sb: array<vec4<f32>, 256>;
var<workgroup> sc: array<vec4<f32>, 256>;

@compute @workgroup_size(256)
fn main(
    @builtin(global_invocation_id) gid: vec3<u32>,
    @builtin(local_invocation_id) lid: vec3<u32>,
    @builtin(workgroup_id) wid: vec3<u32>,
) {
    var a = vec4<f32>(0.0);
    var b = vec4<f32>(0.0);
    var c = vec4<f32>(0.0);
    if (gid.x < P.n) {
        var body = bodies[gid.x];
        body.vel = body.vel + (body.acc + field_at(P.field_next, body.pos)) * P.half_dt;
        bodies[gid.x].vel = body.vel;
        let m = body.mass;
        a = vec4<f32>(m, body.pos * m);
        b = vec4<f32>(body.vel * m, 0.5 * m * dot(body.vel, body.vel));
        c = vec4<f32>(0.5 * m * body.pot, 0.0, 0.0, 0.0);
    }
    sa[lid.x] = a;
    sb[lid.x] = b;
    sc[lid.x] = c;
    workgroupBarrier();
    for (var s = 128u; s > 0u; s = s >> 1u) {
        if (lid.x < s) {
            sa[lid.x] = sa[lid.x] + sa[lid.x + s];
            sb[lid.x] = sb[lid.x] + sb[lid.x + s];
            sc[lid.x] = sc[lid.x] + sc[lid.x + s];
        }
        workgroupBarrier();
    }
    if (lid.x == 0u) {
        partial[3u * wid.x] = sa[0];
        partial[3u * wid.x + 1u] = sb[0];
        partial[3u * wid.x + 2u] = sc[0];
    }
}
