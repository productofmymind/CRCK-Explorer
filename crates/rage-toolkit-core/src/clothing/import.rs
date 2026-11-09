//! Opening a whole built clothing pack as a project: one FiveM resource, or a folder of them (a control resource with the YMTs and
//! metas and stream resources with the files, as GTA World's pack is laid out). Every DLC comes back with its name, every drawable
//! with its number, files and YMT settings, and the creature metadata, alternate variations and first-person entries are
//! matched with the drawables. Nothing is changed on disk.
//!
//! What the project does not model is carried into the build as it is (a YMT that only blanks a Rockstar DLC, loose
//! replacements of Rockstar's files, data files of other types) and each such case is a note, so nothing is lost without a word.
use super::{extras, name_problem, parse_name, ymt, Carried, Dlc, Drawable, Kind, Place, Project, Role, Sex, Texture};
use anyhow::{bail, Context, Result};
use std::{
    collections::{BTreeMap, HashMap},
    fs,
    path::{Path, PathBuf},
};

/// The project and what the import has to say (what was carried as it is, what looks wrong in the pack).
#[derive(Debug, Clone)]
pub struct Imported {
    pub project: Project,
    pub notes: Vec<String>,
}

/// one file of a resource: where it is and its path inside the resource (with `/`)
#[derive(Debug, Clone)]
struct File {
    path: PathBuf,
    rel: String,
}

#[derive(Debug, Default)]
struct Resource {
    dir: PathBuf,
    /// (type, path inside the resource)
    data_files: Vec<(String, String)>,
    stream: Vec<File>,
}

fn walk(dir: &Path, base: &Path, out: &mut Vec<File>, depth: usize) {
    if depth > crate::search::MAX_FOLDER_DEPTH {
        return;
    }
    let mut kids: Vec<PathBuf> = fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path()).collect();
    kids.sort();
    for k in kids {
        if k.is_dir() {
            walk(&k, base, out, depth + 1);
        } else {
            let rel = k.strip_prefix(base).unwrap_or(&k).to_string_lossy().replace('\\', "/");
            out.push(File { path: k, rel });
        }
    }
}

fn manifest_data_files(text: &str) -> Vec<(String, String)> {
    let re = regex_lite::Regex::new(r#"data_file\s*\(?\s*['"]([^'"]+)['"]\s*\)?\s*\(?\s*['"]([^'"]+)['"]"#).unwrap();
    // lines that are commented out do not count
    text.lines().filter(|l| !l.trim_start().starts_with("--")).flat_map(|l| re.captures_iter(l).map(|c| (c[1].to_string(), c[2].replace('\\', "/"))).collect::<Vec<_>>()).collect()
}

fn read_resource(dir: &Path) -> Option<Resource> {
    let manifest = ["fxmanifest.lua", "__resource.lua"].iter().map(|m| dir.join(m)).find(|m| m.is_file())?;
    let text = fs::read_to_string(&manifest).unwrap_or_default();
    let mut r = Resource { dir: dir.to_path_buf(), data_files: manifest_data_files(&text), stream: vec![] };
    let stream = dir.join("stream");
    if stream.is_dir() {
        walk(&stream, dir, &mut r.stream, 0);
    }
    Some(r)
}

/// `(sex, rest)` of a name that starts with a freemode ped (`mp_m_freemode_01_...`), lowercase
fn ped_prefix(lower: &str) -> Option<(Sex, &str)> {
    if let Some(r) = lower.strip_prefix("mp_m_freemode_01_") {
        Some((Sex::Male, r))
    } else {
        lower.strip_prefix("mp_f_freemode_01_").map(|r| (Sex::Female, r))
    }
}

/// What a stream file is.
enum StreamKind {
    /// `<ped>_<dlc>.ymt`
    Ymt { sex: Sex, dlc: String },
    /// `mp_creaturemetadata_<x>.ymt` (the stem, lowercase)
    Creature(String),
    /// `<ped>_[p_]<dlc>^<name>` with what the name is
    Asset { sex: Sex, dlc: String, role: Role },
    Other,
}

fn stream_kind(file_name: &str) -> StreamKind {
    let lower = file_name.to_lowercase();
    if let Some((left, right)) = lower.split_once('^') {
        let (Some((sex, rest)), Some(role)) = (ped_prefix(left), parse_name(right)) else { return StreamKind::Other };
        let prop = matches!(role, Role::Drawable { kind: Kind::Prop(_), .. } | Role::Texture { kind: Kind::Prop(_), .. } | Role::Cloth { kind: Kind::Prop(_), .. } | Role::Alternate { kind: Kind::Prop(_), .. });
        let dlc = if prop { rest.strip_prefix("p_").unwrap_or(rest) } else { rest };
        return StreamKind::Asset { sex, dlc: dlc.to_string(), role };
    }
    let Some(stem) = lower.strip_suffix(".ymt") else { return StreamKind::Other };
    if stem.starts_with("mp_creaturemetadata_") {
        return StreamKind::Creature(stem.to_string());
    }
    match ped_prefix(stem) {
        Some((sex, dlc)) => StreamKind::Ymt { sex, dlc: dlc.to_string() },
        None => StreamKind::Other,
    }
}

/// the DLC name without the sex: `mp_m_gtawclothes1` and `mp_f_gtawclothes1` are one DLC
fn dlc_key(name: &str) -> String {
    let l = name.to_lowercase();
    l.strip_prefix("mp_m_").or_else(|| l.strip_prefix("mp_f_")).map(str::to_string).unwrap_or(l)
}

/// the other sex's name of a DLC: `mp_m_x` <-> `mp_f_x`, else the same
fn other_sex_name(name: &str, to: Sex) -> String {
    let l = name.to_lowercase();
    let rest = l.strip_prefix("mp_m_").or_else(|| l.strip_prefix("mp_f_"));
    match rest {
        Some(r) => format!("{}{}", if to == Sex::Male { "mp_m_" } else { "mp_f_" }, &name[name.len() - r.len()..]),
        None => name.to_string(),
    }
}

#[derive(Debug, Default)]
struct DlcInfo {
    /// as the shop meta (or file name) writes it, per sex (male, female)
    names: [Option<String>; 2],
    creature: Option<String>,
    shop: bool,
    ymt: [Option<PathBuf>; 2],
    /// first seen: shop metas in manifest order, then YMTs
    order: usize,
}

fn sex_i(s: Sex) -> usize {
    if s == Sex::Male {
        0
    } else {
        1
    }
}

/// Open the pack in `dir`: one resource, or a folder whose resources (one or two levels down) are read. Resources that are not
/// clothing (no shop meta, no freemode YMT, no `<ped>_<dlc>^` files) are left out, and with `only`, those whose folder name
/// does not start with it (`gtaw_char_clothes` in a folder that also holds heads and tattoos).
pub fn import_tree(dir: &Path, only: Option<&str>) -> Result<Imported> {
    let mut resources: Vec<Resource> = vec![];
    let mut look = |d: &Path| {
        if let Some(r) = read_resource(d) {
            resources.push(r);
        }
    };
    look(dir);
    let kids = |d: &Path| -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = fs::read_dir(d).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
        v.sort();
        v
    };
    for k in kids(dir) {
        look(&k);
        // a [category] folder of resources
        if k.file_name().is_some_and(|n| n.to_string_lossy().starts_with('[')) {
            for g in kids(&k) {
                look(&g);
            }
        }
    }
    let clothing = |r: &Resource| r.data_files.iter().any(|(t, _)| t == "SHOP_PED_APPAREL_META_FILE") || r.stream.iter().any(|f| !matches!(stream_kind(&f.rel.rsplit('/').next().unwrap_or("")), StreamKind::Other));
    let only = only.map(|o| o.trim().to_lowercase()).filter(|o| !o.is_empty());
    resources.retain(|r| only.as_ref().is_none_or(|o| r.dir.file_name().is_some_and(|n| n.to_string_lossy().to_lowercase().starts_with(o.as_str()))));
    resources.retain(clothing);
    if resources.is_empty() {
        bail!("no clothing resource found in {} (a folder with fxmanifest.lua and shop metas or freemode clothing files)", dir.display());
    }
    let mut notes: Vec<String> = vec![];
    // several packs side by side (gtaw_char_clothes_control + _stream_*, and some other mod): the biggest is opened
    if only.is_none() {
        let mut families: BTreeMap<String, usize> = BTreeMap::new();
        for r in &resources {
            *families.entry(family(&r.dir)).or_default() += r.stream.len().max(1);
        }
        if families.len() > 1 {
            let pick = families.iter().max_by_key(|x| *x.1).map(|x| x.0.clone()).unwrap_or_default();
            let others: Vec<&String> = families.keys().filter(|k| **k != pick).collect();
            notes.push(format!("the folder holds more than one clothing pack; {pick} was opened (also there: {}; open one by its name)", others.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")));
            resources.retain(|r| family(&r.dir) == pick);
        }
    }
    let mut carried: Vec<Carried> = vec![];
    // a resource with data files or YMTs holds the pack's metadata (a control resource); the others only stream
    let is_control = |r: &Resource| !r.data_files.is_empty() || r.stream.iter().any(|f| matches!(stream_kind(f.rel.rsplit('/').next().unwrap_or("")), StreamKind::Ymt { .. } | StreamKind::Creature(_)));
    // a carried file goes back to the resource it came from: the control one, a stream resource of the pack (by its name after
    // the project's, so a renamed project keeps the layout), or the one of Rockstar's DLCs
    let pname = project_name(&resources);
    let place_of = |r: &Resource| -> Place {
        if is_control(r) {
            return Place::Control;
        }
        let n = r.dir.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
        match n.strip_prefix(&format!("{pname}_")) {
            Some("stream_base") | None => Place::BaseStream,
            Some(suffix) => Place::Stream(suffix.to_string()),
        }
    };

    // ---- the DLCs: shop metas first (their order), then YMTs
    let mut dlcs: BTreeMap<String, DlcInfo> = BTreeMap::new();
    let mut order = 0usize;
    let mut other_data: Vec<(&Resource, String, String)> = vec![];
    for r in &resources {
        for (kind, rel) in &r.data_files {
            if kind != "SHOP_PED_APPAREL_META_FILE" {
                other_data.push((r, kind.clone(), rel.clone()));
                continue;
            }
            let path = r.dir.join(rel);
            let Ok(text) = fs::read_to_string(&path) else {
                notes.push(format!("the shop meta {} is missing", path.display()));
                continue;
            };
            let Ok(doc) = roxmltree::Document::parse(&text) else {
                notes.push(format!("the shop meta {} is not valid XML", path.display()));
                continue;
            };
            let field = |n: &str| doc.root_element().children().find(|c| c.has_tag_name(n)).and_then(|c| c.text()).map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
            let (Some(ped), Some(name)) = (field("pedName"), field("dlcName")) else {
                notes.push(format!("the shop meta {} has no pedName or dlcName", path.display()));
                continue;
            };
            let Some((sex, _)) = ped_prefix(&format!("{}_", ped.to_lowercase())) else {
                notes.push(format!("the shop meta {} is for ped {ped}, not a freemode ped: left out", path.display()));
                continue;
            };
            for list in ["pedOutfits", "pedComponents", "pedProps"] {
                if doc.root_element().children().find(|c| c.has_tag_name(list)).is_some_and(|c| c.children().any(|x| x.is_element())) {
                    notes.push(format!("the shop meta {} lists {list}; the build writes it empty (clothes are added by script)", path.display()));
                }
            }
            let info = dlcs.entry(dlc_key(&name)).or_insert_with(|| DlcInfo { order: { order += 1; order }, ..Default::default() });
            info.names[sex_i(sex)] = Some(name);
            info.shop = true;
            if let Some(c) = field("creatureMetaData") {
                if info.creature.as_ref().is_some_and(|x| !x.eq_ignore_ascii_case(&c)) {
                    notes.push(format!("the two shop metas of DLC {} name different creature metadata; {c} is used", dlc_key(&c)));
                }
                info.creature = Some(c);
            }
        }
    }
    let mut assets: HashMap<(Sex, String), Vec<(Role, &File, &Resource)>> = HashMap::new();
    let mut creature_files: Vec<(String, &File, &Resource)> = vec![];
    for r in &resources {
        for f in &r.stream {
            let name = f.rel.rsplit('/').next().unwrap_or("");
            match stream_kind(name) {
                StreamKind::Ymt { sex, dlc } => {
                    let info = dlcs.entry(dlc_key(&dlc)).or_insert_with(|| DlcInfo { order: 1000 + { order += 1; order }, ..Default::default() });
                    if info.ymt[sex_i(sex)].is_some() {
                        notes.push(format!("{} is in the pack twice; the second ({}) is carried as it is", name, f.path.display()));
                        carried.push(Carried { place: Place::Control, dest: f.rel.clone(), source: f.path.clone(), data_file: None });
                        continue;
                    }
                    info.names[sex_i(sex)].get_or_insert(dlc);
                    info.ymt[sex_i(sex)] = Some(f.path.clone());
                }
                StreamKind::Creature(stem) => creature_files.push((stem, f, r)),
                StreamKind::Asset { sex, dlc, role } => assets.entry((sex, dlc)).or_default().push((role, f, r)),
                StreamKind::Other => {
                    notes.push(format!("{} is not a clothing file; carried as it is", f.rel));
                    carried.push(Carried { place: place_of(r), dest: f.rel.clone(), source: f.path.clone(), data_file: None });
                }
            }
        }
    }

    // ---- which YMTs can be modelled; the others are carried
    let mut readable: BTreeMap<String, [Option<ymt::Ymt>; 2]> = BTreeMap::new();
    for (key, info) in &dlcs {
        for s in 0..2 {
            let Some(path) = &info.ymt[s] else { continue };
            let rel = resources.iter().flat_map(|r| &r.stream).find(|f| &f.path == path).map(|f| f.rel.clone()).unwrap_or_default();
            let read = fs::read(path).context("reading").and_then(|b| ymt::read_ymt(&b));
            match read {
                Ok((y, hash, odd)) if odd.is_empty() => {
                    let name = info.names[s].clone().unwrap_or_default();
                    if hash != rage_formats::rage_joaat(&name.to_lowercase()) {
                        notes.push(format!("{}: its dlcName hash is not {name}'s; the build writes {name}'s", rel));
                    }
                    readable.entry(key.clone()).or_default()[s] = Some(y);
                }
                Ok((_, _, odd)) => {
                    let what = if odd.len() > 3 { format!("{}; and {} more", odd[..2].join("; "), odd.len() - 2) } else { odd.join("; ") };
                    notes.push(format!("{rel} holds what the builder does not model ({what}); carried as it is, its drawables are not editable"));
                    carried.push(Carried { place: Place::Control, dest: rel, source: path.clone(), data_file: None });
                }
                Err(e) => {
                    notes.push(format!("{rel} cannot be read as a clothing YMT ({e:#}); carried as it is"));
                    carried.push(Carried { place: Place::Control, dest: rel, source: path.clone(), data_file: None });
                }
            }
        }
    }

    // ---- the project's DLCs: shop-meta DLCs in order, then Rockstar DLCs (a YMT, no shop meta)
    let mut keys: Vec<&String> = readable.keys().collect();
    keys.sort_by_key(|k| (dlcs[*k].order, (*k).clone()));
    let mut p = Project { name: pname.clone(), dlcs: vec![], ..Default::default() };
    for key in &keys {
        let info = &dlcs[*key];
        let m = info.names[0].clone().or_else(|| info.names[1].as_deref().map(|f| other_sex_name(f, Sex::Male))).unwrap_or_else(|| (*key).clone());
        let f = info.names[1].clone().unwrap_or_else(|| other_sex_name(&m, Sex::Female));
        p.dlcs.push(Dlc { names: Some((m, f)), creature: info.creature.clone(), base: !info.shop, left_out: false, game_creature: None });
    }
    let biggest = keys.iter().enumerate().flat_map(|(k, key)| readable[*key].iter().flatten().flat_map(move |y| y.components.values().chain(y.props.values()).map(move |v| (k, v.len())))).map(|x| x.1).max().unwrap_or(0);
    p.limit = if biggest > 128 { 256 } else { p.limit.max(128) };

    // ---- the drawables, slot by slot
    let mut used: std::collections::HashSet<PathBuf> = Default::default();
    for (k, key) in keys.iter().enumerate() {
        for sex in [Sex::Male, Sex::Female] {
            let Some(y) = &readable[*key][sex_i(sex)] else { continue };
            let dlc_name = p.dlc_name(k, sex).to_lowercase();
            let files = assets.get(&(sex, dlc_name.clone()));
            let base = p.dlcs[k].base;
            for (kind, number, entry) in y.entries() {
                let mine: Vec<&(Role, &File, &Resource)> = files.into_iter().flatten().filter(|(r, _, _)| match r {
                    Role::Drawable { kind: k2, number: n, .. } | Role::Texture { kind: k2, number: n, .. } | Role::Cloth { kind: k2, number: n } | Role::Alternate { kind: k2, number: n, .. } => *k2 == kind && *n == number,
                }).collect();
                let mut d = Drawable { dlc: k, sex, kind, number, source_number: number, ..Default::default() };
                let mut models: Vec<_> = mine.iter().filter(|(r, _, _)| matches!(r, Role::Drawable { .. })).collect();
                let want = if entry.prop_mask & 16 != 0 { "r" } else { "u" };
                models.sort_by_key(|(r, _, _)| !matches!(r, Role::Drawable { race, .. } if race == want));
                match models.first() {
                    Some((Role::Drawable { race, .. }, f, _)) => {
                        d.ydd = f.path.clone();
                        d.race = race.clone();
                        used.insert(f.path.clone());
                    }
                    _ if base => d.race = if kind.is_prop() { String::new() } else { "u".into() },
                    _ => {
                        notes.push(format!("{} {} {} {number:03} is in the YMT but its model is not in the pack", p.dlc_name(k, sex), sex.label(), kind.name()));
                        // empty: the build writes the slot as the pack had it, and the checks warn
                        d.race = if kind.is_prop() { String::new() } else { "u".into() };
                    }
                }
                for (r, f, res) in models.iter().skip(1) {
                    if let Role::Drawable { race, .. } = r {
                        notes.push(format!("{} has a second model for {} {number:03} ({race}); carried as it is", p.dlc_name(k, sex), kind.name()));
                    }
                    carried.push(Carried { place: place_of(res), dest: f.rel.clone(), source: f.path.clone(), data_file: None });
                    used.insert(f.path.clone());
                }
                for (r, f, _) in &mine {
                    match r {
                        Role::Cloth { .. } => {
                            d.cloth = Some(f.path.clone());
                            used.insert(f.path.clone());
                        }
                        Role::Alternate { alt, .. } => {
                            d.alternates.push((*alt, f.path.clone()));
                            used.insert(f.path.clone());
                        }
                        _ => {}
                    }
                }
                // alternates and cloth the YMT counts but the pack does not stream: empty (the game's own, or a gap the checks show)
                for n in 1..=u32::from(entry.alternates) {
                    if !d.alternates.iter().any(|(a, _)| *a == n) {
                        d.alternates.push((n, PathBuf::new()));
                    }
                }
                if entry.cloth && d.cloth.is_none() {
                    d.cloth = Some(PathBuf::new());
                }
                d.alternates.sort();
                // textures by letter; the YMT says how many: missing ones are empty (the game's own in a Rockstar DLC)
                let mut tex: BTreeMap<char, (String, PathBuf)> = BTreeMap::new();
                for (r, f, _) in &mine {
                    if let Role::Texture { letter, race, .. } = r {
                        tex.insert(*letter, (race.clone(), f.path.clone()));
                    }
                }
                let count = entry.textures.len();
                let race_default = tex.values().next().map(|t| t.0.clone()).unwrap_or_else(|| if kind.is_prop() { String::new() } else { "uni".into() });
                for n in 0..count {
                    let letter = (b'a' + n.min(25) as u8) as char;
                    match tex.remove(&letter) {
                        Some((race, path)) => {
                            used.insert(path.clone());
                            d.textures.push(Texture { path, letter, race, tex_id: None, edit: None });
                        }
                        None => {
                            if !base {
                                notes.push(format!("{} {} {} {number:03}: texture {letter} is in the YMT but not in the pack", p.dlc_name(k, sex), sex.label(), kind.name()));
                            }
                            d.textures.push(Texture { path: PathBuf::new(), letter, race: race_default.clone(), tex_id: None, edit: None });
                        }
                    }
                }
                for (letter, (_, path)) in tex {
                    notes.push(format!("{} {} {} {number:03}: texture {letter} is past the {count} the YMT has; carried as it is", p.dlc_name(k, sex), sex.label(), kind.name()));
                    let (_, f, res) = mine.iter().find(|(_, f, _)| f.path == path).unwrap();
                    carried.push(Carried { place: place_of(res), dest: f.rel.clone(), source: path.clone(), data_file: None });
                    used.insert(path);
                }
                entry.apply(&mut d);
                p.drawables.push(d);
            }
        }
    }
    // files of DLCs the project does not model (loose replacements of Rockstar's files, or a DLC whose YMT is carried)
    let mut loose: BTreeMap<String, usize> = BTreeMap::new();
    for ((_, dlc), list) in &assets {
        for (_, f, r) in list {
            if used.contains(&f.path) {
                continue;
            }
            let ours = p.dlcs.iter().enumerate().any(|(k, _)| p.dlc_name(k, Sex::Male).eq_ignore_ascii_case(dlc) || p.dlc_name(k, Sex::Female).eq_ignore_ascii_case(dlc));
            if ours {
                notes.push(format!("{} has no slot in its DLC's YMT; carried as it is", f.rel));
            } else {
                *loose.entry(dlc.clone()).or_default() += 1;
            }
            carried.push(Carried { place: place_of(r), dest: f.rel.clone(), source: f.path.clone(), data_file: None });
        }
    }
    for (dlc, n) in loose {
        notes.push(format!("{n} file(s) of {dlc}, a DLC with no YMT in the pack (replacements of the game's own files), carried as they are"));
    }

    // ---- creature metadata: matched with its DLC; the same as the build writes, or carried
    for (stem, f, _) in creature_files {
        let k = (0..p.dlcs.len()).find(|&k| p.dlcs[k].creature.as_ref().is_some_and(|c| c.eq_ignore_ascii_case(&stem))).or_else(|| {
            (0..p.dlcs.len()).find(|&k| p.dlcs[k].base && p.dlcs[k].creature.is_none() && stem.replace('_', "").ends_with(&p.dlc_label(k).replace('_', "")))
        });
        let Some(k) = k else {
            notes.push(format!("{} belongs to no DLC of the pack; carried as it is", f.rel));
            carried.push(Carried { place: Place::Control, dest: f.rel.clone(), source: f.path.clone(), data_file: None });
            continue;
        };
        if p.dlcs[k].creature.is_none() {
            p.dlcs[k].creature = Some(stem.clone());
        }
        let same = fs::read(&f.path).ok().zip(extras::creature_bytes(&extras::creature_expressions(&p, k)).ok()).is_some_and(|(a, b)| a == b);
        if !same {
            notes.push(format!("{} holds more than the build writes for {}; carried as it is", f.rel, p.dlc_label(k)));
            carried.push(Carried { place: Place::Control, dest: format!("stream/{}.ymt", p.creature_name(k).to_lowercase()), source: f.path.clone(), data_file: None });
        }
    }

    // ---- the other data files: alternate variations and first-person alternates are read in, the rest is carried
    let dlc_of = |p: &Project, names: &[&str]| -> Option<usize> {
        let mut count = vec![0usize; p.dlcs.len()];
        for n in names {
            for (k, c) in count.iter_mut().enumerate() {
                if p.dlc_name(k, Sex::Male).eq_ignore_ascii_case(n) || p.dlc_name(k, Sex::Female).eq_ignore_ascii_case(n) {
                    *c += 1;
                }
            }
        }
        count.iter().enumerate().filter(|x| *x.1 > 0).max_by_key(|x| *x.1).map(|x| x.0)
    };
    for (r, kind, rel) in other_data {
        let path = r.dir.join(&rel);
        let text = fs::read_to_string(&path).ok();
        let first_person = matches!(kind.as_str(), "PED_FIRST_PERSON_ALTERNATE_DATA" | "PED_FIRST_PERSON_ASSET_DATA") && text.as_deref().is_some_and(|t| t.contains("<FirstPersonAlternateData"));
        if kind == "ALTERNATE_VARIATIONS_FILE" {
            if let Some(Ok((raws, odd))) = text.as_deref().map(extras::read_alt_variations) {
                let names: Vec<&str> = raws.iter().flat_map(|w| std::iter::once(w.target.dlc.as_str()).chain(w.sources.iter().map(|s| s.dlc.as_str()))).collect();
                if let Some(k) = dlc_of(&p, &names) {
                    for o in odd {
                        notes.push(format!("{rel}: {o}"));
                    }
                    extras::add_switches(&mut p, k, &raws);
                    continue;
                }
            }
        } else if first_person {
            if let Some(Ok(entries)) = text.as_deref().map(extras::read_first_person) {
                let names: Vec<String> = entries.iter().filter_map(|(a, _)| {
                    let left = a.split('/').next()?.to_lowercase();
                    let (_, rest) = ped_prefix(&left)?;
                    Some(rest.strip_prefix("p_").filter(|r| r.starts_with("mp_")).unwrap_or(rest).to_string())
                }).collect();
                if let Some(k) = dlc_of(&p, &names.iter().map(String::as_str).collect::<Vec<_>>()) {
                    if kind == "PED_FIRST_PERSON_ASSET_DATA" {
                        notes.push(format!("{rel} was registered as PED_FIRST_PERSON_ASSET_DATA (the type of first_person.meta), so the game likely never read it; the build registers it as PED_FIRST_PERSON_ALTERNATE_DATA"));
                    }
                    let before = p.other_first_person.len();
                    extras::add_first_person(&mut p, k, &entries);
                    for o in &p.other_first_person[before..] {
                        notes.push(format!("{rel}: {} names no drawable of the pack; kept as it is", o.asset));
                    }
                    continue;
                }
            }
        }
        if !path.is_file() {
            notes.push(format!("the manifest of {} names {rel} ({kind}), which is not there", r.dir.display()));
            continue;
        }
        notes.push(format!("{rel} ({kind}) is carried as it is"));
        carried.push(Carried { place: Place::Control, dest: rel, source: path, data_file: Some(kind) });
    }
    p.carried = carried;
    // a pack of several resources is rebuilt the same way
    if resources.len() > 1 {
        p.layout = super::Layout::ControlAndStreams;
    }
    if p.dlcs.is_empty() {
        p.dlcs.push(Dlc::default());
    }
    Ok(Imported { project: p, notes })
}

/// The pack a resource belongs to by its folder name: `gtaw_char_clothes` for `gtaw_char_clothes_control` and
/// `gtaw_char_clothes_stream_m3_02`; the whole name for any other.
fn family(dir: &Path) -> String {
    let n = dir.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
    for cut in ["_control", "_stream"] {
        if let Some(i) = n.find(cut) {
            return n[..i].to_string();
        }
    }
    n
}

/// The project name from the resources' folder names: what they share (`gtaw_char_clothes` of `gtaw_char_clothes_control`,
/// `gtaw_char_clothes_stream_m1`, ...), or the one resource's name.
fn project_name(resources: &[Resource]) -> String {
    let names: Vec<String> = resources.iter().filter_map(|r| r.dir.file_name().map(|n| n.to_string_lossy().to_lowercase())).collect();
    let mut prefix = names.first().cloned().unwrap_or_default();
    for n in &names[1.min(names.len())..] {
        while !n.starts_with(&prefix) {
            prefix.pop();
        }
    }
    let mut name = prefix.trim_end_matches(|c: char| !c.is_ascii_alphanumeric()).to_string();
    if names.len() == 1 {
        for suffix in ["_control", "_stream"] {
            name = name.strip_suffix(suffix).map(str::to_string).unwrap_or(name);
        }
    }
    let name: String = name.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
    if name_problem(&name).is_none() {
        name
    } else {
        "mypack".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clothing::build::{build, plan_build, Source};

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("rage_toolkit_cloth_import_{name}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn put(d: &Path, name: &str, body: &str) -> PathBuf {
        let p = d.join(name);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, body).unwrap();
        p
    }

    #[test]
    fn a_built_pack_with_every_setting_opens_again_as_the_same_project() {
        use crate::clothing::{AltSwitch, AssetRef, DrawableMeta};
        let d = temp("roundtrip");
        let (y, alt, t1, t2) = (put(&d, "in/x.ydd", "1"), put(&d, "in/alt.ydd", "a"), put(&d, "in/t1.ytd", "t1"), put(&d, "in/t2.ytd", "t2"));
        let tex = |paths: &[&PathBuf]| paths.iter().enumerate().map(|(n, p)| Texture { path: (*p).clone(), letter: (b'a' + n as u8) as char, race: "uni".into(), tex_id: None, edit: None }).collect::<Vec<_>>();
        let mut shoe = Drawable { sex: Sex::Female, kind: Kind::Component(6), ydd: y.clone(), race: "u".into(), textures: tex(&[&t1]), meta: DrawableMeta { expression_mods: [0.0, 0.0, 0.0, 0.0, 0.9], prop_mask: Some(11), ..Default::default() }, ..Default::default() };
        shoe.textures[0].tex_id = Some(4);
        let jacket = Drawable { sex: Sex::Male, kind: Kind::Component(11), ydd: y.clone(), race: "u".into(), alternates: vec![(1, alt.clone())], textures: tex(&[&t1, &t2]), meta: DrawableMeta { first_person: Some(1), ..Default::default() }, ..Default::default() };
        let hat = Drawable { sex: Sex::Male, kind: Kind::Prop(0), ydd: y.clone(), textures: tex(&[&t1]).into_iter().map(|t| Texture { race: String::new(), ..t }).collect(), meta: DrawableMeta { expression_mods: [-0.5, 0.0, 0.0, 0.0, 0.0], ..Default::default() }, ..Default::default() };
        let jid = jacket.id;
        let mut p = Project { name: "packa".into(), drawables: vec![shoe, jacket, hat], ..Default::default() };
        p.alt_switches.push(AltSwitch { file: 0, sex: Sex::Male, target: AssetRef::Other { dlc: "male_heist".into(), component: 5, index: 0 }, alt: 19, sources: vec![AssetRef::Ours(jid)] });
        let out = build(&plan_build(&p), &d.join("out")).unwrap();
        let back = import_tree(&out, None).unwrap();
        assert!(back.notes.is_empty(), "{:#?}", back.notes);
        let q = back.project;
        assert_eq!((q.name.as_str(), q.dlc_name(0, Sex::Male).as_str(), q.dlcs.len()), ("packa", "packa_1", 1));
        let key = |p: &Project| {
            let mut v: Vec<_> = p.drawables.iter().map(|x| (x.sex, x.kind, x.number, x.textures.len(), x.textures.iter().map(|t| t.tex_id).collect::<Vec<_>>(), x.alternates.len(), x.meta.clone())).collect();
            v.sort_by(|a, b| (a.0, a.1, a.2).cmp(&(b.0, b.1, b.2)));
            v
        };
        assert_eq!(key(&q), key(&p), "every drawable with its settings");
        let jacket = q.drawables.iter().find(|x| x.kind == Kind::Component(11)).unwrap();
        assert_eq!(q.alt_switches, [AltSwitch { file: 0, sex: Sex::Male, target: AssetRef::Other { dlc: "male_heist".into(), component: 5, index: 0 }, alt: 19, sources: vec![AssetRef::Ours(jacket.id)] }]);
        // and builds again into the same metadata
        let again = plan_build(&q);
        assert_eq!(again.errors(), 0, "{:?}", again.issues);
        for o in plan_build(&p).outputs {
            if let Source::Bytes(b) = &o.source {
                let ours = again.outputs.iter().find(|x| x.dest == o.dest).unwrap_or_else(|| panic!("{} missing", o.dest));
                assert!(matches!(&ours.source, Source::Bytes(c) if c == b), "{} differs", o.dest);
            }
        }
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn of_two_packs_side_by_side_the_bigger_one_opens_and_the_other_is_named() {
        let d = temp("families");
        for (r, files) in [("big_control", 1), ("big_stream_m1", 3), ("faces", 1)] {
            put(&d.join(r), "fxmanifest.lua", "fx_version 'cerulean'\n");
            for n in 0..files {
                put(&d.join(r), &format!("stream/mp_m_freemode_01_mp_m_x^jbib_{n:03}_u.ydd"), "m");
            }
        }
        let im = import_tree(&d, None).unwrap();
        assert!(im.notes[0].contains("big was opened") && im.notes[0].contains("faces"), "{:?}", im.notes);
        assert_eq!(im.project.name, "big");
        assert_eq!(import_tree(&d, Some("faces")).unwrap().project.name, "faces");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn what_cannot_be_modelled_is_carried_and_said() {
        let d = temp("carry");
        let r = d.join("pack_control");
        put(&r, "fxmanifest.lua", "fx_version 'cerulean'\ngame 'gta5'\ndata_file 'PED_METADATA_FILE' 'data/peds.meta'\n-- data_file 'SHOP_PED_APPAREL_META_FILE' 'data/old.meta'\n");
        put(&r, "data/peds.meta", "<CPedModelInfo__InitDataList/>");
        put(&r, "stream/mp_m_freemode_01_mp_m_heist3^lowr_diff_000_a_uni.ytd", "loose");
        let back = import_tree(&d, None).unwrap();
        assert!(back.project.drawables.is_empty());
        let dests: Vec<&str> = back.project.carried.iter().map(|c| c.dest.as_str()).collect();
        assert_eq!(dests, ["stream/mp_m_freemode_01_mp_m_heist3^lowr_diff_000_a_uni.ytd", "data/peds.meta"]);
        assert_eq!(back.project.carried[1].data_file.as_deref(), Some("PED_METADATA_FILE"));
        assert_eq!(back.notes.len(), 2, "{:#?}", back.notes);
        assert_eq!(back.project.name, "pack");
        let _ = fs::remove_dir_all(&d);
    }

    /// Building the female half of a big pack, timed: CRCK_CLOTHING_TREE as below, CRCK_BUILD_OUT an empty folder to build into.
    #[test]
    #[ignore = "timing on a real pack: CRCK_CLOTHING_TREE=..., CRCK_BUILD_OUT=..., run with --release --ignored --nocapture"]
    fn big_pack_build_timings() {
        let dir = PathBuf::from(std::env::var("CRCK_CLOTHING_TREE").expect("CRCK_CLOTHING_TREE"));
        let out = PathBuf::from(std::env::var("CRCK_BUILD_OUT").expect("CRCK_BUILD_OUT"));
        let ms = |t: std::time::Instant| t.elapsed().as_secs_f64() * 1000.0;
        let mut p = import_tree(&dir, std::env::var("CRCK_CLOTHING_PREFIX").ok().as_deref()).unwrap().project;
        p.drawables.retain(|d| d.sex == Sex::Female);
        p.carried.clear();
        p.alt_switches.clear();
        p.other_first_person.clear();
        p.name = "timing".into();
        let t = std::time::Instant::now();
        let plan = plan_build(&p);
        let bytes: u64 = plan.outputs.iter().map(|o| match &o.source {
            Source::Copy(f) | Source::Optimize { from: f, .. } | Source::Image(f) => fs::metadata(f).map(|m| m.len()).unwrap_or(0),
            Source::Bytes(b) => b.len() as u64,
        }).sum();
        println!("plan_build: {:.0} ms, {} outputs, {:.0} MB, {} errors", ms(t), plan.outputs.len(), bytes as f64 / 1e6, plan.errors());
        let t = std::time::Instant::now();
        let _ = crate::clothing::validate(&p);
        println!("  validate: {:.0} ms", ms(t));
        for (k, ds) in crate::clothing::build::dlc_groups(&p) {
            let ds: Vec<&crate::clothing::Drawable> = ds.into_iter().filter(|d| d.sex == Sex::Female).collect();
            let t = std::time::Instant::now();
            let y = crate::clothing::ymt::Ymt::of(&ds);
            let t1 = ms(t);
            let t = std::time::Instant::now();
            let _ = crate::clothing::ymt::ymt_bytes(&y, &p.dlc_name(k, Sex::Female));
            let t2 = ms(t);
            let t = std::time::Instant::now();
            let (mut o, mut d, mut i) = (vec![], Default::default(), vec![]);
            crate::clothing::build::dlc_extras(&p, k, "", "", &mut o, &mut d, &mut i).unwrap();
            println!("  DLC {k} ({} drawables): Ymt::of {t1:.0} ms, ymt_bytes {t2:.0} ms, extras {:.0} ms", ds.len(), ms(t));
        }
        let t = std::time::Instant::now();
        let (_, stats) = crate::clothing::build::build_with(&plan, &out, &|_, _| {}).unwrap();
        let s = t.elapsed().as_secs_f64();
        println!("build: {s:.1} s ({} files, {:.0} MB/s)", stats.files, bytes as f64 / 1e6 / s);
        let t = std::time::Instant::now();
        let r = crate::clothing::build::build_update(&plan, &out, false).unwrap();
        println!("update right after: {:.0} ms ({} written, {} unchanged)", ms(t), r.written, r.unchanged);
    }

    /// What the Clothing page does with a big pack, timed (CRCK_CLOTHING_TREE / CRCK_CLOTHING_PREFIX as below).
    #[test]
    #[ignore = "timing on a real pack: CRCK_CLOTHING_TREE=..., run with --release --ignored --nocapture"]
    fn big_pack_timings() {
        let dir = PathBuf::from(std::env::var("CRCK_CLOTHING_TREE").expect("CRCK_CLOTHING_TREE"));
        let prefix = std::env::var("CRCK_CLOTHING_PREFIX").ok();
        let ms = |t: std::time::Instant| t.elapsed().as_secs_f64() * 1000.0;
        let t = std::time::Instant::now();
        let p = import_tree(&dir, prefix.as_deref()).unwrap().project;
        println!("import: {:.0} ms ({} drawables)", ms(t), p.drawables.len());
        for run in ["validate, first", "validate, again"] {
            let t = std::time::Instant::now();
            let n = crate::clothing::validate(&p).len();
            println!("{run}: {:.0} ms ({n} issues)", ms(t));
        }
        let t = std::time::Instant::now();
        let q = p.clone();
        println!("clone: {:.2} ms", ms(t));
        let t = std::time::Instant::now();
        let same = q == p;
        println!("compare: {:.2} ms ({same})", ms(t));
        let t = std::time::Instant::now();
        let plan = crate::clothing::build::plan_build(&p);
        println!("plan_build: {:.0} ms ({} outputs)", ms(t), plan.outputs.len());
        // picking every model of the biggest stream folder, as + FEMALE > Models does
        if let Some(folder) = dir.read_dir().unwrap().flatten().map(|e| e.path()).filter(|r| r.join("stream").is_dir()).flat_map(|r| r.join("stream").read_dir().unwrap().flatten().map(|e| e.path())).max_by_key(|f| f.read_dir().map(|d| d.count()).unwrap_or(0)) {
            let models: Vec<PathBuf> = folder.read_dir().unwrap().flatten().map(|e| e.path()).filter(|f| f.extension().is_some_and(|e| e == "ydd")).collect();
            let t = std::time::Instant::now();
            let all = crate::clothing::with_textures(&models);
            let scanned = crate::clothing::scan(&all);
            let paired = crate::clothing::pair(&scanned, crate::clothing::Sex::Female);
            println!("picking {} models in {}: with their textures {} files, {} drawables, {:.0} ms", models.len(), folder.display(), all.len(), paired.drawables.len(), ms(t));
        }
    }

    /// GTA World's pack (set CRCK_CLOTHING_TREE to the folder with its resources, CRCK_CLOTHING_PREFIX to their name start):
    /// imported, planned again, and every YMT, creature file, alternate variations and first-person file compared with the original.
    #[test]
    #[ignore = "needs a pack: set CRCK_CLOTHING_TREE=<folder with the resources> and run with --ignored --nocapture"]
    fn a_real_pack_imports_and_plans_back_the_same_metadata() {
        use crate::clothing::ymt::read_ymt;
        let dir = PathBuf::from(std::env::var("CRCK_CLOTHING_TREE").expect("CRCK_CLOTHING_TREE"));
        let t = std::time::Instant::now();
        let prefix = std::env::var("CRCK_CLOTHING_PREFIX").ok();
        let im = import_tree(&dir, prefix.as_deref()).unwrap();
        let p = &im.project;
        println!("imported in {:.1}s: {} DLCs, {} drawables, {} carried, {} notes", t.elapsed().as_secs_f64(), p.dlcs.len(), p.drawables.len(), p.carried.len(), im.notes.len());
        for n in im.notes.iter().take(40) {
            println!("  note: {n}");
        }
        for k in 0..p.dlcs.len() {
            println!("  DLC {k}: {} / {}{} creature {}", p.dlc_name(k, Sex::Male), p.dlc_name(k, Sex::Female), if p.dlcs[k].base { " (Rockstar's)" } else { "" }, p.creature_name(k));
        }
        let plan = plan_build(p);
        for i in plan.issues.iter().filter(|i| i.sev == crate::clothing::Sev::Error).take(20) {
            println!("  error: {}", i.message);
        }
        assert_eq!(plan.errors(), 0);
        let bytes = |dest: &str| plan.outputs.iter().find(|o| o.dest.eq_ignore_ascii_case(dest)).map(|o| match &o.source {
            Source::Bytes(b) => b.clone(),
            Source::Copy(f) | Source::Image(f) | Source::Optimize { from: f, .. } => fs::read(f).unwrap(),
        });
        let planned: std::collections::HashSet<String> = plan.outputs.iter().map(|o| o.dest.to_lowercase()).collect();
        let mut checked = 0;
        let resources: Vec<PathBuf> = fs::read_dir(&dir).unwrap().flatten().map(|e| e.path()).filter(|p| p.join("fxmanifest.lua").is_file() && prefix.as_ref().is_none_or(|x| p.file_name().unwrap().to_string_lossy().starts_with(x.as_str()))).collect();
        let mut stream_files = 0;
        let mut moved_by_the_limit = 0;
        for r in &resources {
            let rname = r.file_name().unwrap().to_string_lossy().to_string();
            let mut files = vec![];
            walk(&r.join("stream"), r, &mut files, 0);
            for f in files {
                let name = f.rel.rsplit('/').next().unwrap().to_string();
                match stream_kind(&name) {
                    StreamKind::Ymt { .. } => {
                        let orig = fs::read(&f.path).unwrap();
                        let ours = bytes(&format!("{rname}/stream/{name}")).unwrap_or_else(|| panic!("{rname}/stream/{name} is not written"));
                        if ours != orig {
                            let (a, b) = (read_ymt(&ours).unwrap().0, read_ymt(&orig).unwrap().0);
                            if let Some(((k, n, x), (_, _, y))) = a.entries().zip(b.entries()).find(|(x, y)| x != y) {
                                panic!("{name}: {} {n:03} is written as {x:?}, the pack has {y:?}", k.name());
                            }
                            assert_eq!(a.entries().count(), b.entries().count(), "{name}: entries");
                        }
                        checked += 1;
                    }
                    StreamKind::Creature(_) => {
                        assert_eq!(bytes(&format!("{rname}/stream/{name}")).unwrap_or_else(|| panic!("{rname}/stream/{name} is not written")), fs::read(&f.path).unwrap(), "{name}");
                        checked += 1;
                    }
                    _ => {
                        stream_files += 1;
                        // every streamed file lands in the same resource at the same path; files of Rockstar DLCs may move to
                        // _stream_base_02, ... when a type passes the limit
                        let same = planned.contains(&format!("{rname}/{}", f.rel).to_lowercase());
                        if !same {
                            let moved = rname.ends_with("_stream_base") && planned.iter().any(|d| d.starts_with(&format!("{rname}_").to_lowercase()) && d.ends_with(&format!("/{}", f.rel).to_lowercase()));
                            assert!(moved, "{rname}/{} is not written at the same place", f.rel);
                            moved_by_the_limit += 1;
                        }
                    }
                }
            }
        }
        let copies = plan.outputs.iter().filter(|o| matches!(o.source, Source::Copy(_)) && o.dest.contains("/stream/") && !o.dest.ends_with(".ymt")).count();
        assert_eq!(copies, stream_files, "streamed files in the pack and in the build");
        // every stream resource: at most the limit per type and sex
        let mut per: HashMap<(String, Sex, Kind), usize> = HashMap::new();
        for d in p.drawables.iter().filter(|d| !d.ydd.as_os_str().is_empty()) {
            *per.entry((crate::clothing::build::stream_resource(p, d), d.sex, d.kind)).or_default() += 1;
        }
        assert!(per.values().all(|n| *n <= p.stream_limit), "{:?}", per.iter().filter(|x| *x.1 > p.stream_limit).collect::<Vec<_>>());
        let resources_built: std::collections::BTreeSet<&str> = plan.outputs.iter().filter_map(|o| o.dest.split_once('/').map(|x| x.0)).collect();
        println!("resources: {}", resources_built.iter().copied().collect::<Vec<_>>().join(", "));
        // alternate variations and first person: the same switches and entries, over all files
        let collect = |files: Vec<String>, alt: bool| -> Vec<String> {
            let mut v: Vec<String> = files.iter().flat_map(|t| if alt { extras::read_alt_variations(t).unwrap().0.iter().map(|w| format!("{w:?}")).collect::<Vec<_>>() } else { extras::read_first_person(t).unwrap().iter().map(|e| format!("{e:?}")).collect() }).collect();
            v.sort();
            v
        };
        let originals = |pre: &str| -> Vec<String> { resources.iter().flat_map(|r| fs::read_dir(r.join("data")).into_iter().flatten().flatten()).filter(|e| e.file_name().to_string_lossy().starts_with(pre)).map(|e| fs::read_to_string(e.path()).unwrap()).collect() };
        let built = |pre: &str| -> Vec<String> { plan.outputs.iter().filter(|o| o.dest.rsplit('/').next().unwrap_or("").starts_with(pre)).filter_map(|o| if let Source::Bytes(b) = &o.source { Some(String::from_utf8(b.clone()).unwrap()) } else { None }).collect() };
        assert_eq!(collect(built("pedalternatevariations"), true), collect(originals("pedalternatevariations"), true));
        assert_eq!(collect(built("first_person_alternates"), false), collect(originals("first_person_alternates"), false));
        println!("{checked} YMT and creature files the same, {stream_files} streamed files all written ({moved_by_the_limit} of Rockstar's DLCs moved to a _stream_base_NN by the limit), alternate variations and first person the same");
    }
}
