@group(0) @binding(0) var<uniform> P: Params;
@group(0) @binding(1) var<storage, read> partial: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read_write> bbox: array<vec4<f32>>;

var<workgroup> smin: array<vec3<f32>, 256>;
var<workgroup> smax: array<vec3<f32>, 256>;

// Un seul groupe : réduit les partiels de bbox_partial.
@compute @workgroup_size(256)
fn main(@builtin(local_invocation_id) lid: vec3<u32>) {
    var lo = vec3<f32>(3.0e38);
    var hi = vec3<f32>(-3.0e38);
    for (var k = lid.x; k < P.num_wg; k = k + 256u) {
        lo = min(lo, partial[2u * k].xyz);
        hi = max(hi, partial[2u * k + 1u].xyz);
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
        bbox[0] = vec4<f32>(smin[0], 0.0);
        bbox[1] = vec4<f32>(smax[0], 0.0);
    }
}
