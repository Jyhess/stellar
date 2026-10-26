@group(0) @binding(0) var<uniform> R: Pass; // n, shift, num_wg
@group(0) @binding(1) var<storage, read> keys_in: array<u32>;
@group(0) @binding(2) var<storage, read> vals_in: array<u32>;
@group(0) @binding(3) var<storage, read> offsets: array<u32>;
@group(0) @binding(4) var<storage, read_write> keys_out: array<u32>;
@group(0) @binding(5) var<storage, read_write> vals_out: array<u32>;

var<workgroup> sh: array<u32, 256>;
var<workgroup> base: array<u32, 16>;

// Dispersion stable : rang local de chaque élément parmi ceux de même
// chiffre (scans par paires de chiffres, compteurs 16 bits empaquetés).
@compute @workgroup_size(256)
fn main(
    @builtin(global_invocation_id) gid: vec3<u32>,
    @builtin(local_invocation_id) lid: vec3<u32>,
    @builtin(workgroup_id) wid: vec3<u32>,
) {
    let valid = gid.x < R.n;
    var key = 0u;
    var val = 0u;
    var d = 16u;
    if (valid) {
        key = keys_in[gid.x];
        val = vals_in[gid.x];
        d = (key >> R.a) & 15u;
    }
    if (lid.x < 16u) {
        base[lid.x] = offsets[lid.x * R.b + wid.x];
    }
    var rank = 0u;
    for (var p = 0u; p < 8u; p = p + 1u) {
        let d0 = 2u * p;
        var f = 0u;
        if (d == d0) {
            f = 1u;
        }
        if (d == d0 + 1u) {
            f = 1u << 16u;
        }
        workgroupBarrier();
        sh[lid.x] = f;
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
        let incl = sh[lid.x];
        if (d == d0) {
            rank = (incl & 0xffffu) - 1u;
        }
        if (d == d0 + 1u) {
            rank = (incl >> 16u) - 1u;
        }
    }
    workgroupBarrier();
    if (valid) {
        let dst = base[d] + rank;
        keys_out[dst] = key;
        vals_out[dst] = val;
    }
}
