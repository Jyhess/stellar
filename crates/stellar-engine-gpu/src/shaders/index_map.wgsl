@group(0) @binding(0) var<uniform> P: Params;
@group(0) @binding(1) var<storage, read> bodies: array<Body>;
@group(0) @binding(2) var<storage, read_write> index_of: array<u32>;

// Table identifiant → indice courant, refaite après chaque réordonnancement.
// Les entrées des corps disparus ne sont pas effacées : la lecture vérifie
// que le corps trouvé porte bien l'identifiant demandé.
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x < P.n) {
        let id = bodies[gid.x].id;
        if (id < arrayLength(&index_of)) {
            index_of[id] = gid.x;
        }
    }
}
