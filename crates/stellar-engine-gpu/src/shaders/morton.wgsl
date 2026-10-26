@group(0) @binding(0) var<uniform> P: Params;
@group(0) @binding(1) var<storage, read> bodies: array<Body>;
@group(0) @binding(2) var<storage, read> bbox: array<vec4<f32>>;
@group(0) @binding(3) var<storage, read_write> keys: array<u32>;
@group(0) @binding(4) var<storage, read_write> vals: array<u32>;

fn expand(v: u32) -> u32 {
    var x = v & 0x3ffu;
    x = (x | (x << 16u)) & 0x030000ffu;
    x = (x | (x << 8u)) & 0x0300f00fu;
    x = (x | (x << 4u)) & 0x030c30c3u;
    x = (x | (x << 2u)) & 0x09249249u;
    return x;
}

// Code de Morton 30 bits (10 bits par axe) dans la boîte englobante.
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if (i >= P.n) {
        return;
    }
    let lo = bbox[0].xyz;
    let ext = bbox[1].xyz - lo;
    let size = max(max(ext.x, max(ext.y, ext.z)), 1.0e-30);
    let t = clamp((bodies[i].pos - lo) / size * 1024.0, vec3<f32>(0.0), vec3<f32>(1023.0));
    let q = vec3<u32>(t);
    keys[i] = expand(q.x) | (expand(q.y) << 1u) | (expand(q.z) << 2u);
    vals[i] = i;
}
