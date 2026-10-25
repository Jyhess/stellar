//! Classement des corps par masse : ordre décroissant, départagé par
//! identifiant croissant (ordre total et stable).
//!
//! Toutes les fonctions sont en O(n) : aucun tri complet n'est nécessaire.

use std::cmp::Ordering;

use crate::BodyId;

/// Compare deux corps selon le rang : `Less` signifie « mieux classé »
/// (plus massif).
#[inline]
pub fn rank_cmp(a: (f64, BodyId), b: (f64, BodyId)) -> Ordering {
    b.0.total_cmp(&a.0).then(a.1.cmp(&b.1))
}

/// Indice du corps le plus massif.
pub fn heaviest(ids: &[BodyId], masses: &[f64]) -> Option<usize> {
    (0..ids.len()).min_by(|&i, &j| rank_cmp((masses[i], ids[i]), (masses[j], ids[j])))
}

/// Indice du corps classé juste après (`lighter = true`, masse inférieure)
/// ou juste avant (`lighter = false`) le corps de référence (masse, id).
pub fn neighbour(
    ids: &[BodyId],
    masses: &[f64],
    reference: (f64, BodyId),
    lighter: bool,
) -> Option<usize> {
    let mut best: Option<usize> = None;
    for i in 0..ids.len() {
        let key = (masses[i], ids[i]);
        let ord = rank_cmp(key, reference);
        let candidate = if lighter {
            ord == Ordering::Greater
        } else {
            ord == Ordering::Less
        };
        if !candidate {
            continue;
        }
        best = match best {
            None => Some(i),
            Some(b) => {
                let cb = rank_cmp(key, (masses[b], ids[b]));
                let better = if lighter {
                    cb == Ordering::Less
                } else {
                    cb == Ordering::Greater
                };
                if better {
                    Some(i)
                } else {
                    Some(b)
                }
            }
        };
    }
    best
}

/// Rang (1 = le plus massif) du corps de référence.
pub fn rank_of(ids: &[BodyId], masses: &[f64], reference: (f64, BodyId)) -> usize {
    1 + (0..ids.len())
        .filter(|&i| rank_cmp((masses[i], ids[i]), reference) == Ordering::Less)
        .count()
}

/// Les `k` corps les plus massifs, du plus lourd au plus léger : (id, masse).
pub fn top_k(ids: &[BodyId], masses: &[f64], k: usize) -> Vec<(BodyId, f64)> {
    if k == 0 {
        return Vec::new();
    }
    // Sélection en un passage avec un petit tampon trié (k est petit).
    let mut top: Vec<(f64, BodyId)> = Vec::with_capacity(k + 1);
    for i in 0..ids.len() {
        let key = (masses[i], ids[i]);
        if top.len() == k && rank_cmp(key, top[k - 1]) != Ordering::Less {
            continue;
        }
        let pos = top
            .binary_search_by(|probe| rank_cmp(*probe, key))
            .unwrap_or_else(|p| p);
        top.insert(pos, key);
        top.truncate(k);
    }
    top.into_iter().map(|(m, id)| (id, m)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> (Vec<BodyId>, Vec<f64>) {
        (vec![10, 11, 12, 13, 14], vec![1.0, 5.0, 3.0, 5.0, 2.0])
    }

    #[test]
    fn heaviest_breaks_ties_by_id() {
        let (ids, m) = sample();
        assert_eq!(heaviest(&ids, &m), Some(1)); // id 11, masse 5
    }

    #[test]
    fn walk_down_and_up() {
        let (ids, m) = sample();
        // Ordre attendu : 11(5) 13(5) 12(3) 14(2) 10(1)
        let mut cur = heaviest(&ids, &m).unwrap();
        let mut order = vec![ids[cur]];
        while let Some(n) = neighbour(&ids, &m, (m[cur], ids[cur]), true) {
            cur = n;
            order.push(ids[cur]);
        }
        assert_eq!(order, vec![11, 13, 12, 14, 10]);
        let up = neighbour(&ids, &m, (m[cur], ids[cur]), false).unwrap();
        assert_eq!(ids[up], 14);
        assert_eq!(rank_of(&ids, &m, (2.0, 14)), 4);
    }

    #[test]
    fn top_k_is_sorted() {
        let (ids, m) = sample();
        assert_eq!(top_k(&ids, &m, 3), vec![(11, 5.0), (13, 5.0), (12, 3.0)]);
        assert_eq!(top_k(&ids, &m, 10).len(), 5);
    }
}
