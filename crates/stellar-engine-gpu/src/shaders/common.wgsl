// Structures shared by every kernel (prepended to each module).

// Body (48 bytes). The start-of-step position, needed by swept contacts,
// is derived from the drift velocity: pos − vel·dt.
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
    // External field (subsystems) of the first and second half kicks.
    field_prev: Field,
    field_next: Field,
};

// Linear field a(x) = acc + T·(x − center); center.w = 0: no field.
struct Field {
    center: vec4<f32>,
    acc: vec4<f32>,
    t0: vec4<f32>,
    t1: vec4<f32>,
    t2: vec4<f32>,
};

fn field_at(f: Field, x: vec3<f32>) -> vec3<f32> {
    if (f.center.w == 0.0) {
        return vec3<f32>(0.0);
    }
    let d = x - f.center.xyz;
    return f.acc.xyz + vec3<f32>(dot(f.t0.xyz, d), dot(f.t1.xyz, d), dot(f.t2.xyz, d));
}

// Parameters specific to a pass (sort, scan, tree level).
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
