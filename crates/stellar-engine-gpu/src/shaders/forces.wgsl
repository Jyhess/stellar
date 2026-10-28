@group(0) @binding(0) var<uniform> P: Params;
@group(0) @binding(1) var<storage, read_write> bodies: array<Body>;
@group(0) @binding(2) var<storage, read> nodes: array<vec4<f32>>;
@group(0) @binding(3) var<storage, read> children: array<vec2<u32>>;

// Parcours de Barnes-Hut par groupes : les 128 corps consécutifs d'un groupe
// de travail (voisins dans l'ordre de Morton) parcourent l'arbre ensemble.
// Un nœud est assimilé à un point s'il est assez loin de la boîte du groupe
// entier (critère plus strict que pour chaque corps isolément) ; nœuds
// acceptés et corps des feuilles s'accumulent dans une liste en mémoire
// partagée, que chaque fil évalue ensuite pour son corps. Le parcours est
// ainsi fait une fois pour 128 corps, et les lectures de l'arbre sont
// partagées.
//
// Le corps lui-même figure dans la liste (sa feuille n'est jamais acceptée
// comme approximation) : sa contribution au potentiel, −m/ε, est retirée à
// la fin ; celle à l'accélération est nulle.
//
// La pile ne reçoit que des nœuds internes : à l'ouverture d'un nœud, ses
// enfants feuilles passent directement dans la liste.
//
// STACK (nœuds en attente, pile partagée parcourue par blocs de GROUP),
// LIST et LIST_PAD sont fixés par le moteur selon la mémoire partagée de la
// carte (préfixe du module).

const GROUP: u32 = 128u;

var<workgroup> stack: array<u32, STACK>;
// Interactions en attente d'évaluation ; vidée dès qu'elle dépasse
// LIST − 2·GROUP (un tour en ajoute au plus deux par fil). LIST_PAD = LIST
// + 4 : bourrage à un multiple de 4 pour la boucle déroulée.
var<workgroup> list: array<vec4<f32>, LIST_PAD>;
// Corps du groupe rencontrés comme feuilles (terme propre à retirer).
var<workgroup> seen: array<u32, GROUP>;
var<workgroup> list_n: atomic<u32>;
var<workgroup> push: atomic<u32>;
// Fin de la dernière réservation réussie dans la pile.
var<workgroup> pushed: atomic<u32>;
var<workgroup> top: u32;
var<workgroup> pending: u32;

// Ajoute le corps j à la liste.
fn add_body(j: u32, g0: u32) {
    let s = atomicAdd(&list_n, 1u);
    list[s] = vec4<f32>(bodies[j].pos, bodies[j].mass);
    if (j - g0 < GROUP) {
        seen[j - g0] = 1u;
    }
}

// Attraction de e (position, masse) sur p : (accélération, m/r).
fn interact(e: vec4<f32>, p: vec3<f32>) -> vec4<f32> {
    let d = e.xyz - p;
    let r2 = dot(d, d) + P.eps2;
    let inv = select(0.0, inverseSqrt(r2), r2 > 0.0);
    let mi = e.w * inv;
    return vec4<f32>(d * (mi * inv * inv), mi);
}

// Somme des interactions en attente (count arrondi au multiple de 4
// supérieur, entrées de bourrage de masse nulle) ; quatre accumulateurs
// indépendants pour masquer la latence.
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

// Fige le nombre d'interactions en attente et bourre la liste (fil 0).
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

    // Boîte englobante du groupe (réduction dans la liste, encore libre).
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

    // (accélération, −potentiel)
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
            // Point de la boîte du groupe le plus proche du centre de masse.
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
                    // Pile pleine (arbre pathologique) : approximation.
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
            // Les réservations réussies sont contiguës à partir de base.
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
