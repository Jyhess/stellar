use serde::{Deserialize, Serialize};

use crate::Vec3;

/// Identifiant stable d'un corps. Lors d'une fusion, le corps résultant
/// conserve l'identifiant du membre le plus massif.
pub type BodyId = u64;

/// Ensemble de corps, stocké en « structure de tableaux » (SoA) : chaque
/// attribut est un vecteur contigu, ce qui favorise la vectorisation et le
/// parallélisme dans les moteurs de calcul.
///
/// Invariant : tous les vecteurs ont la même longueur.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct BodySet {
    pub ids: Vec<BodyId>,
    pub positions: Vec<Vec3>,
    pub velocities: Vec<Vec3>,
    pub masses: Vec<f64>,
}

impl BodySet {
    pub fn with_capacity(n: usize) -> Self {
        Self {
            ids: Vec::with_capacity(n),
            positions: Vec::with_capacity(n),
            velocities: Vec::with_capacity(n),
            masses: Vec::with_capacity(n),
        }
    }

    pub fn len(&self) -> usize {
        self.ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    pub fn push(&mut self, id: BodyId, position: Vec3, velocity: Vec3, mass: f64) {
        self.ids.push(id);
        self.positions.push(position);
        self.velocities.push(velocity);
        self.masses.push(mass);
    }

    /// Vérifie l'invariant de longueur commune.
    pub fn is_consistent(&self) -> bool {
        let n = self.ids.len();
        self.positions.len() == n && self.velocities.len() == n && self.masses.len() == n
    }

    /// Indice du corps portant l'identifiant donné (recherche linéaire).
    pub fn index_of(&self, id: BodyId) -> Option<usize> {
        self.ids.iter().position(|&x| x == id)
    }

    pub fn total_mass(&self) -> f64 {
        self.masses.iter().sum()
    }

    /// Quantité de mouvement totale Σ m·v.
    pub fn momentum(&self) -> Vec3 {
        self.masses
            .iter()
            .zip(&self.velocities)
            .map(|(&m, &v)| v * m)
            .sum()
    }

    /// Barycentre (position du centre de masse). `None` si l'ensemble est vide.
    pub fn center_of_mass(&self) -> Option<Vec3> {
        let m = self.total_mass();
        if m <= 0.0 {
            return None;
        }
        let s: Vec3 = self
            .masses
            .iter()
            .zip(&self.positions)
            .map(|(&mi, &p)| p * mi)
            .sum();
        Some(s / m)
    }

    /// Énergie cinétique totale Σ ½ m v².
    pub fn kinetic_energy(&self) -> f64 {
        self.masses
            .iter()
            .zip(&self.velocities)
            .map(|(&m, v)| 0.5 * m * v.length_squared())
            .sum()
    }
}
