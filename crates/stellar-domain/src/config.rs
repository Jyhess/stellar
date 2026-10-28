use serde::{Deserialize, Serialize};

/// Profil de densité radiale d'un disque.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum DensityProfile {
    /// Densité surfacique uniforme.
    Uniform,
    /// Densité surfacique ∝ exp(−r / (scale·R)), tronquée au rayon R.
    Exponential { scale: f64 },
}

/// Forme du nuage initial.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum Distribution {
    /// Boule pleine de rayon R, densité uniforme.
    Sphere,
    /// Disque de rayon R dans le plan (x, y), d'épaisseur `thickness`·R.
    Disk {
        thickness: f64,
        profile: DensityProfile,
    },
}

/// Vitesse initiale des particules — modes exclusifs.
///
/// Les mouvements circulaires s'effectuent autour de l'axe vertical (Oz),
/// dans le sens trigonométrique.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum VelocityMode {
    /// Toutes les particules au repos.
    Rest,
    /// Direction isotrope aléatoire, norme uniforme dans [min, max].
    Random { min: f64, max: f64 },
    /// Rotation autour de Oz, norme uniforme dans [min, max].
    CircularFixed { min: f64, max: f64 },
    /// Rotation autour de Oz à la vitesse d'équilibre orbital (calculée sur la
    /// masse intérieure), multipliée par un facteur uniforme dans
    /// [1 − spread, 1 + spread].
    CircularOrbital { spread: f64 },
    /// Vitesse dirigée vers le centre, norme uniforme dans [min, max].
    Radial { min: f64, max: f64 },
}

/// Conditions initiales d'une nouvelle simulation.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct InitialConditions {
    pub count: usize,
    /// Rayon du nuage (sphère ou disque).
    pub radius: f64,
    pub seed: u64,
    pub distribution: Distribution,
    pub velocity: VelocityMode,
}

impl Default for InitialConditions {
    fn default() -> Self {
        Self {
            count: 1_000_000,
            radius: 100.0,
            seed: 42,
            distribution: Distribution::Sphere,
            velocity: VelocityMode::Rest,
        }
    }
}

impl InitialConditions {
    /// Volume occupé par le nuage initial.
    pub fn volume(&self) -> f64 {
        let r = self.radius;
        match self.distribution {
            Distribution::Sphere => 4.0 / 3.0 * std::f64::consts::PI * r.powi(3),
            Distribution::Disk { thickness, .. } => {
                std::f64::consts::PI * r * r * (thickness.max(1e-6) * r)
            }
        }
    }

    /// Distance moyenne entre particules voisines (≈ (V/N)^(1/3)).
    pub fn mean_spacing(&self) -> f64 {
        (self.volume() / self.count.max(1) as f64).cbrt()
    }

    /// Vitesse caractéristique du système, √(G·M/R).
    pub fn characteristic_speed(&self, g: f64) -> f64 {
        (g * self.count as f64 / self.radius.max(1e-12)).sqrt()
    }

    /// Temps de chute libre d'une sphère homogène de même densité moyenne.
    pub fn free_fall_time(&self, g: f64) -> f64 {
        let rho = self.count as f64 / self.volume().max(1e-300);
        (3.0 * std::f64::consts::PI / (32.0 * g * rho)).sqrt()
    }
}

/// Paramètres physiques et numériques — figés une fois la simulation lancée.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PhysicsParams {
    /// Constante de gravitation.
    pub g: f64,
    /// Pas de temps (fixe).
    pub dt: f64,
    /// Longueur d'adoucissement de Plummer ε.
    pub softening: f64,
    /// Critère d'ouverture de Barnes-Hut θ.
    pub theta: f64,
    /// Rayon r₀ d'une particule de masse 1 ; r = r₀·m^(1/3).
    pub base_radius: f64,
    /// Période (en pas) du nettoyage des corps éjectés ; 0 = désactivé.
    pub cleanup_period: u32,
    /// Un corps est éliminé s'il est non lié et à plus de
    /// `cleanup_distance_factor × reference_radius` du barycentre.
    pub cleanup_distance_factor: f64,
    /// Rayon de référence (rayon du nuage initial).
    pub reference_radius: f64,
    /// Amas éjectés (voir [`crate::ejection`]) : distance de liaison des
    /// groupes « amis d'amis ».
    pub cluster_link_length: f64,
    /// Un amas lié d'au moins ce nombre de corps est conservé (0 = critère
    /// ignoré).
    pub cluster_min_bodies: u32,
    /// … ou d'au moins cette part de la masse initiale (0 = critère ignoré).
    pub cluster_min_mass: f64,
}

/// ε suggéré, rapporté à la distance moyenne entre voisins.
pub const SOFTENING_PER_SPACING: f64 = 0.3;

impl PhysicsParams {
    /// Valeurs par défaut déduites des conditions initiales :
    /// - ε = 0,3 × distance moyenne entre voisins ;
    /// - r₀ = 1 % de cette distance ;
    /// - dt = t_ff / 500 (temps de chute libre) ;
    /// - amas éjectés : liaison à la distance moyenne entre voisins, au moins
    ///   10 corps ou 0,1 % de la masse initiale.
    pub fn suggested(initial: &InitialConditions, g: f64) -> Self {
        let spacing = initial.mean_spacing();
        Self {
            g,
            dt: initial.free_fall_time(g) / 500.0,
            softening: SOFTENING_PER_SPACING * spacing,
            theta: 0.7,
            base_radius: 0.01 * spacing,
            cleanup_period: 100,
            cleanup_distance_factor: 10.0,
            reference_radius: initial.radius,
            cluster_link_length: spacing,
            cluster_min_bodies: 10,
            cluster_min_mass: 1e-3,
        }
    }
}

impl Default for PhysicsParams {
    fn default() -> Self {
        Self::suggested(&InitialConditions::default(), 1.0)
    }
}

/// Configuration complète d'une simulation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SimulationConfig {
    pub initial: InitialConditions,
    pub physics: PhysicsParams,
    /// Identifiant du moteur de calcul (voir [`crate::EngineDescriptor`]).
    pub engine: String,
}

impl Default for SimulationConfig {
    fn default() -> Self {
        Self {
            initial: InitialConditions::default(),
            physics: PhysicsParams::default(),
            engine: "cpu-barnes-hut".to_owned(),
        }
    }
}
