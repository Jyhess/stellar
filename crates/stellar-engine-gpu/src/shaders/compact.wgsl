@group(0) @binding(0) var<uniform> P: Params;
@group(0) @binding(1) var<storage, read> src: array<Body>;
@group(0) @binding(2) var<storage, read> offsets: array<u32>;
@group(0) @binding(3) var<storage, read_write> dst: array<Body>;

// Ne garde que les corps de masse non nulle (scan exclusif des drapeaux).
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x < P.n && src[gid.x].mass > 0.0) {
        dst[offsets[gid.x]] = src[gid.x];
    }
}
