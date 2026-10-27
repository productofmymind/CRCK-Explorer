//! Minimal linear algebra types (vectors, 4x4 matrices) for the software renderer.
//!
//! No external crate dependency — small and self-contained. A [`Mat4`]'s sixteen floats are in D3D
//! row-major memory order, the way RAGE stores a transform (a point transforms as `p * M`, the
//! translation in the fourth row); OpenGL would read the same floats as column-major.
use std::ops::{Add, Sub, Mul, Neg};

// ─── Vec2 ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Vec2 {
    pub x: f32,
    pub y: f32,
}

impl Vec2 {
    pub fn new(x: f32, y: f32) -> Self { Self { x, y } }
}

impl Add for Vec2 {
    type Output = Vec2;
    fn add(self, rhs: Vec2) -> Vec2 { Vec2::new(self.x + rhs.x, self.y + rhs.y) }
}

impl Sub for Vec2 {
    type Output = Vec2;
    fn sub(self, rhs: Vec2) -> Vec2 { Vec2::new(self.x - rhs.x, self.y - rhs.y) }
}

impl Mul<f32> for Vec2 {
    type Output = Vec2;
    fn mul(self, rhs: f32) -> Vec2 { Vec2::new(self.x * rhs, self.y * rhs) }
}

// ─── Vec3 ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Vec3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

impl Vec3 {
    pub const ZERO: Vec3 = Vec3 { x: 0.0, y: 0.0, z: 0.0 };
    pub const X: Vec3 = Vec3 { x: 1.0, y: 0.0, z: 0.0 };
    pub const Y: Vec3 = Vec3 { x: 0.0, y: 1.0, z: 0.0 };
    pub const Z: Vec3 = Vec3 { x: 0.0, y: 0.0, z: 1.0 };

    pub fn new(x: f32, y: f32, z: f32) -> Self { Self { x, y, z } }

    pub fn dot(self, rhs: Vec3) -> f32 {
        self.x * rhs.x + self.y * rhs.y + self.z * rhs.z
    }

    pub fn cross(self, rhs: Vec3) -> Vec3 {
        Vec3::new(
            self.y * rhs.z - self.z * rhs.y,
            self.z * rhs.x - self.x * rhs.z,
            self.x * rhs.y - self.y * rhs.x,
        )
    }

    pub fn length(self) -> f32 {
        self.dot(self).sqrt()
    }

    /// Returns a normalized copy, or `self` unchanged when its length is
    /// too small to normalize safely.
    pub fn normalize(self) -> Vec3 {
        let len = self.length();
        if len < 1e-8 {
            self
        } else {
            self * (1.0 / len)
        }
    }

    pub fn min(self, rhs: Vec3) -> Vec3 {
        Vec3::new(self.x.min(rhs.x), self.y.min(rhs.y), self.z.min(rhs.z))
    }

    pub fn max(self, rhs: Vec3) -> Vec3 {
        Vec3::new(self.x.max(rhs.x), self.y.max(rhs.y), self.z.max(rhs.z))
    }

    pub fn abs(self) -> Vec3 {
        Vec3::new(self.x.abs(), self.y.abs(), self.z.abs())
    }
}

impl Add for Vec3 {
    type Output = Vec3;
    fn add(self, rhs: Vec3) -> Vec3 { Vec3::new(self.x + rhs.x, self.y + rhs.y, self.z + rhs.z) }
}

impl Sub for Vec3 {
    type Output = Vec3;
    fn sub(self, rhs: Vec3) -> Vec3 { Vec3::new(self.x - rhs.x, self.y - rhs.y, self.z - rhs.z) }
}

impl Mul<f32> for Vec3 {
    type Output = Vec3;
    fn mul(self, rhs: f32) -> Vec3 { Vec3::new(self.x * rhs, self.y * rhs, self.z * rhs) }
}

impl Neg for Vec3 {
    type Output = Vec3;
    fn neg(self) -> Vec3 { Vec3::new(-self.x, -self.y, -self.z) }
}

// ─── Vec4 ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Vec4 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub w: f32,
}

impl Vec4 {
    pub fn new(x: f32, y: f32, z: f32, w: f32) -> Self { Self { x, y, z, w } }

    pub fn xyz(self) -> Vec3 { Vec3::new(self.x, self.y, self.z) }

    /// `Quaternion * Quaternion` as SharpDX defines it (the Hamilton product,
    /// `self` on the left), treating `self`/`rhs` as `(x, y, z, w)` quaternions.
    /// `parent * child` applies the child's rotation first.
    pub fn quat_mul(self, rhs: Vec4) -> Vec4 {
        let (l, r) = (self, rhs);
        Vec4::new(
            l.x * r.w + r.x * l.w + (l.y * r.z - l.z * r.y),
            l.y * r.w + r.y * l.w + (l.z * r.x - l.x * r.z),
            l.z * r.w + r.z * l.w + (l.x * r.y - l.y * r.x),
            l.w * r.w - (l.x * r.x + l.y * r.y + l.z * r.z),
        )
    }

    /// The reference tool's `Quaternion.Multiply(Vector3)`: rotates `v` by the
    /// `(x, y, z, w)` quaternion `self`.
    pub fn quat_rotate(self, v: Vec3) -> Vec3 {
        let (xx, yy, zz) = (self.x * 2.0 * self.x, self.y * 2.0 * self.y, self.z * 2.0 * self.z);
        let (wx, wy, wz) = (self.w * 2.0 * self.x, self.w * 2.0 * self.y, self.w * 2.0 * self.z);
        let (xy, xz, yz) = (self.x * 2.0 * self.y, self.x * 2.0 * self.z, self.y * 2.0 * self.z);
        Vec3::new(
            v.x * ((1.0 - yy) - zz) + v.y * (xy - wz) + v.z * (xz + wy),
            v.x * (xy + wz) + v.y * ((1.0 - xx) - zz) + v.z * (yz - wx),
            v.x * (xz - wy) + v.y * (yz + wx) + v.z * ((1.0 - xx) - yy),
        )
    }
}

impl Add for Vec4 {
    type Output = Vec4;
    fn add(self, rhs: Vec4) -> Vec4 { Vec4::new(self.x + rhs.x, self.y + rhs.y, self.z + rhs.z, self.w + rhs.w) }
}

impl Sub for Vec4 {
    type Output = Vec4;
    fn sub(self, rhs: Vec4) -> Vec4 { Vec4::new(self.x - rhs.x, self.y - rhs.y, self.z - rhs.z, self.w - rhs.w) }
}

impl Mul<f32> for Vec4 {
    type Output = Vec4;
    fn mul(self, rhs: f32) -> Vec4 { Vec4::new(self.x * rhs, self.y * rhs, self.z * rhs, self.w * rhs) }
}

// ─── Mat4 ───────────────────────────────────────────────────────────────────

/// A 4x4 matrix in D3D row-major memory order (`p * M`, translation in `m[12..15]`), which is the
/// same float order as OpenGL's column-major `m[col * 4 + row]` for the transposed matrix.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Mat4(pub [f32; 16]);

impl Mat4 {
    pub fn identity() -> Mat4 {
        let mut m = [0.0f32; 16];
        m[0] = 1.0;
        m[5] = 1.0;
        m[10] = 1.0;
        m[15] = 1.0;
        Mat4(m)
    }

    /// A matrix as RAGE stores it: D3D row-major, applied as `p * M`, with
    /// the translation in the fourth row. That is this type's memory order,
    /// so the floats are taken verbatim.
    pub fn from_d3d(rows: [f32; 16]) -> Mat4 {
        Mat4(rows)
    }

    pub fn from_translation(t: Vec3) -> Mat4 {
        Mat4::identity().with_translation(t)
    }

    pub fn translation(&self) -> Vec3 {
        Vec3::new(self.0[12], self.0[13], self.0[14])
    }

    /// The same rotation/scale with a different translation.
    pub fn with_translation(mut self, t: Vec3) -> Mat4 {
        self.0[12] = t.x;
        self.0[13] = t.y;
        self.0[14] = t.z;
        self
    }

    pub fn is_identity(&self) -> bool {
        *self == Mat4::identity()
    }

    /// Transforms a direction (implicit w = 0): rotation and scale only.
    pub fn transform_vector(&self, v: Vec3) -> Vec3 {
        Vec3::new(
            self.get(0, 0) * v.x + self.get(1, 0) * v.y + self.get(2, 0) * v.z,
            self.get(0, 1) * v.x + self.get(1, 1) * v.y + self.get(2, 1) * v.z,
            self.get(0, 2) * v.x + self.get(1, 2) * v.y + self.get(2, 2) * v.z,
        )
    }

    /// Row `i` (0..4) of the D3D matrix these floats are (`M{i+1}1..M{i+1}4`);
    /// the translation is row 3.
    pub fn row(&self, i: usize) -> Vec4 {
        Vec4::new(self.0[i * 4], self.0[i * 4 + 1], self.0[i * 4 + 2], self.0[i * 4 + 3])
    }

    /// Replaces row `i` (0..4).
    pub fn set_row(&mut self, i: usize, v: Vec4) {
        self.0[i * 4..i * 4 + 4].copy_from_slice(&[v.x, v.y, v.z, v.w]);
    }

    /// SharpDX `ScaleVector`: the diagonal `(M11, M22, M33)`.
    pub fn scale_vector(&self) -> Vec3 { Vec3::new(self.0[0], self.0[5], self.0[10]) }

    /// Sets the diagonal `(M11, M22, M33)`.
    pub fn set_scale_vector(&mut self, s: Vec3) {
        self.0[0] = s.x;
        self.0[5] = s.y;
        self.0[10] = s.z;
    }

    /// SharpDX `Column4`: `(M14, M24, M34, M44)`, the fourth component of each row.
    pub fn column4(&self) -> Vec4 { Vec4::new(self.0[3], self.0[7], self.0[11], self.0[15]) }

    /// Sets `(M14, M24, M34, M44)`.
    pub fn set_column4(&mut self, v: Vec4) {
        self.0[3] = v.x;
        self.0[7] = v.y;
        self.0[11] = v.z;
        self.0[15] = v.w;
    }

    /// SharpDX `Matrix.AffineTransformation(1.0f, rotation, translation)`: the
    /// row-vector rotation matrix of the `(x, y, z, w)` quaternion `q`, with
    /// `t` in row 3, so that `p * M` rotates and then translates.
    pub fn from_quat_pos(q: Vec4, t: Vec3) -> Mat4 {
        let (xx, yy, zz) = (q.x * q.x, q.y * q.y, q.z * q.z);
        let (xy, zw, zx, yw, yz, xw) = (q.x * q.y, q.z * q.w, q.z * q.x, q.y * q.w, q.y * q.z, q.x * q.w);
        Mat4([
            1.0 - 2.0 * (yy + zz), 2.0 * (xy + zw), 2.0 * (zx - yw), 0.0,
            2.0 * (xy - zw), 1.0 - 2.0 * (zz + xx), 2.0 * (yz + xw), 0.0,
            2.0 * (zx + yw), 2.0 * (yz - xw), 1.0 - 2.0 * (yy + xx), 0.0,
            t.x, t.y, t.z, 1.0,
        ])
    }

    /// SharpDX `Matrix.Invert`: the general 4x4 inverse, the zero matrix when singular.
    pub fn inverse(&self) -> Mat4 {
        let m = |r: usize, c: usize| self.0[(r - 1) * 4 + (c - 1)];
        let (m11, m12, m13, m14) = (m(1, 1), m(1, 2), m(1, 3), m(1, 4));
        let (m21, m22, m23, m24) = (m(2, 1), m(2, 2), m(2, 3), m(2, 4));
        let (m31, m32, m33, m34) = (m(3, 1), m(3, 2), m(3, 3), m(3, 4));
        let (m41, m42, m43, m44) = (m(4, 1), m(4, 2), m(4, 3), m(4, 4));
        let b0 = m31 * m42 - m32 * m41;
        let b1 = m31 * m43 - m33 * m41;
        let b2 = m34 * m41 - m31 * m44;
        let b3 = m32 * m43 - m33 * m42;
        let b4 = m34 * m42 - m32 * m44;
        let b5 = m33 * m44 - m34 * m43;
        let d11 = m22 * b5 + m23 * b4 + m24 * b3;
        let d12 = m21 * b5 + m23 * b2 + m24 * b1;
        let d13 = m21 * -b4 + m22 * b2 + m24 * b0;
        let d14 = m21 * b3 + m22 * -b1 + m23 * b0;
        let det = m11 * d11 - m12 * d12 + m13 * d13 - m14 * d14;
        if det == 0.0 { return Mat4([0.0; 16]); }
        let det = 1.0 / det;
        let a0 = m11 * m22 - m12 * m21;
        let a1 = m11 * m23 - m13 * m21;
        let a2 = m14 * m21 - m11 * m24;
        let a3 = m12 * m23 - m13 * m22;
        let a4 = m14 * m22 - m12 * m24;
        let a5 = m13 * m24 - m14 * m23;
        let d21 = m12 * b5 + m13 * b4 + m14 * b3;
        let d22 = m11 * b5 + m13 * b2 + m14 * b1;
        let d23 = m11 * -b4 + m12 * b2 + m14 * b0;
        let d24 = m11 * b3 + m12 * -b1 + m13 * b0;
        let d31 = m42 * a5 + m43 * a4 + m44 * a3;
        let d32 = m41 * a5 + m43 * a2 + m44 * a1;
        let d33 = m41 * -a4 + m42 * a2 + m44 * a0;
        let d34 = m41 * a3 + m42 * -a1 + m43 * a0;
        let d41 = m32 * a5 + m33 * a4 + m34 * a3;
        let d42 = m31 * a5 + m33 * a2 + m34 * a1;
        let d43 = m31 * -a4 + m32 * a2 + m34 * a0;
        let d44 = m31 * a3 + m32 * -a1 + m33 * a0;
        Mat4([
            d11 * det, -d21 * det, d31 * det, -d41 * det,
            -d12 * det, d22 * det, -d32 * det, d42 * det,
            d13 * det, -d23 * det, d33 * det, -d43 * det,
            -d14 * det, d24 * det, -d34 * det, d44 * det,
        ])
    }

    #[inline]
    fn get(&self, col: usize, row: usize) -> f32 {
        self.0[col * 4 + row]
    }

    /// Returns `self * rhs`.
    pub fn mul(&self, rhs: &Mat4) -> Mat4 {
        let mut out = [0.0f32; 16];
        for col in 0..4 {
            for row in 0..4 {
                let mut sum = 0.0f32;
                for k in 0..4 {
                    sum += self.get(k, row) * rhs.get(col, k);
                }
                out[col * 4 + row] = sum;
            }
        }
        Mat4(out)
    }

    /// Right-handed look-at matrix (view matrix), mapping world space to
    /// eye/view space with the camera looking down -Z.
    pub fn look_at_rh(eye: Vec3, target: Vec3, up: Vec3) -> Mat4 {
        let f = (target - eye).normalize(); // forward
        let s = f.cross(up).normalize();    // right
        let u = s.cross(f);                 // recomputed up

        let mut m = [0.0f32; 16];
        // Column 0
        m[0] = s.x;
        m[1] = u.x;
        m[2] = -f.x;
        m[3] = 0.0;
        // Column 1
        m[4] = s.y;
        m[5] = u.y;
        m[6] = -f.y;
        m[7] = 0.0;
        // Column 2
        m[8] = s.z;
        m[9] = u.z;
        m[10] = -f.z;
        m[11] = 0.0;
        // Column 3 (translation)
        m[12] = -s.dot(eye);
        m[13] = -u.dot(eye);
        m[14] = f.dot(eye);
        m[15] = 1.0;

        Mat4(m)
    }

    /// OpenGL-style right-handed perspective projection with NDC z in [-1, 1].
    pub fn perspective_rh(fov_y_radians: f32, aspect: f32, near: f32, far: f32) -> Mat4 {
        let tan_half_fov = (fov_y_radians * 0.5).tan();
        let mut m = [0.0f32; 16];

        m[0] = 1.0 / (aspect * tan_half_fov);
        m[5] = 1.0 / tan_half_fov;
        m[10] = -(far + near) / (far - near);
        m[11] = -1.0;
        m[14] = -(2.0 * far * near) / (far - near);

        Mat4(m)
    }

    /// Transforms a point (implicit w = 1), returning the full homogeneous
    /// result (caller performs the perspective divide if needed).
    pub fn transform_point(&self, p: Vec3) -> Vec4 {
        Vec4::new(
            self.get(0, 0) * p.x + self.get(1, 0) * p.y + self.get(2, 0) * p.z + self.get(3, 0),
            self.get(0, 1) * p.x + self.get(1, 1) * p.y + self.get(2, 1) * p.z + self.get(3, 1),
            self.get(0, 2) * p.x + self.get(1, 2) * p.y + self.get(2, 2) * p.z + self.get(3, 2),
            self.get(0, 3) * p.x + self.get(1, 3) * p.y + self.get(2, 3) * p.z + self.get(3, 3),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cross_x_y_is_z() {
        assert_eq!(Vec3::X.cross(Vec3::Y), Vec3::Z);
    }

    #[test]
    fn normalize_has_unit_length() {
        let v = Vec3::new(3.0, 4.0, 0.0).normalize();
        assert!((v.length() - 1.0).abs() < 1e-6);
    }

    #[test]
    fn normalize_zero_vector_unchanged() {
        let v = Vec3::ZERO.normalize();
        assert_eq!(v, Vec3::ZERO);
    }

    #[test]
    fn identity_times_vector_is_unchanged() {
        let m = Mat4::identity();
        let p = Vec3::new(1.0, 2.0, 3.0);
        let r = m.transform_point(p);
        assert_eq!(r, Vec4::new(1.0, 2.0, 3.0, 1.0));
    }

    #[test]
    fn perspective_maps_near_and_far_to_ndc_bounds() {
        let near = 0.1f32;
        let far = 100.0f32;
        let proj = Mat4::perspective_rh(std::f32::consts::FRAC_PI_2, 1.0, near, far);

        let p_near = proj.transform_point(Vec3::new(0.0, 0.0, -near));
        let ndc_near_z = p_near.z / p_near.w;
        assert!((ndc_near_z - (-1.0)).abs() < 1e-4, "near ndc z = {ndc_near_z}");

        let p_far = proj.transform_point(Vec3::new(0.0, 0.0, -far));
        let ndc_far_z = p_far.z / p_far.w;
        assert!((ndc_far_z - 1.0).abs() < 1e-4, "far ndc z = {ndc_far_z}");
    }

    /// RAGE stores transforms as D3D row-major matrices applied as `p * M`,
    /// with the translation in the fourth row. Read verbatim they must move
    /// a point by that translation and rotate it by the upper 3x3.
    #[test]
    fn d3d_matrix_applies_rotation_then_row_four_translation() {
        // 90° about Z (x -> y), translated by (10, 20, 30).
        let m = Mat4::from_d3d([
            0.0, 1.0, 0.0, 0.0,
            -1.0, 0.0, 0.0, 0.0,
            0.0, 0.0, 1.0, 0.0,
            10.0, 20.0, 30.0, 1.0,
        ]);
        let p = m.transform_point(Vec3::new(1.0, 0.0, 0.0));
        assert!((p.x - 10.0).abs() < 1e-6 && (p.y - 21.0).abs() < 1e-6 && (p.z - 30.0).abs() < 1e-6, "{p:?}");
        assert_eq!(m.translation(), Vec3::new(10.0, 20.0, 30.0));
    }

    #[test]
    fn transform_vector_ignores_translation() {
        let m = Mat4::from_translation(Vec3::new(5.0, 6.0, 7.0));
        assert_eq!(m.transform_vector(Vec3::X), Vec3::X);
        assert_eq!(m.transform_point(Vec3::ZERO).xyz(), Vec3::new(5.0, 6.0, 7.0));
    }

    #[test]
    fn identity_is_detected() {
        assert!(Mat4::identity().is_identity());
        assert!(!Mat4::from_translation(Vec3::X).is_identity());
    }

    #[test]
    fn with_translation_replaces_only_the_last_column() {
        let m = Mat4::from_translation(Vec3::X).with_translation(Vec3::new(1.0, 2.0, 3.0));
        assert_eq!(m.translation(), Vec3::new(1.0, 2.0, 3.0));
        assert_eq!(m.transform_vector(Vec3::Y), Vec3::Y);
    }

    #[test]
    fn quaternion_matrix_rotates_x_to_y_for_a_quarter_turn_about_z() {
        let h = 0.5f32.sqrt();
        let q = Vec4::new(0.0, 0.0, h, h);
        let m = Mat4::from_quat_pos(q, Vec3::new(1.0, 2.0, 3.0));
        let p = m.transform_point(Vec3::X).xyz();
        assert!((p - Vec3::new(1.0, 3.0, 3.0)).length() < 1e-6, "{p:?}");
        assert!((q.quat_rotate(Vec3::X) - Vec3::Y).length() < 1e-6);
        assert_eq!(m.translation(), Vec3::new(1.0, 2.0, 3.0));
    }

    #[test]
    fn quaternion_product_composes_like_the_rotations() {
        let h = 0.5f32.sqrt();
        let (qz, qx) = (Vec4::new(0.0, 0.0, h, h), Vec4::new(h, 0.0, 0.0, h));
        let q = qz.quat_mul(qx); // qx first, then qz
        let v = Vec3::new(0.3, -0.7, 0.9);
        assert!((q.quat_rotate(v) - qz.quat_rotate(qx.quat_rotate(v))).length() < 1e-6);
        let ident = Vec4::new(0.0, 0.0, 0.0, 1.0);
        assert_eq!(qz.quat_mul(ident), qz);
    }

    #[test]
    fn inverse_undoes_the_matrix() {
        let (a, b, c, d) = (0.1f32, 0.2f32, 0.3f32, 0.9f32);
        let n = (a * a + b * b + c * c + d * d).sqrt();
        let mut m = Mat4::from_quat_pos(Vec4::new(a / n, b / n, c / n, d / n), Vec3::new(4.0, -5.0, 6.0));
        m.set_scale_vector(Vec3::new(2.0, 0.5, 3.0));
        let p = m.mul(&m.inverse());
        for (i, v) in p.0.iter().enumerate() {
            let want = if i % 5 == 0 { 1.0 } else { 0.0 };
            assert!((v - want).abs() < 1e-5, "{p:?}");
        }
        assert_eq!(Mat4([0.0; 16]).inverse(), Mat4([0.0; 16]), "a singular matrix inverts to zero");
    }

    #[test]
    fn rows_and_the_scale_vector_address_the_d3d_layout() {
        let mut m = Mat4::identity();
        m.set_row(3, Vec4::new(7.0, 8.0, 9.0, 10.0));
        assert_eq!(m.row(3), Vec4::new(7.0, 8.0, 9.0, 10.0));
        assert_eq!(m.translation(), Vec3::new(7.0, 8.0, 9.0));
        m.set_scale_vector(Vec3::new(2.0, 3.0, 4.0));
        assert_eq!(m.scale_vector(), Vec3::new(2.0, 3.0, 4.0));
        assert_eq!(m.row(1).y, 3.0);
        m.set_column4(Vec4::new(1.0, 2.0, 3.0, 4.0));
        assert_eq!(m.column4(), Vec4::new(1.0, 2.0, 3.0, 4.0));
        assert_eq!(m.row(2).w, 3.0);
    }

    #[test]
    fn look_at_transforms_origin_into_view_space() {
        let view = Mat4::look_at_rh(Vec3::new(0.0, -5.0, 0.0), Vec3::ZERO, Vec3::Z);
        let r = view.transform_point(Vec3::ZERO);
        assert!((r.x - 0.0).abs() < 1e-5, "x = {}", r.x);
        assert!((r.y - 0.0).abs() < 1e-5, "y = {}", r.y);
        assert!((r.z - (-5.0)).abs() < 1e-5, "z = {}", r.z);
    }
}
