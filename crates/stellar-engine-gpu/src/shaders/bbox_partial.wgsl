@group(0) @binding(0) var<uniform> P: Params;
@group(0) @binding(1) var<storage, read> bodies: array<Body>;
@group(0) @binding(2) var<storage, read_write> partial: array<vec4<f32>>;

var<workgroup> smin: array<vec3<f32>, 256>;
var<workgroup> smax: array<vec3<f32>, 256>;

@compute @workgroup_size(256)
fn main(
    @builtin(global_invocation_id) gid: vec3<u32>,
    @builtin(local_invocation_id) lid: vec3<u32>,
    @builtin(workgroup_id) wid: vec3<u32>,
) {
    var lo = vec3<f32>(3.0e38);
    var hi = vec3<f32>(-3.0e38);
    if (gid.x < P.n) {
        lo = bodies[gid.x].pos;
        hi = lo;
    }
    smin[lid.x] = lo;
    smax[lid.x] = hi;
    workgroupBarrier();
    for (var s = 128u; s > 0u; s = s >> 1u) {
        if (lid.x < s) {
            smin[lid.x] = min(smin[lid.x], smin[lid.x + s]);
            smax[lid.x] = max(smax[lid.x], smax[lid.x + s]);
        }
        workgroupBarrier();
    }
    if (lid.x == 0u) {
        partial[2u * wid.x] = vec4<f32>(smin[0], 0.0);
        partial[2u * wid.x + 1u] = vec4<f32>(smax[0], 0.0);
    }
}
