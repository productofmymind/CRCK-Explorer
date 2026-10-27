//! The orbit camera of the 3D views (the Viewer, the clothing preview): around a target, Z up.
use glam::{Mat4, Vec3};

pub struct Camera {
    pub target: Vec3,
    pub yaw: f32,
    pub pitch: f32,
    pub dist: f32,
}

impl Camera {
    pub fn new() -> Self {
        Self { target: Vec3::ZERO, yaw: -0.8, pitch: 0.35, dist: 5.0 }
    }

    pub fn eye(&self) -> Vec3 {
        self.target + self.dist * Vec3::new(self.pitch.cos() * self.yaw.cos(), self.pitch.cos() * self.yaw.sin(), self.pitch.sin())
    }

    pub fn view_proj(&self, aspect: f32) -> Mat4 {
        let view = Mat4::look_at_rh(self.eye(), self.target, Vec3::Z);
        let proj = Mat4::perspective_rh(45f32.to_radians(), aspect, (self.dist * 0.01).max(0.0005), self.dist * 200.0);
        proj * view
    }

    /// distance at which a sphere around the box fits the 45 degree vertical view
    pub fn fit_dist(mn: Vec3, mx: Vec3, aspect: f32) -> f32 {
        // a tall, narrow viewport is limited by its horizontal field of view
        let half = (22.5f32.to_radians().tan() * aspect.min(1.0)).atan();
        // a little closer than the sphere in a wide view; a long model in a narrow view needs the whole sphere
        let fill = if aspect < 1.3 { 1.0 } else { 0.85 };
        ((mx - mn).length() * 0.5 / half.sin() * fill).max(0.05)
    }

    pub fn pan(&mut self, dx: f32, dy: f32) {
        let fwd = (self.target - self.eye()).normalize();
        let right = fwd.cross(Vec3::Z).normalize_or_zero();
        let up = right.cross(fwd);
        self.target += (-right * dx + up * dy) * self.dist * 0.0016;
    }
}
