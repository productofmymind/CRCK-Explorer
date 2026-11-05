//! Editing a `.ynv` navmesh cell: move vertices, add and remove polygons, set polygon flags, join neighbouring polygons by their shared edges,
//! with undo; plus a basic generator that turns triangles (walkable geometry) into a navmesh cell. Saving builds the file and reads it back;
//! a file whose polygons do not read back (within the file's own precision) is not written.
use anyhow::{bail, Context, Result};
use rage_formats::math::Vec3;
use rage_formats::ynv::{parse_ynv, serialize_ynv, NavEdge, NavPoly, Ynv, ADJACENT_NONE};

/// the most corners a game polygon has
pub const MAX_POLY_VERTICES: usize = 7;
/// two corners closer than this (metres) are the same point when joining edges
const JOIN_TOLERANCE: f32 = 0.05;

pub struct YnvDocument {
    mesh: Ynv,
    undo: Vec<Ynv>,
    redo: Vec<Ynv>,
    saved: Ynv,
}

impl YnvDocument {
    pub fn open(bytes: &[u8]) -> Result<Self> {
        Ok(Self::from_mesh(parse_ynv(bytes).context("reading the navmesh")?))
    }

    pub fn from_mesh(mesh: Ynv) -> Self {
        Self { saved: mesh.clone(), mesh, undo: vec![], redo: vec![] }
    }

    pub fn mesh(&self) -> &Ynv {
        &self.mesh
    }

    pub fn poly_count(&self) -> usize {
        self.mesh.polys.len()
    }

    pub fn is_dirty(&self) -> bool {
        self.mesh != self.saved
    }

    pub fn undo(&mut self) -> bool {
        match self.undo.pop() {
            Some(m) => {
                self.redo.push(std::mem::replace(&mut self.mesh, m));
                true
            }
            None => false,
        }
    }

    pub fn redo(&mut self) -> bool {
        match self.redo.pop() {
            Some(m) => {
                self.undo.push(std::mem::replace(&mut self.mesh, m));
                true
            }
            None => false,
        }
    }

    fn edit(&mut self, f: impl FnOnce(&mut Ynv) -> Result<()>) -> Result<()> {
        let before = self.mesh.clone();
        match f(&mut self.mesh) {
            Ok(()) => {
                self.undo.push(before);
                self.redo.clear();
                Ok(())
            }
            Err(e) => {
                self.mesh = before;
                Err(e)
            }
        }
    }

    fn inside(&self, p: Vec3) -> bool {
        let (lo, hi) = (self.mesh.bb_min, self.mesh.bb_max);
        p.x.is_finite() && p.y.is_finite() && p.z.is_finite() && p.x >= lo.x && p.x <= hi.x && p.y >= lo.y && p.y <= hi.y && p.z >= lo.z && p.z <= hi.z
    }

    /// Move every corner at `from` (in every polygon: the polygons sharing a corner stay joined) to `to`. Returns how many corners moved.
    pub fn move_vertex(&mut self, from: Vec3, to: Vec3) -> Result<usize> {
        if !self.inside(to) {
            bail!("a vertex must stay inside the cell's box");
        }
        let mut moved = 0;
        self.edit(|m| {
            for p in m.polys.iter_mut() {
                for v in p.vertices.iter_mut() {
                    if (*v - from).length() <= JOIN_TOLERANCE {
                        *v = to;
                        moved += 1;
                    }
                }
            }
            if moved == 0 {
                bail!("no vertex there");
            }
            Ok(())
        })?;
        Ok(moved)
    }

    /// Add a polygon from 3 to 7 corners inside the cell's box (convex in the ground plane; clockwise corners are reversed). Joins it to
    /// the polygons it shares an edge with. Returns its number.
    pub fn add_poly(&mut self, corners: &[Vec3], flags: (u16, u32, u32)) -> Result<usize> {
        if corners.iter().any(|&c| !self.inside(c)) {
            bail!("every corner must lie inside the cell's box");
        }
        let ring = convex_ring(corners)?;
        let at = self.poly_count();
        self.edit(|m| {
            let mut p = NavPoly::new(ring);
            (p.flags0, p.flags1, p.flags2) = flags;
            m.polys.push(p);
            relink(m);
            Ok(())
        })?;
        Ok(at)
    }

    /// Remove polygon `i`: the edges that led to it become open and the later polygons are renumbered. Refused when portals use it.
    pub fn remove_poly(&mut self, i: usize) -> Result<()> {
        if i >= self.poly_count() {
            bail!("no polygon {i}");
        }
        let area = self.mesh.area_id;
        if !self.mesh.polys[i].portal_links.is_empty() || self.mesh.portals.iter().any(|p| (p.area_from as u32 == area && p.poly_from as usize == i) || (p.area_to as u32 == area && p.poly_to as usize == i)) {
            bail!("polygon {i} has a portal (a climb or drop link): remove the portal first");
        }
        self.edit(|m| {
            m.polys.remove(i);
            for p in m.portals.iter_mut() {
                if p.area_from as u32 == area && p.poly_from as usize > i {
                    p.poly_from -= 1;
                }
                if p.area_to as u32 == area && p.poly_to as usize > i {
                    p.poly_to -= 1;
                }
            }
            for p in m.polys.iter_mut() {
                for e in p.edges.iter_mut() {
                    for end in [&mut e.a, &mut e.b] {
                        if end.area_id == area && end.poly != ADJACENT_NONE {
                            if end.poly as usize == i {
                                *end = rage_formats::ynv::NavEdgeEnd::NONE;
                            } else if end.poly as usize > i {
                                end.poly -= 1;
                            }
                        }
                    }
                }
            }
            Ok(())
        })
    }

    pub fn set_flags(&mut self, i: usize, flags0: u16, flags1: u32, flags2: u32) -> Result<()> {
        if i >= self.poly_count() {
            bail!("no polygon {i}");
        }
        self.edit(|m| {
            let p = &mut m.polys[i];
            (p.flags0, p.flags1, p.flags2) = (flags0, flags1, flags2);
            Ok(())
        })
    }

    /// Join polygons that share an edge (within 5 cm); edges that lead to another cell are kept.
    pub fn join_edges(&mut self) -> Result<()> {
        self.edit(|m| {
            relink(m);
            Ok(())
        })
    }

    /// the built file, read back: same polygons (corners within the file's precision) and same neighbours, or an error and nothing written
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        let bytes = serialize_ynv(&self.mesh).context("building the navmesh file")?;
        let back = parse_ynv(&bytes).context("reading the built file back")?;
        if back.polys.len() != self.mesh.polys.len() {
            bail!("saving would change the polygon count ({} to {}): nothing was written", self.mesh.polys.len(), back.polys.len());
        }
        let tol = (self.mesh.bb_size.x.max(self.mesh.bb_size.y).max(self.mesh.bb_size.z) / 65535.0) * 1.5 + 0.001;
        for (k, (a, b)) in self.mesh.polys.iter().zip(&back.polys).enumerate() {
            if a.vertices.len() != b.vertices.len() || a.vertices.iter().zip(&b.vertices).any(|(p, q)| (*p - *q).length() > tol * 2.0) || a.edges != b.edges {
                bail!("polygon {k} would change when saved: nothing was written");
            }
        }
        Ok(bytes)
    }

    pub fn mark_saved(&mut self) {
        self.saved = self.mesh.clone();
    }
}

/// the corners as a counter-clockwise convex ring (seen from above), or why not
fn convex_ring(c: &[Vec3]) -> Result<Vec<Vec3>> {
    if c.len() < 3 || c.len() > MAX_POLY_VERTICES {
        bail!("a polygon has 3 to {MAX_POLY_VERTICES} corners");
    }
    let n = c.len();
    let cross = |i: usize| {
        let (a, b, d) = (c[i], c[(i + 1) % n], c[(i + 2) % n]);
        (b.x - a.x) * (d.y - b.y) - (b.y - a.y) * (d.x - b.x)
    };
    let signs: Vec<f32> = (0..n).map(cross).collect();
    let positive = signs.iter().all(|&s| s > 1e-6);
    let negative = signs.iter().all(|&s| s < -1e-6);
    if !positive && !negative {
        bail!("the corners must form a convex polygon seen from above");
    }
    let mut ring = c.to_vec();
    if negative {
        ring.reverse();
    }
    Ok(ring)
}

/// Set every edge from the polygons that share it. An edge with no partner in this cell stays as it was when it already leads to another
/// cell, and is open otherwise.
fn relink(m: &mut Ynv) {
    let area = m.area_id;
    let n = m.polys.len();
    // quantised edge keys: (from, to) corner cells
    let key = |v: Vec3| ((v.x / JOIN_TOLERANCE).round() as i64, (v.y / JOIN_TOLERANCE).round() as i64, (v.z / (JOIN_TOLERANCE * 4.0)).round() as i64);
    let mut by_edge: std::collections::HashMap<((i64, i64, i64), (i64, i64, i64)), Vec<(usize, usize)>> = std::collections::HashMap::new();
    for (pi, p) in m.polys.iter().enumerate() {
        let k = p.vertices.len();
        for e in 0..k {
            by_edge.entry((key(p.vertices[e]), key(p.vertices[(e + 1) % k]))).or_default().push((pi, e));
        }
    }
    let mut result: Vec<Vec<NavEdge>> = Vec::with_capacity(n);
    for (pi, p) in m.polys.iter().enumerate() {
        let k = p.vertices.len();
        let mut edges = Vec::with_capacity(k);
        for e in 0..k {
            let reverse = (key(p.vertices[(e + 1) % k]), key(p.vertices[e]));
            let partner = by_edge.get(&reverse).and_then(|v| v.iter().find(|(q, _)| *q != pi));
            let old = p.edges.get(e).copied().unwrap_or(NavEdge::NONE);
            edges.push(match partner {
                Some((q, _)) => NavEdge::neighbour(area, *q as u32),
                None if !old.a.is_none() && old.a.area_id != area => old,
                None => NavEdge::NONE,
            });
        }
        result.push(edges);
    }
    for (p, e) in m.polys.iter_mut().zip(result) {
        p.edges = e;
    }
}

/// A navmesh cell from triangles: triangles inside the cell's box whose slope is at most `max_slope_deg` become polygons (flat ones are
/// flagged flat ground), and polygons sharing an edge are joined. Triangles are given in any winding.
pub fn generate_from_triangles(area_id: u32, bb_min: Vec3, bb_max: Vec3, triangles: &[[Vec3; 3]], max_slope_deg: f32) -> Result<Ynv> {
    let mut mesh = Ynv::new_cell(area_id, bb_min, bb_max);
    let min_up = max_slope_deg.to_radians().cos();
    for t in triangles {
        let inside = t.iter().all(|v| v.x >= bb_min.x && v.x <= bb_max.x && v.y >= bb_min.y && v.y <= bb_max.y && v.z >= bb_min.z && v.z <= bb_max.z);
        if !inside {
            continue;
        }
        let n = (t[1] - t[0]).cross(t[2] - t[0]);
        let len = n.length();
        if len < 1e-6 || (n.z.abs() / len) < min_up {
            continue;
        }
        let Ok(ring) = convex_ring(t) else { continue };
        let mut p = NavPoly::new(ring);
        if n.z.abs() / len > 0.999 {
            p.set_flat_ground(true);
        }
        mesh.polys.push(p);
        if mesh.polys.len() >= 0x3FFF {
            bail!("a cell holds at most 16383 polygons");
        }
    }
    relink(&mut mesh);
    Ok(mesh)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(x: f32, y: f32, z: f32) -> Vec3 {
        Vec3::new(x, y, z)
    }

    fn cell() -> YnvDocument {
        YnvDocument::from_mesh(Ynv::new_cell(0, v(0.0, 0.0, -10.0), v(150.0, 150.0, 50.0)))
    }

    /// a square of two triangles
    fn square() -> Vec<[Vec3; 3]> {
        vec![[v(10.0, 10.0, 5.0), v(20.0, 10.0, 5.0), v(20.0, 20.0, 5.0)], [v(10.0, 10.0, 5.0), v(20.0, 20.0, 5.0), v(10.0, 20.0, 5.0)]]
    }

    #[test]
    fn the_generator_makes_polygons_of_walkable_triangles_and_joins_them() {
        let mut tris = square();
        tris.push([v(30.0, 30.0, 0.0), v(31.0, 30.0, 10.0), v(30.0, 31.0, 0.0)]); // a steep ramp
        tris.push([v(500.0, 0.0, 0.0), v(501.0, 0.0, 0.0), v(500.0, 1.0, 0.0)]); // outside the cell
        let m = generate_from_triangles(0, v(0.0, 0.0, -10.0), v(150.0, 150.0, 50.0), &tris, 45.0).unwrap();
        assert_eq!(m.polys.len(), 2);
        assert!(m.polys[0].is_flat_ground());
        let joined = |p: &NavPoly| p.edges.iter().filter(|e| !e.a.is_none()).count();
        assert_eq!((joined(&m.polys[0]), joined(&m.polys[1])), (1, 1));
        assert!(m.polys[0].edges.iter().any(|e| e.a.poly == 1) && m.polys[1].edges.iter().any(|e| e.a.poly == 0));
    }

    #[test]
    fn polygons_are_added_checked_removed_and_undone() {
        let mut d = cell();
        assert!(d.add_poly(&[v(0.0, 0.0, 0.0), v(1.0, 0.0, 0.0)], (0, 0, 0)).is_err(), "two corners");
        assert!(d.add_poly(&[v(-5.0, 0.0, 0.0), v(1.0, 0.0, 0.0), v(0.0, 1.0, 0.0)], (0, 0, 0)).is_err(), "outside the box");
        assert!(d.add_poly(&[v(0.0, 0.0, 0.0), v(4.0, 0.0, 0.0), v(1.0, 1.0, 0.0), v(4.0, 4.0, 0.0), v(0.0, 4.0, 0.0)], (0, 0, 0)).is_err(), "concave");
        // clockwise corners are reversed
        let a = d.add_poly(&[v(0.0, 0.0, 0.0), v(0.0, 4.0, 0.0), v(4.0, 0.0, 0.0)], (0, 0, 0)).unwrap();
        let b = d.add_poly(&[v(4.0, 0.0, 0.0), v(0.0, 4.0, 0.0), v(4.0, 4.0, 0.0)], (0, 0, 0)).unwrap();
        assert_eq!((a, b), (0, 1));
        assert!(d.mesh().polys[0].edges.iter().any(|e| e.a.poly == 1), "the new polygon joined its neighbour");
        d.remove_poly(0).unwrap();
        assert_eq!(d.poly_count(), 1);
        assert!(d.mesh().polys[0].edges.iter().all(|e| e.a.is_none()), "the edge to the removed polygon is open");
        assert!(d.undo() && d.poly_count() == 2);
        assert!(d.remove_poly(9).is_err());
    }

    #[test]
    fn moving_a_vertex_moves_every_polygon_that_shares_it() {
        let mut d = YnvDocument::from_mesh(generate_from_triangles(0, v(0.0, 0.0, -10.0), v(150.0, 150.0, 50.0), &square(), 45.0).unwrap());
        let moved = d.move_vertex(v(10.0, 10.0, 5.0), v(9.0, 9.0, 6.0)).unwrap();
        assert_eq!(moved, 2);
        assert!(d.mesh().polys.iter().all(|p| p.vertices.contains(&v(9.0, 9.0, 6.0))));
        assert!(d.move_vertex(v(99.0, 99.0, 5.0), v(1.0, 1.0, 1.0)).is_err());
        assert!(d.move_vertex(v(9.0, 9.0, 6.0), v(-3.0, 0.0, 0.0)).is_err(), "outside the box");
    }

    #[test]
    fn a_generated_mesh_saves_and_reads_back() {
        let mut d = YnvDocument::from_mesh(generate_from_triangles(0, v(0.0, 0.0, -10.0), v(150.0, 150.0, 50.0), &square(), 45.0).unwrap());
        d.set_flags(0, 4, 512 | 1024, 0).unwrap();
        let back = YnvDocument::open(&d.to_bytes().unwrap()).unwrap();
        assert_eq!(back.poly_count(), 2);
        assert!(back.mesh().polys[0].is_road() && back.mesh().polys[0].is_footpath());
        d.mark_saved();
        assert!(!d.is_dirty());
    }
}
