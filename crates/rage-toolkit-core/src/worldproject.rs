//! A World project: the changes made to entities of the game's map, kept in a small text file so work survives closing the window,
//! and turned into edited `.ymap` files (written to a folder of the user's choice, never into the game) on export.
use crate::{
    game::Game,
    worldload::{WorldData, WorldEnt},
    ymap_doc::{ArchBounds, ExtentsMode, YmapDocument},
};
use anyhow::{bail, Context, Result};
use rage_formats::Vec3;
use std::{collections::{BTreeMap, BTreeSet}, fs, path::{Path, PathBuf}};

/// A changed placement. `rot` is the quaternion as the file stores it (x, y, z, w).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EntityEdit {
    pub pos: [f32; 3],
    pub rot: [f32; 4],
    pub scale_xy: f32,
    pub scale_z: f32,
}

impl EntityEdit {
    pub fn of(e: &WorldEnt) -> Self {
        Self { pos: e.pos, rot: e.rot, scale_xy: e.scale_xy, scale_z: e.scale_z }
    }
}

/// Which entity an edit is for: the map file, the entity's place in it, and its archetype and guid (to notice that the game changed
/// underneath the project).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct EntityKey {
    pub map: String,
    pub index: u32,
    pub hash: u32,
    pub guid: u32,
}

impl EntityKey {
    pub fn of(w: &WorldData, e: &WorldEnt) -> Self {
        Self { map: w.maps[e.map as usize].clone(), index: e.index, hash: e.hash, guid: e.guid }
    }
}

/// An entity the project adds (it goes into a new map file of the project).
#[derive(Clone, Debug, PartialEq)]
pub struct NewEntity {
    /// the archetype's name as typed (hashed in lowercase)
    pub archetype: String,
    pub edit: EntityEdit,
}

/// Where a map of the project came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MapKind {
    /// made in the project
    New,
    /// a `.ymap` imported from a folder of the user's
    File,
    /// a copy of one of the game's own maps, opened for editing
    Game,
}

/// A map file that belongs to the project: a working copy kept in the project folder, saved there as it is edited.
#[derive(Clone, Debug, PartialEq)]
pub struct MapRef {
    /// the map's own name (what it is called in the game, without `.ymap`)
    pub name: String,
    /// the working copy, relative to the project folder
    pub file: String,
    /// where it came from: the imported file's path, or the game path of the map
    pub source: Option<String>,
    pub kind: MapKind,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Project {
    pub edits: BTreeMap<EntityKey, EntityEdit>,
    /// entities of the game's maps taken out
    pub deleted: BTreeSet<EntityKey>,
    /// version 2 projects kept added entities here; they are moved into a map of `maps` when such a project is opened
    pub added: Vec<NewEntity>,
    /// the name of the map file that held the added entities (without `.ymap`)
    pub new_map: String,
    /// the maps of the project, in the order they were added
    pub maps: Vec<MapRef>,
    /// the name of the map new entities go into
    pub active: Option<String>,
}

impl Default for Project {
    fn default() -> Self {
        Self { edits: BTreeMap::new(), deleted: BTreeSet::new(), added: vec![], new_map: "crck_map".into(), maps: vec![], active: None }
    }
}

const MAGIC: &str = "CRCK-WORLD-PROJECT 3";
const MAGIC_V2: &str = "CRCK-WORLD-PROJECT 2";
const MAGIC_V1: &str = "CRCK-WORLD-PROJECT 1";

fn bits(v: &[f32]) -> String {
    v.iter().map(|f| format!("{:08x}", f.to_bits())).collect::<Vec<_>>().join(" ")
}

fn unbits(s: &str, n: usize) -> Option<Vec<f32>> {
    let v: Vec<f32> = s.split_whitespace().map(|h| u32::from_str_radix(h, 16).ok().map(f32::from_bits)).collect::<Option<_>>()?;
    (v.len() == n).then_some(v)
}

fn edit_of(n: &[f32]) -> EntityEdit {
    EntityEdit { pos: [n[0], n[1], n[2]], rot: [n[3], n[4], n[5], n[6]], scale_xy: n[7], scale_z: n[8] }
}

impl Project {
    pub fn is_empty(&self) -> bool {
        self.edits.is_empty() && self.deleted.is_empty() && self.added.is_empty() && self.maps.is_empty()
    }

    /// the game's maps with at least one change (moved or deleted entities)
    pub fn maps(&self) -> Vec<&str> {
        let mut v: Vec<&str> = self.edits.keys().map(|k| k.map.as_str()).chain(self.deleted.iter().map(|k| k.map.as_str())).collect();
        v.sort();
        v.dedup();
        v
    }

    pub fn to_text(&self) -> String {
        let mut s = format!("{MAGIC}\nM\t{}\n", self.new_map);
        for m in &self.maps {
            let kind = match m.kind {
                MapKind::New => "new",
                MapKind::File => "file",
                MapKind::Game => "game",
            };
            s.push_str(&format!("P\t{}\t{}\t{}\t{}\n", m.name, m.file, kind, m.source.as_deref().unwrap_or("")));
        }
        if let Some(a) = &self.active {
            s.push_str(&format!("A\t{a}\n"));
        }
        for (k, e) in &self.edits {
            // floats as their bits: the project reads back exactly what was edited
            s.push_str(&format!("E\t{}\t{}\t{:08x}\t{:08x}\t{} {} {} {}\n", k.map, k.index, k.hash, k.guid, bits(&e.pos), bits(&e.rot), bits(&[e.scale_xy]), bits(&[e.scale_z])));
        }
        for k in &self.deleted {
            s.push_str(&format!("D\t{}\t{}\t{:08x}\t{:08x}\n", k.map, k.index, k.hash, k.guid));
        }
        for n in &self.added {
            let e = &n.edit;
            s.push_str(&format!("N\t{}\t{} {} {} {}\n", n.archetype, bits(&e.pos), bits(&e.rot), bits(&[e.scale_xy]), bits(&[e.scale_z])));
        }
        s
    }

    /// `None` for text that is not a project (or is damaged): the caller keeps the old file and starts empty
    pub fn from_text(text: &str) -> Option<Self> {
        let mut lines = text.lines();
        let head = lines.next()?;
        if head != MAGIC && head != MAGIC_V2 && head != MAGIC_V1 {
            return None;
        }
        let v1 = head == MAGIC_V1;
        let mut p = Project::default();
        let key = |f: &[&str]| -> Option<EntityKey> { Some(EntityKey { map: f[0].to_string(), index: f[1].parse().ok()?, hash: u32::from_str_radix(f[2], 16).ok()?, guid: u32::from_str_radix(f[3], 16).ok()? }) };
        for l in lines.filter(|l| !l.trim().is_empty()) {
            let all: Vec<&str> = l.split('\t').collect();
            // version 1 lines had no kind letter and were all edits
            let (kind, f) = if v1 { ("E", &all[..]) } else { (all[0], &all[1..]) };
            match (kind, f.len()) {
                ("M", 1) => p.new_map = f[0].to_string(),
                ("A", 1) => p.active = Some(f[0].to_string()),
                ("P", 4) => {
                    let kind = match f[2] {
                        "new" => MapKind::New,
                        "file" => MapKind::File,
                        "game" => MapKind::Game,
                        _ => return None,
                    };
                    p.maps.push(MapRef { name: f[0].to_string(), file: f[1].to_string(), source: (!f[3].is_empty()).then(|| f[3].to_string()), kind });
                }
                ("E", 5) => {
                    p.edits.insert(key(f)?, edit_of(&unbits(f[4], 9)?));
                }
                ("D", 4) => {
                    p.deleted.insert(key(f)?);
                }
                ("N", 2) => p.added.push(NewEntity { archetype: f[0].to_string(), edit: edit_of(&unbits(f[1], 9)?) }),
                _ => return None,
            }
        }
        Some(p)
    }

    /// Write to `path` through a temporary file, so a crash never leaves half a project.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(d) = path.parent() {
            fs::create_dir_all(d)?;
        }
        let tmp = path.with_extension("tmp");
        fs::write(&tmp, self.to_text())?;
        fs::rename(&tmp, path)?;
        Ok(())
    }

    pub fn load(path: &Path) -> Option<Self> {
        Self::from_text(&fs::read_to_string(path).ok()?)
    }
}

/// What an export did to one map.
#[derive(Debug, Clone)]
pub struct ExportedMap {
    pub file: PathBuf,
    pub entities_changed: usize,
    pub entities_deleted: usize,
    pub entities_added: usize,
    pub extents_changed: bool,
}

/// Write an edited copy of each changed `.ymap` into `out_dir` (named like the original, so a resource that streams it replaces the
/// game's). The original is read from the game and never written. Each file is built, read back and compared before it is written; the
/// extents are updated for the moved entities. Nothing is written for a map when any of its entities cannot be found.
pub fn export(game: &Game, world: &WorldData, project: &Project, out_dir: &Path, mode: ExtentsMode) -> Result<Vec<ExportedMap>> {
    export_with_maps(game, world, project, None, out_dir, mode)
}

/// [`export`], and the project's own maps: each working copy in `project_dir` is read back (it must open as a map) and written to
/// `out_dir` under its map's name.
pub fn export_with_maps(game: &Game, world: &WorldData, project: &Project, project_dir: Option<&Path>, out_dir: &Path, mode: ExtentsMode) -> Result<Vec<ExportedMap>> {
    if project.is_empty() {
        bail!("there are no changes to export");
    }
    fs::create_dir_all(out_dir).with_context(|| format!("creating {}", out_dir.display()))?;
    let bounds = |h: u32| world.arch.get(&h).map(|a| ArchBounds { min: a.bb_min, max: a.bb_max, lod_dist: a.lod_dist });
    let mut by_map: BTreeMap<&str, (Vec<(&EntityKey, &EntityEdit)>, Vec<&EntityKey>)> = BTreeMap::new();
    for (k, e) in &project.edits {
        by_map.entry(k.map.as_str()).or_default().0.push((k, e));
    }
    for k in &project.deleted {
        by_map.entry(k.map.as_str()).or_default().1.push(k);
    }
    let mut done = vec![];
    for (map, (edits, deletes)) in by_map {
        let data = game.read(map).with_context(|| format!("reading {map} from the game"))?;
        let mut doc = YmapDocument::open(&data, None).with_context(|| format!("opening {map}"))?;
        for (key, edit) in &edits {
            let i = find_entity(&doc, key).with_context(|| format!("{map}: entity {} ({:08X}) is not in the game's file any more", key.index, key.hash))?;
            doc.set_position(i, Vec3::new(edit.pos[0], edit.pos[1], edit.pos[2]))?;
            doc.set_rotation_exact(i, edit.rot)?;
            doc.set_scale(i, edit.scale_xy, edit.scale_z)?;
        }
        // deleted entities last, highest index first so the indices still mean what they did
        let mut gone: Vec<usize> = deletes.iter().map(|k| find_entity(&doc, k).with_context(|| format!("{map}: entity {} ({:08X}) is not in the game's file any more", k.index, k.hash))).collect::<Result<_>>()?;
        gone.sort_unstable_by(|a, b| b.cmp(a));
        for i in &gone {
            doc.remove_entity(*i).with_context(|| map.to_string())?;
        }
        let extents_changed = doc.update_extents(&bounds, mode)?;
        doc.update_content_flags()?;
        let leaf = map.rsplit('/').next().unwrap_or(map);
        let file = out_dir.join(leaf);
        // to_bytes reads the file back and compares it with the edited tree before anything is written
        let bytes = doc.to_bytes().with_context(|| format!("building {leaf}"))?;
        fs::write(&file, bytes).with_context(|| format!("writing {}", file.display()))?;
        done.push(ExportedMap { file, entities_changed: edits.len(), entities_deleted: gone.len(), entities_added: 0, extents_changed });
    }
    if !project.added.is_empty() {
        let mut doc = YmapDocument::new_map(&project.new_map)?;
        for n in &project.added {
            let i = doc.add_entity(&n.archetype, Vec3::new(n.edit.pos[0], n.edit.pos[1], n.edit.pos[2]))?;
            doc.set_rotation_exact(i, n.edit.rot)?;
            doc.set_scale(i, n.edit.scale_xy, n.edit.scale_z)?;
        }
        doc.update_extents(&bounds, ExtentsMode::Recalculate)?;
        doc.update_content_flags()?;
        let file = out_dir.join(format!("{}.ymap", project.new_map));
        let bytes = doc.to_bytes().with_context(|| format!("building {}", project.new_map))?;
        fs::write(&file, bytes).with_context(|| format!("writing {}", file.display()))?;
        done.push(ExportedMap { file, entities_changed: 0, entities_deleted: 0, entities_added: project.added.len(), extents_changed: true });
    }
    if let Some(dir) = project_dir {
        for m in &project.maps {
            let src = dir.join(&m.file);
            let bytes = fs::read(&src).with_context(|| format!("reading {}", src.display()))?;
            // it has to be a map the game's tools can read before it leaves the project
            let doc = YmapDocument::open(&bytes, None).with_context(|| format!("{} is not a readable map", m.name))?;
            let file = out_dir.join(format!("{}.ymap", m.name));
            fs::write(&file, bytes).with_context(|| format!("writing {}", file.display()))?;
            done.push(ExportedMap { file, entities_changed: 0, entities_deleted: 0, entities_added: doc.len(), extents_changed: false });
        }
    }
    let mut log = String::from("Written by CRCK Explorer from a World project.\nThese files replace the game's .ymap files of the same name; the game itself was not changed.\n\n");
    for d in &done {
        log.push_str(&format!("{}: {} moved, {} removed, {} added{}\n", d.file.file_name().unwrap_or_default().to_string_lossy(), d.entities_changed, d.entities_deleted, d.entities_added, if d.extents_changed { ", extents updated" } else { "" }));
    }
    fs::write(out_dir.join("CRCK-changes.txt"), log)?;
    Ok(done)
}

/// The entity of `doc` the key names: at the stored index when the archetype and guid agree, else the one with the same guid and archetype.
fn find_entity(doc: &YmapDocument, key: &EntityKey) -> Option<usize> {
    let ok = |i: usize| doc.entity(i).map_or(false, |e| e.archetype_hash == key.hash && e.guid == key.guid);
    if ok(key.index as usize) {
        return Some(key.index as usize);
    }
    (0..doc.len()).find(|&i| ok(i))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(i: u32) -> EntityKey {
        EntityKey { map: "x64a.rpf/levels/a.rpf/m.ymap".into(), index: i, hash: 0xDEADBEEF, guid: 7 }
    }

    #[test]
    fn a_project_reads_back_exactly_what_was_saved() {
        let mut p = Project::default();
        p.edits.insert(key(3), EntityEdit { pos: [1.0 / 3.0, -2.5e7, 0.1], rot: [0.0, 0.0, 0.7071068, 0.7071068], scale_xy: 1.25, scale_z: 1.0 });
        p.edits.insert(key(1), EntityEdit { pos: [0.0; 3], rot: [0.0, 0.0, 0.0, 1.0], scale_xy: 1.0, scale_z: 1.0 });
        let back = Project::from_text(&p.to_text()).unwrap();
        assert_eq!(back, p);
        assert_eq!(back.maps(), vec!["x64a.rpf/levels/a.rpf/m.ymap"]);
        assert!(Project::from_text("not a project").is_none());
        assert!(Project::from_text(&format!("{MAGIC}\nbroken line\n")).is_none());
        assert!(Project::from_text(&p.to_text().replace("deadbeef", "zz")).is_none());
    }

    #[test]
    fn deletes_added_entities_and_the_new_map_name_survive_too() {
        let mut p = Project::default();
        p.new_map = "my_new_map".into();
        p.deleted.insert(key(4));
        p.added.push(NewEntity { archetype: "prop_chair_01a".into(), edit: EntityEdit { pos: [1.5, 2.5, 3.5], rot: [0.0, 0.0, 0.0, 1.0], scale_xy: 1.0, scale_z: 1.0 } });
        p.edits.insert(key(2), EntityEdit { pos: [9.0; 3], rot: [0.0, 0.0, 0.0, 1.0], scale_xy: 1.0, scale_z: 1.0 });
        let back = Project::from_text(&p.to_text()).unwrap();
        assert_eq!(back, p);
        assert!(!back.is_empty());
        assert_eq!(back.maps(), vec!["x64a.rpf/levels/a.rpf/m.ymap"]);
        // a version 1 file (edits only, no kind letters) still opens
        let v1 = format!("CRCK-WORLD-PROJECT 1\n{}", p.to_text().lines().find(|l| l.starts_with("E\t")).unwrap().trim_start_matches("E\t"));
        assert_eq!(Project::from_text(&v1).unwrap().edits.len(), 1);
    }

    #[test]
    fn the_maps_of_a_project_and_the_active_one_survive_and_older_projects_still_open() {
        let mut p = Project::default();
        p.maps.push(MapRef { name: "mymap".into(), file: "maps/mymap.ymap".into(), source: None, kind: MapKind::New });
        p.maps.push(MapRef { name: "other".into(), file: "maps/other.ymap".into(), source: Some("D:/mods/other.ymap".into()), kind: MapKind::File });
        p.maps.push(MapRef { name: "dt1_rd1_strm_6".into(), file: "maps/dt1_rd1_strm_6.ymap".into(), source: Some("x64i.rpf/levels/dt1.rpf/dt1_rd1_strm_6.ymap".into()), kind: MapKind::Game });
        p.active = Some("other".into());
        let back = Project::from_text(&p.to_text()).unwrap();
        assert_eq!(back, p);
        assert!(!Project::default().eq(&p) && !p.is_empty());
        // a version 2 file (an added entity, a name for the new map) is read; a damaged map line is not
        let v2 = "CRCK-WORLD-PROJECT 2\nM\told_map\nN\tprop_a\t00000000 00000000 00000000 00000000 00000000 00000000 3f800000 3f800000 3f800000\n";
        let old = Project::from_text(v2).unwrap();
        assert_eq!((old.new_map.as_str(), old.added.len(), old.maps.len()), ("old_map", 1, 0));
        assert!(Project::from_text("CRCK-WORLD-PROJECT 3\nP\tname\tfile\tweird\t\n").is_none());
    }

    #[test]
    fn saving_goes_through_a_temporary_file() {
        let dir = std::env::temp_dir().join(format!("crck_proj_test_{}", std::process::id()));
        let file = dir.join("p.crckproj");
        let mut p = Project::default();
        p.edits.insert(key(0), EntityEdit { pos: [1.0, 2.0, 3.0], rot: [0.0, 0.0, 0.0, 1.0], scale_xy: 1.0, scale_z: 1.0 });
        p.save(&file).unwrap();
        assert_eq!(Project::load(&file), Some(p));
        assert!(!file.with_extension("tmp").exists());
        let _ = fs::remove_dir_all(&dir);
    }
}
