@group(0) @binding(0) var<uniform> S: Pass; // n
@group(0) @binding(1) var<storage, read_write> data: array<u32>;
@group(0) @binding(2) var<storage, read_write> sums: array<u32>;

var<workgroup> sh: array<u32, 256>;

// Scan exclusif par blocs de 256 ; total de chaque bloc dans `sums`.
@compute @workgroup_size(256)
fn main(
    @builtin(global_invocation_id) gid: vec3<u32>,
    @builtin(local_invocation_id) lid: vec3<u32>,
    @builtin(workgroup_id) wid: vec3<u32>,
) {
    var v = 0u;
    if (gid.x < S.n) {
        v = data[gid.x];
    }
    sh[lid.x] = v;
    workgroupBarrier();
    for (var off = 1u; off < 256u; off = off << 1u) {
        var t = 0u;
        if (lid.x >= off) {
            t = sh[lid.x - off];
        }
        workgroupBarrier();
        sh[lid.x] = sh[lid.x] + t;
        workgroupBarrier();
    }
    if (gid.x < S.n) {
        data[gid.x] = sh[lid.x] - v;
    }
    if (lid.x == 255u) {
        sums[wid.x] = sh[255];
    }
}
