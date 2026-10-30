//! Camera: free flight or orbit around a target (body or barycenter).
//!
//! World frame: Z up, right-handed. Orientation is unrestricted
//! (quaternion): the camera can roll around its line of sight and go all
//! the way around a target. Every rotation happens in the camera's frame,
//! as seen on screen.
//!
//! Local frame: X = forward, -Y = right, Z = up (the identity orientation
//! looks towards +X with up towards +Z).
//!
//! Computation in `f64`; matrices sent to the GPU are expressed relative to
//! an anchor.

use glam::{DMat4, DQuat, DVec3, EulerRot, Mat4};
use stellar_application::ViewState;
use stellar_domain::BodyId;

pub const MIN_FOV: f64 = 0.2 * std::f64::consts::PI / 180.0;
pub const MAX_FOV: f64 = 120.0 * std::f64::consts::PI / 180.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Mode {
    Free,
    /// Orbit around body `target`, at distance `distance`.
    Orbit {
        target: BodyId,
        distance: f64,
    },
    /// Orbit around the center of gravity, at distance `distance`.
    Barycenter {
        distance: f64,
    },
}

#[derive(Debug, Clone)]
pub struct Camera {
    pub position: DVec3,
    /// Transformation from the camera's local frame to the world frame.
    pub orientation: DQuat,
    pub fov_y: f64,
    pub mode: Mode,
    /// Speed multiplier: in free flight, the camera travels this fraction of
    /// its distance to the reference every second; in orbit, the distance to
    /// the target changes at the same (exponential) rate.
    pub speed: f64,
}

impl Camera {
    /// Camera looking at `target` from `position`, up towards +Z.
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

    /// Turns the line of sight towards `dir` by the smallest possible rotation:
    /// the roll of the view is kept as much as possible.
    pub fn look_towards(&mut self, dir: DVec3) {
        let Some(d) = dir.try_normalize() else { return };
        let arc = DQuat::from_rotation_arc(self.forward(), d);
        self.orientation = (arc * self.orientation).normalize();
    }

    /// Turns the line of sight towards `dir`, with the top of the screen as
    /// close as possible to +Z.
    fn set_upright(&mut self, dir: DVec3) {
        let d = dir.normalize_or(DVec3::X);
        let yaw = d.y.atan2(d.x);
        let pitch = d.z.clamp(-1.0, 1.0).asin();
        self.orientation = DQuat::from_euler(EulerRot::ZYX, yaw, -pitch, 0.0);
    }

    /// Mouse rotation, in the screen's frame (radians): `d_yaw` > 0 turns
    /// left, `d_pitch` > 0 looks up.
    pub fn rotate(&mut self, d_yaw: f64, d_pitch: f64) {
        let local = DQuat::from_rotation_z(d_yaw) * DQuat::from_rotation_y(-d_pitch);
        self.orientation = (self.orientation * local).normalize();
    }

    /// Roll around the line of sight; `angle` > 0 tilts the top of the screen
    /// to the right.
    pub fn roll(&mut self, angle: f64) {
        self.orientation = (self.orientation * DQuat::from_rotation_x(angle)).normalize();
    }

    /// Roll angle (signed, around the line of sight) that would bring the top
    /// of the screen onto the projection of `normal`. `None` if `normal` is
    /// (nearly) parallel to the line of sight.
    pub fn roll_to(&self, normal: DVec3) -> Option<f64> {
        let f = self.forward();
        let target = (normal - f * normal.dot(f)).try_normalize()?;
        if normal.normalize_or_zero().dot(f).abs() > 0.999 {
            return None;
        }
        let up = self.up();
        // Positive rotation (around +forward): the top moves to the right.
        let sin = up.cross(target).dot(f);
        let cos = up.dot(target);
        Some(sin.atan2(cos))
    }

    /// Free-flight movement; `local` = (right, up, forward).
    pub fn translate_local(&mut self, local: DVec3) {
        self.position += self.right() * local.x + self.up() * local.y + self.forward() * local.z;
    }

    /// Recomputes the position in orbit mode, around `target_pos`.
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

    /// Distance to the target in orbit mode (body or barycenter).
    pub fn orbit_distance(&self) -> Option<f64> {
        match self.mode {
            Mode::Orbit { distance, .. } | Mode::Barycenter { distance } => Some(distance),
            Mode::Free => None,
        }
    }

    pub fn is_orbiting(&self) -> bool {
        !matches!(self.mode, Mode::Free)
    }

    /// View matrix in `f32`, relative to the anchor.
    pub fn view_matrix(&self, anchor: DVec3) -> Mat4 {
        let eye = self.position - anchor;
        DMat4::look_to_rh(eye, self.forward(), self.up()).as_mat4()
    }

    pub fn projection(&self, aspect: f32) -> Mat4 {
        Mat4::perspective_infinite_rh(self.fov_y as f32, aspect.max(1e-3), 1e-5)
    }

    /// Ray (origin, unit direction) through a point of the viewport given in
    /// normalized coordinates ([-1, 1], y up).
    pub fn ray(&self, ndc_x: f64, ndc_y: f64, aspect: f64) -> (DVec3, DVec3) {
        let t = (0.5 * self.fov_y).tan();
        let dir = self.forward() + self.right() * (ndc_x * t * aspect) + self.up() * (ndc_y * t);
        (self.position, dir.normalize())
    }

    /// Coordinates of a point in the camera's frame: (right, up, forward).
    pub fn to_local(&self, p: DVec3) -> DVec3 {
        let v = p - self.position;
        DVec3::new(v.dot(self.right()), v.dot(self.up()), v.dot(self.forward()))
    }

    /// Projection of a point of the camera frame to normalized coordinates;
    /// `None` if it is behind the camera.
    pub fn project_local(&self, l: DVec3, aspect: f64) -> Option<(f64, f64)> {
        if l.z <= 0.0 {
            return None;
        }
        let t = (0.5 * self.fov_y).tan();
        Some((l.x / (l.z * t * aspect), l.y / (l.z * t)))
    }

    /// Projection of a world point to normalized coordinates; `None` if it is
    /// behind the camera.
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
        c.roll(std::f64::consts::FRAC_PI_2); // top of the screen towards -Y (the former right)
        let up = c.up();
        c.rotate(0.0, 0.1);
        // Looking up tilts the view towards the top of the screen.
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
        // Looking along the normal: nothing to level.
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
