//! Occlusion culling, as the game does it: the occluder triangles of the maps near the camera are drawn into a small depth picture on the CPU
//! (nearest distance per pixel), and an entity whose box lies wholly behind that picture is not drawn. Only the game's own occluders are used.
use glam::{Mat4, Quat, Vec3, Vec4};

/// the depth picture's width in pixels; the height follows the view's shape
const WIDTH: usize = 128;
/// distance from the camera (metres) under which a corner counts as at the camera: a box with one is never culled
const NEAR: f32 = 0.5;

/// The occluder triangles of the whole game, in cells of `CELL` metres (by their first corner), each with the box of its triangles, so a view reads
/// only the cells that are near and in front of the camera.
#[derive(Default)]
pub(super) struct OccluderGrid {
    cells: std::collections::HashMap<(i32, i32), (Vec3, Vec3, Vec<[[f32; 3]; 3]>)>,
    pub total: usize,
}

const CELL: f32 = 64.0;

impl OccluderGrid {
    pub fn new(tris: Vec<[[f32; 3]; 3]>) -> Self {
        let total = tris.len();
        let mut cells: std::collections::HashMap<(i32, i32), (Vec3, Vec3, Vec<[[f32; 3]; 3]>)> = Default::default();
        for t in tris {
            let e = cells.entry(((t[0][0] / CELL).floor() as i32, (t[0][1] / CELL).floor() as i32)).or_insert((Vec3::splat(f32::MAX), Vec3::splat(f32::MIN), vec![]));
            for p in t {
                e.0 = e.0.min(Vec3::from(p));
                e.1 = e.1.max(Vec3::from(p));
            }
            e.2.push(t);
        }
        Self { cells, total }
    }

    /// The triangles of the cells within `reach` metres of `eye` whose box `visible` (the view's test) keeps.
    pub fn near<'a>(&'a self, eye: Vec3, reach: f32, visible: &'a dyn Fn(Vec3, Vec3) -> bool) -> impl Iterator<Item = [[f32; 3]; 3]> + 'a {
        let r = ((reach + 64.0) / CELL).ceil() as i32;
        let (cx, cy) = ((eye.x / CELL).floor() as i32, (eye.y / CELL).floor() as i32);
        (cx - r..=cx + r)
            .flat_map(move |x| (cy - r..=cy + r).map(move |y| (x, y)))
            .filter_map(|k| self.cells.get(&k))
            .filter(move |(lo, hi, _)| {
                // the box's nearest point to the camera is within reach, and the box is in the view
                let nearest = eye.clamp(*lo, *hi);
                (nearest - eye).length_squared() < reach * reach && visible(*lo, *hi)
            })
            .flat_map(|c| c.2.iter().copied())
    }
}

pub(super) struct Occlusion {
    vp: Mat4,
    w: usize,
    h: usize,
    /// per pixel: the distance along the view direction of the nearest occluder, infinity where there is none
    depth: Vec<f32>,
}

impl Occlusion {
    /// Draw `tris` (world space) for the view `vp`; `aspect` is the view's width over its height.
    pub fn build(vp: &Mat4, aspect: f32, tris: impl Iterator<Item = [[f32; 3]; 3]>) -> Self {
        let w = WIDTH;
        let h = ((w as f32 / aspect.max(0.2)).round() as usize).clamp(32, 512);
        let started = std::time::Instant::now();
        let tris: Vec<[[f32; 3]; 3]> = tris.collect();
        // the triangles are shared out between a few threads, each drawing its own picture; the nearest depth of each pixel wins
        let threads = if tris.len() < 4000 { 1 } else { std::thread::available_parallelism().map_or(2, |n| n.get()).min(6) };
        let draw = |part: &[[[f32; 3]; 3]]| {
            let mut o = Occlusion { vp: *vp, w, h, depth: vec![f32::INFINITY; w * h] };
            for t in part {
                o.draw(*t);
            }
            o.depth
        };
        let depth = if threads == 1 {
            draw(&tris)
        } else {
            let chunk = tris.len().div_ceil(threads);
            let parts: Vec<Vec<f32>> = std::thread::scope(|s| {
                let handles: Vec<_> = tris.chunks(chunk).map(|c| s.spawn(|| draw(c))).collect();
                handles.into_iter().filter_map(|h| h.join().ok()).collect()
            });
            let mut it = parts.into_iter();
            let mut first = it.next().unwrap_or_else(|| vec![f32::INFINITY; w * h]);
            for p in it {
                for (a, b) in first.iter_mut().zip(p) {
                    *a = a.min(b);
                }
            }
            first
        };
        if std::env::var_os("CRCK_WORLD_OCCLOG").is_some() {
            eprintln!("occlusion: {} triangles, {threads} threads, {:.1} ms", tris.len(), started.elapsed().as_secs_f32() * 1000.0);
        }
        Occlusion { vp: *vp, w, h, depth }
    }

    fn clip(&self, p: [f32; 3]) -> Vec4 {
        self.vp * Vec4::new(p[0], p[1], p[2], 1.0)
    }

    /// Rasterise one triangle (both sides count), cut at the near plane first.
    fn draw(&mut self, t: [[f32; 3]; 3]) {
        let c = [self.clip(t[0]), self.clip(t[1]), self.clip(t[2])];
        // wholly outside one side of the view (or behind the camera): nothing to draw
        if c.iter().all(|v| v.x < -v.w) || c.iter().all(|v| v.x > v.w) || c.iter().all(|v| v.y < -v.w) || c.iter().all(|v| v.y > v.w) || c.iter().all(|v| v.w < NEAR) {
            return;
        }
        // clip against w >= NEAR: a polygon of up to four corners
        let mut poly = [Vec4::ZERO; 4];
        let mut n = 0;
        for k in 0..3 {
            let (a, b) = (c[k], c[(k + 1) % 3]);
            if a.w >= NEAR {
                poly[n] = a;
                n += 1;
            }
            if (a.w >= NEAR) != (b.w >= NEAR) {
                let s = (NEAR - a.w) / (b.w - a.w);
                poly[n] = a + (b - a) * s;
                n += 1;
            }
        }
        for k in 1..n.saturating_sub(1) {
            self.fill([poly[0], poly[k], poly[k + 1]]);
        }
    }

    fn fill(&mut self, c: [Vec4; 3]) {
        let (wf, hf) = (self.w as f32, self.h as f32);
        // pixel coordinates (y down) and 1/w, which is what varies linearly across the screen
        let s = c.map(|v| ((v.x / v.w * 0.5 + 0.5) * wf, (0.5 - v.y / v.w * 0.5) * hf, 1.0 / v.w));
        let (min_x, max_x) = (s.iter().map(|p| p.0).fold(f32::MAX, f32::min), s.iter().map(|p| p.0).fold(f32::MIN, f32::max));
        let (min_y, max_y) = (s.iter().map(|p| p.1).fold(f32::MAX, f32::min), s.iter().map(|p| p.1).fold(f32::MIN, f32::max));
        if max_x < 0.0 || max_y < 0.0 || min_x > wf || min_y > hf {
            return;
        }
        let area = (s[1].0 - s[0].0) * (s[2].1 - s[0].1) - (s[2].0 - s[0].0) * (s[1].1 - s[0].1);
        if area.abs() < 1e-6 {
            return;
        }
        let (x0, x1) = (min_x.floor().max(0.0) as usize, (max_x.ceil() as usize).min(self.w - 1));
        let (y0, y1) = (min_y.floor().max(0.0) as usize, (max_y.ceil() as usize).min(self.h - 1));
        for y in y0..=y1 {
            for x in x0..=x1 {
                let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                let w0 = ((s[1].0 - px) * (s[2].1 - py) - (s[2].0 - px) * (s[1].1 - py)) / area;
                let w1 = ((s[2].0 - px) * (s[0].1 - py) - (s[0].0 - px) * (s[2].1 - py)) / area;
                let w2 = 1.0 - w0 - w1;
                if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                    continue;
                }
                let inv_w = w0 * s[0].2 + w1 * s[1].2 + w2 * s[2].2;
                if inv_w > 0.0 {
                    let d = 1.0 / inv_w;
                    let cell = &mut self.depth[y * self.w + x];
                    if d < *cell {
                        *cell = d;
                    }
                }
            }
        }
    }

    /// Whether the oriented box (centre, rotation, half sizes) is wholly hidden behind the occluders drawn: every pixel it covers (less a one pixel
    /// margin, as the picture samples at pixel centres) has an occluder nearer than the box's nearest corner.
    pub fn hides(&self, centre: Vec3, rot: Quat, half: Vec3) -> bool {
        let (mut lo_x, mut lo_y, mut hi_x, mut hi_y, mut near) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN, f32::MAX);
        for i in 0..8 {
            let corner = centre + rot * Vec3::new(if i & 1 == 0 { -half.x } else { half.x }, if i & 2 == 0 { -half.y } else { half.y }, if i & 4 == 0 { -half.z } else { half.z });
            let c = self.clip(corner.to_array());
            if c.w < NEAR {
                return false;
            }
            let (x, y) = ((c.x / c.w * 0.5 + 0.5) * self.w as f32, (0.5 - c.y / c.w * 0.5) * self.h as f32);
            lo_x = lo_x.min(x);
            hi_x = hi_x.max(x);
            lo_y = lo_y.min(y);
            hi_y = hi_y.max(y);
            near = near.min(c.w);
        }
        // partly off the picture: the part outside cannot be checked
        if lo_x < 0.0 || lo_y < 0.0 || hi_x > self.w as f32 || hi_y > self.h as f32 {
            return false;
        }
        let (x0, x1) = ((lo_x + 1.0).ceil() as usize, ((hi_x - 1.0).floor() as isize).max(-1));
        let (y0, y1) = ((lo_y + 1.0).ceil() as usize, ((hi_y - 1.0).floor() as isize).max(-1));
        if x1 < x0 as isize || y1 < y0 as isize {
            // smaller than a few pixels: one sample at its middle
            let (x, y) = (((lo_x + hi_x) * 0.5) as usize, ((lo_y + hi_y) * 0.5) as usize);
            return self.depth.get(y * self.w + x).is_some_and(|d| *d < near);
        }
        for y in y0..=y1 as usize {
            for x in x0..=x1 as usize {
                if self.depth[y * self.w + x] >= near {
                    return false;
                }
            }
        }
        true
    }

    /// pixels with an occluder, for the diagnostics
    pub fn covered(&self) -> usize {
        self.depth.iter().filter(|d| d.is_finite()).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view() -> Mat4 {
        // at the origin looking along +Y, Z up
        let proj = Mat4::perspective_rh(60f32.to_radians(), 16.0 / 9.0, 0.1, 2000.0);
        proj * Mat4::look_at_rh(Vec3::ZERO, Vec3::Y, Vec3::Z)
    }

    /// a wall 20 m ahead, 20 m wide and 40 m tall
    fn wall() -> Vec<[[f32; 3]; 3]> {
        let (a, b, c, d) = ([-10.0, 20.0, -20.0], [10.0, 20.0, -20.0], [10.0, 20.0, 20.0], [-10.0, 20.0, 20.0]);
        vec![[a, b, c], [a, c, d]]
    }

    #[test]
    fn a_box_behind_a_wall_is_hidden_and_one_in_front_or_beside_is_not() {
        let o = Occlusion::build(&view(), 16.0 / 9.0, wall().into_iter());
        assert!(o.covered() > 1000);
        let half = Vec3::splat(2.0);
        assert!(o.hides(Vec3::new(0.0, 60.0, 0.0), Quat::IDENTITY, half), "behind the wall");
        assert!(!o.hides(Vec3::new(0.0, 10.0, 0.0), Quat::IDENTITY, half), "in front of the wall");
        assert!(!o.hides(Vec3::new(40.0, 60.0, 0.0), Quat::IDENTITY, half), "beside the wall");
        // a box that pokes out of the wall's side is partly seen
        assert!(!o.hides(Vec3::new(10.0, 60.0, 0.0), Quat::IDENTITY, Vec3::splat(25.0)), "wider than the wall's cover");
        // a box that reaches the camera is never culled
        assert!(!o.hides(Vec3::new(0.0, 0.2, 0.0), Quat::IDENTITY, half));
    }

    #[test]
    fn an_occluder_behind_the_camera_or_cut_by_the_near_plane_does_not_break_it() {
        let behind = [[[-5.0, -10.0, -5.0], [5.0, -10.0, -5.0], [0.0, -10.0, 5.0]]];
        let o = Occlusion::build(&view(), 16.0 / 9.0, behind.into_iter());
        assert_eq!(o.covered(), 0);
        // a floor under the camera running far ahead is cut at the near plane and still covers the lower picture
        let floor = [[[-50.0, -50.0, -2.0], [50.0, -50.0, -2.0], [0.0, 300.0, -2.0]]];
        let o = Occlusion::build(&view(), 16.0 / 9.0, floor.into_iter());
        assert!(o.covered() > 0);
        assert!(o.hides(Vec3::new(0.0, 100.0, -10.0), Quat::IDENTITY, Vec3::splat(1.0)), "a box under the floor, far ahead");
    }
}
