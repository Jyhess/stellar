//! Caméra : vol libre (ZQSD + souris) ou orbite autour d'un corps suivi.
//!
//! Repère : Z vers le haut, main droite. Calculs en `f64` ; les matrices
//! envoyées au GPU sont exprimées relativement à une ancre.

use glam::{DMat4, DVec3, Mat4};
use stellar_application::ViewState;
use stellar_domain::BodyId;

const MAX_PITCH: f64 = 89.0 * std::f64::consts::PI / 180.0;
pub const MIN_FOV: f64 = 0.2 * std::f64::consts::PI / 180.0;
pub const MAX_FOV: f64 = 120.0 * std::f64::consts::PI / 180.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Mode {
    Free,
    /// Orbite autour du corps `target`, à la distance `distance`.
    Orbit {
        target: BodyId,
        distance: f64,
    },
}

#[derive(Debug, Clone)]
pub struct Camera {
    pub position: DVec3,
    pub yaw: f64,
    pub pitch: f64,
    pub fov_y: f64,
    pub mode: Mode,
    /// Vitesse de déplacement en vol libre (unités/s).
    pub speed: f64,
}

impl Camera {
    /// Caméra regardant `target` depuis `position`.
    pub fn looking_at(position: DVec3, target: DVec3, speed: f64) -> Self {
        let mut c = Self {
            position,
            yaw: 0.0,
            pitch: 0.0,
            fov_y: 60f64.to_radians(),
            mode: Mode::Free,
            speed,
        };
        c.look_towards(target - position);
        c
    }

    pub fn from_view(v: &ViewState, speed: f64) -> Self {
        Self {
            position: DVec3::from_array(v.position),
            yaw: v.yaw,
            pitch: v.pitch.clamp(-MAX_PITCH, MAX_PITCH),
            fov_y: v.fov_y.clamp(MIN_FOV, MAX_FOV),
            mode: match v.followed {
                Some(target) => Mode::Orbit {
                    target,
                    distance: v.orbit_distance,
                },
                None => Mode::Free,
            },
            speed,
        }
    }

    pub fn to_view(&self) -> ViewState {
        let (followed, orbit_distance) = match self.mode {
            Mode::Orbit { target, distance } => (Some(target), distance),
            Mode::Free => (None, 0.0),
        };
        ViewState {
            position: self.position.to_array(),
            yaw: self.yaw,
            pitch: self.pitch,
            fov_y: self.fov_y,
            followed,
            orbit_distance,
        }
    }

    pub fn forward(&self) -> DVec3 {
        let (sp, cp) = self.pitch.sin_cos();
        let (sy, cy) = self.yaw.sin_cos();
        DVec3::new(cp * cy, cp * sy, sp)
    }

    pub fn right(&self) -> DVec3 {
        self.forward().cross(DVec3::Z).normalize_or(DVec3::X)
    }

    pub fn up(&self) -> DVec3 {
        self.right().cross(self.forward())
    }

    /// Oriente le regard dans la direction `dir`.
    pub fn look_towards(&mut self, dir: DVec3) {
        let d = dir.normalize_or(DVec3::X);
        self.yaw = d.y.atan2(d.x);
        self.pitch = d.z.clamp(-1.0, 1.0).asin().clamp(-MAX_PITCH, MAX_PITCH);
    }

    /// Rotation à la souris (radians).
    pub fn rotate(&mut self, d_yaw: f64, d_pitch: f64) {
        self.yaw = (self.yaw + d_yaw).rem_euclid(std::f64::consts::TAU);
        self.pitch = (self.pitch + d_pitch).clamp(-MAX_PITCH, MAX_PITCH);
    }

    /// Déplacement en vol libre ; `local` = (droite, haut, avant).
    pub fn translate_local(&mut self, local: DVec3) {
        self.position += self.right() * local.x + self.up() * local.y + self.forward() * local.z;
    }

    /// Recalcule la position en mode orbite, autour de `target_pos`.
    pub fn place_in_orbit(&mut self, target_pos: DVec3) {
        if let Mode::Orbit { distance, .. } = self.mode {
            self.position = target_pos - self.forward() * distance;
        }
    }

    pub fn followed(&self) -> Option<BodyId> {
        match self.mode {
            Mode::Orbit { target, .. } => Some(target),
            Mode::Free => None,
        }
    }

    /// Matrice de vue en `f32`, relative à l'ancre.
    pub fn view_matrix(&self, anchor: DVec3) -> Mat4 {
        let eye = self.position - anchor;
        DMat4::look_to_rh(eye, self.forward(), DVec3::Z).as_mat4()
    }

    pub fn projection(&self, aspect: f32) -> Mat4 {
        Mat4::perspective_infinite_rh(self.fov_y as f32, aspect.max(1e-3), 1e-5)
    }

    /// Rayon (origine, direction unitaire) passant par un point du viewport
    /// exprimé en coordonnées normalisées ([-1, 1], y vers le haut).
    pub fn ray(&self, ndc_x: f64, ndc_y: f64, aspect: f64) -> (DVec3, DVec3) {
        let t = (0.5 * self.fov_y).tan();
        let dir = self.forward() + self.right() * (ndc_x * t * aspect) + self.up() * (ndc_y * t);
        (self.position, dir.normalize())
    }

    /// Projection d'un point du monde en coordonnées normalisées ; `None`
    /// s'il est derrière la caméra.
    pub fn project(&self, p: DVec3, aspect: f64) -> Option<(f64, f64)> {
        let v = p - self.position;
        let z = v.dot(self.forward());
        if z <= 0.0 {
            return None;
        }
        let t = (0.5 * self.fov_y).tan();
        Some((
            v.dot(self.right()) / (z * t * aspect),
            v.dot(self.up()) / (z * t),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn looking_at_points_forward() {
        let c = Camera::looking_at(DVec3::new(-10.0, 0.0, 0.0), DVec3::ZERO, 1.0);
        assert!((c.forward() - DVec3::X).length() < 1e-9);
        assert!((c.up() - DVec3::Z).length() < 1e-9);
        let (x, y) = c.project(DVec3::ZERO, 1.0).unwrap();
        assert!(x.abs() < 1e-9 && y.abs() < 1e-9);
    }

    #[test]
    fn ray_and_projection_agree() {
        let c = Camera::looking_at(DVec3::new(3.0, -7.0, 2.0), DVec3::new(0.5, 0.2, -0.1), 1.0);
        let (o, d) = c.ray(0.3, -0.4, 1.6);
        let (x, y) = c.project(o + d * 5.0, 1.6).unwrap();
        assert!((x - 0.3).abs() < 1e-9 && (y + 0.4).abs() < 1e-9);
    }

    #[test]
    fn orbit_keeps_distance() {
        let mut c = Camera::looking_at(DVec3::new(0.0, -5.0, 0.0), DVec3::ZERO, 1.0);
        c.mode = Mode::Orbit {
            target: 1,
            distance: 5.0,
        };
        c.rotate(0.7, 0.3);
        c.place_in_orbit(DVec3::new(1.0, 1.0, 1.0));
        assert!(((c.position - DVec3::ONE).length() - 5.0).abs() < 1e-9);
    }
}
