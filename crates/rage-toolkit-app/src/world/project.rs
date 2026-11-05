//! The maps of a World project: real `.ymap` documents held in memory, each with a working copy in the project folder that is saved as
//! it is edited (the game's files and the user's imported originals are never written until they say so). Entities of these maps are
//! addressed by an id that carries the map's slot and the entity's index, in the range from [`BASE`] up; ids below it are the game's own
//! entities (indices into the loaded world).
use rage_toolkit_core::{
    worldload::{WorldEnt, NO_PARENT},
    worldproject::{MapKind, MapRef},
    ymap_doc::{ArchBounds, EntityView, ExtentsMode, YmapDocument},
};
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};

/// ids from here up belong to the project's maps
pub const BASE: u32 = 0x8000_0000;
const INDEX_BITS: u32 = 20;
/// most entities one project map can hold (an entity id has 20 bits for the index)
pub const MAX_ENTITIES: usize = 1 << INDEX_BITS;
const MAX_SLOT: usize = 0x7FF;
const MAX_INDEX: usize = (1 << INDEX_BITS) - 1;

/// The id of entity `index` of the map in `slot`.
pub fn id(slot: usize, index: usize) -> u32 {
    debug_assert!(slot <= MAX_SLOT && index <= MAX_INDEX);
    BASE | ((slot as u32) << INDEX_BITS) | index as u32
}

/// The map slot and entity index an id stands for, or `None` for one of the game's own entities.
pub fn split(id: u32) -> Option<(usize, usize)> {
    (id >= BASE).then(|| (((id & !BASE) >> INDEX_BITS) as usize, (id & MAX_INDEX as u32) as usize))
}

pub struct ProjectMap {
    pub name: String,
    pub doc: YmapDocument,
    /// the working copy in the project folder
    pub file: PathBuf,
    /// the imported file's path or the game path this was opened from
    pub source: Option<String>,
    pub kind: MapKind,
}

impl ProjectMap {
    pub fn dirty(&self) -> bool {
        self.doc.is_dirty()
    }
}

pub struct ProjectMaps {
    pub dir: PathBuf,
    pub maps: Vec<ProjectMap>,
    /// the map new entities go into
    pub active: usize,
}

fn clean_name(s: &str) -> String {
    let n: String = s.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' }).collect();
    if n.is_empty() {
        "map".into()
    } else {
        n
    }
}

impl ProjectMaps {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir, maps: vec![], active: 0 }
    }

    fn unique_name(&self, wanted: &str) -> String {
        let base = clean_name(wanted);
        if !self.maps.iter().any(|m| m.name.eq_ignore_ascii_case(&base)) {
            return base;
        }
        (2..).map(|n| format!("{base}_{n}")).find(|c| !self.maps.iter().any(|m| m.name.eq_ignore_ascii_case(c))).unwrap()
    }

    fn working_file(&self, name: &str) -> PathBuf {
        self.dir.join("maps").join(format!("{name}.ymap"))
    }

    fn push(&mut self, name: String, doc: YmapDocument, source: Option<String>, kind: MapKind) -> Result<usize> {
        if self.maps.len() > MAX_SLOT {
            bail!("a project holds at most {} maps", MAX_SLOT + 1);
        }
        if doc.len() > MAX_INDEX {
            bail!("a map of this project holds at most {} entities", MAX_INDEX + 1);
        }
        let file = self.working_file(&name);
        self.maps.push(ProjectMap { name, doc, file, source, kind });
        let slot = self.maps.len() - 1;
        self.active = slot;
        Ok(slot)
    }

    /// A new empty map (called `map`, `map_2`, ... unless `name` is given) saved at once in the project folder.
    pub fn new_map(&mut self, name: Option<&str>) -> Result<usize> {
        let name = self.unique_name(name.unwrap_or("map"));
        let mut doc = YmapDocument::new_map(&name)?;
        // a new map has HD entities in it as soon as there are any: the usual start of a new map
        doc.write_to(&self.working_file(&name), false)?;
        self.push(name, doc, None, MapKind::New)
    }

    /// Add a map made in memory (a generated one) as a new map of the project, saved at once.
    pub fn add_doc(&mut self, name: &str, mut doc: YmapDocument) -> Result<usize> {
        let name = self.unique_name(name);
        doc.write_to(&self.working_file(&name), false)?;
        self.push(name, doc, None, MapKind::New)
    }

    /// Bring in a `.ymap` of the user's: it is copied into the project (the original is only written by [`ProjectMaps::write_source`]).
    pub fn import_file(&mut self, path: &Path) -> Result<usize> {
        let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        self.import_bytes(&stem, &bytes, Some(path.display().to_string()), MapKind::File)
    }

    /// Open one of the game's maps for editing as a copy (`game_path` says which).
    pub fn import_game(&mut self, name: &str, bytes: &[u8], game_path: &str) -> Result<usize> {
        self.import_bytes(name, bytes, Some(game_path.to_string()), MapKind::Game)
    }

    fn import_bytes(&mut self, wanted: &str, bytes: &[u8], source: Option<String>, kind: MapKind) -> Result<usize> {
        // already in the project: do not make a second copy
        if let Some(src) = &source {
            if let Some(slot) = self.maps.iter().position(|m| m.source.as_ref() == Some(src)) {
                self.active = slot;
                return Ok(slot);
            }
        }
        let name = self.unique_name(wanted);
        let mut doc = YmapDocument::open(bytes, None).with_context(|| format!("{wanted} is not a map file"))?;
        // keep the exact original bytes as the working copy until the first change, so nothing is rewritten that was not edited
        let file = self.working_file(&name);
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&file, bytes).with_context(|| format!("writing {}", file.display()))?;
        doc.path = Some(file);
        self.push(name, doc, source, kind)
    }

    /// Take a map out of the project and delete its working copy. Entity ids of the later maps change, so nothing that holds ids
    /// (a selection, an undo history) may be kept across this.
    pub fn remove(&mut self, slot: usize) {
        if slot >= self.maps.len() {
            return;
        }
        let m = self.maps.remove(slot);
        let _ = std::fs::remove_file(&m.file);
        if self.active >= self.maps.len() {
            self.active = self.maps.len().saturating_sub(1);
        }
    }

    pub fn active_map(&self) -> Option<&ProjectMap> {
        self.maps.get(self.active)
    }

    /// Update the flags and extents of every changed map before saving (`update_all`), then write the working
    /// copies. Returns what went wrong, per map. A map whose file would not read back the same is not written.
    pub fn save_all(&mut self, update_all: bool, bounds: &dyn Fn(u32) -> Option<ArchBounds>) -> Vec<String> {
        let mut problems = vec![];
        for slot in 0..self.maps.len() {
            if self.maps[slot].dirty() {
                if let Err(e) = self.save_one(slot, update_all, bounds) {
                    problems.push(format!("{}: {e:#}", self.maps[slot].name));
                }
            }
        }
        problems
    }

    /// [`ProjectMaps::save_all`] for the map in `slot` only (saved even when it has no changes).
    pub fn save_one(&mut self, slot: usize, update_all: bool, bounds: &dyn Fn(u32) -> Option<ArchBounds>) -> Result<()> {
        let m = self.maps.get_mut(slot).context("no such map")?;
        if update_all {
            // a new map is sized from scratch; an edited copy of a game map only ever grows (it keeps the game's values)
            let mode = if m.kind == MapKind::New { ExtentsMode::Recalculate } else { ExtentsMode::GrowOnly };
            m.doc.update_extents(bounds, mode).and_then(|_| m.doc.update_flags())?;
        }
        m.doc.write_to(&m.file, false)
    }

    /// Write map `slot` back to where it came from (the imported file, with a `.bak` of the first version). Game maps have no place to
    /// go back to: they are exported instead.
    pub fn write_source(&mut self, slot: usize) -> Result<PathBuf> {
        let m = self.maps.get_mut(slot).context("no such map")?;
        match (&m.kind, &m.source) {
            (MapKind::File, Some(src)) => {
                let dest = PathBuf::from(src);
                m.doc.write_to(&dest, true)?;
                // the working copy follows, so the two are the same file
                m.doc.write_to(&m.file.clone(), false)?;
                Ok(dest)
            }
            _ => bail!("this map did not come from a file of yours: use Export to write it to a folder"),
        }
    }

    /// The references to store in the project file.
    pub fn refs(&self) -> Vec<MapRef> {
        self.maps
            .iter()
            .map(|m| MapRef { name: m.name.clone(), file: format!("maps/{}.ymap", m.name), source: m.source.clone(), kind: m.kind })
            .collect()
    }

    /// Open the maps a project file lists from their working copies. A file that is missing or damaged is reported and skipped; the others
    /// load.
    pub fn load(dir: PathBuf, refs: &[MapRef], active: Option<&str>) -> (Self, Vec<String>) {
        let mut me = Self::new(dir);
        let mut warnings = vec![];
        for r in refs {
            // the project file is not trusted: a map's file is `maps/<name>.ymap` under the project folder and nothing else
            if clean_name(&r.name) != r.name || r.file != format!("maps/{}.ymap", r.name) {
                warnings.push(format!("{}: skipped, its file name is not one this project writes", r.name));
                continue;
            }
            let file = me.dir.join(&r.file);
            match YmapDocument::open_file(&file) {
                Ok(doc) if me.maps.len() <= MAX_SLOT && doc.len() <= MAX_INDEX => me.maps.push(ProjectMap { name: r.name.clone(), doc, file, source: r.source.clone(), kind: r.kind }),
                Ok(_) => warnings.push(format!("{}: too big for a project map", r.name)),
                Err(e) => warnings.push(format!("{}: could not open {} ({e:#})", r.name, file.display())),
            }
        }
        me.active = active.and_then(|a| me.maps.iter().position(|m| m.name == a)).unwrap_or(0);
        (me, warnings)
    }

    /// The entity `id` stands for: its slot, its index and what the file says about it.
    pub fn entity(&self, id: u32) -> Option<(usize, usize, EntityView)> {
        let (slot, index) = split(id)?;
        Some((slot, index, self.maps.get(slot)?.doc.entity(index)?))
    }

    /// An entity of a project map as the world's entity record.
    pub fn world_ent(&self, id: u32) -> Option<WorldEnt> {
        let (slot, index, e) = self.entity(id)?;
        Some(WorldEnt {
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
            map: u32::MAX - slot as u32,
            index: index as u32,
            lod_level: e.lod_level,
            is_mlo: e.is_mlo_instance,
            parent: NO_PARENT,
        })
    }

}

#[cfg(test)]
mod tests {
    use super::*;
    use rage_toolkit_core::ymap_doc::Vec3;

    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("crck_proj_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn ids_carry_the_slot_and_the_index_and_game_ids_are_below() {
        assert_eq!(split(5), None);
        assert_eq!(split(id(3, 1234)), Some((3, 1234)));
        assert_eq!(split(id(0, 0)), Some((0, 0)));
        assert!(id(0, 0) >= BASE && 1_627_754 < BASE);
        assert_eq!(split(id(MAX_SLOT, MAX_INDEX)), Some((MAX_SLOT, MAX_INDEX)));
    }

    #[test]
    fn new_maps_get_unique_names_and_a_working_copy_at_once() {
        let d = dir("new");
        let mut p = ProjectMaps::new(d.clone());
        let a = p.new_map(None).unwrap();
        let b = p.new_map(None).unwrap();
        let c = p.new_map(Some("my map!")).unwrap();
        assert_eq!((p.maps[a].name.as_str(), p.maps[b].name.as_str(), p.maps[c].name.as_str()), ("map", "map_2", "my_map_"));
        assert!(d.join("maps/map.ymap").exists() && d.join("maps/map_2.ymap").exists());
        assert_eq!(p.active, c, "the newest map is the active one");
        assert!(p.maps.iter().all(|m| !m.dirty()));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn edits_mark_a_map_dirty_and_saving_writes_it_and_a_reload_finds_the_same_entities() {
        let d = dir("save");
        let mut p = ProjectMaps::new(d.clone());
        p.new_map(Some("alpha")).unwrap();
        p.new_map(Some("beta")).unwrap();
        let (doc_a, doc_b) = (0, 1);
        p.maps[doc_a].doc.add_entity("prop_a", Vec3::new(1.0, 2.0, -50.0)).unwrap();
        p.maps[doc_a].doc.add_entity("prop_b", Vec3::new(3.0, 4.0, 5.0)).unwrap();
        assert!(p.maps[doc_a].dirty() && !p.maps[doc_b].dirty());
        let bounds = |_: u32| Some(ArchBounds { min: [-1.0; 3], max: [1.0; 3], lod_dist: 100.0 });
        assert!(p.save_all(true, &bounds).is_empty());
        assert!(p.maps.iter().all(|m| !m.dirty()));
        // the saved map got its flags and extents worked out
        let flags = p.maps[doc_a].doc.flag_report();
        assert!(flags.iter().find(|l| l.name == "HD").unwrap().stored);
        let refs = p.refs();
        let (q, warnings) = ProjectMaps::load(d.clone(), &refs, Some("beta"));
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!((q.maps.len(), q.active), (2, 1));
        assert_eq!(q.maps[0].doc.len(), 2);
        assert_eq!(q.maps[0].doc.entity(0).unwrap().position, Vec3::new(1.0, 2.0, -50.0));
        assert!(q.entity(id(0, 0)).is_some() && q.entity(id(0, 2)).is_none() && q.entity(id(2, 0)).is_none());
        let e = q.world_ent(id(0, 1)).unwrap();
        assert_eq!((e.pos, e.index), ([3.0, 4.0, 5.0], 1));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn importing_copies_the_file_keeps_the_original_and_does_not_import_twice() {
        let d = dir("import");
        let src_dir = dir("import_src");
        std::fs::create_dir_all(&src_dir).unwrap();
        let src = src_dir.join("theirs.ymap");
        let mut doc = YmapDocument::new_map("theirs").unwrap();
        doc.add_entity("prop_x", Vec3::new(9.0, 9.0, 9.0)).unwrap();
        doc.write_to(&src, false).unwrap();
        let original = std::fs::read(&src).unwrap();
        let mut p = ProjectMaps::new(d.clone());
        let slot = p.import_file(&src).unwrap();
        assert_eq!((p.maps[slot].name.as_str(), p.maps[slot].doc.len(), p.maps[slot].kind), ("theirs", 1, MapKind::File));
        assert_eq!(p.import_file(&src).unwrap(), slot, "the same file is not brought in twice");
        assert_eq!(p.maps.len(), 1);
        // edit and save the working copy: the original is untouched until write_source
        p.maps[slot].doc.set_position(0, Vec3::new(0.0, 0.0, 0.0)).unwrap();
        let bounds = |_: u32| None;
        assert!(p.save_all(false, &bounds).is_empty());
        assert_eq!(std::fs::read(&src).unwrap(), original);
        // writing back keeps the first version as .bak
        p.write_source(slot).unwrap();
        assert!(src_dir.join("theirs.ymap.bak").exists());
        assert_eq!(YmapDocument::open_file(&src).unwrap().entity(0).unwrap().position, Vec3::new(0.0, 0.0, 0.0));
        // a map of the game has nowhere to write back to
        let bytes = std::fs::read(&src).unwrap();
        let g = p.import_game("game_one", &bytes, "x64a.rpf/levels/game_one.ymap").unwrap();
        assert!(p.write_source(g).is_err());
        // a file that is not a map is refused without leaving a copy
        let junk = src_dir.join("junk.ymap");
        std::fs::write(&junk, b"not a map at all").unwrap();
        assert!(p.import_file(&junk).is_err());
        assert!(!d.join("maps/junk.ymap").exists());
        let _ = std::fs::remove_dir_all(&d);
        let _ = std::fs::remove_dir_all(&src_dir);
    }

    #[test]
    fn a_project_file_cannot_point_outside_its_folder() {
        let d = dir("evil");
        let refs = vec![
            MapRef { name: "x".into(), file: "../../victim.txt".into(), source: None, kind: MapKind::New },
            MapRef { name: "../y".into(), file: "maps/../y.ymap".into(), source: None, kind: MapKind::New },
        ];
        let (p, warnings) = ProjectMaps::load(d.clone(), &refs, None);
        assert!(p.maps.is_empty());
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_missing_working_copy_is_reported_and_the_others_still_load_and_removing_deletes_the_copy() {
        let d = dir("missing");
        let mut p = ProjectMaps::new(d.clone());
        p.new_map(Some("one")).unwrap();
        p.new_map(Some("two")).unwrap();
        let refs = p.refs();
        std::fs::remove_file(d.join("maps/one.ymap")).unwrap();
        let (q, warnings) = ProjectMaps::load(d.clone(), &refs, None);
        assert_eq!(q.maps.len(), 1);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("one"));
        let mut p = q;
        p.remove(0);
        assert!(p.maps.is_empty() && !d.join("maps/two.ymap").exists());
        let _ = std::fs::remove_dir_all(&d);
    }
}
