//! Traces : trajectoires de quelques corps, enregistrées à chaque pas par le
//! thread de simulation à partir de leur activation.
//!
//! Les traces sont une aide visuelle propre à la session : elles ne sont pas
//! sauvegardées. Chaque trace garde deux mémoires d'échantillons (pas, position) :
//! - l'**historique complet**, borné à [`MAX_HISTORY`] points par décimation
//!   (un point sur deux est éliminé et le pas d'échantillonnage double) ;
//! - une **queue** récente à pleine résolution (un point par pas), d'au plus
//!   [`MAX_TAIL`] points.
//!
//! Les traces peuvent être affichées **relativement à un corps de
//! référence** (voir [`relative_to`]) : chaque point devient
//! p(t) − r(t) + r(maintenant), ce qui fait apparaître les orbites autour de ce
//! corps. La trajectoire de la référence provient de sa propre trace si elle
//! est tracée, sinon d'une piste dédiée enregistrée depuis sa sélection.

use std::collections::VecDeque;

use stellar_domain::{BodyId, Vec3};

pub const MAX_HISTORY: usize = 4096;
pub const MAX_TAIL: usize = 5000;

/// Échantillon : (numéro de pas, position).
pub type Sample = (u64, Vec3);

#[derive(Debug, Clone)]
pub struct Trace {
    /// Corps tracé à l'activation.
    pub origin: BodyId,
    /// Corps actuellement suivi (après fusions) ; `None` s'il a été éliminé.
    pub current: Option<BodyId>,
    history: Vec<Sample>,
    stride: u32,
    counter: u32,
    tail: VecDeque<Sample>,
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

    fn push(&mut self, s: Sample) {
        if self.tail.back().is_some_and(|b| b.0 == s.0) {
            return; // un seul point par pas
        }
        self.tail.push_back(s);
        if self.tail.len() > MAX_TAIL {
            self.tail.pop_front();
        }
        self.counter += 1;
        if self.counter >= self.stride {
            self.counter = 0;
            self.history.push(s);
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
    pub fn full(&self) -> Vec<Sample> {
        let mut v = self.history.clone();
        if let Some(&last) = self.tail.back() {
            if v.last().map(|s| s.0) != Some(last.0) {
                v.push(last);
            }
        }
        v
    }

    /// Les `n` derniers points, à pleine résolution (un par pas).
    pub fn recent(&self, n: usize) -> Vec<Sample> {
        let skip = self.tail.len().saturating_sub(n);
        self.tail.iter().skip(skip).copied().collect()
    }

    /// Tous les échantillons connus, triés par pas : l'historique décimé
    /// pour le passé lointain, la queue à pleine résolution ensuite.
    pub fn samples(&self) -> Vec<Sample> {
        let first_tail = self.tail.front().map_or(u64::MAX, |s| s.0);
        let mut v: Vec<Sample> = self
            .history
            .iter()
            .copied()
            .filter(|s| s.0 < first_tail)
            .collect();
        v.extend(self.tail.iter().copied());
        v
    }

    /// Dernière position enregistrée.
    pub fn last(&self) -> Option<Sample> {
        self.tail.back().copied()
    }
}

/// Référentiel d'affichage relatif des traces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraceReference {
    /// Un corps (suivi à travers ses fusions).
    Body(BodyId),
    /// Le centre de gravité du système.
    Barycenter,
}

/// Ensemble des traces actives, plus les pistes de référence.
#[derive(Debug, Clone)]
pub struct TraceSet {
    pub traces: Vec<Trace>,
    /// Piste du corps de référence, quand il ne fait pas partie des traces.
    pub reference: Option<Trace>,
    /// Trajectoire du barycentre, enregistrée en permanence depuis
    /// l'activation des traces.
    pub barycenter: Trace,
}

impl TraceSet {
    pub fn new(ids: impl IntoIterator<Item = BodyId>) -> Self {
        let mut barycenter = Trace::new(BodyId::MAX);
        barycenter.current = None; // n'est pas un corps : jamais résolu ni recherché
        Self {
            traces: ids.into_iter().map(Trace::new).collect(),
            reference: None,
            barycenter,
        }
    }

    /// Ajoute un point à la trajectoire du barycentre.
    pub fn push_barycenter(&mut self, step: u64, p: Vec3) {
        self.barycenter.push((step, p));
    }

    /// Choisit le référentiel (corps ou barycentre).
    pub fn set_reference_kind(&mut self, reference: Option<TraceReference>) {
        match reference {
            Some(TraceReference::Body(id)) => self.set_reference(Some(id)),
            Some(TraceReference::Barycenter) | None => self.reference = None,
        }
    }

    /// Trajectoire connue de la référence, pour l'affichage relatif.
    pub fn samples_for(&self, reference: TraceReference) -> Option<Vec<Sample>> {
        match reference {
            TraceReference::Body(id) => self.reference_samples(id),
            TraceReference::Barycenter => {
                let s = self.barycenter.samples();
                (!s.is_empty()).then_some(s)
            }
        }
    }

    /// Corps dont les positions sont à enregistrer à chaque pas : traces
    /// actives, puis référence éventuelle.
    pub fn current_ids(&self) -> Vec<BodyId> {
        self.traces
            .iter()
            .chain(self.reference.iter())
            .filter_map(|t| t.current)
            .collect()
    }

    /// Choisit le corps de référence. Si ce corps est déjà tracé (et n'a pas
    /// fusionné dans un autre), sa trace sert de référence ; sinon une piste
    /// dédiée démarre maintenant.
    pub fn set_reference(&mut self, id: Option<BodyId>) {
        match id {
            Some(id) if self.own_trace(id).is_none() => {
                if self.reference.as_ref().and_then(|r| r.current) != Some(id) {
                    self.reference = Some(Trace::new(id));
                }
            }
            _ => self.reference = None,
        }
    }

    fn own_trace(&self, id: BodyId) -> Option<&Trace> {
        self.traces
            .iter()
            .find(|t| t.origin == id && t.current == Some(id))
    }

    /// Trajectoire connue du corps `id`, pour l'affichage relatif.
    pub fn reference_samples(&self, id: BodyId) -> Option<Vec<Sample>> {
        if let Some(t) = self.own_trace(id) {
            return Some(t.samples());
        }
        self.reference
            .as_ref()
            .filter(|r| r.current == Some(id))
            .map(Trace::samples)
    }

    /// Met à jour les corps suivis après un pas : `merges` liste les couples
    /// (absorbé, résultant), `removed` les corps éliminés.
    pub fn apply_fates(&mut self, merges: &[(BodyId, BodyId)], removed: &[BodyId]) {
        if merges.is_empty() && removed.is_empty() {
            return;
        }
        for t in self.traces.iter_mut().chain(self.reference.iter_mut()) {
            if let Some(c) = t.current {
                if removed.contains(&c) {
                    t.current = None;
                } else if let Some(&(_, s)) = merges.iter().find(|(a, _)| *a == c) {
                    t.current = Some(s);
                }
            }
        }
    }

    /// Met à jour les corps suivis à l'aide d'une fonction de résolution
    /// (identifiant actuel après fusions, `None` si éliminé).
    pub fn resolve_with(&mut self, resolve: impl Fn(BodyId) -> Option<BodyId>) {
        for t in self.traces.iter_mut().chain(self.reference.iter_mut()) {
            t.current = t.current.and_then(&resolve);
        }
    }

    /// Ajoute un point au pas `step`. `positions` est aligné sur
    /// [`Self::current_ids`].
    pub fn push(&mut self, step: u64, positions: &[Option<Vec3>]) {
        let mut k = 0;
        for t in self.traces.iter_mut().chain(self.reference.iter_mut()) {
            if t.current.is_some() {
                if let Some(Some(p)) = positions.get(k) {
                    t.push((step, *p));
                }
                k += 1;
            }
        }
    }
}

/// Position interpolée de la référence au pas `step`, si `step` est couvert.
fn interpolate(reference: &[Sample], step: u64) -> Option<Vec3> {
    let i = reference.partition_point(|s| s.0 < step);
    let after = reference.get(i)?;
    if after.0 == step {
        return Some(after.1);
    }
    let before = reference.get(i.checked_sub(1)?)?;
    let t = (step - before.0) as f64 / (after.0 - before.0) as f64;
    Some(before.1.lerp(after.1, t))
}

/// Exprime une trace dans le référentiel d'un corps : p(t) − r(t) + r(maintenant).
/// Les points antérieurs à la trajectoire connue de la référence donnent `None`
/// (le tracé est interrompu).
pub fn relative_to(points: &[Sample], reference: &[Sample]) -> Vec<Option<Vec3>> {
    let Some(&(_, now)) = reference.last() else {
        return vec![None; points.len()];
    };
    points
        .iter()
        .map(|&(step, p)| interpolate(reference, step).map(|r| p - r + now))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn follows_merges_and_removals() {
        let mut t = TraceSet::new([1, 2]);
        t.push(0, &[Some(Vec3::ZERO), Some(Vec3::X)]);
        t.apply_fates(&[(2, 3)], &[1]);
        assert_eq!(t.traces[0].current, None);
        assert_eq!(t.traces[1].current, Some(3));
        assert_eq!(t.current_ids(), vec![3]);
        t.push(1, &[Some(Vec3::Y)]);
        assert_eq!(t.traces[1].full(), vec![(0, Vec3::X), (1, Vec3::Y)]);
        assert_eq!(t.traces[0].full(), vec![(0, Vec3::ZERO)]);
    }

    #[test]
    fn history_is_bounded_and_tail_is_exact() {
        let mut t = TraceSet::new([7]);
        for s in 0..20_000u64 {
            t.push(s, &[Some(Vec3::new(s as f64, 0.0, 0.0))]);
        }
        let tr = &t.traces[0];
        let full = tr.full();
        assert!(full.len() <= MAX_HISTORY + 1);
        assert_eq!(full.first().unwrap().0, 0);
        assert_eq!(full.last().unwrap().0, 19_999);
        let recent = tr.recent(10);
        assert_eq!(recent.len(), 10);
        assert_eq!(recent[0].0, 19_990);
        let samples = tr.samples();
        assert!(samples.windows(2).all(|w| w[0].0 < w[1].0));
    }

    #[test]
    fn circular_orbit_becomes_a_circle_around_the_reference() {
        // Une étoile en translation uniforme, une planète en orbite autour.
        let star = |k: u64| Vec3::new(k as f64, 0.0, 0.0);
        let planet = |k: u64| {
            let a = k as f64 * 0.1;
            star(k) + Vec3::new(a.cos(), a.sin(), 0.0)
        };
        let mut t = TraceSet::new([1, 2]);
        for k in 0..100 {
            t.push(k, &[Some(star(k)), Some(planet(k))]);
        }
        let reference = t.reference_samples(1).unwrap();
        let rel = relative_to(&t.traces[1].full(), &reference);
        let now = star(99);
        for p in rel {
            let d = (p.unwrap() - now).length();
            assert!((d - 1.0).abs() < 1e-9, "distance {d}");
        }
        // La référence elle-même se réduit à un point.
        let own = relative_to(&t.traces[0].full(), &reference);
        assert!(own.iter().all(|p| (p.unwrap() - now).length() < 1e-9));
    }

    #[test]
    fn dedicated_reference_track_starts_at_selection() {
        let mut t = TraceSet::new([1]);
        t.push(0, &[Some(Vec3::ZERO)]);
        t.set_reference(Some(9));
        assert_eq!(t.current_ids(), vec![1, 9]);
        t.push(1, &[Some(Vec3::X), Some(Vec3::Y)]);
        let reference = t.reference_samples(9).unwrap();
        let rel = relative_to(&t.traces[0].full(), &reference);
        assert_eq!(rel, vec![None, Some(Vec3::X)]);
        // Une référence déjà tracée n'ouvre pas de piste dédiée.
        t.set_reference(Some(1));
        assert!(t.reference.is_none());
    }

    #[test]
    fn orbits_around_the_barycenter() {
        // Deux corps égaux en orbite autour d'un barycentre qui dérive.
        let mut t = TraceSet::new([1, 2]);
        for k in 0..50u64 {
            let a = k as f64 * 0.2;
            let c = Vec3::new(k as f64 * 0.5, 0.0, 0.0);
            let r = Vec3::new(a.cos(), a.sin(), 0.0);
            t.push(k, &[Some(c + r), Some(c - r)]);
            t.push_barycenter(k, c);
        }
        t.set_reference_kind(Some(TraceReference::Barycenter));
        assert!(t.reference.is_none());
        let reference = t.samples_for(TraceReference::Barycenter).unwrap();
        let now = Vec3::new(49.0 * 0.5, 0.0, 0.0);
        for tr in &t.traces {
            for p in relative_to(&tr.full(), &reference) {
                assert!(((p.unwrap() - now).length() - 1.0).abs() < 1e-9);
            }
        }
        // Le barycentre n'est jamais demandé au moteur.
        assert_eq!(t.current_ids(), vec![1, 2]);
    }

    #[test]
    fn interpolation_between_decimated_samples() {
        let r = vec![(0, Vec3::ZERO), (4, Vec3::new(4.0, 0.0, 0.0))];
        assert_eq!(interpolate(&r, 1), Some(Vec3::new(1.0, 0.0, 0.0)));
        assert_eq!(interpolate(&r, 5), None);
    }
}
