//! The whole map in memory: every entity of every `.ymap` of the game, the size of every archetype from the `.ytyp` files, and a
//! grid to ask "what is near here". Only the entity lists are read (not the full file trees), so it is some tens of seconds of work,
//! done on several threads, once per run.
//!
//! A `.ymap` that exists in several places (the base game and an update or DLC) is loaded once, from the last one: that is how the
//! game picks it.
use crate::{game::Game, index::{FileIndex, Filter}, job::Job};
use anyhow::{bail, Result};
use rage_formats::{parse_ymap_entities, ytyp::parse_ytyp};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicUsize, Ordering::Relaxed},
        Mutex,
    },
};

/// side of a grid cell in metres
pub const CELL: f32 = 256.0;

#[derive(Clone, Copy, Debug)]
pub struct WorldEnt {
    pub pos: [f32; 3],
    /// quaternion as stored in the file (x, y, z, w)
    pub rot: [f32; 4],
    pub scale_xy: f32,
    pub scale_z: f32,
    pub lod_dist: f32,
    /// how close the camera must be for this entity's LOD children to be drawn instead of it; 0 or less = half of `lod_dist`
    pub child_lod_dist: f32,
    /// how many entities use this one as their LOD parent (all files together)
    pub num_children: u32,
    pub hash: u32,
    pub flags: u32,
    /// the entity's guid in its file (with the archetype, it tells an entity apart even if the file's order changes)
    pub guid: u32,
    /// index into [`WorldData::maps`]
    pub map: u32,
    /// the entity's place in its own `.ymap` (what `YmapDocument::entity` takes)
    pub index: u32,
    /// 0 = HD, 1 = LOD, 2.. = SLOD levels, 5 = orphan HD (`rage__eLodType`)
    pub lod_level: u8,
    pub is_mlo: bool,
    /// the entity this one is a LOD child of, in this file or in the parent map ([`NO_PARENT`] for a root)
    pub parent: u32,
}

/// [`WorldEnt::parent`] of an entity with no LOD parent.
pub const NO_PARENT: u32 = u32::MAX;

/// What an archetype's `.ytyp` says about it.
#[derive(Clone, Copy, Debug)]
pub struct ArchInfo {
    pub bb_min: [f32; 3],
    pub bb_max: [f32; 3],
    /// `Archetype::ASSET_TYPE_*`
    pub asset_type: u32,
    /// the `.ydd` that holds the model, when the type says so
    pub ydd_hash: u32,
    pub asset_hash: u32,
    /// `textureDictionary` hash, 0 when none
    pub txd_hash: u32,
    /// the archetype's own LOD distance: what an entity with a `lodDist` of 0 or less (-1) uses
    pub lod_dist: f32,
    /// the archetype's flags (bit 2048 marks a shadow proxy)
    pub flags: u32,
    /// index into [`WorldData::ytyps`]
    pub ytyp: u32,
    pub is_mlo: bool,
}

pub struct WorldData {
    /// full game paths of the `.ymap` files, by [`WorldEnt::map`]
    pub maps: Vec<String>,
    pub ytyps: Vec<String>,
    pub ents: Vec<WorldEnt>,
    pub arch: HashMap<u32, ArchInfo>,
    /// files that could not be read, with the reason of the first few
    pub skipped: Vec<String>,
    /// HD entities (and orphan HD) and everything else (LOD, SLOD, interiors) in separate grids, so showing only HD skips the rest
    grid_hd: HashMap<(i32, i32), Cell>,
    grid_other: HashMap<(i32, i32), Cell>,
    kid_start: Vec<u32>,
    kid_list: Vec<u32>,
    /// the entities of each map file (by [`WorldEnt::map`]), as offsets into one list
    map_start: Vec<u32>,
    map_list: Vec<u32>,
}

/// The entities of one grid cell, and how far the model of any of them reaches from its position: a cell can be left out of a view when
/// even that far reach would not come into it.
struct Cell {
    ents: Vec<u32>,
    reach: f32,
}

pub fn cell_of(x: f32, y: f32) -> (i32, i32) {
    ((x / CELL).floor() as i32, (y / CELL).floor() as i32)
}

/// archetypes of the user's own `.ytyp` files, known next to the game's (a modder's props have a size and a name too)
fn custom() -> &'static std::sync::RwLock<HashMap<u32, ArchInfo>> {
    static C: std::sync::OnceLock<std::sync::RwLock<HashMap<u32, ArchInfo>>> = std::sync::OnceLock::new();
    C.get_or_init(Default::default)
}

/// The archetypes a `.ytyp` file defines, as the world uses them.
pub fn archetypes_of_ytyp(bytes: &[u8]) -> Result<Vec<(u32, ArchInfo)>> {
    let v = parse_ytyp(bytes)?;
    Ok(v.archetypes
        .iter()
        .map(|a| {
            (
                a.name_hash,
                ArchInfo {
                    bb_min: [a.bb_min.x, a.bb_min.y, a.bb_min.z],
                    bb_max: [a.bb_max.x, a.bb_max.y, a.bb_max.z],
                    asset_type: a.asset_type,
                    ydd_hash: a.drawable_dictionary_hash,
                    asset_hash: a.asset_name_hash,
                    txd_hash: a.texture_dict_hash,
                    lod_dist: a.lod_dist,
                    flags: a.flags,
                    ytyp: u32::MAX,
                    is_mlo: a.is_mlo,
                },
            )
        })
        .collect())
}

/// Make these archetypes known (the game's own win when both define a name).
pub fn add_custom_archetypes(list: Vec<(u32, ArchInfo)>) {
    let mut c = custom().write().unwrap();
    for (h, a) in list {
        c.insert(h, a);
    }
}

pub fn clear_custom_archetypes() {
    custom().write().unwrap().clear();
}

pub fn custom_archetype_count() -> usize {
    custom().read().unwrap().len()
}

impl WorldData {
    /// The archetype `hash` names: the game's, else one the user loaded from a `.ytyp`.
    pub fn arch_of(&self, hash: &u32) -> Option<ArchInfo> {
        self.arch.get(hash).copied().or_else(|| custom().read().unwrap().get(hash).copied())
    }

    /// How far from the camera an entity is shown: its own `lodDist`, or its archetype's when that is 0 or less.
    pub fn lod_dist_of(&self, e: &WorldEnt) -> f32 {
        if e.lod_dist > 0.0 {
            e.lod_dist
        } else {
            self.arch_of(&e.hash).map_or(0.0, |a| a.lod_dist)
        }
    }

    /// the entities (as indices into `ents`) in the grid cells within `radius` metres of (x, y), nearest cells first
    pub fn near(&self, x: f32, y: f32, radius: f32) -> Vec<&[u32]> {
        self.near_layers(x, y, radius, true, true)
    }

    /// Like [`WorldData::near`], for only the HD entities (`hd`) and/or only the others (`other`: LOD, SLOD and interiors).
    pub fn near_layers(&self, x: f32, y: f32, radius: f32, hd: bool, other: bool) -> Vec<&[u32]> {
        self.near_layers_visible(x, y, radius, hd, other, &|_, _, _| true)
    }

    /// [`WorldData::near_layers`] leaving out the cells `visible` rejects. It is given a cell's x and y bounds (min, max) and how far the models
    /// of the cell's entities reach beyond them; it should say whether anything of that cell could be in view (a camera frustum test).
    pub fn near_layers_visible(&self, x: f32, y: f32, radius: f32, hd: bool, other: bool, visible: &dyn Fn([f32; 2], [f32; 2], f32) -> bool) -> Vec<&[u32]> {
        let (cx, cy) = cell_of(x, y);
        let r = (radius / CELL).ceil() as i32 + 1;
        let mut cells: Vec<(f32, &[u32])> = vec![];
        for (on, grid) in [(hd, &self.grid_hd), (other, &self.grid_other)] {
            if !on {
                continue;
            }
            for gx in cx - r..=cx + r {
                for gy in cy - r..=cy + r {
                    if let Some(c) = grid.get(&(gx, gy)) {
                        let (lo, hi) = ([gx as f32 * CELL, gy as f32 * CELL], [(gx + 1) as f32 * CELL, (gy + 1) as f32 * CELL]);
                        if !visible(lo, hi, c.reach) {
                            continue;
                        }
                        let (dx, dy) = ((gx as f32 + 0.5) * CELL - x, (gy as f32 + 0.5) * CELL - y);
                        cells.push((dx * dx + dy * dy, c.ents.as_slice()));
                    }
                }
            }
        }
        cells.sort_by(|a, b| a.0.total_cmp(&b.0));
        cells.into_iter().map(|c| c.1).collect()
    }

    /// Build from loaded parts (the grid is made here). `ents` need their `map` and `index` set.
    pub fn assemble(maps: Vec<String>, ytyps: Vec<String>, ents: Vec<WorldEnt>, arch: HashMap<u32, ArchInfo>, skipped: Vec<String>) -> Self {
        let (mut grid_hd, mut grid_other): (HashMap<(i32, i32), Cell>, HashMap<(i32, i32), Cell>) = (HashMap::new(), HashMap::new());
        for (i, e) in ents.iter().enumerate() {
            let hd = !e.is_mlo && (e.lod_level == 0 || e.lod_level == 5);
            // how far the model reaches from the entity's position: the farthest corner of its box, scaled (an unknown model counts as 2 m)
            let reach = match arch.get(&e.hash) {
                Some(a) => {
                    let far = |lo: f32, hi: f32| lo.abs().max(hi.abs());
                    (far(a.bb_min[0], a.bb_max[0]).powi(2) + far(a.bb_min[1], a.bb_max[1]).powi(2) + far(a.bb_min[2], a.bb_max[2]).powi(2)).sqrt() * e.scale_xy.abs().max(e.scale_z.abs())
                }
                None => 2.0,
            };
            let cell = (if hd { &mut grid_hd } else { &mut grid_other }).entry(cell_of(e.pos[0], e.pos[1])).or_insert_with(|| Cell { ents: vec![], reach: 0.0 });
            cell.ents.push(i as u32);
            cell.reach = cell.reach.max(reach);
        }
        // LOD children of each entity, as offsets into one list
        let mut kid_start = vec![0u32; ents.len() + 1];
        for e in &ents {
            if (e.parent as usize) < ents.len() {
                kid_start[e.parent as usize + 1] += 1;
            }
        }
        for i in 0..ents.len() {
            kid_start[i + 1] += kid_start[i];
        }
        let mut fill = kid_start.clone();
        let mut kid_list = vec![0u32; *kid_start.last().unwrap_or(&0) as usize];
        for (i, e) in ents.iter().enumerate() {
            if (e.parent as usize) < ents.len() {
                let f = &mut fill[e.parent as usize];
                kid_list[*f as usize] = i as u32;
                *f += 1;
            }
        }
        let map_count = ents.iter().map(|e| e.map as usize + 1).max().unwrap_or(0).max(maps.len());
        let mut map_start = vec![0u32; map_count + 1];
        for e in &ents {
            map_start[e.map as usize + 1] += 1;
        }
        for i in 0..map_count {
            map_start[i + 1] += map_start[i];
        }
        let mut fill = map_start.clone();
        let mut map_list = vec![0u32; ents.len()];
        for (i, e) in ents.iter().enumerate() {
            let f = &mut fill[e.map as usize];
            map_list[*f as usize] = i as u32;
            *f += 1;
        }
        WorldData { maps, ytyps, ents, arch, skipped, grid_hd, grid_other, kid_start, kid_list, map_start, map_list }
    }

    /// Problems in the LOD links of the game's entities: a parent that is missing, not coarser than its child, or that counts a different number of
    /// children than point at it. At most `limit` lines; the second value is the total.
    pub fn lod_problems(&self, limit: usize) -> (Vec<String>, usize) {
        let (mut out, mut total) = (vec![], 0usize);
        let mut note = |s: String| {
            total += 1;
            if out.len() < limit {
                out.push(s);
            }
        };
        for (i, e) in self.ents.iter().enumerate() {
            let what = || format!("entity {} (hash {:08X}, level {})", i, e.hash, e.lod_level);
            if e.parent != NO_PARENT {
                match self.ents.get(e.parent as usize) {
                    None => note(format!("{}: parent index {} does not exist", what(), e.parent)),
                    Some(p) => {
                        // levels: 0 HD, 1 LOD, 2..4 SLOD1-3, 6 SLOD4 (5 is orphan HD and has no parent)
                        let rank = |l: u8| if l == 6 { 5 } else { l };
                        if rank(p.lod_level) <= rank(e.lod_level) {
                            note(format!("{}: parent {} is level {}, not coarser", what(), e.parent, p.lod_level));
                        }
                    }
                }
            }
            let kids = self.children_of(i).len() as u32;
            if e.num_children != 0 && e.num_children != kids {
                note(format!("{}: says {} children, {} point at it", what(), e.num_children, kids));
            }
        }
        (out, total)
    }

    /// the entities of map file `m` (indices into `ents`)
    pub fn ents_of_map(&self, m: usize) -> &[u32] {
        match (self.map_start.get(m), self.map_start.get(m + 1)) {
            (Some(&a), Some(&b)) => &self.map_list[a as usize..b as usize],
            _ => &[],
        }
    }

    /// the LOD children of entity `i` (empty for a leaf)
    pub fn children_of(&self, i: usize) -> &[u32] {
        match (self.kid_start.get(i), self.kid_start.get(i + 1)) {
            (Some(&a), Some(&b)) => &self.kid_list[a as usize..b as usize],
            _ => &[],
        }
    }

    /// Read everything from the game. `ix` is the game's file list (see [`FileIndex`]); `job` reports progress per archive and can cancel.
    pub fn load(game: &Game, ix: &FileIndex, job: &Job) -> Result<Self> {
        // a file name is won by the pack that comes last (a pack's patch in the update archive after its own files), not by the path that sorts last
        let levels = crate::worlddlc::dlc_levels(game, ix);
        let rank = |path: &str| -> (i64, bool) {
            let lower = path.to_lowercase();
            (crate::worlddlc::dlc_name_of(&lower).and_then(|n| levels.iter().position(|l| l.name == n)).map_or(-1, |i| i as i64), lower.contains("dlc_patch/"))
        };
        let keep = |ext: &str| -> Vec<(String, String)> {
            let filter = Filter { exts: vec![ext.into()], ..Default::default() };
            let mut paths: Vec<String> = ix.find("*", &filter, false, usize::MAX).into_iter().map(|r| r.path.clone()).collect();
            if ext == "ymap" {
                paths.retain(|p| !crate::worlddlc::is_left_out(p));
            }
            paths.sort_by_key(|p| rank(p));
            last_wins(paths.into_iter())
        };
        let (ymaps, ytyps) = (keep("ymap"), keep("ytyp"));
        // group by the archive a file sits in, so each archive is opened once
        let mut groups: HashMap<String, Vec<(usize, bool, String)>> = HashMap::new();
        for (kind, list) in [(false, &ymaps), (true, &ytyps)] {
            for (n, (_, path)) in list.iter().enumerate() {
                if let Some((arc, inner)) = split_archive(path) {
                    groups.entry(arc.to_string()).or_default().push((n, kind, inner.to_string()));
                }
            }
        }
        let groups: Vec<(String, Vec<(usize, bool, String)>)> = groups.into_iter().collect();
        job.set_total(groups.len());
        let next = AtomicUsize::new(0);
        // (map, its entities, (the map's name hash, its parent map's name hash))
        type YmapOut = (usize, Vec<rage_formats::YmapEntity>, (u32, u32));
        type YtypOut = (usize, Vec<rage_formats::Archetype>);
        let out: Mutex<(Vec<YmapOut>, Vec<YtypOut>, Vec<String>)> = Mutex::new((vec![], vec![], vec![]));
        let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).min(8);
        std::thread::scope(|s| {
            for _ in 0..threads {
                s.spawn(|| loop {
                    let g = next.fetch_add(1, Relaxed);
                    if g >= groups.len() || job.is_cancelled() {
                        break;
                    }
                    let (arc, files) = &groups[g];
                    let (mut y, mut t, mut bad) = (vec![], vec![], vec![]);
                    match game.archive(arc) {
                        Ok(ar) => {
                            for (n, is_ytyp, inner) in files {
                                match ar.read(inner) {
                                    Ok(data) if *is_ytyp => match parse_ytyp(&data) {
                                        Ok(v) => t.push((*n, v.archetypes)),
                                        // a few DLC `.ytyp` are PSO files the archetype reader does not take: no sizes from them
                                        Err(e) => bad.push(format!("{arc}/{inner}: {e:#}")),
                                    },
                                    Ok(data) => match parse_ymap_entities(&data) {
                                        Ok(v) => {
                                            let h = rage_formats::ymap::parse_ymap_header(&data).map_or((0, 0), |h| (h.name_hash, h.parent_hash));
                                            y.push((*n, v, h))
                                        }
                                        Err(e) => bad.push(format!("{arc}/{inner}: {e:#}")),
                                    },
                                    Err(e) => bad.push(format!("{arc}/{inner}: {e:#}")),
                                }
                            }
                        }
                        Err(e) => bad.push(format!("{arc}: {e:#}")),
                    }
                    let mut o = out.lock().unwrap();
                    o.0.extend(y);
                    o.1.extend(t);
                    o.2.extend(bad);
                    drop(o);
                    job.step();
                });
            }
        });
        if job.is_cancelled() {
            bail!("loading the map was cancelled");
        }
        let (mut y, mut t, skipped) = out.into_inner().unwrap();
        // a fixed order whatever thread finished first
        y.sort_by_key(|(n, _, _)| *n);
        t.sort_by_key(|(n, _)| *n);
        let maps: Vec<String> = ymaps.iter().map(|(_, p)| p.clone()).collect();
        let ytyp_paths: Vec<String> = ytyps.iter().map(|(_, p)| p.clone()).collect();
        let mut ents = Vec::with_capacity(y.iter().map(|(_, v, _)| v.len()).sum());
        for (map, list, _) in &y {
            for (i, e) in list.iter().enumerate() {
                ents.push(WorldEnt {
                    pos: [e.position.x, e.position.y, e.position.z],
                    rot: e.rotation,
                    scale_xy: e.scale_xy,
                    scale_z: e.scale_z,
                    lod_dist: e.lod_dist,
                    child_lod_dist: e.child_lod_dist,
                    num_children: e.num_children,
                    hash: e.archetype_hash,
                    flags: e.flags,
                    guid: e.guid,
                    map: *map as u32,
                    index: i as u32,
                    lod_level: e.lod_level.min(255) as u8,
                    is_mlo: e.is_mlo_instance,
                    parent: NO_PARENT,
                });
            }
        }
        // LOD parents: an entity names its parent by place, in its own file or (flag 8) in the file its map names as its parent
        {
            let mut start: HashMap<usize, usize> = HashMap::new();
            let mut count: HashMap<usize, usize> = HashMap::new();
            let mut by_name: HashMap<u32, usize> = HashMap::new();
            let mut at = 0;
            for (map, list, _) in &y {
                start.insert(*map, at);
                count.insert(*map, list.len());
                // a parent map is found by its file's name, as the game does, not by the name written in the header (two files can
                // carry the same header name)
                let leaf = ymaps[*map].1.rsplit('/').next().unwrap_or("").to_lowercase();
                by_name.insert(crate::rage_joaat(leaf.trim_end_matches(".ymap")), *map);
                at += list.len();
            }
            let mut g = 0;
            for (map, list, (_, parent_map)) in &y {
                for e in list {
                    if e.parent_index >= 0 {
                        // the parent is in this file when the index is in range, the entity is not flagged "LOD in
                        // parent YMAP" and the parent's level is coarser; any other entity is a root, and a root is linked to the entity of that index
                        // in the file its map names as its parent (`ConnectToParent`)
                        let pi = e.parent_index as usize;
                        let own = pi < list.len() && !e.lod_in_parent_ymap() && {
                            let p = &list[pi];
                            !(p.lod_level <= e.lod_level || (p.lod_level == 5 && e.lod_level != 5))
                        };
                        let target = if own { Some(*map) } else { by_name.get(parent_map).copied() };
                        if let Some(t) = target {
                            if pi < count.get(&t).copied().unwrap_or(0) {
                                ents[g].parent = (start[&t] + pi) as u32;
                            }
                        }
                    }
                    g += 1;
                }
            }
        }
        let mut arch = HashMap::new();
        for (n, list) in &t {
            for a in list {
                arch.entry(a.name_hash).or_insert(ArchInfo {
                    bb_min: [a.bb_min.x, a.bb_min.y, a.bb_min.z],
                    bb_max: [a.bb_max.x, a.bb_max.y, a.bb_max.z],
                    asset_type: a.asset_type,
                    ydd_hash: a.drawable_dictionary_hash,
                    asset_hash: a.asset_name_hash,
                    txd_hash: a.texture_dict_hash,
                    lod_dist: a.lod_dist,
                    flags: a.flags,
                    ytyp: *n as u32,
                    is_mlo: a.is_mlo,
                });
            }
        }
        Ok(Self::assemble(maps, ytyp_paths, ents, arch, skipped))
    }
}

/// `(leaf name, path)` of each distinct file name, the last path in `paths` winning (later archives override earlier ones)
fn last_wins(paths: impl Iterator<Item = String>) -> Vec<(String, String)> {
    let mut seen: HashMap<String, usize> = HashMap::new();
    let mut out: Vec<(String, String)> = vec![];
    for p in paths {
        let leaf = p.rsplit('/').next().unwrap_or(&p).to_lowercase();
        match seen.get(&leaf) {
            Some(&i) => out[i].1 = p,
            None => {
                seen.insert(leaf.clone(), out.len());
                out.push((leaf, p));
            }
        }
    }
    out
}

/// `x64a.rpf/levels/gta5/props.rpf/sub/f.ymap` -> (`x64a.rpf/levels/gta5/props.rpf`, `sub/f.ymap`): the innermost archive and the
/// path inside it.
fn split_archive(path: &str) -> Option<(&str, &str)> {
    let i = path.to_lowercase().rfind(".rpf/")?;
    Some((&path[..i + 4], &path[i + 5..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_at_the_innermost_archive() {
        assert_eq!(split_archive("x64a.rpf/levels/props.rpf/sub/f.ymap"), Some(("x64a.rpf/levels/props.rpf", "sub/f.ymap")));
        assert_eq!(split_archive("loose/f.ymap"), None);
    }

    #[test]
    fn the_last_copy_of_a_name_wins() {
        let v = last_wins(["a.rpf/x.ymap", "a.rpf/y.ymap", "update/b.rpf/X.ymap"].into_iter().map(String::from));
        assert_eq!(v, vec![("x.ymap".to_string(), "update/b.rpf/X.ymap".to_string()), ("y.ymap".to_string(), "a.rpf/y.ymap".to_string())]);
    }

    #[test]
    fn the_grid_finds_what_is_near_and_not_what_is_far() {
        let e = |x: f32, y: f32| WorldEnt { pos: [x, y, 0.0], rot: [0.0, 0.0, 0.0, 1.0], scale_xy: 1.0, scale_z: 1.0, lod_dist: 100.0, child_lod_dist: 0.0, num_children: 0, hash: 1, flags: 0, guid: 0, map: 0, index: 0, lod_level: 0, is_mlo: false, parent: NO_PARENT };
        let w = WorldData::assemble(vec![], vec![], vec![e(10.0, 10.0), e(-300.0, 5.0), e(5000.0, 5000.0)], HashMap::new(), vec![]);
        let near: Vec<u32> = w.near(0.0, 0.0, 400.0).into_iter().flatten().copied().collect();
        assert!(near.contains(&0) && near.contains(&1) && !near.contains(&2), "{near:?}");
        assert_eq!(cell_of(-1.0, 255.0), (-1, 0));
    }

    #[test]
    fn cells_a_view_rejects_are_left_out_and_each_cell_reports_how_far_its_models_reach() {
        let e = |x: f32, hash: u32| WorldEnt { pos: [x, 10.0, 0.0], rot: [0.0, 0.0, 0.0, 1.0], scale_xy: 2.0, scale_z: 2.0, lod_dist: 100.0, child_lod_dist: 0.0, num_children: 0, hash, flags: 0, guid: 0, map: 0, index: 0, lod_level: 0, is_mlo: false, parent: NO_PARENT };
        let mut arch = HashMap::new();
        arch.insert(7, ArchInfo { bb_min: [-3.0, -4.0, 0.0], bb_max: [1.0, 2.0, 12.0], asset_type: 2, ydd_hash: 0, asset_hash: 0, txd_hash: 0, lod_dist: 100.0, flags: 0, ytyp: 0, is_mlo: false });
        // entity 0 is in cell (0,0), 1 in cell (2,0); the model 7 reaches sqrt(3^2 + 4^2 + 12^2) = 13 from its position, twice that scaled
        let w = WorldData::assemble(vec![], vec![], vec![e(10.0, 7), e(600.0, 99)], arch, vec![]);
        let seen = std::cell::RefCell::new(vec![]);
        let all: Vec<u32> = w.near_layers_visible(0.0, 0.0, 800.0, true, false, &|lo, hi, reach| {
            seen.borrow_mut().push((lo, hi, reach));
            true
        })
        .into_iter()
        .flatten()
        .copied()
        .collect();
        assert_eq!(all.len(), 2);
        let seen = seen.into_inner();
        let reach_of = |x: f32| seen.iter().find(|(lo, hi, _)| lo[0] <= x && x < hi[0]).map(|c| c.2).unwrap();
        assert!((reach_of(10.0) - 26.0).abs() < 1e-4, "13 m scaled by 2: {}", reach_of(10.0));
        assert_eq!(reach_of(600.0), 2.0, "a model nobody described counts as 2 m");
        // a view that only sees x above 300 gets only the second
        let some: Vec<u32> = w.near_layers_visible(0.0, 0.0, 800.0, true, false, &|_, hi, _| hi[0] > 300.0).into_iter().flatten().copied().collect();
        assert_eq!(some, vec![1]);
        assert!(w.near_layers_visible(0.0, 0.0, 800.0, false, true, &|_, _, _| true).is_empty(), "no LOD entities here");
    }

    #[test]
    fn entities_are_listed_under_their_map() {
        let mk = |map: u32| WorldEnt { pos: [0.0; 3], rot: [0.0, 0.0, 0.0, 1.0], scale_xy: 1.0, scale_z: 1.0, lod_dist: 100.0, child_lod_dist: 0.0, num_children: 0, hash: 1, flags: 0, guid: 0, map, index: 0, lod_level: 0, is_mlo: false, parent: NO_PARENT };
        let w = WorldData::assemble(vec!["a".into(), "b".into(), "c".into()], vec![], vec![mk(0), mk(2), mk(0), mk(2), mk(2)], HashMap::new(), vec![]);
        assert_eq!(w.ents_of_map(0), &[0, 2]);
        assert!(w.ents_of_map(1).is_empty());
        assert_eq!(w.ents_of_map(2), &[1, 3, 4]);
        assert!(w.ents_of_map(9).is_empty());
    }

    #[test]
    fn children_are_listed_under_their_parent() {
        let mk = |parent: u32| WorldEnt { pos: [0.0; 3], rot: [0.0, 0.0, 0.0, 1.0], scale_xy: 1.0, scale_z: 1.0, lod_dist: 100.0, child_lod_dist: 0.0, num_children: 0, hash: 1, flags: 0, guid: 0, map: 0, index: 0, lod_level: 0, is_mlo: false, parent };
        let w = WorldData::assemble(vec![], vec![], vec![mk(NO_PARENT), mk(0), mk(0), mk(1)], HashMap::new(), vec![]);
        assert_eq!(w.children_of(0), &[1, 2]);
        assert_eq!(w.children_of(1), &[3]);
        assert!(w.children_of(2).is_empty() && w.children_of(99).is_empty());
    }

    #[test]
    fn lod_links_that_are_wrong_are_reported() {
        let mk = |parent: u32, level: u8, n: u32| WorldEnt { pos: [0.0; 3], rot: [0.0, 0.0, 0.0, 1.0], scale_xy: 1.0, scale_z: 1.0, lod_dist: 100.0, child_lod_dist: 0.0, num_children: n, hash: 1, flags: 0, guid: 0, map: 0, index: 0, lod_level: level, is_mlo: false, parent };
        // 0 LOD with 2 children counted but one pointing at it; 1 HD child ok; 2 HD child of a missing parent; 3 LOD whose parent is HD
        let w = WorldData::assemble(vec![], vec![], vec![mk(NO_PARENT, 1, 2), mk(0, 0, 0), mk(77, 0, 0), mk(1, 1, 0)], HashMap::new(), vec![]);
        let (lines, total) = w.lod_problems(10);
        assert_eq!(total, 3, "{lines:?}");
    }
}

