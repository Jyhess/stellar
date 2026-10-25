//! Traces : trajectoires de quelques corps, enregistrées à chaque pas par le
//! thread de simulation à partir de leur activation.
//!
//! Les traces sont une aide visuelle propre à la session : elles ne sont pas
//! sauvegardées. Chaque trace garde deux mémoires :
//! - l'**historique complet**, borné à [`MAX_HISTORY`] points par décimation
//!   (un point sur deux est éliminé et le pas d'échantillonnage double) ;
//! - une **queue** récente à pleine résolution (un point par pas), d'au plus
//!   [`MAX_TAIL`] points.

use std::collections::VecDeque;

use stellar_domain::{BodyId, Vec3};

pub const MAX_HISTORY: usize = 4096;
pub const MAX_TAIL: usize = 5000;

#[derive(Debug, Clone)]
pub struct Trace {
    /// Corps tracé à l'activation.
    pub origin: BodyId,
    /// Corps actuellement suivi (après fusions) ; `None` s'il a été éliminé.
    pub current: Option<BodyId>,
    history: Vec<Vec3>,
    stride: u32,
    counter: u32,
    tail: VecDeque<Vec3>,
}

impl Trace {
    fn new(origin: BodyId) -> Self {
        Self {
            origin,
            current: Some(origin),
            history: Vec::new(),
            stride: 1,
            counter: 0,
            tail: VecDeque::new(),
        }
    }

    fn push(&mut self, p: Vec3) {
        self.tail.push_back(p);
        if self.tail.len() > MAX_TAIL {
            self.tail.pop_front();
        }
        self.counter += 1;
        if self.counter >= self.stride {
            self.counter = 0;
            self.history.push(p);
            if self.history.len() > MAX_HISTORY {
                // Décimation : on garde un point sur deux (dont le premier).
                let mut k = 0;
                self.history.retain(|_| {
                    k += 1;
                    k % 2 == 1
                });
                self.stride *= 2;
            }
        }
    }

    /// Historique complet (décimé), prolongé jusqu'à la dernière position.
    pub fn full(&self) -> Vec<Vec3> {
        let mut v = self.history.clone();
        if let Some(&last) = self.tail.back() {
            if v.last() != Some(&last) {
                v.push(last);
            }
        }
        v
    }

    /// Les `n` derniers points, à pleine résolution (un par pas).
    pub fn recent(&self, n: usize) -> Vec<Vec3> {
        let skip = self.tail.len().saturating_sub(n);
        self.tail.iter().skip(skip).copied().collect()
    }
}

/// Ensemble des traces actives.
#[derive(Debug, Clone, Default)]
pub struct TraceSet {
    pub traces: Vec<Trace>,
}

impl TraceSet {
    pub fn new(ids: impl IntoIterator<Item = BodyId>) -> Self {
        Self { traces: ids.into_iter().map(Trace::new).collect() }
    }

    /// Identifiants actuellement suivis (une entrée par trace encore active).
    pub fn current_ids(&self) -> Vec<BodyId> {
        self.traces.iter().filter_map(|t| t.current).collect()
    }

    /// Met à jour les corps suivis après un pas : `merges` liste les couples
    /// (absorbé, résultant), `removed` les corps éliminés.
    pub fn apply_fates(&mut self, merges: &[(BodyId, BodyId)], removed: &[BodyId]) {
        if merges.is_empty() && removed.is_empty() {
            return;
        }
        for t in &mut self.traces {
            if let Some(c) = t.current {
                if removed.contains(&c) {
                    t.current = None;
                } else if let Some(&(_, s)) = merges.iter().find(|(a, _)| *a == c) {
                    t.current = Some(s);
                }
            }
        }
    }

    /// Ajoute un point à chaque trace active. `positions` est aligné sur
    /// [`Self::current_ids`].
    pub fn push(&mut self, positions: &[Option<Vec3>]) {
        let mut k = 0;
        for t in &mut self.traces {
            if t.current.is_some() {
                if let Some(Some(p)) = positions.get(k) {
                    t.push(*p);
                }
                k += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn follows_merges_and_removals() {
        let mut t = TraceSet::new([1, 2]);
        t.push(&[Some(Vec3::ZERO), Some(Vec3::X)]);
        t.apply_fates(&[(2, 3)], &[1]);
        assert_eq!(t.traces[0].current, None);
        assert_eq!(t.traces[1].current, Some(3));
        assert_eq!(t.current_ids(), vec![3]);
        t.push(&[Some(Vec3::Y)]);
        assert_eq!(t.traces[1].full(), vec![Vec3::X, Vec3::Y]);
        assert_eq!(t.traces[0].full(), vec![Vec3::ZERO]);
    }

    #[test]
    fn history_is_bounded_and_tail_is_exact() {
        let mut t = TraceSet::new([7]);
        for s in 0..20_000 {
            t.push(&[Some(Vec3::new(s as f64, 0.0, 0.0))]);
        }
        let tr = &t.traces[0];
        let full = tr.full();
        assert!(full.len() <= MAX_HISTORY + 1);
        assert_eq!(full.first().unwrap().x, 0.0);
        assert_eq!(full.last().unwrap().x, 19_999.0);
        let recent = tr.recent(10);
        assert_eq!(recent.len(), 10);
        assert_eq!(recent[0].x, 19_990.0);
    }
}
