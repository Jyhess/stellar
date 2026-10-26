@group(0) @binding(0) var<uniform> P: Params;
@group(0) @binding(1) var<storage, read> partial: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read_write> moments: array<vec4<f32>>;

var<workgroup> sa: array<vec4<f32>, 256>;
var<workgroup> sb: array<vec4<f32>, 256>;
var<workgroup> sc: array<vec4<f32>, 256>;

@compute @workgroup_size(256)
fn main(@builtin(local_invocation_id) lid: vec3<u32>) {
    var a = vec4<f32>(0.0);
    var b = vec4<f32>(0.0);
    var c = vec4<f32>(0.0);
    for (var k = lid.x; k < P.num_wg; k = k + 256u) {
        a = a + partial[3u * k];
        b = b + partial[3u * k + 1u];
        c = c + partial[3u * k + 2u];
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
        moments[0] = sa[0];
        moments[1] = sb[0];
        moments[2] = sc[0];
    }
}
