@group(0) @binding(0) var<storage, read> level_count: array<u32>;
@group(0) @binding(1) var<storage, read_write> level_offset: array<u32>;
@group(0) @binding(2) var<storage, read_write> args: array<u32>;

const LEVELS: u32 = 72u;

// Décalages des niveaux et arguments de lancement indirect (un seul fil).
@compute @workgroup_size(1)
fn main() {
    var acc = 0u;
    for (var l = 0u; l < LEVELS; l = l + 1u) {
        let c = level_count[l];
        level_offset[l] = acc;
        acc = acc + c;
        args[3u * l] = (c + 255u) / 256u;
        args[3u * l + 1u] = 1u;
        args[3u * l + 2u] = 1u;
    }
}
