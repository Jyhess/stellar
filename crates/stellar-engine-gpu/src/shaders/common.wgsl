// Structures partagées par tous les noyaux (préfixées à chaque module).

// Corps (48 octets). La position de début de pas, utile aux contacts
// balayés, se déduit de la vitesse de dérive : pos − vel·dt.
struct Body {
    pos: vec3<f32>,
    mass: f32,
    vel: vec3<f32>,
    id: u32,
    acc: vec3<f32>,
    pot: f32,
};

struct Params {
    n: u32,
    num_wg: u32,
    cap: u32,
    n_ops: u32,
    dt: f32,
    half_dt: f32,
    eps2: f32,
    inv_theta: f32,
    r0: f32,
    g: f32,
    limit2: f32,
    pad: f32,
};

// Paramètres propres à une passe (tri, scan, niveau d'arbre).
struct Pass {
    n: u32,
    a: u32,
    b: u32,
    c: u32,
};

struct PairRec {
    a: Body,
    b: Body,
    ia: u32,
    ib: u32,
    pad0: u32,
    pad1: u32,
};

struct MergeOp {
    index: u32,
    kind: u32,
    pad0: u32,
    pad1: u32,
    body: Body,
};

const LEAF: u32 = 0x80000000u;
const NONE: u32 = 0xffffffffu;
const WG: u32 = 256u;
