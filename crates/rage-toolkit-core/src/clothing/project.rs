//! A clothing project on disk: a small text file (`.ragetoolkitclothes`) listing the project name, its DLCs and, for every
//! drawable, its model, cloth file, textures and YMT settings. Paths inside the project's folder are stored relative to it, so the
//! folder can be moved; others stay absolute. Saving writes a temporary file and moves it over, keeping the previous file as
//! `.bak` the first time.
//!
//! Version 2 adds DLCs, drawable ids, the YMT settings (`meta ...` lines) and alternate variations (`switch ...`). Version 1
//! files still load: their pack becomes one DLC with the same name (or `name_01`, `name_02`, ... where the old build split it),
//! so nothing changes in the game.
use super::{AltSwitch, AssetRef, Carried, Dlc, Drawable, DrawableMeta, Kind, Layout, OtherFirstPerson, Place, Project, Sex, Texture, ANCHORS, COMPONENTS};
use anyhow::{bail, Context, Result};
use std::{
    fs,
    path::{Path, PathBuf},
};

const HEADER_V1: &str = "rage-toolkit-clothing 1";
const HEADER: &str = "rage-toolkit-clothing 2";

fn kind_from_name(s: &str) -> Option<Kind> {
    if let Some(a) = s.strip_prefix("p_") {
        ANCHORS.iter().position(|x| *x == a).map(|i| Kind::Prop(i as u8))
    } else {
        COMPONENTS.iter().position(|x| *x == s).map(|i| Kind::Component(i as u8))
    }
}

/// A path as the file stores it: relative to the project's folder when inside it, `-` when empty (a file the pack does not have).
fn store(p: &Path, base: &Path) -> String {
    if p.as_os_str().is_empty() {
        return "-".into();
    }
    match p.strip_prefix(base) {
        Ok(rel) => rel.to_string_lossy().replace('\\', "/"),
        Err(_) => p.to_string_lossy().to_string(),
    }
}

fn resolve(s: &str, base: &Path) -> PathBuf {
    if s == "-" {
        return PathBuf::new();
    }
    let p = PathBuf::from(s);
    if p.is_absolute() {
        p
    } else {
        base.join(p)
    }
}

fn sex_word(s: Sex) -> &'static str {
    s.label()
}

fn sex_of(s: &str) -> Option<Sex> {
    match s {
        "male" => Some(Sex::Male),
        "female" => Some(Sex::Female),
        _ => None,
    }
}

/// `-` for an empty word
fn word(s: &str) -> &str {
    if s.is_empty() {
        "-"
    } else {
        s
    }
}

fn unword(s: &str) -> String {
    if s == "-" {
        String::new()
    } else {
        s.to_string()
    }
}

/// `id:<id>` for one of ours, `<dlc>:<component>:<index>` (dlc `-` for the base game) for one elsewhere
fn ref_text(r: &AssetRef) -> String {
    match r {
        AssetRef::Ours(id) => format!("id:{id}"),
        AssetRef::Other { dlc, component, index } => format!("{}:{component}:{index}", word(dlc)),
    }
}

fn ref_parse(s: &str) -> Option<AssetRef> {
    if let Some(id) = s.strip_prefix("id:") {
        return id.parse().ok().map(AssetRef::Ours);
    }
    let mut it = s.rsplitn(3, ':');
    let index = it.next()?.parse().ok()?;
    let component = it.next()?.parse().ok()?;
    Some(AssetRef::Other { dlc: unword(it.next()?), component, index })
}

fn meta_lines(m: &DrawableMeta) -> String {
    let d = DrawableMeta::default();
    let mut s = String::new();
    if let Some(v) = m.prop_mask {
        s += &format!("  meta propmask {v}\n");
    }
    if m.flags != d.flags {
        s += &format!("  meta flags {}\n", m.flags);
    }
    if let Some(a) = &m.audio {
        s += &format!("  meta audio {}\n", word(a));
    }
    if let Some(a) = &m.audio2 {
        s += &format!("  meta audio2 {}\n", word(a));
    }
    if m.expression_mods != d.expression_mods {
        // the shortest text that reads back as the same f32
        s += &format!("  meta expression {}\n", m.expression_mods.iter().map(|f| f.to_string()).collect::<Vec<_>>().join(" "));
    }
    if m.flags2 != 0 {
        s += &format!("  meta flags2 {}\n", m.flags2);
    }
    if m.render_flags != 0 {
        s += &format!("  meta render {}\n", m.render_flags);
    }
    if m.stickyness != 0 {
        s += &format!("  meta stickyness {}\n", m.stickyness);
    }
    if m.hides_hair {
        s += "  meta hideshair\n";
    }
    if let Some(n) = m.first_person {
        s += &format!("  meta firstperson {n}\n");
    }
    s
}

pub fn to_text(project: &Project, file: &Path) -> String {
    let base = file.parent().unwrap_or(Path::new(""));
    let mut s = format!("{HEADER}\nname {}\nlimit {}\nstream-limit {}\n{}", project.name, project.limit, project.stream_limit, if project.sorted { "folders sorted\n" } else { "" });
    if project.layout == Layout::ControlAndStreams {
        s += "layout control-streams\n";
    }
    for (k, d) in project.dlcs.iter().enumerate() {
        s += &format!("dlc {k}");
        if let Some((m, f)) = &d.names {
            s += &format!(" names {m} {f}");
        }
        if let Some(c) = &d.creature {
            s += &format!(" creature {c}");
        }
        if d.base {
            s += " base";
        }
        if d.left_out {
            s += " left-out";
        }
        if d.game_creature.is_some() {
            s += " game-creature";
        }
        s += "\n";
        let hex = |b: &[u8]| if b.is_empty() { "-".to_string() } else { b.iter().map(|x| format!("{x:02x}")).collect() };
        for e in d.game_creature.iter().flatten() {
            s += &format!("creature-entry {k} {} {} {} {} {} {} {} {}\n", if e.prop { "prop" } else { "comp" }, e.slot, e.var, e.index, hex(&e.tracks), hex(&e.ids), hex(&e.types), hex(&e.components));
        }
    }
    for (what, d) in project.drawables.iter().map(|d| ("drawable", d)).chain(project.archived.iter().map(|d| ("removed", d))) {
        s += &format!("{what} {} {} {} {} {} {}\n", sex_word(d.sex), d.kind.name(), d.number, d.source_number, d.dlc, d.id);
        s += &format!("  ydd {}\n", store(&d.ydd, base));
        s += &format!("  race {}\n", word(&d.race));
        if let Some(c) = &d.cloth {
            s += &format!("  cloth {}\n", store(c, base));
        }
        for (n, a) in &d.alternates {
            s += &format!("  alternate {n} {}\n", store(a, base));
        }
        for t in &d.textures {
            s += &format!("  texture {} {} {}\n", t.letter, word(&t.race), store(&t.path, base));
            if let Some(id) = t.tex_id {
                s += &format!("  texid {} {id}\n", t.letter);
            }
            if let Some(e) = &t.edit {
                s += &format!("  texedit {} {} {}\n", t.letter, e.max.map_or("-".to_string(), |m| m.to_string()), word(e.format.as_deref().unwrap_or("")));
            }
        }
        s += &meta_lines(&d.meta);
        for (name, e) in &d.model_edits {
            s += &format!("  modeledit {} {} {name}\n", e.max.map_or("-".to_string(), |m| m.to_string()), word(e.format.as_deref().unwrap_or("")));
        }
    }
    for w in &project.alt_switches {
        s += &format!("switch {} {} {} {} {}\n", sex_word(w.sex), w.file, w.alt, ref_text(&w.target), w.sources.iter().map(ref_text).collect::<Vec<_>>().join(" "));
    }
    for o in &project.other_first_person {
        s += &format!("firstperson-other {} {} {}\n", o.file, o.alt, o.asset);
    }
    for c in &project.carried {
        let place = match &c.place {
            Place::Control => "control".to_string(),
            Place::BaseStream => "base".to_string(),
            Place::Stream(suffix) => format!("resource:{suffix}"),
        };
        // tabs between the paths: names of carried files can have spaces
        s += &format!("carry {place} {}\t{}\t{}\n", word(c.data_file.as_deref().unwrap_or("")), c.dest, store(&c.source, base));
    }
    s
}

pub fn from_text(text: &str, file: &Path) -> Result<Project> {
    let base = file.parent().unwrap_or(Path::new(""));
    let mut lines = text.lines().map(|l| l.trim_end_matches('\r'));
    let v1 = match lines.next().map(str::trim) {
        Some(HEADER) => false,
        Some(HEADER_V1) => true,
        _ => bail!("not a CRCK Explorer clothing project (the first line must be '{HEADER}')"),
    };
    let mut p = Project { name: String::new(), limit: 128, dlcs: vec![], ..Default::default() };
    let mut cur: Option<Drawable> = None;
    // the block being read is an archived (removed) drawable
    let mut removed = false;
    let push = |p: &mut Project, d: Drawable, removed: bool| if removed { p.archived.push(d) } else { p.drawables.push(d) };
    for (n, line) in lines.enumerate() {
        let at = || format!("line {}", n + 2);
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        let (word, rest) = t.split_once(' ').unwrap_or((t, ""));
        let rest = rest.trim();
        match word {
            "name" => p.name = rest.to_string(),
            "folders" => p.sorted = rest == "sorted",
            // texture settings are asked for per build and not kept: a line from an older save is passed over, so loading never
            // turns shrinking on
            "optimize" => {}
            "layout" => {
                p.layout = match rest {
                    "control-streams" => Layout::ControlAndStreams,
                    "single" => Layout::Single,
                    other => bail!("{}: unknown layout '{other}' (control-streams or single)", at()),
                }
            }
            "limit" => p.limit = rest.parse().with_context(|| format!("{}: the limit is not a number", at()))?,
            "stream-limit" => p.stream_limit = rest.parse().with_context(|| format!("{}: the stream limit is not a number", at()))?,
            "dlc" => {
                let f: Vec<&str> = rest.split_whitespace().collect();
                let k: usize = f.first().and_then(|x| x.parse().ok()).with_context(|| format!("{}: expected 'dlc <number> ...'", at()))?;
                if k != p.dlcs.len() {
                    bail!("{}: DLC {k} is out of order", at());
                }
                let mut d = Dlc::default();
                let mut i = 1;
                while i < f.len() {
                    match f[i] {
                        "names" if i + 2 < f.len() => {
                            d.names = Some((f[i + 1].to_string(), f[i + 2].to_string()));
                            i += 3;
                        }
                        "creature" if i + 1 < f.len() => {
                            d.creature = Some(f[i + 1].to_string());
                            i += 2;
                        }
                        "base" => {
                            d.base = true;
                            i += 1;
                        }
                        "left-out" => {
                            d.left_out = true;
                            i += 1;
                        }
                        "game-creature" => {
                            d.game_creature = Some(vec![]);
                            i += 1;
                        }
                        other => bail!("{}: unknown DLC setting '{other}'", at()),
                    }
                }
                p.dlcs.push(d);
            }
            "creature-entry" => {
                let f: Vec<&str> = rest.split_whitespace().collect();
                let bytes = |s: &str| -> Option<Vec<u8>> { if s == "-" { Some(vec![]) } else { (0..s.len()).step_by(2).map(|i| s.get(i..i + 2).and_then(|h| u8::from_str_radix(h, 16).ok())).collect() } };
                let num = |s: &str| s.parse::<u32>().ok();
                let e = match f[..] {
                    [k, kind @ ("prop" | "comp"), slot, var, index, tracks, ids, types, comps] => k.parse::<usize>().ok().zip((|| Some(super::extras::Expression { prop: kind == "prop", slot: num(slot)?, var: num(var)?, index: num(index)?, tracks: bytes(tracks)?, ids: bytes(ids)?, types: bytes(types)?, components: bytes(comps)? }))()),
                    _ => None,
                };
                let (k, e) = e.with_context(|| format!("{}: expected 'creature-entry <dlc> <prop|comp> <slot> <var> <index> <tracks> <ids> <types> <components>'", at()))?;
                p.dlcs.get_mut(k).and_then(|d| d.game_creature.as_mut()).with_context(|| format!("{}: DLC {k} has no game creature metadata", at()))?.push(e);
            }
            "drawable" | "removed" => {
                if let Some(d) = cur.take() {
                    push(&mut p, d, removed);
                }
                removed = word == "removed";
                let f: Vec<&str> = rest.split_whitespace().collect();
                let (sex, kind, number, source, dlc, id) = match f[..] {
                    [sex, kind, number, source] if v1 => (sex, kind, number, source, "0", None),
                    [sex, kind, number, source, dlc, id] if !v1 => (sex, kind, number, source, dlc, Some(id)),
                    _ => bail!("{}: expected 'drawable <sex> <kind> <number> <source number>{}'", at(), if v1 { "" } else { " <dlc> <id>" }),
                };
                let mut d = Drawable {
                    sex: sex_of(sex).with_context(|| format!("{}: the sex is male or female", at()))?,
                    kind: kind_from_name(kind).with_context(|| format!("{}: '{kind}' is not a component or prop", at()))?,
                    number: number.parse().with_context(|| format!("{}: bad number", at()))?,
                    source_number: source.parse().with_context(|| format!("{}: bad source number", at()))?,
                    dlc: dlc.parse().with_context(|| format!("{}: bad DLC number", at()))?,
                    ..Default::default()
                };
                if let Some(id) = id {
                    d.id = id.parse().with_context(|| format!("{}: bad id", at()))?;
                }
                cur = Some(d);
            }
            "ydd" | "race" | "cloth" | "texture" | "texid" | "texedit" | "modeledit" | "alternate" | "meta" => {
                let d = cur.as_mut().with_context(|| format!("{}: '{word}' before any drawable", at()))?;
                match word {
                    "ydd" => d.ydd = resolve(rest, base),
                    "race" => d.race = unword(rest),
                    "cloth" => d.cloth = Some(resolve(rest, base)),
                    "alternate" => {
                        let (n, path) = rest.split_once(' ').with_context(|| format!("{}: expected 'alternate <number> <path>'", at()))?;
                        d.alternates.push((n.parse().with_context(|| format!("{}: bad alternate number", at()))?, resolve(path.trim(), base)));
                    }
                    "texid" => {
                        let (letter, id) = rest.split_once(' ').with_context(|| format!("{}: expected 'texid <letter> <id>'", at()))?;
                        let id: u8 = id.trim().parse().with_context(|| format!("{}: bad texture id", at()))?;
                        let t = d.textures.iter_mut().rev().find(|t| t.letter.to_string() == letter).with_context(|| format!("{}: no texture {letter} before it", at()))?;
                        t.tex_id = Some(id);
                    }
                    "meta" => read_meta(&mut d.meta, rest).with_context(at)?,
                    "texedit" | "modeledit" => {
                        // texedit <letter> <max|-> <format|->   /   modeledit <max|-> <format|-> <texture name>
                        let mut it = rest.splitn(3, ' ');
                        let (a, b, c) = (it.next().unwrap_or(""), it.next().unwrap_or(""), it.next().unwrap_or("").trim());
                        let (target, max, format) = if word == "texedit" { (a, b, c) } else { (c, a, b) };
                        let bad = || format!("{}: expected '{word} ...' with a size or -, a format or -", at());
                        let e = crate::clothing::TexEdit { max: if max == "-" { None } else { Some(max.parse().with_context(bad)?) }, format: Some(unword(format)).filter(|f| !f.is_empty()) };
                        if word == "texedit" {
                            let t = d.textures.iter_mut().rev().find(|t| t.letter.to_string() == target).with_context(|| format!("{}: no texture {target} before it", at()))?;
                            t.edit = Some(e);
                        } else {
                            d.model_edits.push((target.to_string(), e));
                        }
                    }
                    _ => {
                        let mut it = rest.splitn(3, ' ');
                        let (letter, race, path) = (it.next().unwrap_or(""), it.next().unwrap_or(""), it.next().unwrap_or(""));
                        let letter = letter.chars().next().filter(|c| c.is_ascii_lowercase()).with_context(|| format!("{}: the texture letter is a..z", at()))?;
                        d.textures.push(Texture { letter, race: unword(race), path: resolve(path.trim(), base), tex_id: None, edit: None });
                    }
                }
            }
            "switch" => {
                let f: Vec<&str> = rest.split_whitespace().collect();
                let bad = || format!("{}: expected 'switch <sex> <dlc file> <alt> <target> <sources...>'", at());
                if f.len() < 4 {
                    bail!(bad());
                }
                p.alt_switches.push(AltSwitch {
                    sex: sex_of(f[0]).with_context(bad)?,
                    file: f[1].parse().with_context(bad)?,
                    alt: f[2].parse().with_context(bad)?,
                    target: ref_parse(f[3]).with_context(bad)?,
                    sources: f[4..].iter().map(|s| ref_parse(s)).collect::<Option<Vec<_>>>().with_context(bad)?,
                });
            }
            "carry" => {
                let bad = || format!("{}: expected 'carry <control|base|resource:<name>> <data file type|-><tab><dest><tab><source>'", at());
                let mut it = rest.split('\t');
                let (head, dest, source) = (it.next().unwrap_or(""), it.next().unwrap_or(""), it.next().unwrap_or(""));
                let (place, kind) = head.trim().split_once(' ').unwrap_or((head.trim(), "-"));
                let place = match place {
                    "control" => Place::Control,
                    "base" => Place::BaseStream,
                    other => match other.strip_prefix("resource:") {
                        Some(suffix) if !suffix.is_empty() => Place::Stream(suffix.to_string()),
                        _ => bail!(bad()),
                    },
                };
                if dest.is_empty() || source.is_empty() {
                    bail!(bad());
                }
                p.carried.push(Carried { place, dest: dest.to_string(), source: resolve(source, base), data_file: Some(unword(kind)).filter(|k| !k.is_empty()) });
            }
            "firstperson-other" => {
                let mut it = rest.splitn(3, ' ');
                let bad = || format!("{}: expected 'firstperson-other <dlc file> <alt> <asset>'", at());
                let (file, alt, asset) = (it.next().unwrap_or(""), it.next().unwrap_or(""), it.next().unwrap_or("").trim());
                if asset.is_empty() {
                    bail!(bad());
                }
                p.other_first_person.push(OtherFirstPerson { file: file.parse().with_context(bad)?, alt: alt.parse().with_context(bad)?, asset: asset.to_string() });
            }
            other => bail!("{}: unknown line '{other}'", at()),
        }
    }
    if let Some(d) = cur.take() {
        push(&mut p, d, removed);
    }
    if v1 {
        // the old build named the addon after the project, or split it into name_01, name_02, ... at the limit
        let limit = p.limit.max(1) as u32;
        let chunks = p.drawables.iter().map(|d| d.number / limit + 1).max().unwrap_or(1) as usize;
        p.dlcs = (0..chunks).map(|k| {
            let n = if chunks == 1 { p.name.clone() } else { format!("{}_{:02}", p.name, k + 1) };
            Dlc { names: Some((n.clone(), n)), ..Default::default() }
        }).collect();
        if chunks > 1 {
            for d in &mut p.drawables {
                d.dlc = (d.number / limit) as usize;
                d.number %= limit;
            }
        }
    }
    if p.dlcs.is_empty() {
        p.dlcs.push(Dlc::default());
    }
    Ok(p)
}

fn read_meta(m: &mut DrawableMeta, rest: &str) -> Result<()> {
    let (key, value) = rest.split_once(' ').unwrap_or((rest, ""));
    let value = value.trim();
    let num = || -> Result<u32> { value.parse().with_context(|| format!("meta {key}: '{value}' is not a number")) };
    match key {
        "propmask" => m.prop_mask = Some(num()? as u8),
        "flags" => m.flags = num()?,
        "audio" => m.audio = Some(unword(value)),
        "audio2" => m.audio2 = Some(unword(value)),
        "expression" => {
            let v: Vec<f32> = value.split_whitespace().map(|x| x.parse()).collect::<std::result::Result<_, _>>().context("meta expression: five numbers")?;
            m.expression_mods = v.try_into().map_err(|_| anyhow::anyhow!("meta expression: five numbers"))?;
        }
        "flags2" => m.flags2 = num()?,
        "render" => m.render_flags = num()?,
        "stickyness" => m.stickyness = num()? as u8,
        "hideshair" => m.hides_hair = true,
        "firstperson" => m.first_person = Some(num()?),
        other => bail!("unknown meta setting '{other}'"),
    }
    Ok(())
}

/// Write the project; the first overwrite of a file keeps the old one as `<file>.bak`.
pub fn save(project: &Project, file: &Path) -> Result<()> {
    if file.exists() {
        let bak = PathBuf::from(format!("{}.bak", file.display()));
        if !bak.exists() {
            fs::copy(file, &bak).with_context(|| format!("keeping a backup of {}", file.display()))?;
        }
    }
    let tmp = PathBuf::from(format!("{}.rage-toolkit-tmp", file.display()));
    fs::write(&tmp, to_text(project, file))?;
    fs::rename(&tmp, file).with_context(|| format!("saving {}", file.display()))?;
    Ok(())
}

pub fn load(file: &Path) -> Result<Project> {
    let text = fs::read_to_string(file).with_context(|| format!("reading {}", file.display()))?;
    from_text(&text, file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_project_survives_saving_and_loading_and_paths_stay_relative() {
        let d = std::env::temp_dir().join(format!("rage_toolkit_cloth_proj_{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("src")).unwrap();
        let file = d.join("pack.ragetoolkitclothes");
        let heels = DrawableMeta { prop_mask: Some(11), expression_mods: [0.0, 0.0, 0.0, 0.0, 0.9], first_person: Some(1), audio: Some("jbib_leather".into()), audio2: Some(String::new()), flags2: 2, ..Default::default() };
        let hat = DrawableMeta { expression_mods: [-0.34999996, 0.0, 0.0, 0.0, 0.0], render_flags: 1, hides_hair: true, ..Default::default() };
        let jacket = Drawable { sex: Sex::Male, kind: Kind::Component(11), number: 0, source_number: 7, ydd: d.join("src").join("jbib 007_u.ydd"), race: "u".into(), cloth: Some(d.join("src/jbib_007_u.yld")), alternates: vec![(1, d.join("src/jbib_007_u_1.ydd")), (2, PathBuf::new())], textures: vec![Texture { path: d.join("src/a b.ytd"), letter: 'a', race: "uni".into(), tex_id: Some(3), edit: None }, Texture { path: PathBuf::from("C:\\elsewhere\\t.ytd"), letter: 'b', race: String::new(), tex_id: None, edit: None }], meta: heels, ..Default::default() };
        let p = Project {
            name: "mypack".into(),
            limit: 256,
            dlcs: vec![Dlc::default(), Dlc { names: Some(("mp_m_gtawclothes3".into(), "mp_f_gtawclothes3".into())), creature: Some("MP_CreatureMetadata_gtawclothes3".into()), base: false, left_out: false, game_creature: None }, Dlc { names: Some(("mp_m_2023_02".into(), "mp_f_2023_02".into())), creature: None, base: true, left_out: true, game_creature: Some(vec![crate::clothing::extras::Expression::hat(3), crate::clothing::extras::Expression { prop: false, slot: 4, var: 2, index: 1, tracks: vec![], ids: vec![1, 2], types: vec![2], components: vec![1] }]) }],
            carried: vec![Carried { place: Place::BaseStream, dest: "stream/mp_f_freemode_01_mp_f_heist3/mp_f_freemode_01_mp_f_heist3^lowr_diff_000_a_uni.ytd".into(), source: d.join("src/x y.ytd"), data_file: None }, Carried { place: Place::Stream("stream_f1".into()), dest: "stream/x/y z .ytd".into(), source: d.join("y z .ytd"), data_file: None }, Carried { place: Place::Control, dest: "data/other.meta".into(), source: PathBuf::from(r"C:\elsewhere\o.meta"), data_file: Some("PED_METADATA_FILE".into()) }],
            other_first_person: vec![OtherFirstPerson { file: 1, asset: "MP_F_Freemode_01_mp_m_2023_02/jbib_009_u".into(), alt: 1 }],
            layout: Layout::ControlAndStreams,
            alt_switches: vec![AltSwitch { file: 2, sex: Sex::Male, target: AssetRef::Other { dlc: "mp_m_Heist3".into(), component: 5, index: 0 }, alt: 19, sources: vec![AssetRef::Ours(jacket.id), AssetRef::Other { dlc: String::new(), component: 11, index: 4 }] }],
            drawables: vec![jacket, Drawable { dlc: 1, sex: Sex::Female, kind: Kind::Prop(0), number: 3, source_number: 3, ydd: d.join("p_head_003.ydd"), meta: hat, ..Default::default() }],
            ..Default::default()
        };
        save(&p, &file).unwrap();
        let text = fs::read_to_string(&file).unwrap();
        assert!(text.contains("ydd src/jbib 007_u.ydd") && text.contains("C:\\elsewhere\\t.ytd"), "{text}");
        assert_eq!(load(&file).unwrap(), p);
        // the second save keeps the first as .bak, once
        save(&p, &file).unwrap();
        assert!(PathBuf::from(format!("{}.bak", file.display())).is_file());
        assert!(from_text("nonsense", &file).is_err());
        assert!(from_text("rage-toolkit-clothing 2\ndrawable alien jbib 0 0 0 1\n", &file).is_err());
        assert!(from_text("rage-toolkit-clothing 2\n  ydd x\n", &file).is_err());
        // texture shrinking is never saved, and an older file that has it does not turn it on
        let shrinking = Project { optimize: crate::clothing::TextureOptimize { ytd_max: Some(1024), ydd_max: Some(512), ydd_formats: "ati2".into() }, ..p.clone() };
        save(&shrinking, &file).unwrap();
        assert!(!load(&file).unwrap().optimize.is_on());
        let old = format!("{}optimize ytd 1024\noptimize ydd 512 ati2\n", to_text(&p, &file));
        assert!(!from_text(&old, &file).unwrap().optimize.is_on());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_version_1_project_keeps_the_dlc_names_its_old_build_used() {
        let file = Path::new("C:\\p\\x.ragetoolkitclothes");
        let one = from_text("rage-toolkit-clothing 1\nname mypack\nlimit 128\ndrawable male jbib 0 0\n  ydd a.ydd\n", file).unwrap();
        assert_eq!((one.dlc_name(0, Sex::Male), one.dlcs.len()), ("mypack".to_string(), 1));
        // the old build split at the limit into name_01, name_02
        let split = from_text("rage-toolkit-clothing 1\nname big\nlimit 3\ndrawable male uppr 1 1\n  ydd a.ydd\ndrawable male uppr 4 4\n  ydd b.ydd\n", file).unwrap();
        assert_eq!(split.dlcs.len(), 2);
        assert_eq!((split.dlc_name(1, Sex::Female), split.drawables[1].dlc, split.drawables[1].number), ("big_02".to_string(), 1, 1));
    }
}
