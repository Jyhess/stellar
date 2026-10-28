//! Sous-systèmes : une simulation découpée en amas calculés séparément.
//!
//! Un amas éjecté et détaché devient un sous-système à part, calculé par son
//! propre moteur dans son propre repère (origine près de son barycentre). Les
//! sous-systèmes avancent au même pas ; chacun ressent les autres par un
//! **champ extérieur linéaire** ([`ExternalField`]) : accélération au
//! barycentre et tenseur de marée, déduits des masses et barycentres des
//! autres (approximation monopolaire des sources, développement au premier
//! ordre autour de la cible).
//!
//! Critères (rapport taille / distance, avec hystérésis) :
//! - séparation d'un amas détecté par le nettoyage quand
//!   (taille de l'amas + taille du système) < [`SPLIT_RATIO`] × distance ;
//! - réunion de deux sous-systèmes quand
//!   (somme des tailles) > [`MERGE_RATIO`] × distance.

use std::any::Any;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::{BodyId, Vec3};

/// Seuil de séparation (taille / distance).
pub const SPLIT_RATIO: f64 = 0.3;
/// Seuil de réunion (taille / distance) ; nettement au-dessus du seuil de
/// séparation pour éviter les allers-retours.
pub const MERGE_RATIO: f64 = 0.6;

/// Champ gravitationnel extérieur, linéarisé autour de `center` (coordonnées
/// du moteur) : a(x) = `acceleration` + `tidal` · (x − `center`).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ExternalField {
    pub center: Vec3,
    pub acceleration: Vec3,
    /// Tenseur de marée (lignes), symétrique et de trace nulle.
    pub tidal: [[f64; 3]; 3],
}

impl ExternalField {
    /// Accélération au point `x`.
    pub fn at(&self, x: Vec3) -> Vec3 {
        let d = x - self.center;
        let t = &self.tidal;
        self.acceleration
            + Vec3::new(
                t[0][0] * d.x + t[0][1] * d.y + t[0][2] * d.z,
                t[1][0] * d.x + t[1][1] * d.y + t[1][2] * d.z,
                t[2][0] * d.x + t[2][1] * d.y + t[2][2] * d.z,
            )
    }

    /// Champ créé en `target` par une masse ponctuelle `mass` placée en
    /// `source` (adoucissement de Plummer `eps2`), linéarisé autour de
    /// `target`. Coordonnées quelconques, communes aux deux points.
    pub fn of_point_mass(g: f64, mass: f64, source: Vec3, target: Vec3, eps2: f64) -> Self {
        let r = target - source;
        let d2 = r.length_squared() + eps2;
        let inv = 1.0 / d2.sqrt();
        let inv3 = inv * inv * inv;
        let inv5 = inv3 * inv * inv;
        let gm = g * mass;
        let ra = [r.x, r.y, r.z];
        let mut tidal = [[0.0; 3]; 3];
        for (i, row) in tidal.iter_mut().enumerate() {
            for (j, t) in row.iter_mut().enumerate() {
                let delta = if i == j { 1.0 } else { 0.0 };
                *t = -gm * (delta * inv3 - 3.0 * ra[i] * ra[j] * inv5);
            }
        }
        Self {
            center: target,
            acceleration: -r * (gm * inv3),
            tidal,
        }
    }

    /// Somme de deux champs développés autour du même centre.
    pub fn add(&mut self, other: &Self) {
        self.acceleration += other.acceleration;
        for i in 0..3 {
            for j in 0..3 {
                self.tidal[i][j] += other.tidal[i][j];
            }
        }
    }

    /// Même champ, exprimé dans un repère décalé de `-shift`.
    pub fn shifted(mut self, shift: Vec3) -> Self {
        self.center -= shift;
        self
    }
}

/// Masse, barycentre et vitesse du barycentre d'un système (coordonnées du
/// moteur).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Moments {
    pub mass: f64,
    pub center: Vec3,
    pub velocity: Vec3,
}

/// Plage des corps d'un sous-système dans un état composite (corps rangés
/// par sous-système), et sa taille caractéristique.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SystemSpan {
    pub start: usize,
    pub len: usize,
    /// Taille caractéristique (rayon de référence du nettoyage et des
    /// critères de séparation et de réunion).
    pub size: f64,
}

/// Résumé d'un sous-système, pour l'affichage (coordonnées globales).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SystemSummary {
    /// Corps repère (le plus massif lors de la séparation).
    pub anchor: BodyId,
    pub bodies: usize,
    pub mass: f64,
    pub center: Vec3,
    pub velocity: Vec3,
    pub size: f64,
}

/// Élément d'un affichage direct composite : accès direct d'un moteur
/// (voir [`crate::PhysicsEngine::live_view`]) et origine de son repère.
#[derive(Clone)]
pub struct ViewPart {
    pub view: Arc<dyn Any + Send + Sync>,
    pub origin: Vec3,
}

/// Affichage direct de plusieurs sous-systèmes : la liste change quand des
/// sous-systèmes naissent, se réunissent ou sont recentrés.
#[derive(Default)]
pub struct SystemsView {
    parts: Mutex<Arc<Vec<ViewPart>>>,
}

impl SystemsView {
    pub fn parts(&self) -> Arc<Vec<ViewPart>> {
        Arc::clone(&self.parts.lock().expect("verrou empoisonné"))
    }

    pub fn set(&self, parts: Vec<ViewPart>) {
        *self.parts.lock().expect("verrou empoisonné") = Arc::new(parts);
    }
}
