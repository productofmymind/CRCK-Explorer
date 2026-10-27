//! Checks of an open `.ytyp` before it is saved: the mistakes that only show up in game (a name used twice, a box the wrong way round,
//! a model or texture dictionary that is not there, a portal into a room that does not exist). Every finding says what to do.
//! The thresholds were calibrated on the game's own files: they are meant to say nothing about them.
use super::{
    document::{archetype_path, ArchKind, YtypDocument},
    edit::{field, Path, Step},
};
use crate::game::Game;
use rage_formats::{rage_joaat, MetaValue};
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::Path as FsPath,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Sev {
    Error,
    Warning,
    Info,
}

#[derive(Debug, Clone)]
pub struct Issue {
    pub sev: Sev,
    pub code: &'static str,
    pub message: String,
    pub fix: String,
    /// the archetype it is about
    pub archetype: Option<usize>,
    /// the value it is about, when there is one
    pub path: Option<Path>,
}

/// Which model and texture dictionary names exist: in the game and in folders the user added. Names are lowercase file stems, hashed.
#[derive(Debug, Clone, Default)]
pub struct AssetIndex {
    /// the file stems seen (lowercase), to turn archetype name hashes back into names: an archetype is usually called like its model
    stems: Vec<String>,
    /// `.ydr` `.yft` `.ydd` stems
    models: HashSet<u32>,
    /// `.ytd` stems
    dictionaries: HashSet<u32>,
    /// `.ybn` stems
    collision: HashSet<u32>,
    pub files: usize,
}

fn stem_hash(file: &str) -> Option<(u32, &str)> {
    let name = file.rsplit(['/', '\\']).next()?;
    let (stem, ext) = name.rsplit_once('.')?;
    Some((rage_joaat(&stem.to_lowercase()), ext))
}

impl AssetIndex {
    fn add(&mut self, file: &str) {
        let Some((h, ext)) = stem_hash(file) else { return };
        let stem = file.rsplit(['/', '\\']).next().and_then(|n| n.rsplit_once('.')).map(|(s, _)| s.to_lowercase());
        match ext.to_lowercase().as_str() {
            "ydr" | "yft" | "ydd" => {
                self.models.insert(h);
            }
            "ytd" => {
                self.dictionaries.insert(h);
            }
            "ybn" => {
                self.collision.insert(h);
            }
            _ => return,
        }
        if let (Some(s), true) = (stem, ext.eq_ignore_ascii_case("ydr") || ext.eq_ignore_ascii_case("yft") || ext.eq_ignore_ascii_case("ytd")) {
            self.stems.push(s);
        }
        self.files += 1;
    }

    /// every model, texture dictionary and collision file in the game's archives
    pub fn from_game(g: &Game) -> Self {
        let mut ix = Self::default();
        for f in g.find_where(&|p: &str| {
            let l = p.to_lowercase();
            l.ends_with(".ydr") || l.ends_with(".yft") || l.ends_with(".ydd") || l.ends_with(".ytd") || l.ends_with(".ybn")
        }) {
            ix.add(&f.path);
        }
        ix
    }

    /// the files below a folder (your own streamed models)
    pub fn add_folder(&mut self, dir: &FsPath) {
        if let Ok(rd) = fs::read_dir(dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    self.add_folder(&p);
                } else if let Some(n) = p.to_str() {
                    self.add(n);
                }
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.files == 0
    }

    /// the model and dictionary names seen, lowercase
    pub fn names(&self) -> &[String] {
        &self.stems
    }

    pub fn has_model(&self, hash: u32) -> bool {
        self.models.contains(&hash)
    }

    pub fn has_dictionary(&self, hash: u32) -> bool {
        self.dictionaries.contains(&hash)
    }

    pub fn has_collision(&self, hash: u32) -> bool {
        self.collision.contains(&hash)
    }
}

fn f32_of(s: &rage_formats::value::MetaStruct, name: &str) -> Option<f32> {
    s.field(name).and_then(|v| v.as_f32())
}

fn vec_of(s: &rage_formats::value::MetaStruct, name: &str) -> Option<[f32; 3]> {
    s.field(name).and_then(|v| v.as_vec3()).map(|v| [v.x, v.y, v.z])
}

fn push(out: &mut Vec<Issue>, sev: Sev, code: &'static str, archetype: Option<usize>, message: String, fix: &str, path: Option<Path>) {
    out.push(Issue { sev, code, message, fix: fix.to_string(), archetype, path });
}

/// Check the whole file. With an `index`, the references of every archetype are looked up too.
pub fn validate(doc: &YtypDocument, index: Option<&AssetIndex>) -> Vec<Issue> {
    let mut out: Vec<Issue> = vec![];
    let mut seen: HashMap<u32, usize> = HashMap::new();
    for (i, a) in doc.archetypes().iter().enumerate() {
        let Some(s) = a.as_struct() else { continue };
        let who = doc.name(i);
        let at = |f: &str| -> Option<Path> {
            let mut p = archetype_path(i);
            p.push(field(f));
            Some(p)
        };
        let kind = ArchKind::of(a);
        if kind == ArchKind::Other {
            push(&mut out, Sev::Info, "ARCH_KIND", Some(i), format!("{who}: this entry is a {} (not a base, time or MLO archetype); it is kept as it is", super::schema_view::name_of(s.type_hash)), "nothing to do unless it is a mistake", None);
        }
        let h = doc.name_hash(i);
        if h == 0 {
            push(&mut out, Sev::Error, "ARCH_NO_NAME", Some(i), format!("archetype {i} has no name"), "give it a name; the game finds an archetype by it", at("name"));
        } else if let Some(first) = seen.insert(h, i) {
            push(&mut out, Sev::Error, "ARCH_DUPLICATE", Some(i), format!("{who} is also archetype {first}; the game can only use one of them"), "rename one of them, or remove the copy", at("name"));
        }
        // the box
        let (bmin, bmax) = (vec_of(s, "bbMin"), vec_of(s, "bbMax"));
        if let (Some(a), Some(b)) = (bmin, bmax) {
            if (0..3).any(|k| a[k] > b[k]) {
                push(&mut out, Sev::Error, "ARCH_BBOX", Some(i), format!("{who}: the bounding box is the wrong way round (bbMin {:?} is above bbMax {:?} on some axis)", a, b), "swap the corners, or recompute the box from the model", at("bbMin"));
            }
        }
        if let Some(r) = f32_of(s, "bsRadius") {
            if r < 0.0 || !r.is_finite() {
                push(&mut out, Sev::Error, "ARCH_RADIUS", Some(i), format!("{who}: the bounding sphere radius is {r}"), "use the distance from the centre to the farthest corner of the box", at("bsRadius"));
            }
        }
        if let Some(l) = f32_of(s, "lodDist") {
            if !l.is_finite() || l < 0.0 {
                push(&mut out, Sev::Error, "ARCH_LOD", Some(i), format!("{who}: lodDist is {l}"), "the distance the model is drawn at: a positive number such as 100", at("lodDist"));
            }
        }
        // references
        if let Some(ix) = index.filter(|ix| !ix.is_empty()) {
            let asset = s.field("assetName").and_then(|v| v.as_hash()).filter(|h| *h != 0).unwrap_or(h);
            let asset_type = s.field("assetType").map(|v| super::schema_view::to_text(v)).unwrap_or_default();
            // a drawable dictionary names its own dictionary; a plain drawable or fragment is a file of its own name
            let by_dictionary = asset_type.contains("DICTIONARY");
            // an assetless archetype (an interior, a marker) has no model file at all
            let assetless = asset_type.contains("ASSETLESS");
            if !by_dictionary && !assetless && asset != 0 && !ix.has_model(asset) {
                push(&mut out, Sev::Warning, "ARCH_MODEL_MISSING", Some(i), format!("{who}: no model file named {} was found", doc.names.resolve(asset)), "add the folder that holds your .ydr / .yft to the asset search, or check the assetName", at("assetName"));
            }
            if let Some(t) = s.field("textureDictionary").and_then(|v| v.as_hash()).filter(|h| *h != 0) {
                if !ix.has_dictionary(t) {
                    push(&mut out, Sev::Info, "ARCH_TXD_MISSING", Some(i), format!("{who}: no texture dictionary file named {} was found (it may be a shared one)", doc.names.resolve(t)), "check the textureDictionary name; shared dictionaries listed in gtxd are not files", at("textureDictionary"));
                }
            }
        }
        if kind == ArchKind::Mlo {
            mlo_checks(doc, i, s, &who, index, &mut out);
        }
    }
    out.sort_by_key(|x| (x.sev, x.archetype));
    out
}

fn mlo_checks(doc: &YtypDocument, i: usize, s: &rage_formats::value::MetaStruct, who: &str, index: Option<&AssetIndex>, out: &mut Vec<Issue>) {
    let rooms = s.field("rooms").map(MetaValue::items).unwrap_or(&[]);
    let portals = s.field("portals").map(MetaValue::items).unwrap_or(&[]);
    let entities = s.field("entities").map(MetaValue::items).unwrap_or(&[]);
    let mut add = |sev, code, message: String, fix: &str, path: Option<Path>| out.push(Issue { sev, code, message, fix: fix.to_string(), archetype: Some(i), path });
    if rooms.is_empty() {
        add(Sev::Warning, "MLO_NO_ROOMS", format!("{who}: the interior has no rooms (an MLO needs the limbo room and at least one more)"), "add a room", None);
    }
    for (n, p) in portals.iter().enumerate().filter_map(|(n, p)| p.as_struct().map(|s| (n, s))) {
        for side in ["roomFrom", "roomTo"] {
            if let Some(r) = p.field(side).and_then(|v| v.as_i64()) {
                if r < 0 || r as usize >= rooms.len() {
                    let mut path = archetype_path(i);
                    path.extend([field("portals"), Step::Index(n), field(side)]);
                    add(Sev::Error, "MLO_PORTAL_ROOM", format!("{who}: portal {n} {side} is room {r}, but there are only {} rooms (0 to {})", rooms.len(), rooms.len().saturating_sub(1)), "point it at a room that exists", Some(path));
                }
            }
        }
    }
    // an entity placed in the interior names an archetype: one of this file's, or a model of the game (when the game is indexed)
    if let Some(ix) = index.filter(|ix| !ix.is_empty()) {
        let mine: HashSet<u32> = (0..doc.len()).map(|k| doc.name_hash(k)).collect();
        let mut missing = 0;
        let mut first = None;
        for (n, e) in entities.iter().enumerate().filter_map(|(n, e)| e.as_struct().map(|s| (n, s))) {
            let a = e.field("archetypeName").and_then(|v| v.as_hash()).unwrap_or(0);
            if a != 0 && !mine.contains(&a) && !ix.has_model(a) {
                missing += 1;
                first.get_or_insert((n, a));
            }
        }
        if let Some((n, a)) = first {
            let mut path = archetype_path(i);
            path.extend([field("entities"), Step::Index(n), field("archetypeName")]);
            add(Sev::Info, "MLO_ENTITY_MODEL", format!("{who}: {missing} entities use a model that is neither in this file nor in the game's files (first: entity {n}, {})", doc.names.resolve(a)), "check the archetypeName of those entities (a model from another add-on is fine)", Some(path));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archetype::document::YtypDocument;

    fn doc(xml: &str) -> YtypDocument {
        let mut d = YtypDocument::open(xml.as_bytes(), None).unwrap();
        for n in ["prop_a", "prop_b", "my_interior"] {
            d.names.add_lowercase(n);
        }
        d
    }

    fn codes(d: &YtypDocument, ix: Option<&AssetIndex>) -> Vec<&'static str> {
        validate(d, ix).iter().map(|i| i.code).collect()
    }

    #[test]
    fn the_mistakes_that_only_show_up_in_game_are_found_and_a_good_file_is_clean() {
        let good = doc(r#"<CMapTypes><archetypes>
<Item type="CBaseArchetypeDef"><lodDist value="100"/><bbMin x="-1" y="-1" z="-1"/><bbMax x="1" y="1" z="1"/><bsRadius value="1.8"/><name>prop_a</name><assetName>prop_a</assetName><textureDictionary>prop_a</textureDictionary></Item>
<Item type="CBaseArchetypeDef"><lodDist value="100"/><bbMin x="0" y="0" z="0"/><bbMax x="2" y="2" z="2"/><bsRadius value="3"/><name>prop_b</name><assetName>prop_b</assetName></Item>
</archetypes></CMapTypes>"#);
        assert!(codes(&good, None).is_empty(), "{:?}", validate(&good, None));
        let bad = doc(r#"<CMapTypes><archetypes>
<Item type="CBaseArchetypeDef"><lodDist value="-5"/><bbMin x="3" y="0" z="0"/><bbMax x="1" y="1" z="1"/><bsRadius value="-1"/><name>prop_a</name></Item>
<Item type="CBaseArchetypeDef"><name>prop_a</name></Item>
<Item type="CBaseArchetypeDef"><lodDist value="100"/></Item>
</archetypes></CMapTypes>"#);
        let c = codes(&bad, None);
        for want in ["ARCH_LOD", "ARCH_BBOX", "ARCH_RADIUS", "ARCH_DUPLICATE", "ARCH_NO_NAME"] {
            assert!(c.contains(&want), "{want} missing from {c:?}");
        }
        assert!(validate(&bad, None).iter().all(|i| !i.fix.is_empty()));
    }

    #[test]
    fn references_are_looked_up_in_the_asset_index_and_portals_must_point_at_rooms() {
        let d = doc(r#"<CMapTypes><archetypes>
<Item type="CBaseArchetypeDef"><lodDist value="100"/><name>prop_a</name><assetName>prop_a</assetName><textureDictionary>prop_a</textureDictionary></Item>
<Item type="CBaseArchetypeDef"><lodDist value="100"/><name>prop_b</name><assetName>prop_b</assetName></Item>
</archetypes></CMapTypes>"#);
        let dir = std::env::temp_dir().join(format!("rage_toolkit_ytyp_ix_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("stream")).unwrap();
        fs::write(dir.join("stream/PROP_B.ydr"), "x").unwrap();
        let mut ix = AssetIndex::default();
        ix.add_folder(&dir);
        assert_eq!(ix.files, 1);
        let c = codes(&d, Some(&ix));
        assert!(c.contains(&"ARCH_MODEL_MISSING") && c.contains(&"ARCH_TXD_MISSING"), "{c:?}");
        assert_eq!(validate(&d, Some(&ix)).iter().filter(|i| i.code == "ARCH_MODEL_MISSING").count(), 1, "prop_b is found (names are not case-sensitive), prop_a is not");
        assert!(codes(&d, Some(&AssetIndex::default())).is_empty(), "with nothing indexed there is nothing to compare against");
        let _ = fs::remove_dir_all(&dir);
        // an MLO whose portal points past its rooms
        let m = doc(r#"<CMapTypes><archetypes>
<Item type="CMloArchetypeDef"><lodDist value="100"/><name>my_interior</name><rooms><Item><name>limbo</name></Item><Item><name>room1</name></Item></rooms><portals><Item><roomFrom value="0"/><roomTo value="5"/></Item><Item><roomFrom value="0"/><roomTo value="1"/></Item></portals><entities/></Item>
</archetypes></CMapTypes>"#);
        let issues = validate(&m, None);
        let p: Vec<&Issue> = issues.iter().filter(|i| i.code == "MLO_PORTAL_ROOM").collect();
        assert_eq!(p.len(), 1, "{issues:?}");
        assert!(p[0].message.contains("portal 0") && p[0].message.contains("2 rooms"));
        assert!(p[0].path.is_some());
    }
}
