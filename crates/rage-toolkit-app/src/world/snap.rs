//! Ground snapping and placing under the cursor, from the game's own collision (which includes the terrain). The collision files are read
//! on demand (a few milliseconds each) and kept as plain triangles. Nothing here limits where an entity may be: a snap only moves it
//! when the user asks for one, and underground stays possible.
use super::*;
use rage_toolkit_core::worldextra::{self, CollMesh};

/// collision meshes kept at once before the oldest are dropped
const MAX_MESHES: usize = 400;

/// the nearest surface below (`Ground`) or the highest one (`Top`) when an entity is snapped
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum GroundMode {
    Below,
    Top,
}

impl World {
    fn coll_mesh(&mut self, hash: u32) -> Option<Arc<CollMesh>> {
        if let Some(m) = self.coll_cpu.get(&hash) {
            return Some(m.clone());
        }
        let path = self.ybn_paths.get(&hash)?.clone();
        let game = self.game.clone()?;
        let bytes = game.read(&path).ok()?;
        let parts = worldextra::collision_parts(&bytes).ok()?;
        let mesh = Arc::new(CollMesh::from_parts(&parts));
        if self.coll_cpu.len() >= MAX_MESHES {
            self.coll_cpu.clear();
        }
        self.coll_cpu.insert(hash, mesh.clone());
        Some(mesh)
    }

    /// the collision files whose box covers (x, y), seen from above
    fn files_covering(&self, x: f32, y: f32) -> Vec<u32> {
        match &self.state {
            LoadState::Ready(l) => l.bounds.iter().filter(|(_, lo, hi)| x >= lo[0] && x <= hi[0] && y >= lo[1] && y <= hi[1]).map(|(h, _, _)| *h).collect(),
            _ => vec![],
        }
    }

    /// The height of the ground at (x, y) as seen from `z`, and its unit normal: the nearest surface below (from 0.1 m above `z`, up to 500 m
    /// down), or with [`GroundMode::Top`] the highest one. `None` when the collision there has no surface (or is not in the game's files).
    pub(super) fn ground_at(&mut self, x: f32, y: f32, z: f32, mode: GroundMode) -> Option<(f32, Vec3)> {
        let mut best: Option<(f32, Vec3)> = None;
        for h in self.files_covering(x, y) {
            let Some(mesh) = self.coll_mesh(h) else { continue };
            let (origin, max_t) = match mode {
                GroundMode::Below => ([x, y, z + 0.1], 500.0),
                GroundMode::Top => ([x, y, mesh.max[2] + 1.0], mesh.max[2] - mesh.min[2] + 2.0),
            };
            if let Some((t, n)) = mesh.ray(origin, [0.0, 0.0, -1.0], max_t) {
                let hit_z = origin[2] - t;
                let better = match (best, mode) {
                    (None, _) => true,
                    (Some((bz, _)), GroundMode::Below) => hit_z > bz,
                    (Some((bz, _)), GroundMode::Top) => hit_z > bz,
                };
                if better {
                    best = Some((hit_z, Vec3::from(n)));
                }
            }
        }
        best
    }

    /// The nearest collision surface along a ray (the mouse), loading the files on its way as needed: the distance along the ray and the
    /// surface's unit normal.
    pub(super) fn ray_hit(&mut self, o: Vec3, d: Vec3, max_t: f32) -> Option<(f32, Vec3)> {
        self.ray_hit_detail(o, d, max_t).map(|(t, n, _, _)| (t, n))
    }

    /// Like `ray_hit`, and which collision file (by the hash of its name) and which of its triangles was hit.
    pub(super) fn ray_hit_detail(&mut self, o: Vec3, d: Vec3, max_t: f32) -> Option<(f32, Vec3, u32, usize)> {
        let bounds = match &self.state {
            LoadState::Ready(l) => l.bounds.clone(),
            _ => return None,
        };
        // the files the ray passes through, nearest entry first
        let mut hits: Vec<(f32, u32)> = vec![];
        for (h, lo, hi) in bounds.iter() {
            let (mut t0, mut t1) = (0.0f32, max_t);
            let mut miss = false;
            for k in 0..3 {
                let (ok, dk, lk, hk) = (o[k], d[k], lo[k], hi[k]);
                if dk.abs() < 1e-9 {
                    if ok < lk || ok > hk {
                        miss = true;
                        break;
                    }
                    continue;
                }
                let (a, b) = ((lk - ok) / dk, (hk - ok) / dk);
                let (a, b) = if a < b { (a, b) } else { (b, a) };
                t0 = t0.max(a);
                t1 = t1.min(b);
                if t0 > t1 {
                    miss = true;
                    break;
                }
            }
            if !miss {
                hits.push((t0, *h));
            }
        }
        hits.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut best: Option<(f32, Vec3, u32, usize)> = None;
        for (entry, h) in hits.into_iter().take(24) {
            // a file that starts after the best hit so far cannot have a nearer one
            if best.map_or(false, |(bt, _, _, _)| entry > bt) {
                break;
            }
            let Some(mesh) = self.coll_mesh(h) else { continue };
            if let Some((t, n, tri)) = mesh.ray_tri(o.to_array(), d.to_array(), max_t) {
                if best.map_or(true, |(bt, _, _, _)| t < bt) {
                    best = Some((t, Vec3::from(n), h, tri));
                }
            }
        }
        best
    }

    /// Put the middle of the selection where the mouse points (on the game's collision there, else in the horizontal plane through it).
    pub(super) fn place_selection_at_cursor(&mut self) {
        let list: Vec<(u32, EntityEdit)> = self.sel.clone().into_iter().filter_map(|i| self.placement_of(i).map(|p| (i, p))).collect();
        if list.is_empty() {
            return;
        }
        let Some(target) = self.cursor_point() else {
            self.project_msg = "Point the mouse at the scene, then press G.".into();
            return;
        };
        let pivot = list.iter().map(|(_, p)| Vec3::from(p.pos)).sum::<Vec3>() / list.len() as f32;
        let delta = target - pivot;
        let moved: Vec<(u32, EntityEdit)> = list
            .into_iter()
            .map(|(i, mut p)| {
                p.pos = [p.pos[0] + delta.x, p.pos[1] + delta.y, p.pos[2] + delta.z];
                (i, p)
            })
            .collect();
        self.set_placements(&moved);
        self.end_groups();
        self.project_msg = format!("Put {} entit{} under the cursor.", moved.len(), if moved.len() == 1 { "y" } else { "ies" });
    }

    /// Snap the selected entities down onto the ground (or onto the highest surface), keeping x and y. With `align`, they are also turned to
    /// lie along the surface (their heading is kept). Returns how many found no ground there (those are left where they are).
    pub(super) fn snap_selection_to_ground(&mut self, mode: GroundMode, align: bool) -> usize {
        let ids = self.sel.clone();
        let mut list = vec![];
        let mut missed = 0;
        for id in ids {
            let Some(mut e) = self.placement_of(id) else { continue };
            match self.ground_at(e.pos[0], e.pos[1], e.pos[2], mode) {
                Some((z, normal)) => {
                    e.pos[2] = z;
                    if align {
                        e.rot = align_to_normal(e.rot, normal);
                    }
                    list.push((id, e));
                }
                None => missed += 1,
            }
        }
        if !list.is_empty() {
            self.set_placements(&list);
            self.end_groups();
        }
        self.project_msg = match (list.len(), missed) {
            (0, 0) => "Nothing is selected.".into(),
            (0, m) => format!("No ground was found under {m} entit{}.", if m == 1 { "y" } else { "ies" }),
            (n, 0) => format!("Snapped {n} entit{} to the ground.", if n == 1 { "y" } else { "ies" }),
            (n, m) => format!("Snapped {n} to the ground; no ground was found under {m}."),
        };
        missed
    }
}

/// The stored rotation (the inverse of the world rotation) turned so the entity's up axis is `normal`, keeping its heading.
pub(super) fn align_to_normal(stored: [f32; 4], normal: Vec3) -> [f32; 4] {
    let q = Quat::from_xyzw(stored[0], stored[1], stored[2], stored[3]);
    let world = if q.length_squared() > 0.0 { q.normalize().conjugate() } else { Quat::IDENTITY };
    let heading = world.to_euler(glam::EulerRot::ZYX).0;
    let n = if normal.length_squared() > 0.0 { normal.normalize() } else { Vec3::Z };
    let aligned = Quat::from_rotation_arc(Vec3::Z, n) * Quat::from_rotation_z(heading);
    let s = aligned.conjugate();
    [s.x, s.y, s.z, s.w]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aligning_to_a_flat_surface_keeps_the_heading_and_a_slope_tilts_the_up_axis() {
        // heading 90 degrees about Z, stored as the inverse
        let world = Quat::from_rotation_z(std::f32::consts::FRAC_PI_2);
        let s = world.conjugate();
        let stored = [s.x, s.y, s.z, s.w];
        let flat = align_to_normal(stored, Vec3::Z);
        let back = Quat::from_xyzw(flat[0], flat[1], flat[2], flat[3]).conjugate();
        assert!((back * Vec3::X - Vec3::Y).length() < 1e-4, "heading kept: {:?}", back * Vec3::X);
        assert!((back * Vec3::Z - Vec3::Z).length() < 1e-4);
        // a surface leaning towards +X: the entity's up axis follows the normal
        let n = Vec3::new(0.3, 0.0, 1.0).normalize();
        let tilted = align_to_normal(stored, n);
        let back = Quat::from_xyzw(tilted[0], tilted[1], tilted[2], tilted[3]).conjugate();
        assert!((back * Vec3::Z - n).length() < 1e-4, "{:?}", back * Vec3::Z);
    }
}

/// What a collision click found.
pub(super) struct CollHit {
    pub file: String,
    /// the game's layer for the file (0 to 2), when its cache lists it
    pub layer: Option<u32>,
    pub material: u8,
    pub point: Vec3,
    pub normal: Vec3,
    pub dist: f32,
    pub tri: [[f32; 3]; 3],
    pub file_tris: usize,
    /// (material, triangle count) in that file, most used first
    pub materials: Vec<(u8, usize)>,
    pub file_min: [f32; 3],
    pub file_max: [f32; 3],
}

impl World {
    /// Inspect the collision surface under the mouse (key C): its file, surface material, the triangle and what else the file holds.
    pub(super) fn inspect_collision(&mut self) {
        let Some((o, d)) = self.cursor_ray else { return };
        let Some((t, n, h, tri)) = self.ray_hit_detail(o, d, 4000.0) else {
            self.coll_hit = None;
            self.coll_hit_model = None;
            return;
        };
        let Some(mesh) = self.coll_mesh(h) else { return };
        let file = self.ybn_paths.get(&h).map(|p| p.rsplit('/').next().unwrap_or(p).to_string()).unwrap_or_else(|| format!("{h:08x}"));
        let idx = &mesh.indices[tri * 3..tri * 3 + 3];
        let tri_pos = [mesh.positions[idx[0] as usize], mesh.positions[idx[1] as usize], mesh.positions[idx[2] as usize]];
        let mut counts: std::collections::BTreeMap<u8, usize> = Default::default();
        for m in &mesh.tri_material {
            *counts.entry(*m).or_default() += 1;
        }
        let mut materials: Vec<(u8, usize)> = counts.into_iter().collect();
        materials.sort_by(|a, b| b.1.cmp(&a.1));
        self.coll_hit = Some(CollHit { layer: match &self.state { LoadState::Ready(l) => l.bound_layers.get(&h).copied(), _ => None }, file, material: mesh.tri_material.get(tri).copied().unwrap_or(0), point: o + d * t, normal: n, dist: t, tri: tri_pos, file_tris: mesh.indices.len() / 3, materials, file_min: mesh.min, file_max: mesh.max });
        // the triangle outlined, lifted a little
        let up = |p: [f32; 3]| [p[0], p[1], p[2] + 0.05];
        let seg = [up(tri_pos[0]), up(tri_pos[1]), up(tri_pos[1]), up(tri_pos[2]), up(tri_pos[2]), up(tri_pos[0])];
        self.coll_hit_model = self.gpu.as_ref().and_then(|g| g.factory().line_part(&seg, [255, 60, 60, 255], "line:hit")).map(|p| Arc::new(crate::world_gpu::GpuModel { parts: vec![p] }));
        self.scene_rev = self.scene_rev.wrapping_add(1);
    }
}
