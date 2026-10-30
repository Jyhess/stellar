@group(0) @binding(0) var<uniform> P: Params;
@group(0) @binding(1) var<storage, read_write> bodies: array<Body>;
@group(0) @binding(2) var<storage, read> nodes: array<vec4<f32>>;
@group(0) @binding(3) var<storage, read> children: array<vec2<u32>>;

// Barnes-Hut traversal by groups: the 128 consecutive bodies of a
// workgroup (neighbours in Morton order) walk the tree together. A node is
// treated as a point if it is far enough from the box of the whole group
// (a stricter criterion than for each body alone); accepted nodes and
// bodies of leaves accumulate in a list in shared memory, which each
// thread then evaluates for its own body. The traversal is thus done once
// for 128 bodies, and tree reads are shared.
//
// The body itself is in the list (its leaf is never accepted as an
// approximation): its contribution to the potential, −m/ε, is subtracted
// at the end; its contribution to the acceleration is zero.
//
// The stack receives only internal nodes: when a node is opened, its leaf
// children go straight into the list.
//
// STACK (pending nodes, shared stack walked in blocks of GROUP), LIST and
// LIST_PAD are set by the engine according to the device's shared memory
// (module prefix).

const GROUP: u32 = 128u;

var<workgroup> stack: array<u32, STACK>;
// Interactions waiting for evaluation; flushed as soon as it exceeds
// LIST − 2·GROUP (one round adds at most two per thread). LIST_PAD = LIST
// + 4: padding to a multiple of 4 for the unrolled loop.
var<workgroup> list: array<vec4<f32>, LIST_PAD>;
// Bodies of the group met as leaves (self term to subtract).
var<workgroup> seen: array<u32, GROUP>;
var<workgroup> list_n: atomic<u32>;
var<workgroup> push: atomic<u32>;
// End of the last successful reservation in the stack.
var<workgroup> pushed: atomic<u32>;
var<workgroup> top: u32;
var<workgroup> pending: u32;

// Adds body j to the list.
fn add_body(j: u32, g0: u32) {
    let s = atomicAdd(&list_n, 1u);
    list[s] = vec4<f32>(bodies[j].pos, bodies[j].mass);
    if (j - g0 < GROUP) {
        seen[j - g0] = 1u;
    }
}

// Attraction of e (position, mass) on p: (acceleration, m/r).
fn interact(e: vec4<f32>, p: vec3<f32>) -> vec4<f32> {
    let d = e.xyz - p;
    let r2 = dot(d, d) + P.eps2;
    let inv = select(0.0, inverseSqrt(r2), r2 > 0.0);
    let mi = e.w * inv;
    return vec4<f32>(d * (mi * inv * inv), mi);
}

// Sum of the pending interactions (count rounded up to a multiple of 4,
// padding entries of zero mass); four independent accumulators to hide
// latency.
fn evaluate(count: u32, p: vec3<f32>) -> vec4<f32> {
    var s0 = vec4<f32>(0.0);
    var s1 = vec4<f32>(0.0);
    var s2 = vec4<f32>(0.0);
    var s3 = vec4<f32>(0.0);
    for (var k = 0u; k < count; k = k + 4u) {
        s0 = s0 + interact(list[k], p);
        s1 = s1 + interact(list[k + 1u], p);
        s2 = s2 + interact(list[k + 2u], p);
        s3 = s3 + interact(list[k + 3u], p);
    }
    return (s0 + s1) + (s2 + s3);
}

// Freezes the number of pending interactions and pads the list (thread 0).
fn close_list() {
    let n = atomicLoad(&list_n);
    for (var k = n; k < (n + 3u) / 4u * 4u; k = k + 1u) {
        list[k] = vec4<f32>(0.0);
    }
    pending = n;
}

@compute @workgroup_size(128)
fn main(
    @builtin(local_invocation_id) lid: vec3<u32>,
    @builtin(workgroup_id) wid: vec3<u32>,
    @builtin(num_workgroups) nwg: vec3<u32>,
) {
    let t = lid.x;
    let g0 = (wid.x + wid.y * nwg.x) * GROUP;
    let i = g0 + t;
    let valid = i < P.n;
    var p = vec3<f32>(0.0);
    var m = 0.0;
    if (valid) {
        p = bodies[i].pos;
        m = bodies[i].mass;
    }
    seen[t] = 0u;

    // Bounding box of the group (reduction in the list, still free).
    list[t] = vec4<f32>(select(vec3<f32>(3.0e38), p, valid), 0.0);
    list[t + GROUP] = vec4<f32>(select(vec3<f32>(-3.0e38), p, valid), 0.0);
    workgroupBarrier();
    for (var s = GROUP / 2u; s > 0u; s = s >> 1u) {
        if (t < s) {
            list[t] = min(list[t], list[t + s]);
            list[t + GROUP] = max(list[t + GROUP], list[t + GROUP + s]);
        }
        workgroupBarrier();
    }
    let lo = list[0].xyz;
    let hi = list[GROUP].xyz;
    workgroupBarrier();

    if (t == 0u) {
        stack[0] = 0u;
        top = select(0u, 1u, P.n >= 2u && g0 < P.n);
        atomicStore(&list_n, 0u);
    }

    // (acceleration, −potential)
    var sum = vec4<f32>(0.0);
    loop {
        let n_top = workgroupUniformLoad(&top);
        if (n_top == 0u) {
            break;
        }
        let take = min(n_top, GROUP);
        let base = n_top - take;
        var c = NONE;
        if (t < take) {
            c = stack[base + t];
        }
        if (t == 0u) {
            atomicStore(&push, base);
            atomicStore(&pushed, base);
        }
        workgroupBarrier();
        if (c != NONE) {
            let v0 = nodes[3u * c];
            let rc2 = nodes[3u * c + 1u].w;
            // Point of the group's box closest to the center of mass.
            let d = clamp(v0.xyz, lo, hi) - v0.xyz;
            var accept = dot(d, d) > rc2;
            if (!accept) {
                let ch = children[c];
                let ia = (ch.x & LEAF) == 0u;
                let ib = (ch.y & LEAF) == 0u;
                let k = select(0u, 1u, ia) + select(0u, 1u, ib);
                var room = true;
                if (k > 0u) {
                    let s = atomicAdd(&push, k);
                    room = s + k <= STACK;
                    if (room) {
                        var w = s;
                        if (ia) {
                            stack[w] = ch.x;
                            w = w + 1u;
                        }
                        if (ib) {
                            stack[w] = ch.y;
                        }
                        atomicMax(&pushed, s + k);
                    }
                }
                if (room) {
                    if (!ia) {
                        add_body(ch.x & ~LEAF, g0);
                    }
                    if (!ib) {
                        add_body(ch.y & ~LEAF, g0);
                    }
                } else {
                    // Stack full (pathological tree): approximation.
                    accept = true;
                }
            }
            if (accept) {
                let s = atomicAdd(&list_n, 1u);
                list[s] = v0;
            }
        }
        workgroupBarrier();
        if (t == 0u) {
            // Successful reservations are contiguous from base.
            top = atomicLoad(&pushed);
            close_list();
        }
        let count = workgroupUniformLoad(&pending);
        if (count > LIST - 2u * GROUP) {
            sum = sum + evaluate(count, p);
            workgroupBarrier();
            if (t == 0u) {
                atomicStore(&list_n, 0u);
            }
        }
    }
    if (t == 0u) {
        close_list();
    }
    let count = workgroupUniformLoad(&pending);
    sum = sum + evaluate(count, p);

    if (valid) {
        var own = 0.0;
        if (seen[t] != 0u && P.eps2 > 0.0) {
            own = m * inverseSqrt(P.eps2);
        }
        bodies[i].acc = sum.xyz * P.g;
        bodies[i].pot = (own - sum.w) * P.g;
    }
}
