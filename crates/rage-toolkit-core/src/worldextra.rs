//! More of what the World view needs from the game: collision (`.ybn`) as coloured triangles, and a whole-game lookup of which texture
//! dictionary holds a texture name.
use crate::{
    game::{cache_dir, Game},
    index::{FileIndex, Filter},
    job::Job,
    worldmodel::Ytd,
};
use anyhow::Result;
use std::collections::HashMap;

/// One piece of collision: triangles of one surface material, in the model's own space.
pub struct CollPart {
    pub material: u8,
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub indices: Vec<u32>,
}

/// A colour for a collision material, the same for the same material every time.
pub fn material_color(material: u8) -> [u8; 3] {
    // spread the hue by the golden angle so neighbouring numbers differ
    let h = (material as f32 * 137.508) % 360.0;
    let (s, v) = (0.55, 0.9);
    let c = v * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = v - c;
    let (r, g, b) = match (h / 60.0) as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    [((r + m) * 255.0) as u8, ((g + m) * 255.0) as u8, ((b + m) * 255.0) as u8]
}

/// The collision of a `.ybn`: its triangles grouped by surface material, flat-shaded; boxes, spheres, capsules, cylinders and discs
/// as their bounding boxes (material 255).
pub fn collision_parts(bytes: &[u8]) -> Result<Vec<CollPart>> {
    use rage_formats::ybn::{parse_ybn, Bound, BoundKind, BoundTransform};
    let ybn = parse_ybn(bytes)?;
    let mut parts: HashMap<u8, CollPart> = HashMap::new();
    fn push_tri(parts: &mut HashMap<u8, CollPart>, material: u8, v: [[f32; 3]; 3]) {
        let (a, b, c) = (glam::Vec3::from(v[0]), glam::Vec3::from(v[1]), glam::Vec3::from(v[2]));
        let n = (b - a).cross(c - a).normalize_or_zero().to_array();
        let p = parts.entry(material).or_insert_with(|| CollPart { material, positions: vec![], normals: vec![], indices: vec![] });
        for vert in v {
            p.indices.push(p.positions.len() as u32);
            p.positions.push(vert);
            p.normals.push(n);
        }
    }
    for t in ybn.triangles() {
        push_tri(&mut parts, t.material, t.vertices.map(|v| [v.x, v.y, v.z]));
    }
    fn walk(b: &Bound, stack: &mut Vec<BoundTransform>, f: &mut dyn FnMut([[f32; 3]; 8])) {
        if matches!(b.kind, BoundKind::Sphere | BoundKind::Capsule | BoundKind::Box | BoundKind::Cylinder | BoundKind::Disc) {
            let (lo, hi) = (b.box_min, b.box_max);
            let corners = std::array::from_fn(|i| {
                let mut p = rage_formats::Vec3::new(if i & 1 == 0 { lo.x } else { hi.x }, if i & 2 == 0 { lo.y } else { hi.y }, if i & 4 == 0 { lo.z } else { hi.z });
                for xf in stack.iter().rev() {
                    p = xf.apply(p);
                }
                [p.x, p.y, p.z]
            });
            f(corners);
        }
        for (child, xf) in &b.children {
            stack.push(*xf);
            walk(child, stack, f);
            stack.pop();
        }
    }
    let mut boxes: Vec<[[f32; 3]; 8]> = vec![];
    walk(&ybn.root, &mut Vec::new(), &mut |c| boxes.push(c));
    const FACES: [[usize; 4]; 6] = [[0, 1, 3, 2], [4, 6, 7, 5], [0, 2, 6, 4], [1, 5, 7, 3], [0, 4, 5, 1], [2, 3, 7, 6]];
    for c in boxes {
        for f in FACES {
            push_tri(&mut parts, 255, [c[f[0]], c[f[1]], c[f[2]]]);
            push_tri(&mut parts, 255, [c[f[0]], c[f[2]], c[f[3]]]);
        }
    }
    let mut v: Vec<CollPart> = parts.into_values().collect();
    v.sort_by_key(|p| p.material);
    Ok(v)
}

/// Which texture dictionary holds each texture name, over the whole game: what to fall back to when a model's own dictionary and its
/// parents (`gtxd`) do not have a texture (the game keeps such a lookup too). Built by reading every `.ytd` once, on
/// several threads, and kept in the cache folder until the game is updated. Lowercase texture name -> lowercase dictionary name; the
/// first dictionary that has a name wins, and the `+hi` / `+hidr` copies are not used.
pub fn texture_index(game: &Game, ix: &FileIndex, job: &Job) -> HashMap<String, String> {
    use std::sync::{
        atomic::{AtomicUsize, Ordering::Relaxed},
        Mutex,
    };
    let file = cache_dir().join("texture_index.tsv");
    let stamp = game.stamp();
    if let Ok(text) = std::fs::read_to_string(&file) {
        let mut lines = text.lines();
        if lines.next() == Some(stamp.as_str()) {
            return lines.filter_map(|l| l.split_once('\t').map(|(n, d)| (n.to_string(), d.to_string()))).collect();
        }
    }
    let ytds: Vec<(String, String)> = ix
        .find("*", &Filter { exts: vec!["ytd".into()], ..Default::default() }, false, usize::MAX)
        .into_iter()
        .filter(|r| !r.name().contains('+'))
        .map(|r| (r.path.clone(), r.name().trim_end_matches(".ytd").to_lowercase()))
        .collect();
    let mut groups: HashMap<String, Vec<(usize, String)>> = HashMap::new();
    for (n, (path, _)) in ytds.iter().enumerate() {
        if let Some(i) = path.to_lowercase().rfind(".rpf/") {
            groups.entry(path[..i + 4].to_string()).or_default().push((n, path[i + 5..].to_string()));
        }
    }
    let groups: Vec<(String, Vec<(usize, String)>)> = groups.into_iter().collect();
    job.set_total(groups.len());
    let next = AtomicUsize::new(0);
    let found: Mutex<Vec<(usize, Vec<String>)>> = Mutex::new(vec![]);
    std::thread::scope(|s| {
        for _ in 0..std::thread::available_parallelism().map_or(4, |n| n.get()).min(8) {
            s.spawn(|| loop {
                let g = next.fetch_add(1, Relaxed);
                if g >= groups.len() || job.is_cancelled() {
                    break;
                }
                let (arc, files) = &groups[g];
                let mut local = vec![];
                if let Ok(ar) = game.archive(arc) {
                    for (n, inner) in files {
                        if let Some(names) = ar.read(inner).ok().and_then(|d| Ytd::parse(&d).ok()).map(|y| y.names()) {
                            local.push((*n, names));
                        }
                    }
                }
                found.lock().unwrap().extend(local);
                job.step();
            });
        }
    });
    let mut found = found.into_inner().unwrap();
    found.sort_by_key(|(n, _)| *n);
    let mut out: HashMap<String, String> = HashMap::new();
    for (n, names) in found {
        for name in names {
            out.entry(name.to_lowercase()).or_insert_with(|| ytds[n].1.clone());
        }
    }
    if !job.is_cancelled() {
        let mut text = format!("{stamp}\n");
        for (n, d) in &out {
            text.push_str(&format!("{n}\t{d}\n"));
        }
        let _ = std::fs::create_dir_all(cache_dir());
        let _ = std::fs::write(&file, text);
    }
    out
}

/// Where each static collision file (`.ybn`) of the game lies: (name hash, min, max), the box of its root bound. Those files hold world
/// collision in world coordinates, so a view only needs the ones whose box is near the camera. Built by reading every `.ybn` once, on
/// several threads, and kept in the cache folder until the game is updated.
pub fn bounds_index(game: &Game, ix: &FileIndex, job: &Job) -> Vec<(u32, [f32; 3], [f32; 3])> {
    use std::sync::{
        atomic::{AtomicUsize, Ordering::Relaxed},
        Mutex,
    };
    let file = cache_dir().join("bounds_index.tsv");
    let stamp = game.stamp();
    if let Ok(text) = std::fs::read_to_string(&file) {
        let mut lines = text.lines();
        if lines.next() == Some(stamp.as_str()) {
            fn parse_line(l: &str) -> Option<(u32, [f32; 3], [f32; 3])> {
                let (h, rest) = l.split_once('\t')?;
                let n: Vec<f32> = rest.split_whitespace().map(|v| v.parse().ok()).collect::<Option<_>>()?;
                (n.len() == 6).then(|| u32::from_str_radix(h, 16).ok().map(|h| (h, [n[0], n[1], n[2]], [n[3], n[4], n[5]]))).flatten()
            }
            let parsed: Option<Vec<(u32, [f32; 3], [f32; 3])>> = lines.map(parse_line).collect();
            if let Some(v) = parsed {
                return v;
            }
        }
    }
    let ybns: Vec<(String, u32)> = ix
        .find("*", &Filter { exts: vec!["ybn".into()], ..Default::default() }, false, usize::MAX)
        .into_iter()
        .map(|r| (r.path.clone(), rage_formats::rage_joaat(&r.name().trim_end_matches(".ybn").to_lowercase())))
        .collect();
    let mut groups: HashMap<String, Vec<(String, u32)>> = HashMap::new();
    for (path, h) in &ybns {
        if let Some(i) = path.to_lowercase().rfind(".rpf/") {
            groups.entry(path[..i + 4].to_string()).or_default().push((path[i + 5..].to_string(), *h));
        }
    }
    let groups: Vec<(String, Vec<(String, u32)>)> = groups.into_iter().collect();
    job.set_total(groups.len());
    let next = AtomicUsize::new(0);
    let found: Mutex<Vec<(u32, [f32; 3], [f32; 3])>> = Mutex::new(vec![]);
    std::thread::scope(|s| {
        for _ in 0..std::thread::available_parallelism().map_or(4, |n| n.get()).min(8) {
            s.spawn(|| loop {
                let g = next.fetch_add(1, Relaxed);
                if g >= groups.len() || job.is_cancelled() {
                    break;
                }
                let (arc, files) = &groups[g];
                let mut local = vec![];
                if let Ok(ar) = game.archive(arc) {
                    for (inner, h) in files {
                        if let Some(y) = ar.read(inner).ok().and_then(|d| rage_formats::ybn::parse_ybn(&d).ok()) {
                            let (lo, hi) = (y.root.box_min, y.root.box_max);
                            if lo.x.is_finite() && hi.x.is_finite() && lo.x <= hi.x {
                                local.push((*h, [lo.x, lo.y, lo.z], [hi.x, hi.y, hi.z]));
                            }
                        }
                    }
                }
                found.lock().unwrap().extend(local);
                job.step();
            });
        }
    });
    let mut out = found.into_inner().unwrap();
    out.sort_by_key(|(h, _, _)| *h);
    if !job.is_cancelled() {
        let mut text = format!("{stamp}\n");
        for (h, lo, hi) in &out {
            text.push_str(&format!("{h:08x}\t{} {} {} {} {} {}\n", lo[0], lo[1], lo[2], hi[0], hi[1], hi[2]));
        }
        let _ = std::fs::create_dir_all(cache_dir());
        let _ = std::fs::write(&file, text);
    }
    out
}

/// The collision of one `.ybn` as plain triangles, with its box, for ray tests (ground snapping, placing under the cursor).
pub struct CollMesh {
    pub positions: Vec<[f32; 3]>,
    /// three indices per triangle
    pub indices: Vec<u32>,
    pub min: [f32; 3],
    pub max: [f32; 3],
    /// the surface material of each triangle
    pub tri_material: Vec<u8>,
}

impl CollMesh {
    /// All the parts of a `.ybn` (see [`collision_parts`]) as one mesh.
    pub fn from_parts(parts: &[CollPart]) -> Self {
        let (mut positions, mut indices, mut tri_material) = (vec![], vec![], vec![]);
        for p in parts {
            let base = positions.len() as u32;
            positions.extend_from_slice(&p.positions);
            indices.extend(p.indices.iter().map(|i| i + base));
            tri_material.extend(std::iter::repeat(p.material).take(p.indices.len() / 3));
        }
        let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
        for p in &positions {
            for k in 0..3 {
                lo[k] = lo[k].min(p[k]);
                hi[k] = hi[k].max(p[k]);
            }
        }
        Self { positions, indices, min: lo, max: hi, tri_material }
    }

    /// Whether the box contains the point seen from above (x and y only).
    pub fn covers_xy(&self, x: f32, y: f32) -> bool {
        x >= self.min[0] && x <= self.max[0] && y >= self.min[1] && y <= self.max[1]
    }

    /// The nearest hit of the ray (origin `o`, direction `d`, not necessarily unit) within `max_t` of it: the distance along the ray and the
    /// triangle's unit normal turned to face the ray. Triangles are hit from either side.
    pub fn ray(&self, o: [f32; 3], d: [f32; 3], max_t: f32) -> Option<(f32, [f32; 3])> {
        self.ray_tri(o, d, max_t).map(|(t, n, _)| (t, n))
    }

    /// Like [`CollMesh::ray`], and the index of the triangle hit (its material is `tri_material[i]`).
    pub fn ray_tri(&self, o: [f32; 3], d: [f32; 3], max_t: f32) -> Option<(f32, [f32; 3], usize)> {
        // the box first (slab test)
        let (mut t0, mut t1) = (0.0f32, max_t);
        for k in 0..3 {
            if d[k].abs() < 1e-9 {
                if o[k] < self.min[k] || o[k] > self.max[k] {
                    return None;
                }
                continue;
            }
            let (a, b) = ((self.min[k] - o[k]) / d[k], (self.max[k] - o[k]) / d[k]);
            let (a, b) = if a < b { (a, b) } else { (b, a) };
            t0 = t0.max(a);
            t1 = t1.min(b);
            if t0 > t1 {
                return None;
            }
        }
        let sub = |a: [f32; 3], b: [f32; 3]| [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
        let cross = |a: [f32; 3], b: [f32; 3]| [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
        let dot = |a: [f32; 3], b: [f32; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
        let mut best: Option<(f32, [f32; 3], usize)> = None;
        for (ti, t) in self.indices.chunks_exact(3).enumerate() {
            let (a, b, c) = (self.positions[t[0] as usize], self.positions[t[1] as usize], self.positions[t[2] as usize]);
            // Moller-Trumbore
            let (e1, e2) = (sub(b, a), sub(c, a));
            let p = cross(d, e2);
            let det = dot(e1, p);
            if det.abs() < 1e-12 {
                continue;
            }
            let inv = 1.0 / det;
            let tv = sub(o, a);
            let u = dot(tv, p) * inv;
            if !(0.0..=1.0).contains(&u) {
                continue;
            }
            let q = cross(tv, e1);
            let v = dot(d, q) * inv;
            if v < 0.0 || u + v > 1.0 {
                continue;
            }
            let dist = dot(e2, q) * inv;
            if dist < 0.0 || dist > max_t || best.map_or(false, |(bt, _, _)| bt <= dist) {
                continue;
            }
            let mut n = cross(e1, e2);
            let len = dot(n, n).sqrt();
            if len < 1e-12 {
                continue;
            }
            n = [n[0] / len, n[1] / len, n[2] / len];
            if dot(n, d) > 0.0 {
                n = [-n[0], -n[1], -n[2]];
            }
            best = Some((dist, n, ti));
        }
        best
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ray_finds_the_nearest_floor_below_and_its_normal() {
        // two horizontal squares: z = 0 and z = 10, 20 wide
        let sq = |z: f32| vec![[-10.0, -10.0, z], [10.0, -10.0, z], [10.0, 10.0, z], [-10.0, 10.0, z]];
        let mut positions = sq(0.0);
        positions.extend(sq(10.0));
        let m = CollMesh { positions, indices: vec![0, 1, 2, 0, 2, 3, 4, 5, 6, 4, 6, 7], min: [-10.0, -10.0, 0.0], max: [10.0, 10.0, 10.0], tri_material: vec![0; 4] };
        assert!(m.covers_xy(0.0, 0.0) && !m.covers_xy(11.0, 0.0));
        // from 5 m up, straight down: the floor at 0 (5 m away), facing up
        let (t, n) = m.ray([1.0, 1.0, 5.0], [0.0, 0.0, -1.0], 100.0).unwrap();
        assert!((t - 5.0).abs() < 1e-4 && n[2] > 0.99, "{t} {n:?}");
        // from 20 m up: the upper one first
        let (t, _) = m.ray([1.0, 1.0, 20.0], [0.0, 0.0, -1.0], 100.0).unwrap();
        assert!((t - 10.0).abs() < 1e-4);
        // from below, looking up, the normal turns to face the ray (down)
        let (_, n) = m.ray([1.0, 1.0, -5.0], [0.0, 0.0, 1.0], 100.0).unwrap();
        assert!(n[2] < -0.99);
        // a miss beside it, and too far
        assert!(m.ray([50.0, 0.0, 5.0], [0.0, 0.0, -1.0], 100.0).is_none());
        assert!(m.ray([1.0, 1.0, 5.0], [0.0, 0.0, -1.0], 2.0).is_none());
        // below the underground: nothing is clamped, the ray simply finds no floor further down
        assert!(m.ray([1.0, 1.0, -50.0], [0.0, 0.0, -1.0], 100.0).is_none());
    }

    #[test]
    fn material_colours_are_stable_and_differ() {
        assert_eq!(material_color(7), material_color(7));
        assert_ne!(material_color(1), material_color(2));
        assert_ne!(material_color(255), material_color(0));
    }
}

/// The links of a path node file (`.ynd`) as line segments in world space, two points each, lifted a little above the road. Links to nodes in
/// another cell are drawn as a short stub, as the other end is in another file.
pub fn path_segments(bytes: &[u8]) -> Result<Vec<[f32; 3]>> {
    let d = rage_formats::ynd::parse_node_dictionary(bytes)?;
    let pos = |n: &rage_formats::ynd::Node| [n.position_x as f32 / 4.0, n.position_y as f32 / 4.0, n.position_z as f32 / 32.0 + 0.4];
    let mut out = vec![];
    for n in &d.nodes {
        let start = n.link_id as usize;
        let count = (n.link_count_flags >> 3) as usize;
        let a = pos(n);
        let Some(links) = d.links.get(start..start + count) else { continue };
        for l in links {
            if l.area_id == n.area_id {
                if let Some(t) = d.nodes.get(l.node_id as usize) {
                    // each pair once: from the lower node id
                    if t.node_id > n.node_id || t.area_id != n.area_id {
                        out.push(a);
                        out.push(pos(t));
                    }
                }
            } else {
                out.push(a);
                out.push([a[0], a[1], a[2] + 2.0]);
            }
        }
    }
    Ok(out)
}

/// The polygon edges of a navigation mesh (`.ynv`) as line segments in world space.
pub fn nav_segments(bytes: &[u8]) -> Result<Vec<[f32; 3]>> {
    let m = rage_formats::ynv::parse_ynv(bytes)?;
    let mut out = vec![];
    for p in &m.polys {
        let n = p.vertices.len();
        for i in 0..n {
            let (a, b) = (p.vertices[i], p.vertices[(i + 1) % n]);
            out.push([a.x, a.y, a.z + 0.15]);
            out.push([b.x, b.y, b.z + 0.15]);
        }
    }
    Ok(out)
}

/// The grid cells of a line overlay (kind 0: path node files, the `nodes<n>.ynd` number; kind 1: navigation meshes, x * 100 + y) that touch the
/// box `x0..x1`, `y0..y1`.
pub fn overlay_cells(kind: u8, x0: f32, y0: f32, x1: f32, y1: f32) -> Vec<u32> {
    let mut out = vec![];
    if kind == 0 {
        let (a, b) = (rage_formats::ynd::cell_for_position(x0, y0), rage_formats::ynd::cell_for_position(x1, y1));
        for cy in a.1..=b.1 {
            for cx in a.0..=b.0 {
                out.push(rage_formats::ynd::area_id(cx, cy));
            }
        }
    } else {
        let (a, b) = (rage_formats::ynv::cell_for_position(x0, y0), rage_formats::ynv::cell_for_position(x1, y1));
        for cy in a.1..=b.1 {
            for cx in a.0..=b.0 {
                out.push(cx * 100 + cy);
            }
        }
    }
    out
}


/// The scenario points of one region file (`levels/gta5/scenario/*.ymt`, a `CScenarioPointRegion`), as the World view draws them.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct ScenarioRegion {
    /// each point: position, heading (the fourth value of `vPositionAndDirection`), scenario type index (`iType`)
    pub points: Vec<([f32; 3], f32, u8)>,
    /// chaining graph nodes (positions) and the edges between them (node indices)
    pub chain_nodes: Vec<[f32; 3]>,
    pub chain_edges: Vec<(u16, u16)>,
}

// field name hashes of a `CScenarioPointRegion` (the names hashed as written, not lowercased)
const SC_POINTS: u32 = 0x29e21837;
const SC_MY_POINTS: u32 = 0x45c8b3d0;
const SC_CLUSTERS: u32 = 0xd5dc5baa;
const SC_POS_DIR: u32 = 0x00477ced;
const SC_TYPE: u32 = 0xc1d9350e;
const SC_CHAINING: u32 = 0xdc4d2d41;
const SC_NODES: u32 = 0x2feea5d2;
const SC_EDGES: u32 = 0x56c94556;
const SC_NODE_POS: u32 = 0x0c928991;
const SC_EDGE_FROM: u32 = 0xc0ed9f26;
const SC_EDGE_TO: u32 = 0xa9fb1b57;

/// Read a scenario region file: its points (also those inside its clusters) and its chaining graph.
pub fn scenario_region(bytes: &[u8]) -> Result<ScenarioRegion> {
    use rage_formats::value::MetaValue;
    let (_, dump) = rage_formats::ymf::dump_metadata(bytes)?;
    let root = dump.root.as_struct().ok_or_else(|| anyhow::anyhow!("scenario: the root is not a structure"))?;
    let mut out = ScenarioRegion::default();
    let mut take_points = |container: &MetaValue| {
        let Some(c) = container.as_struct() else { return };
        for p in c.get(SC_MY_POINTS).map(MetaValue::items).unwrap_or(&[]) {
            let Some(s) = p.as_struct() else { continue };
            if let Some(MetaValue::Vec4(v)) = s.get(SC_POS_DIR) {
                let ty = match s.get(SC_TYPE) {
                    Some(MetaValue::U8(t)) => *t,
                    _ => 0,
                };
                out.points.push(([v.x, v.y, v.z], v.w, ty));
            }
        }
    };
    if let Some(p) = root.get(SC_POINTS) {
        take_points(p);
    }
    for cl in root.get(SC_CLUSTERS).map(MetaValue::items).unwrap_or(&[]) {
        if let Some(p) = cl.as_struct().and_then(|s| s.get(SC_POINTS)) {
            take_points(p);
        }
    }
    if let Some(g) = root.get(SC_CHAINING).and_then(MetaValue::as_struct) {
        for n in g.get(SC_NODES).map(MetaValue::items).unwrap_or(&[]) {
            if let Some(MetaValue::Vec3(v)) = n.as_struct().and_then(|s| s.get(SC_NODE_POS)) {
                out.chain_nodes.push([v.x, v.y, v.z]);
            }
        }
        for e in g.get(SC_EDGES).map(MetaValue::items).unwrap_or(&[]) {
            if let Some(s) = e.as_struct() {
                if let (Some(MetaValue::U16(a)), Some(MetaValue::U16(b))) = (s.get(SC_EDGE_FROM), s.get(SC_EDGE_TO)) {
                    out.chain_edges.push((*a, *b));
                }
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod scenario_real {
    use super::*;

    /// Reads a copy of a scenario file when one is at `target/scratch/scen.ymt` (the user's own game file; nothing is shipped).
    #[test]
    fn a_scenario_region_has_points_and_a_chaining_graph() {
        let Ok(d) = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../../target/scratch/scen.ymt")) else { return };
        let r = scenario_region(&d).unwrap();
        eprintln!("scenario: {} points, {} chain nodes, {} edges", r.points.len(), r.chain_nodes.len(), r.chain_edges.len());
        assert!(r.points.len() > 100 && r.chain_nodes.len() > 100);
        assert!(r.chain_edges.iter().all(|(a, b)| (*a as usize) < r.chain_nodes.len() && (*b as usize) < r.chain_nodes.len()));
    }
}

/// An ambient audio zone of a `*.dat151.rel` file: where an ambient sound set plays.
#[derive(Debug, Clone, PartialEq)]
pub struct AudioZone {
    pub name: String,
    /// 0 box, 1 sphere, 2 line
    pub shape: u8,
    /// the zone that activates it (outer): centre, full size, rotation about Z in degrees
    pub act_centre: [f32; 3],
    pub act_size: [f32; 3],
    pub act_angle: f32,
    /// the zone the sound is positioned in (inner)
    pub pos_centre: [f32; 3],
    pub pos_size: [f32; 3],
    pub pos_angle: f32,
}

/// The ambient zones (type 37) of a `dat151.rel` file: the file is a data block, a name table, an index of (name hash, offset, length), and each
/// item starts with a word holding its type in the low byte and its name's offset in the rest.
pub fn audio_zones(bytes: &[u8]) -> Result<Vec<AudioZone>> {
    let u32_at = |o: usize| -> Result<u32> { bytes.get(o..o + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]])).ok_or_else(|| anyhow::anyhow!("rel: truncated")) };
    let f32_at = |o: usize| -> Result<f32> { Ok(f32::from_bits(u32_at(o)?)) };
    let u16_at = |o: usize| -> Result<u16> { bytes.get(o..o + 2).map(|b| u16::from_le_bytes([b[0], b[1]])).ok_or_else(|| anyhow::anyhow!("rel: truncated")) };
    let v3 = |o: usize| -> Result<[f32; 3]> { Ok([f32_at(o)?, f32_at(o + 4)?, f32_at(o + 8)?]) };
    if u32_at(0)? != 151 {
        anyhow::bail!("rel: not a dat151 file");
    }
    let data_len = u32_at(4)? as usize;
    // the name table follows the data block
    let mut p = 8 + data_len;
    let _table_len = u32_at(p)?;
    let count = u32_at(p + 4)? as usize;
    p += 8;
    let offsets: Vec<u32> = (0..count).map(|i| u32_at(p + i * 4)).collect::<Result<_>>()?;
    p += count * 4;
    let mut names = Vec::with_capacity(count);
    for _ in 0..count {
        let end = bytes[p..].iter().position(|&b| b == 0).ok_or_else(|| anyhow::anyhow!("rel: unterminated name"))?;
        names.push(String::from_utf8_lossy(&bytes[p..p + end]).to_string());
        p += end + 1;
    }
    let index_count = u32_at(p)? as usize;
    p += 4;
    let mut out = vec![];
    for i in 0..index_count {
        let (offset, length) = (u32_at(p + i * 12 + 4)? as usize, u32_at(p + i * 12 + 8)? as usize);
        // item bytes are inside the data block, which starts at file offset 8
        let base = offset + 8;
        if length < 4 || bytes.len() < base + length {
            continue;
        }
        let head = u32_at(base)?;
        if head & 0xFF != 37 || length < 0xB0 {
            continue;
        }
        let _ = (&offsets, &names, head >> 8);
        // the item's name is its index hash: the name table of these files is empty
        let name = rage_formats::names::unknown(u32_at(p + i * 12)?).to_string();
        // 4 header bytes, flags, shape + padding (8 bytes), then the two zones in the same layout
        let shape = bytes[base + 8];
        let zone = |at: usize| -> Result<([f32; 3], [f32; 3], f32)> { Ok((v3(base + at)?, v3(base + at + 16)?, u16_at(base + at + 64)? as f32)) };
        let (act_centre, act_size, act_angle) = zone(16)?;
        let (pos_centre, pos_size, pos_angle) = zone(96)?;
        out.push(AudioZone { name, shape, act_centre, act_size, act_angle, pos_centre, pos_size, pos_angle });
    }
    Ok(out)
}

#[cfg(test)]
mod audio_real {
    use super::*;

    /// Reads a copy of the game's `game.dat151.rel` when one is at `target/scratch/game.dat151.rel` (the user's own game file).
    #[test]
    fn the_game_has_ambient_audio_zones() {
        let Ok(d) = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../../target/scratch/game.dat151.rel")) else { return };
        let z = audio_zones(&d).unwrap();
        eprintln!("audio zones: {}", z.len());
        for q in z.iter().take(3) {
            eprintln!("  {q:?}");
        }
        assert!(z.len() > 100);
    }
}

/// The game's water map (`waterheight.dat`, big-endian, magic `WMAP`): rivers and lakes as lists of (x, y, z or half width, half height) vectors,
/// and pools as boxes.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Watermap {
    /// each river: its points (x, y, z, width)
    pub rivers: Vec<Vec<[f32; 4]>>,
    /// each lake: its height and its boxes (x, y, half x, half y)
    pub lakes: Vec<(f32, Vec<[f32; 4]>)>,
    /// each pool: position and half sizes
    pub pools: Vec<([f32; 3], [f32; 3])>,
}

pub fn watermap(b: &[u8]) -> Result<Watermap> {
    let be32 = |o: usize| -> Result<u32> { b.get(o..o + 4).map(|v| u32::from_be_bytes([v[0], v[1], v[2], v[3]])).ok_or_else(|| anyhow::anyhow!("watermap: truncated")) };
    let be16 = |o: usize| -> Result<u16> { b.get(o..o + 2).map(|v| u16::from_be_bytes([v[0], v[1]])).ok_or_else(|| anyhow::anyhow!("watermap: truncated")) };
    let bef = |o: usize| -> Result<f32> { Ok(f32::from_bits(be32(o)?)) };
    if be32(0)? != 0x574D4150 {
        anyhow::bail!("watermap: not a WMAP file");
    }
    let height = be16(0x1E)? as usize;
    let (inds, refs) = (be32(0x20)? as usize, be32(0x24)? as usize);
    let (river_vecs, river_count, lake_vecs, lake_count, pool_count) = (be16(0x28)? as usize, be16(0x2A)? as usize, be16(0x2C)? as usize, be16(0x2E)? as usize, be16(0x30)? as usize);
    // the header is 60 bytes (with its 8 flag bytes); then the row headers, the indices and the references
    let mut p = 60;
    let shorts = (inds + refs) * 2 + height * 4;
    let pad = (16 - shorts % 16) % 16;
    p += shorts + pad;
    let vec4 = |o: usize| -> Result<[f32; 4]> { Ok([bef(o)?, bef(o + 4)?, bef(o + 8)?, bef(o + 12)?]) };
    let rv: Vec<[f32; 4]> = (0..river_vecs).map(|i| vec4(p + i * 16)).collect::<Result<_>>()?;
    p += river_vecs * 16;
    let flow = |o: usize, vecs: &[[f32; 4]]| -> Result<(f32, Vec<[f32; 4]>)> {
        let z = bef(o + 8)?;
        let (n, off) = (*b.get(o + 32).ok_or_else(|| anyhow::anyhow!("watermap: truncated"))? as usize, be16(o + 34)? as usize);
        Ok((z, (0..n).filter_map(|i| vecs.get(off + i).copied()).collect()))
    };
    let mut out = Watermap::default();
    for i in 0..river_count {
        out.rivers.push(flow(p + i * 48, &rv)?.1);
    }
    p += river_count * 48;
    let lv: Vec<[f32; 4]> = (0..lake_vecs).map(|i| vec4(p + i * 16)).collect::<Result<_>>()?;
    p += lake_vecs * 16;
    for i in 0..lake_count {
        out.lakes.push(flow(p + i * 48, &lv)?);
    }
    p += lake_count * 48;
    for i in 0..pool_count {
        let o = p + i * 32;
        out.pools.push(([bef(o)?, bef(o + 4)?, bef(o + 8)?], [bef(o + 16)?, bef(o + 20)?, bef(o + 24)?]));
    }
    Ok(out)
}

#[cfg(test)]
mod watermap_real {
    use super::*;

    /// Reads a copy of the game's `waterheight.dat` when one is at `target/scratch/waterheight.dat` (the user's own game file).
    #[test]
    fn the_games_watermap_has_rivers_lakes_and_pools() {
        let Ok(d) = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../../target/scratch/waterheight.dat")) else { return };
        let w = watermap(&d).unwrap();
        eprintln!("watermap: {} rivers, {} lakes, {} pools; first pool {:?}", w.rivers.len(), w.lakes.len(), w.pools.len(), w.pools.first());
        assert_eq!((w.rivers.len(), w.lakes.len(), w.pools.len()), (13, 15, 314));
    }
}

/// The occluders of a `.ymap` as world-space triangles: its occlude models (their vertices are floats, then the triangle indices as bytes) and
/// its box occluders (centre, size in quarter metres, turned about Z by `atan2(cos, sin)`), as 12 triangles each.
/// The game culls what hides behind these; empty for a map without any.
pub fn occluder_triangles(ymap: &[u8]) -> Result<Vec<[[f32; 3]; 3]>> {
    use rage_formats::{dump_meta, dump_pso, is_pso, MetaValue};
    let root = if is_pso(ymap) { dump_pso(ymap)?.root } else { dump_meta(ymap)?.root };
    let Some(map) = root.as_struct() else { return Ok(vec![]) };
    let mut out = vec![];
    for m in map.field("occludeModels").map_or(&[][..], MetaValue::items) {
        let Some(m) = m.as_struct() else { continue };
        let Some(MetaValue::Bytes(data)) = m.field("verts") else { continue };
        let split = m.field("numVertsInBytes").and_then(MetaValue::as_i64).unwrap_or(0).max(0) as usize;
        let split = split.min(data.len());
        let f = |o: usize| f32::from_le_bytes([data[o], data[o + 1], data[o + 2], data[o + 3]]);
        let verts: Vec<[f32; 3]> = (0..split / 12).map(|i| [f(i * 12), f(i * 12 + 4), f(i * 12 + 8)]).collect();
        for t in data[split..].chunks_exact(3) {
            if let (Some(a), Some(b), Some(c)) = (verts.get(t[0] as usize), verts.get(t[1] as usize), verts.get(t[2] as usize)) {
                out.push([*a, *b, *c]);
            }
        }
    }
    for b in map.field("boxOccluders").map_or(&[][..], MetaValue::items) {
        let Some(b) = b.as_struct() else { continue };
        let g = |n: &str| b.field(n).and_then(MetaValue::as_f32).unwrap_or(0.0);
        let (c, s) = ([g("iCenterX") * 0.25, g("iCenterY") * 0.25, g("iCenterZ") * 0.25], [g("iLength") * 0.125, g("iWidth") * 0.125, g("iHeight") * 0.125]);
        let angle = (g("iCosZ") / 32767.0).atan2(g("iSinZ") / 32767.0);
        let (sn, cs) = angle.sin_cos();
        let p = |x: f32, y: f32, z: f32| [c[0] + cs * x * s[0] - sn * y * s[1], c[1] + sn * x * s[0] + cs * y * s[1], c[2] + z * s[2]];
        let v: Vec<[f32; 3]> = (0..8).map(|i| p(if i & 1 == 0 { -1.0 } else { 1.0 }, if i & 2 == 0 { -1.0 } else { 1.0 }, if i & 4 == 0 { -1.0 } else { 1.0 })).collect();
        for q in [[0, 1, 3, 2], [4, 5, 7, 6], [0, 1, 5, 4], [2, 3, 7, 6], [0, 2, 6, 4], [1, 3, 7, 5]] {
            out.push([v[q[0]], v[q[1]], v[q[2]]]);
            out.push([v[q[0]], v[q[2]], v[q[3]]]);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod occluders_real {
    use super::*;

    /// A copy of one of the game's occluder maps at `target/scratch/occl.ymap` (the user's own file): 8 occlude models, no box occluders.
    #[test]
    fn a_games_occluder_map_gives_triangles_inside_its_extents() {
        let Ok(d) = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../../target/scratch/occl.ymap")) else { return };
        let t = occluder_triangles(&d).unwrap();
        eprintln!("{} occluder triangles", t.len());
        assert!(t.len() > 200);
        // the file's entity extents are x -437.9..-138.5, y -1441.4..-469.5
        assert!(t.iter().flatten().all(|p| p[0] > -440.0 && p[0] < -130.0 && p[1] > -1445.0 && p[1] < -465.0), "triangles lie in the map's extents");
    }
}
