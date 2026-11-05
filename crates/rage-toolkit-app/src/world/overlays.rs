//! World overlays made from the game's data files: train tracks (`trains*.dat`), population zones (`popzone.ipl`) and the world heightmap
//! (`heightmap.dat`). They are read once with the map and drawn on request.
use rage_toolkit_core::{
    worldextra,
    game::Game,
    index::{FileIndex, Filter},
};

/// What the overlay files hold.
#[derive(Default)]
pub(super) struct OverlayData {
    /// each train track as its nodes
    pub trains: Vec<Vec<[f32; 3]>>,
    /// population zones: name, label, min x, min y, max x, max y
    pub popzones: Vec<(String, String, [f32; 4])>,
    pub heightmap: Option<rage_formats::WorldHeightmap>,
    /// scenario points (position, heading, type) and the chaining graph links as segments, from every region file
    pub scenario_points: Vec<([f32; 3], f32, u8)>,
    pub scenario_chains: Vec<[f32; 3]>,
    pub scenario_files: usize,
    /// ambient audio zones of every `dat151.rel`
    pub audio_zones: Vec<worldextra::AudioZone>,
    /// rivers, lakes and pools of the water map
    pub watermap: Option<worldextra::Watermap>,
    /// the weathers' light, sky and fog by hour
    pub timecycle: rage_toolkit_core::timecycle::Timecycle,
    /// the occluders of every map (world-space triangles), which hide what lies behind them
    pub occluders: super::occlusion::OccluderGrid,
}

/// The nodes of a `trains*.dat` file: a count line, then `x y z flag` per node.
pub(super) fn parse_train(text: &str) -> Vec<[f32; 3]> {
    text.lines()
        .skip(1)
        .filter_map(|l| {
            let v: Vec<f32> = l.split_whitespace().take(3).filter_map(|t| t.parse().ok()).collect();
            (v.len() == 3).then(|| [v[0], v[1], v[2]])
        })
        .collect()
}

/// The zones of a `popzone.ipl`: lines `name, minx, miny, minz, maxx, maxy, maxz, label, ...` between `zone` and `end`.
pub(super) fn parse_popzones(text: &str) -> Vec<(String, String, [f32; 4])> {
    let mut out = vec![];
    let mut inside = false;
    for l in text.lines() {
        let l = l.trim();
        if l.eq_ignore_ascii_case("zone") {
            inside = true;
        } else if l.eq_ignore_ascii_case("end") {
            inside = false;
        } else if inside {
            let p: Vec<&str> = l.split(',').map(str::trim).collect();
            if p.len() >= 8 {
                let f: Vec<f32> = p[1..7].iter().filter_map(|t| t.parse().ok()).collect();
                if f.len() == 6 {
                    out.push((p[0].to_string(), p[7].to_string(), [f[0], f[1], f[3], f[4]]));
                }
            }
        }
    }
    out
}

/// Read the overlay files from the game (the last copy of each file wins).
pub(super) fn load(game: &Game, ix: &FileIndex) -> OverlayData {
    let last = |pattern: &str, must: &str| -> Option<String> { ix.find(pattern, &Filter::default(), false, usize::MAX).into_iter().filter(|r| r.path.contains(must)).last().map(|r| r.path.clone()) };
    let mut d = OverlayData::default();
    for r in ix.find("trains*.dat", &Filter::default(), false, usize::MAX) {
        if r.path.contains("levels/gta5/") {
            if let Ok(b) = game.read(&r.path) {
                let t = parse_train(&String::from_utf8_lossy(&b));
                if t.len() > 1 {
                    d.trains.push(t);
                }
            }
        }
    }
    for r in ix.find("*.ymt", &Filter::default(), false, usize::MAX) {
        if r.path.contains("/scenario/") {
            if let Ok(region) = game.read(&r.path).map_err(anyhow::Error::from).and_then(|b| worldextra::scenario_region(&b)) {
                d.scenario_files += 1;
                for (a, b) in &region.chain_edges {
                    if let (Some(p), Some(q)) = (region.chain_nodes.get(*a as usize), region.chain_nodes.get(*b as usize)) {
                        d.scenario_chains.push(*p);
                        d.scenario_chains.push(*q);
                    }
                }
                d.scenario_points.extend(region.points);
            }
        }
    }
    if let Some(p) = last("waterheight.dat", "levels/gta5/") {
        d.watermap = game.read(&p).ok().and_then(|b| worldextra::watermap(&b).ok());
    }
    d.timecycle = load_timecycle(game, ix);
    let mut occluders = vec![];
    for r in ix.find("*occl*.ymap", &Filter::default(), false, usize::MAX) {
        if let Some(t) = game.read(&r.path).ok().and_then(|b| worldextra::occluder_triangles(&b).ok()) {
            occluders.extend(t);
        }
    }
    d.occluders = super::occlusion::OccluderGrid::new(occluders);
    let mut seen = std::collections::HashSet::new();
    for r in ix.find("*dat151.rel", &Filter::default(), false, usize::MAX) {
        if let Ok(zones) = game.read(&r.path).map_err(anyhow::Error::from).and_then(|b| worldextra::audio_zones(&b)) {
            for z in zones {
                // the same zone can be in a file and in its update
                if seen.insert((z.name.clone(), z.act_centre.map(f32::to_bits))) {
                    d.audio_zones.push(z);
                }
            }
        }
    }
    if let Some(p) = last("popzone.ipl", "levels/gta5/") {
        if let Ok(b) = game.read(&p) {
            d.popzones = parse_popzones(&String::from_utf8_lossy(&b));
        }
    }
    if let Some(p) = last("heightmap.dat", "levels/gta5/") {
        if let Ok(b) = game.read(&p) {
            d.heightmap = rage_formats::parse_heightmap(&b).ok();
        }
    }
    d
}

/// `time.xml` and every `w_*.xml` of the timecycle folder (the last copy of a file wins, so the update's replaces the base game's), in the order of
/// `WEATHER_FILES`
fn load_timecycle(game: &Game, ix: &FileIndex) -> rage_toolkit_core::timecycle::Timecycle {
    use rage_toolkit_core::timecycle::{parse_cycle, parse_hours, Timecycle, WEATHER_FILES};
    let read = |path: &str| game.read(path).ok().map(|b| String::from_utf8_lossy(&b).into_owned());
    let hours = ix.find("time.xml", &Filter::default(), false, usize::MAX).into_iter().filter(|r| r.path.contains("levels/gta5/")).last().and_then(|r| read(&r.path)).map(|t| parse_hours(&t)).unwrap_or_default();
    let mut files = std::collections::HashMap::new();
    for r in ix.find("w_*.xml", &Filter::default(), false, usize::MAX) {
        if r.path.contains("/timecycle/") {
            files.insert(r.path.rsplit('/').next().unwrap_or("").to_ascii_lowercase(), r.path.clone());
        }
    }
    let cycles = WEATHER_FILES.iter().filter_map(|w| files.get(&format!("w_{w}.xml")).and_then(|p| read(p)).and_then(|t| parse_cycle(&t))).collect();
    Timecycle { hours, cycles }
}

/// the train tracks as line segments
pub(super) fn train_segments(d: &OverlayData) -> Vec<[f32; 3]> {
    let mut out = vec![];
    for t in &d.trains {
        for w in t.windows(2) {
            out.push([w[0][0], w[0][1], w[0][2] + 1.5]);
            out.push([w[1][0], w[1][1], w[1][2] + 1.5]);
        }
    }
    out
}

/// each scenario point as a post with a short arrow in its heading, as line segments
pub(super) fn scenario_segments(d: &OverlayData) -> Vec<[f32; 3]> {
    let mut out = Vec::with_capacity(d.scenario_points.len() * 4);
    for (p, heading, _) in &d.scenario_points {
        let top = [p[0], p[1], p[2] + 1.6];
        out.push(*p);
        out.push(top);
        // the direction the point faces (the heading in radians about Z)
        out.push(top);
        out.push([p[0] - heading.sin() * 0.9, p[1] + heading.cos() * 0.9, p[2] + 1.6]);
    }
    out
}

/// the water map as line segments: each river with its two banks (20 m either side), the box of each lake part and each pool
pub(super) fn watermap_segments(w: &worldextra::Watermap) -> Vec<[f32; 3]> {
    let mut out = vec![];
    let rect = |out: &mut Vec<[f32; 3]>, cx: f32, cy: f32, hx: f32, hy: f32, z: f32| {
        let c = [[cx - hx, cy - hy, z], [cx + hx, cy - hy, z], [cx + hx, cy + hy, z], [cx - hx, cy + hy, z]];
        for k in 0..4 {
            out.push(c[k]);
            out.push(c[(k + 1) % 4]);
        }
    };
    for r in &w.rivers {
        for pair in r.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
            let len = (dx * dx + dy * dy).sqrt().max(1e-3);
            let (sx, sy) = (-dy / len * 20.0, dx / len * 20.0);
            for side in [-1.0f32, 0.0, 1.0] {
                out.push([a[0] + sx * side, a[1] + sy * side, a[2]]);
                out.push([b[0] + sx * side, b[1] + sy * side, b[2]]);
            }
        }
    }
    for (z, parts) in &w.lakes {
        for v in parts {
            rect(&mut out, v[0], v[1], v[2], v[3], *z);
        }
    }
    for (p, h) in &w.pools {
        rect(&mut out, p[0], p[1], h[0], h[1], p[2]);
    }
    out
}

/// the heightmap's highest ground as a grid of lines (each cell joined to the next one along and across)
pub(super) fn heightmap_segments(h: &rage_formats::WorldHeightmap) -> Vec<[f32; 3]> {
    let (w, ht) = (h.width as usize, h.height as usize);
    if w < 2 || ht < 2 {
        return vec![];
    }
    let (sx, sy) = ((h.bb_max.x - h.bb_min.x) / w as f32, (h.bb_max.y - h.bb_min.y) / ht as f32);
    let dz = h.bb_max.z - h.bb_min.z;
    let at = |x: usize, y: usize| -> Option<[f32; 3]> {
        let v = h.max_heights[y * w + x];
        (v > 0).then(|| [h.bb_min.x + (x as f32 + 0.5) * sx, h.bb_min.y + (y as f32 + 0.5) * sy, h.bb_min.z + v as f32 / 255.0 * dz])
    };
    let mut out = vec![];
    for y in 0..ht {
        for x in 0..w {
            let Some(a) = at(x, y) else { continue };
            if x + 1 < w {
                if let Some(b) = at(x + 1, y) {
                    out.push(a);
                    out.push(b);
                }
            }
            if y + 1 < ht {
                if let Some(b) = at(x, y + 1) {
                    out.push(a);
                    out.push(b);
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trains_and_zones_are_read() {
        assert_eq!(parse_train("3\n1 2 3 0\n4 5 6 0\nx y z\n"), vec![[1.0, 2.0, 3.0], [4.0, 5.0, 6.0]]);
        let z = parse_popzones("zone\nZ_A, -10, -20, 0.0, 30, 40, 1250.0, Label, 0\nend\nZ_B, 1, 2, 3, 4, 5, 6, X, 0\n");
        assert_eq!(z, vec![("Z_A".to_string(), "Label".to_string(), [-10.0, -20.0, 30.0, 40.0])]);
    }
}
