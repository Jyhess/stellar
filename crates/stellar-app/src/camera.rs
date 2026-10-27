//! Caméra : vol libre ou orbite autour d'une cible (corps ou barycentre).
//!
//! Repère du monde : Z vers le haut, main droite. L'orientation est libre
//! (quaternion) : la caméra peut basculer sur son axe de visée et faire le
//! tour complet d'une cible. Toutes les rotations se font dans le repère de
//! la caméra, telles qu'on les voit à l'écran.
//!
//! Repère local : X = avant, -Y = droite, Z = haut (l'orientation identité
//! regarde vers +X avec le haut vers +Z).
//!
//! Calculs en `f64` ; les matrices envoyées au GPU sont exprimées
//! relativement à une ancre.

use glam::{DMat4, DQuat, DVec3, EulerRot, Mat4};
use stellar_application::ViewState;
use stellar_domain::BodyId;

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
    /// Orbite autour du centre de gravité, à la distance `distance`.
    Barycenter {
        distance: f64,
    },
}

#[derive(Debug, Clone)]
pub struct Camera {
    pub position: DVec3,
    /// Passage du repère local de la caméra au repère du monde.
    pub orientation: DQuat,
    pub fov_y: f64,
    pub mode: Mode,
    /// Multiplicateur de vitesse : en vol libre, la caméra parcourt chaque
    /// seconde cette fraction de sa distance à la référence ; en orbite, la
    /// distance à la cible varie au même rythme (exponentiel).
    pub speed: f64,
}

impl Camera {
    /// Caméra regardant `target` depuis `position`, le haut vers +Z.
    pub fn looking_at(position: DVec3, target: DVec3, speed: f64) -> Self {
        let mut c = Self {
            position,
            orientation: DQuat::IDENTITY,
            fov_y: 60f64.to_radians(),
            mode: Mode::Free,
            speed,
        };
        c.set_upright(target - position);
        c
    }

    pub fn from_view(v: &ViewState, speed: f64) -> Self {
        Self {
            position: DVec3::from_array(v.position),
            orientation: DQuat::from_euler(EulerRot::ZYX, v.yaw, -v.pitch, v.roll).normalize(),
            fov_y: v.fov_y.clamp(MIN_FOV, MAX_FOV),
            mode: match (v.followed, v.barycenter) {
                (Some(target), _) => Mode::Orbit {
                    target,
                    distance: v.orbit_distance,
                },
                (None, true) => Mode::Barycenter {
                    distance: v.orbit_distance,
                },
                (None, false) => Mode::Free,
            },
            speed,
        }
    }

    pub fn to_view(&self) -> ViewState {
        let (followed, barycenter, orbit_distance) = match self.mode {
            Mode::Orbit { target, distance } => (Some(target), false, distance),
            Mode::Barycenter { distance } => (None, true, distance),
            Mode::Free => (None, false, 0.0),
        };
        let (yaw, neg_pitch, roll) = self.orientation.to_euler(EulerRot::ZYX);
        ViewState {
            position: self.position.to_array(),
            yaw,
            pitch: -neg_pitch,
            roll,
            fov_y: self.fov_y,
            followed,
            barycenter,
            orbit_distance,
        }
    }

    pub fn forward(&self) -> DVec3 {
        self.orientation * DVec3::X
    }

    pub fn right(&self) -> DVec3 {
        self.orientation * DVec3::NEG_Y
    }

    pub fn up(&self) -> DVec3 {
        self.orientation * DVec3::Z
    }

    /// Oriente le regard dans la direction `dir` par la plus petite rotation
    /// possible : la bascule de la vue est conservée autant que possible.
    pub fn look_towards(&mut self, dir: DVec3) {
        let Some(d) = dir.try_normalize() else { return };
        let arc = DQuat::from_rotation_arc(self.forward(), d);
        self.orientation = (arc * self.orientation).normalize();
    }

    /// Oriente le regard dans la direction `dir`, le haut de l'écran aussi
    /// près que possible de +Z.
    fn set_upright(&mut self, dir: DVec3) {
        let d = dir.normalize_or(DVec3::X);
        let yaw = d.y.atan2(d.x);
        let pitch = d.z.clamp(-1.0, 1.0).asin();
        self.orientation = DQuat::from_euler(EulerRot::ZYX, yaw, -pitch, 0.0);
    }

    /// Rotation à la souris, dans le repère de l'écran (radians) :
    /// `d_yaw` > 0 tourne vers la gauche, `d_pitch` > 0 lève le regard.
    pub fn rotate(&mut self, d_yaw: f64, d_pitch: f64) {
        let local = DQuat::from_rotation_z(d_yaw) * DQuat::from_rotation_y(-d_pitch);
        self.orientation = (self.orientation * local).normalize();
    }

    /// Bascule autour de l'axe de visée ; `angle` > 0 incline le haut de
    /// l'écran vers la droite.
    pub fn roll(&mut self, angle: f64) {
        self.orientation = (self.orientation * DQuat::from_rotation_x(angle)).normalize();
    }

    /// Angle de bascule (signé, autour de l'axe de visée) qui amènerait le
    /// haut de l'écran sur la projection de `normal`. `None` si `normal` est
    /// (presque) parallèle à l'axe de visée.
    pub fn roll_to(&self, normal: DVec3) -> Option<f64> {
        let f = self.forward();
        let target = (normal - f * normal.dot(f)).try_normalize()?;
        if normal.normalize_or_zero().dot(f).abs() > 0.999 {
            return None;
        }
        let up = self.up();
        // Rotation positive (autour de +avant) : le haut part vers la droite.
        let sin = up.cross(target).dot(f);
        let cos = up.dot(target);
        Some(sin.atan2(cos))
    }

    /// Déplacement en vol libre ; `local` = (droite, haut, avant).
    pub fn translate_local(&mut self, local: DVec3) {
        self.position += self.right() * local.x + self.up() * local.y + self.forward() * local.z;
    }

    /// Recalcule la position en mode orbite, autour de `target_pos`.
    pub fn place_in_orbit(&mut self, target_pos: DVec3) {
        if let Mode::Orbit { distance, .. } | Mode::Barycenter { distance } = self.mode {
            self.position = target_pos - self.forward() * distance;
        }
    }

    pub fn followed(&self) -> Option<BodyId> {
        match self.mode {
            Mode::Orbit { target, .. } => Some(target),
            Mode::Free | Mode::Barycenter { .. } => None,
        }
    }

    /// Distance à la cible en mode orbite (corps ou barycentre).
    pub fn orbit_distance(&self) -> Option<f64> {
        match self.mode {
            Mode::Orbit { distance, .. } | Mode::Barycenter { distance } => Some(distance),
            Mode::Free => None,
        }
    }

    pub fn is_orbiting(&self) -> bool {
        !matches!(self.mode, Mode::Free)
    }

    /// Matrice de vue en `f32`, relative à l'ancre.
    pub fn view_matrix(&self, anchor: DVec3) -> Mat4 {
        let eye = self.position - anchor;
        DMat4::look_to_rh(eye, self.forward(), self.up()).as_mat4()
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

    /// Coordonnées d'un point dans le repère de la caméra : (droite, haut, avant).
    pub fn to_local(&self, p: DVec3) -> DVec3 {
        let v = p - self.position;
        DVec3::new(v.dot(self.right()), v.dot(self.up()), v.dot(self.forward()))
    }

    /// Projection d'un point du repère caméra en coordonnées normalisées ;
    /// `None` s'il est derrière la caméra.
    pub fn project_local(&self, l: DVec3, aspect: f64) -> Option<(f64, f64)> {
        if l.z <= 0.0 {
            return None;
        }
        let t = (0.5 * self.fov_y).tan();
        Some((l.x / (l.z * t * aspect), l.y / (l.z * t)))
    }

    /// Projection d'un point du monde en coordonnées normalisées ; `None`
    /// s'il est derrière la caméra.
    pub fn project(&self, p: DVec3, aspect: f64) -> Option<(f64, f64)> {
        self.project_local(self.to_local(p), aspect)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: DVec3, b: DVec3) -> bool {
        (a - b).length() < 1e-9
    }

    #[test]
    fn looking_at_points_forward() {
        let c = Camera::looking_at(DVec3::new(-10.0, 0.0, 0.0), DVec3::ZERO, 1.0);
        assert!(close(c.forward(), DVec3::X));
        assert!(close(c.up(), DVec3::Z));
        assert!(close(c.right(), DVec3::NEG_Y));
        let (x, y) = c.project(DVec3::ZERO, 1.0).unwrap();
        assert!(x.abs() < 1e-9 && y.abs() < 1e-9);
    }

    #[test]
    fn basis_is_right_handed() {
        let mut c = Camera::looking_at(DVec3::new(3.0, -7.0, 2.0), DVec3::ZERO, 1.0);
        c.rotate(0.4, 0.9);
        c.roll(1.3);
        assert!(close(c.right().cross(c.up()), -c.forward()));
    }

    #[test]
    fn ray_and_projection_agree() {
        let mut c = Camera::looking_at(DVec3::new(3.0, -7.0, 2.0), DVec3::new(0.5, 0.2, -0.1), 1.0);
        c.roll(0.6);
        let (o, d) = c.ray(0.3, -0.4, 1.6);
        let (x, y) = c.project(o + d * 5.0, 1.6).unwrap();
        assert!((x - 0.3).abs() < 1e-9 && (y + 0.4).abs() < 1e-9);
    }

    #[test]
    fn mouse_follows_screen_axes_after_roll() {
        let mut c = Camera::looking_at(DVec3::ZERO, DVec3::X, 1.0);
        c.roll(std::f64::consts::FRAC_PI_2); // haut de l'écran vers -Y (ancienne droite)
        let up = c.up();
        c.rotate(0.0, 0.1);
        // Lever le regard le fait pencher vers le haut de l'écran.
        assert!(c.forward().dot(up) > 0.09);
    }

    #[test]
    fn full_loop_over_the_pole() {
        let mut c = Camera::looking_at(DVec3::ZERO, DVec3::X, 1.0);
        for _ in 0..100 {
            c.rotate(0.0, std::f64::consts::TAU / 100.0);
        }
        assert!(close(c.forward(), DVec3::X));
        assert!(close(c.up(), DVec3::Z));
    }

    #[test]
    fn view_state_round_trip_keeps_roll() {
        let mut c = Camera::looking_at(DVec3::new(1.0, 2.0, 3.0), DVec3::ZERO, 1.0);
        c.rotate(0.3, -0.2);
        c.roll(0.8);
        let back = Camera::from_view(&c.to_view(), 1.0);
        assert!(close(back.forward(), c.forward()));
        assert!(close(back.up(), c.up()));
    }

    #[test]
    fn roll_to_levels_the_horizon() {
        let mut c = Camera::looking_at(DVec3::ZERO, DVec3::new(1.0, 1.0, 0.3), 1.0);
        c.roll(-1.1);
        let a = c.roll_to(DVec3::Z).unwrap();
        assert!((a - 1.1).abs() < 1e-9);
        c.roll(a);
        assert!(c.roll_to(DVec3::Z).unwrap().abs() < 1e-9);
        // Visée selon la normale : rien à redresser.
        let c = Camera::looking_at(DVec3::ZERO, DVec3::NEG_Z, 1.0);
        assert!(c.roll_to(DVec3::Z).is_none());
    }

    #[test]
    fn look_towards_keeps_roll_when_possible() {
        let mut c = Camera::looking_at(DVec3::ZERO, DVec3::X, 1.0);
        c.roll(0.5);
        c.look_towards(DVec3::new(1.0, 0.05, 0.0));
        assert!(close(c.forward(), DVec3::new(1.0, 0.05, 0.0).normalize()));
        let a = c.roll_to(DVec3::Z).unwrap();
        assert!((a + 0.5).abs() < 1e-3);
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

    #[test]
    fn view_state_keeps_barycenter_follow_and_roll() {
        let mut c = Camera::looking_at(DVec3::new(1.0, 2.0, 3.0), DVec3::new(-4.0, 0.5, 1.0), 1.0);
        c.mode = Mode::Barycenter { distance: 7.0 };
        let v = c.to_view();
        assert!(v.barycenter && v.followed.is_none());
        let back = Camera::from_view(&v, 1.0);
        assert_eq!(back.mode, c.mode);
        assert!((back.forward() - c.forward()).length() < 1e-9);
        assert!((back.up() - c.up()).length() < 1e-9);
    }
}
