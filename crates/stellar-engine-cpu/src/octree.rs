//! Octree de Barnes-Hut, construit sur des corps triés par code de Morton.
//!
//! Les nœuds sont rangés en ordre préfixe (parcours en profondeur) : le
//! premier enfant d'un nœud interne le suit immédiatement, et `subtree`
//! (taille du sous-arbre) permet de sauter un sous-arbre entier. Le parcours
//! se fait donc sans pile ni pointeurs, de manière très favorable au cache.
//!
//! Le tri de Morton garantit que les corps d'un nœud sont contigus :
//! `[start, start + count)`.

use glam::DVec3;
use rayon::prelude::*;

use crate::morton::{Cube, BITS_PER_AXIS};

/// Nombre maximal de corps dans une feuille.
pub const LEAF_SIZE: usize = 16;
/// En dessous de ce nombre de corps, un sous-arbre est construit séquentiellement.
const PARALLEL_THRESHOLD: usize = 32_768;

#[derive(Debug, Clone, Copy)]
pub struct Node {
    /// Centre de masse.
    pub com: DVec3,
    pub mass: f64,
    /// Carré du rayon critique : au-delà, le nœud est assimilé à un point.
    /// r_crit = côté/θ + |centre de masse − centre géométrique|.
    pub crit2: f64,
    pub start: u32,
    pub count: u32,
    /// Nombre de nœuds du sous-arbre, nœud courant compris (1 = feuille).
    pub subtree: u32,
}

impl Node {
    #[inline]
    pub fn is_leaf(&self) -> bool {
        self.subtree == 1
    }
}

struct Ctx<'a> {
    codes: &'a [u64],
    pos: &'a [DVec3],
    mass: &'a [f64],
    inv_theta: f64,
}

/// Construit l'octree. `codes` doivent être triés et correspondre à `pos`.
pub fn build(codes: &[u64], pos: &[DVec3], mass: &[f64], cube: Cube, theta: f64) -> Vec<Node> {
    let mut out = Vec::with_capacity(pos.len() / 4 + 16);
    if pos.is_empty() {
        return out;
    }
    let ctx = Ctx {
        codes,
        pos,
        mass,
        inv_theta: 1.0 / theta.max(1e-3),
    };
    build_node(&ctx, 0, pos.len(), 0, cube.min, cube.side, &mut out);
    out
}

/// Bornes des 8 enfants d'une plage triée au niveau `level`.
fn child_ranges(codes: &[u64], start: usize, end: usize, level: u32) -> [(usize, usize); 8] {
    let shift = 3 * (BITS_PER_AXIS - level - 1);
    let slice = &codes[start..end];
    let mut bounds = [0usize; 9];
    bounds[8] = slice.len();
    for c in 1..8u64 {
        bounds[c as usize] = slice.partition_point(|&code| ((code >> shift) & 7) < c);
    }
    let mut r = [(0, 0); 8];
    for c in 0..8 {
        r[c] = (start + bounds[c], start + bounds[c + 1]);
    }
    r
}

#[inline]
fn child_min(cmin: DVec3, half: f64, c: usize) -> DVec3 {
    cmin + DVec3::new(
        (c & 1) as f64 * half,
        ((c >> 1) & 1) as f64 * half,
        ((c >> 2) & 1) as f64 * half,
    )
}

/// Nœud couvrant les corps `[start, end)` dans le cube (`cmin`, `side`).
fn make_node(
    ctx: &Ctx,
    com: DVec3,
    mass: f64,
    (cmin, side): (DVec3, f64),
    (start, end): (usize, usize),
    subtree: usize,
) -> Node {
    let center = cmin + DVec3::splat(0.5 * side);
    let r_crit = side * ctx.inv_theta + (com - center).length();
    Node {
        com,
        mass,
        crit2: r_crit * r_crit,
        start: start as u32,
        count: (end - start) as u32,
        subtree: subtree as u32,
    }
}

fn build_node(
    ctx: &Ctx,
    start: usize,
    end: usize,
    level: u32,
    cmin: DVec3,
    side: f64,
    out: &mut Vec<Node>,
) {
    let count = end - start;
    let me = out.len();

    if count <= LEAF_SIZE || level >= BITS_PER_AXIS {
        let mut m = 0.0;
        let mut mx = DVec3::ZERO;
        for j in start..end {
            m += ctx.mass[j];
            mx += ctx.pos[j] * ctx.mass[j];
        }
        let com = if m > 0.0 {
            mx / m
        } else {
            cmin + DVec3::splat(0.5 * side)
        };
        out.push(make_node(ctx, com, m, (cmin, side), (start, end), 1));
        return;
    }

    let ranges = child_ranges(ctx.codes, start, end, level);
    let half = 0.5 * side;
    // Emplacement réservé pour le nœud courant, complété après les enfants.
    out.push(Node {
        com: DVec3::ZERO,
        mass: 0.0,
        crit2: 0.0,
        start: 0,
        count: 0,
        subtree: 0,
    });

    let mut m = 0.0;
    let mut mx = DVec3::ZERO;

    if count >= PARALLEL_THRESHOLD {
        let subs: Vec<Vec<Node>> = ranges
            .par_iter()
            .enumerate()
            .filter(|(_, (a, b))| b > a)
            .map(|(c, &(a, b))| {
                let mut v = Vec::with_capacity((b - a) / 4 + 8);
                build_node(ctx, a, b, level + 1, child_min(cmin, half, c), half, &mut v);
                v
            })
            .collect();
        for sub in subs {
            m += sub[0].mass;
            mx += sub[0].com * sub[0].mass;
            out.extend(sub);
        }
    } else {
        for (c, &(a, b)) in ranges.iter().enumerate() {
            if b > a {
                let child = out.len();
                build_node(ctx, a, b, level + 1, child_min(cmin, half, c), half, out);
                m += out[child].mass;
                mx += out[child].com * out[child].mass;
            }
        }
    }

    let com = if m > 0.0 {
        mx / m
    } else {
        cmin + DVec3::splat(half)
    };
    let subtree = out.len() - me;
    out[me] = make_node(ctx, com, m, (cmin, side), (start, end), subtree);
}

/// Nombre maximal de corps d'un groupe du calcul des forces : nœud le plus
/// haut de l'arbre qui en contient au plus autant.
pub const GROUP_SIZE: usize = 32;

/// Groupes du calcul des forces, en ordre préfixe : leurs corps se suivent
/// et couvrent tous les corps, dans l'ordre.
pub fn groups(nodes: &[Node]) -> Vec<&Node> {
    let mut out = Vec::new();
    let mut k = 0;
    while k < nodes.len() {
        let n = &nodes[k];
        if n.count as usize <= GROUP_SIZE || n.is_leaf() {
            out.push(n);
            k += n.subtree as usize;
        } else {
            k += 1;
        }
    }
    out
}

/// Liste d'interactions d'un groupe, en structure de tableaux : nœuds
/// lointains (centre de masse, masse) et corps des feuilles voisines ; plus
/// les tableaux de travail des corps du groupe.
#[derive(Default)]
pub struct Interactions {
    x: Vec<f64>,
    y: Vec<f64>,
    z: Vec<f64>,
    m: Vec<f64>,
    px: Vec<f64>,
    py: Vec<f64>,
    pz: Vec<f64>,
    ax: Vec<f64>,
    ay: Vec<f64>,
    az: Vec<f64>,
    pot: Vec<f64>,
}

impl Interactions {
    fn clear(&mut self) {
        self.x.clear();
        self.y.clear();
        self.z.clear();
        self.m.clear();
    }

    #[inline]
    fn push(&mut self, p: DVec3, m: f64) {
        self.x.push(p.x);
        self.y.push(p.y);
        self.z.push(p.z);
        self.m.push(m);
    }

    pub fn len(&self) -> usize {
        self.m.len()
    }

    pub fn is_empty(&self) -> bool {
        self.m.is_empty()
    }
}

/// Champ (accélération et potentiel, sans le facteur G) subi par les corps
/// du groupe `group`, écrit dans `out` = (`acc`, `phi`) (une case par corps
/// du groupe), avec adoucissement de Plummer ε² = `eps2`.
///
/// L'arbre est descendu une seule fois pour tout le groupe. Un nœud est
/// assimilé à un point si son centre de masse est assez loin de la boîte
/// englobante du groupe (critère plus strict que pour chaque corps
/// isolément) ; les corps des feuilles proches sont pris un à un. La liste
/// ainsi dressée est ensuite évaluée pour chaque corps du groupe, en boucle
/// sans branchement (vectorisable). Le corps lui-même y figure : sa
/// contribution à l'accélération est nulle, celle au potentiel (−m/ε) est
/// retirée à la fin.
pub fn field_of_group(
    nodes: &[Node],
    pos: &[DVec3],
    mass: &[f64],
    group: &Node,
    eps2: f64,
    list: &mut Interactions,
    (acc, phi): (&mut [DVec3], &mut [f64]),
) {
    let s = group.start as usize;
    let e = s + group.count as usize;
    let count = e - s;
    let mut lo = DVec3::splat(f64::INFINITY);
    let mut hi = DVec3::splat(f64::NEG_INFINITY);
    for p in &pos[s..e] {
        lo = lo.min(*p);
        hi = hi.max(*p);
    }

    list.clear();
    let mut k = 0usize;
    let len = nodes.len();
    while k < len {
        let n = &nodes[k];
        let r2 = (n.com.clamp(lo, hi) - n.com).length_squared();
        if r2 > n.crit2 {
            // Nœud lointain pour tout le groupe : approximation monopolaire.
            list.push(n.com, n.mass);
            k += n.subtree as usize;
        } else if n.is_leaf() {
            // Feuille proche : somme directe.
            let a = n.start as usize;
            for j in a..a + n.count as usize {
                list.push(pos[j], mass[j]);
            }
            k += 1;
        } else {
            k += 1;
        }
    }

    // Évaluation : les corps du groupe sont parcourus dans la boucle
    // intérieure, chacun avec ses propres accumulateurs.
    let Interactions {
        x,
        y,
        z,
        m,
        px,
        py,
        pz,
        ax,
        ay,
        az,
        pot,
    } = list;
    for v in [&mut *px, &mut *py, &mut *pz] {
        v.clear();
    }
    for p in &pos[s..e] {
        px.push(p.x);
        py.push(p.y);
        pz.push(p.z);
    }
    for v in [&mut *ax, &mut *ay, &mut *az, &mut *pot] {
        v.clear();
        v.resize(count, 0.0);
    }
    let (px, py, pz) = (&px[..count], &py[..count], &pz[..count]);
    let (ax, ay, az, pot) = (
        &mut ax[..count],
        &mut ay[..count],
        &mut az[..count],
        &mut pot[..count],
    );
    let sources = Sources {
        x,
        y,
        z,
        m,
        eps2,
    };
    let targets = Targets {
        px,
        py,
        pz,
        ax,
        ay,
        az,
        pot,
    };
    #[cfg(target_arch = "x86_64")]
    {
        if std::is_x86_feature_detected!("avx2") && std::is_x86_feature_detected!("fma") {
            // SAFETY : jeu d'instructions vérifié à l'exécution.
            unsafe { evaluate_avx2(&sources, targets) };
        } else {
            evaluate(&sources, targets);
        }
    }
    #[cfg(not(target_arch = "x86_64"))]
    evaluate(&sources, targets);
    let own = if eps2 > 0.0 { 1.0 / eps2.sqrt() } else { 0.0 };
    for t in 0..count {
        acc[t] = DVec3::new(ax[t], ay[t], az[t]);
        phi[t] = pot[t] + mass[s + t] * own;
    }
}

/// Sources d'une évaluation (liste d'interactions).
struct Sources<'a> {
    x: &'a [f64],
    y: &'a [f64],
    z: &'a [f64],
    m: &'a [f64],
    eps2: f64,
}

/// Corps évalués : positions et accumulateurs.
struct Targets<'a> {
    px: &'a [f64],
    py: &'a [f64],
    pz: &'a [f64],
    ax: &'a mut [f64],
    ay: &'a mut [f64],
    az: &'a mut [f64],
    pot: &'a mut [f64],
}

/// Somme des interactions, corps du groupe dans la boucle intérieure.
#[inline(always)]
fn evaluate(s: &Sources, t: Targets) {
    let n = t.px.len();
    let (px, py, pz) = (&t.px[..n], &t.py[..n], &t.pz[..n]);
    let (ax, ay, az, pot) = (&mut t.ax[..n], &mut t.ay[..n], &mut t.az[..n], &mut t.pot[..n]);
    for j in 0..s.m.len() {
        let (xj, yj, zj, mj) = (s.x[j], s.y[j], s.z[j], s.m[j]);
        for k in 0..n {
            let dx = xj - px[k];
            let dy = yj - py[k];
            let dz = zj - pz[k];
            let r2 = dx * dx + dy * dy + dz * dz + s.eps2;
            let inv = if r2 > 0.0 { 1.0 / r2.sqrt() } else { 0.0 };
            let mi = mj * inv;
            let mi3 = mi * inv * inv;
            ax[k] += dx * mi3;
            ay[k] += dy * mi3;
            az[k] += dz * mi3;
            pot[k] -= mi;
        }
    }
}

/// [`evaluate`] en AVX2 et FMA : quatre corps du groupe par registre, leurs
/// accumulateurs restant en registres sur toute la liste ; le reste (moins
/// de quatre corps) passe par la version générale. L'inverse de la racine
/// part de l'estimation matérielle en simple précision, affinée par deux
/// itérations de Newton (erreur relative ~10⁻¹⁴, bien en deçà de celle de
/// Barnes-Hut), au lieu d'une racine et d'une division exactes, quatre fois
/// plus lentes.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,fma")]
unsafe fn evaluate_avx2(s: &Sources, t: Targets) {
    use std::arch::x86_64::*;
    let n = t.px.len();
    let blocks = n / 4;
    let len = s.m.len();
    let eps2 = _mm256_set1_pd(s.eps2);
    let zero = _mm256_setzero_pd();
    let half = _mm256_set1_pd(0.5);
    let three_halves = _mm256_set1_pd(1.5);
    // Domaine de la simple précision pour l'estimation initiale.
    let tiny = _mm256_set1_pd(1e-30);
    let huge = _mm256_set1_pd(1e30);
    for b in 0..blocks {
        let o = 4 * b;
        let px = _mm256_loadu_pd(t.px.as_ptr().add(o));
        let py = _mm256_loadu_pd(t.py.as_ptr().add(o));
        let pz = _mm256_loadu_pd(t.pz.as_ptr().add(o));
        let mut ax = zero;
        let mut ay = zero;
        let mut az = zero;
        let mut pot = zero;
        for j in 0..len {
            let dx = _mm256_sub_pd(_mm256_set1_pd(*s.x.get_unchecked(j)), px);
            let dy = _mm256_sub_pd(_mm256_set1_pd(*s.y.get_unchecked(j)), py);
            let dz = _mm256_sub_pd(_mm256_set1_pd(*s.z.get_unchecked(j)), pz);
            let r2 = _mm256_fmadd_pd(dx, dx, _mm256_fmadd_pd(dy, dy, _mm256_fmadd_pd(dz, dz, eps2)));
            // 1/√r² : estimation en simple précision (12 bits), puis deux
            // itérations de Newton en double (≈ 46 bits).
            let y = _mm256_cvtps_pd(_mm_rsqrt_ps(_mm256_cvtpd_ps(_mm256_min_pd(
                _mm256_max_pd(r2, tiny),
                huge,
            ))));
            let h = _mm256_mul_pd(half, r2);
            let y = _mm256_mul_pd(y, _mm256_fnmadd_pd(h, _mm256_mul_pd(y, y), three_halves));
            let y = _mm256_mul_pd(y, _mm256_fnmadd_pd(h, _mm256_mul_pd(y, y), three_halves));
            // r² nul (le corps lui-même sans adoucissement) : contribution nulle.
            let inv = _mm256_and_pd(y, _mm256_cmp_pd::<_CMP_GT_OQ>(r2, zero));
            let mi = _mm256_mul_pd(_mm256_set1_pd(*s.m.get_unchecked(j)), inv);
            let mi3 = _mm256_mul_pd(mi, _mm256_mul_pd(inv, inv));
            ax = _mm256_fmadd_pd(dx, mi3, ax);
            ay = _mm256_fmadd_pd(dy, mi3, ay);
            az = _mm256_fmadd_pd(dz, mi3, az);
            pot = _mm256_sub_pd(pot, mi);
        }
        _mm256_storeu_pd(t.ax.as_mut_ptr().add(o), ax);
        _mm256_storeu_pd(t.ay.as_mut_ptr().add(o), ay);
        _mm256_storeu_pd(t.az.as_mut_ptr().add(o), az);
        _mm256_storeu_pd(t.pot.as_mut_ptr().add(o), pot);
    }
    let o = 4 * blocks;
    if o < n {
        evaluate(
            s,
            Targets {
                px: &t.px[o..],
                py: &t.py[o..],
                pz: &t.pz[o..],
                ax: &mut t.ax[o..],
                ay: &mut t.ay[o..],
                az: &mut t.az[o..],
                pot: &mut t.pot[o..],
            },
        );
    }
}
