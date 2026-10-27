//! Siren settings (`<Sirens>` in carcols.meta, `sirenSettings` in carvariations.meta): validator and the calculators from
//! the modding-forum guides. Severity as in `validate`: E = game will likely fail, W = likely a mistake, I = worth knowing.
//!
//! Every rule is either **verified** against the vanilla data of the user's own install (checked 2026-10-26: the 14 base
//! settings, ids 0-13, decoded from `update/x64/data/carcols.ymt`, plus the 7 DLC settings, ids 14-20, in the DLC
//! `carcols.meta` files; 21 settings, 420 siren items, 58 vanilla carvariations.meta) or its message says "unverified".
//! What the vanilla data showed, rule by rule:
//!   - 20 `<sirens>` items per setting: all 21 settings have exactly 20. Verified.
//!   - setting ids unique per file, ids 0-20 (0 = "0_unused", the "no lights" setting every plain car uses). Verified.
//!   - sequencers (rotation, flashiness, head/tail lights) are plain u32 in 0..=4294967295. Verified.
//!   - colour is `0xAARRGGBB`, 8 hex digits (alpha FF on 419 of 420 items). Verified.
//!   - rotation/flashiness `delta` and `start` are radians: max |delta| 5.82, max |start| 5.76. Verified. `speed` is NOT an
//!     angle (vanilla goes up to 60), so it is never range-checked.
//!   - "corona intensity and size > 0 when lit": true for all 280 base items but NOT for the 7 newer DLC settings (52 of 140
//!     items have a real light and no corona sprite; 27 have neither light nor corona; 31 have intensity 0 with size > 0).
//!     No item has intensity > 0 with size 0, or a negative value. So "no corona" is only info, and the guide's rule is dropped.
//!   - `syncToBpm=false` with speed 0 = steady light: used by vanilla (13 lit flashiness items: foglights, police van). Verified.
//!   - a pack's carvariations may name a siren id its own carcols.meta does not define: 3 of the 9 vanilla DLC packs do.
//!     Verified, hence info only.
//! The calculators implement the guides' formulas. Both are unverified in game; the start-value formula is also not what
//! Rockstar's own data follows (see [`start_value`]).
use crate::validate::{Issue, Report};
use anyhow::{anyhow, bail, Result};
use roxmltree::{Document, Node};
use std::{
    collections::{BTreeMap, HashSet},
    fs,
    path::{Path, PathBuf},
};

/// Siren setting ids the game ships, as (id, name): carcols.ymt 0-13, DLC carcols.meta 14-20 (install of 2026-10-26).
pub const VANILLA: [(u32, &str); 21] = [
    (0, "0_unused"),
    (1, "police/sheriff"),
    (2, "police3"),
    (3, "fbiold"),
    (4, "ambulance"),
    (5, "police van"),
    (6, "policebike"),
    (7, "firetruck"),
    (8, "police old"),
    (9, "orange_globes"),
    (10, "orange_lightbar"),
    (11, "foglights"),
    (12, "fbi"),
    (13, "police dual lightbar"),
    (14, "riot2"),
    (15, "pbus2"),
    (16, "STD Lightbar"),
    (17, "CNC Rotators"),
    (18, "2024_01 Sirens"),
    (19, "2024_01 Sirens2"),
    (20, "PolBati Lightbar"),
];

/// items per `<sirens>` list in every vanilla setting; the SirenSetting Limit Adjuster mod raises the engine limit to 32
const SIRENS_PER_SETTING: usize = 20;
/// radians sanity bound for delta/start (vanilla stays below 2*pi)
const RADIANS_MAX: f64 = 7.0;

// ---- XML helpers -----------------------------------------------------------------------------------------------

fn child<'a, 'b>(n: Node<'a, 'b>, name: &str) -> Option<Node<'a, 'b>> {
    n.children().find(|c| c.is_element() && c.tag_name().name() == name)
}

fn items<'a, 'b>(n: Option<Node<'a, 'b>>) -> Vec<Node<'a, 'b>> {
    n.map(|c| c.children().filter(|x| x.is_element() && x.tag_name().name() == "Item").collect()).unwrap_or_default()
}

/// `<name value="..."/>` of a child
fn attr<'a>(n: Node<'a, '_>, name: &str) -> Option<&'a str> {
    child(n, name).and_then(|c| c.attribute("value")).map(str::trim)
}

fn text(n: Option<Node>) -> String {
    n.map(|n| n.descendants().filter(|d| d.is_text()).map(|d| d.text().unwrap_or("")).collect::<String>().trim().to_string()).unwrap_or_default()
}

fn line(doc: &Document, n: Node) -> String {
    format!(" (line {})", doc.text_pos_at(n.range().start).row)
}

/// Some(Ok(v)) number, Some(Err(text)) present but not a finite number, None absent
fn num(n: Node, name: &str) -> Option<Result<f64, String>> {
    attr(n, name).map(|t| t.parse::<f64>().ok().filter(|v| v.is_finite()).ok_or_else(|| t.to_string()))
}

fn is_true(n: Node, name: &str) -> bool {
    attr(n, name).map(|t| t.eq_ignore_ascii_case("true")).unwrap_or(false)
}

fn push(out: &mut Vec<Issue>, sev: char, code: &'static str, file: &str, msg: String, fix: &str) {
    out.push(Issue { sev, code, file: file.into(), msg, fix: fix.into() });
}

fn list(ns: &[usize]) -> String {
    ns.iter().map(|n| n.to_string()).collect::<Vec<_>>().join(", ")
}

fn vanilla_name(id: i64) -> Option<&'static str> {
    VANILLA.iter().find(|(v, _)| *v as i64 == id).map(|(_, n)| *n)
}

// ---- validator -------------------------------------------------------------------------------------------------

struct Defined {
    id: i64,
    file: String,
}

/// One carcols.meta. Returns the setting ids it defines.
fn check_carcols(file: &str, doc: &Document, out: &mut Vec<Issue>) -> Vec<Defined> {
    let root = doc.root_element();
    if root.tag_name().name() != "CVehicleModelInfoVarGlobal" {
        push(out, 'E', "siren.root", file, format!("root element is <{}>, expected <CVehicleModelInfoVarGlobal>", root.tag_name().name()), "wrong template or wrong file type");
        return vec![];
    }
    let mut defined: Vec<Defined> = vec![];
    let mut seen: Vec<(i64, usize)> = vec![];
    for s in items(child(root, "Sirens")) {
        let at = line(doc, s);
        let name = text(child(s, "name"));
        let id = match attr(s, "id").map(|t| t.parse::<i64>()) {
            Some(Ok(id)) if id >= 0 => id,
            _ => {
                push(out, 'E', "siren.id", file, format!("siren setting '{name}' without a numeric id >= 0 in <id value=\"...\"/>{at}"), "give every setting a unique whole number id, e.g. <id value=\"100\" />");
                continue;
            }
        };
        let label = format!("setting {id} '{name}'");
        seen.push((id, doc.text_pos_at(s.range().start).row as usize));
        defined.push(Defined { id, file: file.into() });
        if id > 255 {
            push(out, 'W', "siren.id-range", file, format!("{label}: id above 255 (unverified: the guides say the game stops at 255 unless the SirenSetting Limit Adjuster mod is installed){at}"), "use an id up to 255, or make sure players run the Limit Adjuster");
        }
        match vanilla_name(id) {
            Some(v) if !v.eq_ignore_ascii_case(&name) => push(out, 'W', "siren.id-vanilla", file, format!("{label}: id {id} belongs to the game's own setting '{v}' (unverified: defining it again conflicts with, or replaces, the vanilla one){at}"), "pick an id above 20 that no other add-on uses"),
            _ => {}
        }
        check_setting(file, doc, s, &label, &at, out);
    }
    // unique per file (verified: vanilla has no duplicate)
    let mut done: HashSet<i64> = HashSet::new();
    for (id, _) in &seen {
        let rows: Vec<String> = seen.iter().filter(|(i, _)| i == id).map(|(_, l)| l.to_string()).collect();
        if rows.len() > 1 && done.insert(*id) {
            push(out, 'E', "siren.id-dup", file, format!("siren setting id {id} used {} times (lines {})", rows.len(), rows.join(", ")), "every setting needs its own id; only one of the duplicates takes effect");
        }
    }
    defined
}

/// everything inside one `<Item>` of `<Sirens>`
fn check_setting(file: &str, doc: &Document, s: Node, label: &str, at: &str, out: &mut Vec<Issue>) {
    let sirens = items(child(s, "sirens"));
    match sirens.len() {
        SIRENS_PER_SETTING => {}
        0 => push(out, 'W', "siren.count", file, format!("{label}: no <sirens> items, the setting lights nothing (vanilla settings have {SIRENS_PER_SETTING}){at}"), "add the siren items, one per siren bone (siren1..siren20)"),
        n if n < SIRENS_PER_SETTING => push(out, 'I', "siren.count", file, format!("{label}: {n} <sirens> items; every vanilla setting has {SIRENS_PER_SETTING} (verified). Fine if the model has no siren bones above siren{n} (unverified){at}"), "pad to 20 items if unsure"),
        n => push(out, 'W', "siren.count", file, format!("{label}: {n} <sirens> items; vanilla has {SIRENS_PER_SETTING} each and the guides say 20 is the engine limit (32 with the SirenSetting Limit Adjuster, unverified){at}"), "trim to 20 items, or make sure players run the Limit Adjuster"),
    }
    match num(s, "sequencerBpm") {
        Some(Ok(b)) if b > 0.0 => {}
        _ => push(out, 'W', "siren.bpm", file, format!("{label}: sequencerBpm missing or not above 0 (vanilla uses 90-300; unverified what the game does with 0){at}"), "set <sequencerBpm value=\"220\" />"),
    }
    for ln in ["leftHeadLight", "rightHeadLight", "leftTailLight", "rightTailLight"] {
        if let Some(sq) = child(s, ln).and_then(|c| child(c, "sequencer")) {
            seq_check(file, label, ln, sq, doc, out);
        }
    }
    let (mut steady, mut dark) = (vec![], vec![]);
    for (i, it) in sirens.iter().enumerate() {
        let n = i + 1;
        let tag = format!("{label} siren{n}");
        let at = line(doc, *it);
        for part in ["rotation", "flashiness"] {
            let Some(m) = child(*it, part) else { continue };
            for k in ["delta", "start", "speed"] {
                match num(m, k) {
                    Some(Err(t)) => push(out, 'E', "siren.number", file, format!("{tag}: {part} {k} '{t}' is not a number{}", line(doc, m)), "use a plain decimal such as 1.570796"),
                    Some(Ok(v)) if k != "speed" && v.abs() > RADIANS_MAX => push(
                        out,
                        'W',
                        "siren.radians",
                        file,
                        format!("{tag}: {part} {k} {v} looks like degrees, but carcols.meta stores radians (vanilla never exceeds 5.9){}", line(doc, m)),
                        &format!("crck-explorer siren calc deg2rad {v}   (= {})", fmt_rad(deg2rad(v))),
                    ),
                    _ => {}
                }
            }
            if let Some(sq) = child(m, "sequencer") {
                seq_check(file, &tag, &format!("{part} sequencer"), sq, doc, out);
            }
        }
        if let Some(c) = attr(*it, "color") {
            let ok = (c.starts_with("0x") || c.starts_with("0X")) && c.len() == 10 && c[2..].chars().all(|h| h.is_ascii_hexdigit());
            if !ok {
                push(out, 'W', "siren.color", file, format!("{tag}: color '{c}' is not 0xAARRGGBB with 8 hex digits (vanilla always writes e.g. 0xFFFF0300){at}"), "write all 8 digits; alpha FF = opaque, then red, green, blue");
            }
        }
        let (ci, cs) = match child(*it, "corona") {
            Some(c) => (num(c, "intensity").and_then(Result::ok).unwrap_or(0.0), num(c, "size").and_then(Result::ok).unwrap_or(0.0)),
            None => (0.0, 0.0),
        };
        if ci < 0.0 || cs < 0.0 || (ci > 0.0 && cs <= 0.0) {
            push(out, 'W', "siren.corona", file, format!("{tag}: corona intensity {ci} / size {cs} (unverified: no vanilla item has a negative value, or intensity above 0 with size 0; that corona would be invisible){at}"), "give the corona a size above 0, or set its intensity to 0");
        }
        let lit = is_true(*it, "light") || (ci > 0.0 && cs > 0.0);
        if !lit {
            dark.push(n);
        } else if let Some(f) = child(*it, "flashiness") {
            let still = !is_true(f, "syncToBpm") && matches!(num(f, "speed"), Some(Ok(v)) if v == 0.0) && attr(f, "sequencer").map(|t| t != "0").unwrap_or(false);
            if still {
                steady.push(n);
            }
        }
    }
    if !steady.is_empty() {
        push(out, 'I', "siren.steady", file, format!("{label}: siren {} are steady (flashiness syncToBpm=false with speed 0, so the pattern never advances); vanilla does this for fog and work lights (verified){at}", list(&steady)), "intended? For a flashing light set syncToBpm=true, or a speed above 0 (crck-explorer siren calc speed)");
    }
    if !dark.is_empty() {
        push(out, 'I', "siren.dark", file, format!("{label}: siren {} have no real light and no visible corona (light=false, corona intensity or size 0); only the siren's emissive model part, if it has one, lights up. The newer vanilla DLC settings do this too (verified){at}", list(&dark)), "fine for rotators/emissive-only sirens and unused slots; otherwise set light=true or give the corona intensity and size above 0");
    }
}

/// sequencers are u32 in the file format; negative numbers (-1 for "all on"), floats and 33+ bit values are mistakes
fn seq_check(file: &str, tag: &str, what: &str, n: Node, doc: &Document, out: &mut Vec<Issue>) {
    let Some(t) = n.attribute("value").map(str::trim) else { return };
    if t.parse::<u32>().is_err() {
        push(out, 'E', "siren.sequencer", file, format!("{tag}: {what} '{t}' is not a whole number in 0..4294967295 (32 bits){}", line(doc, n)), "convert the pattern with: crck-explorer siren calc seq2dec <32 binary digits>; all lights on = 4294967295");
    }
}

/// one carvariations.meta: every `sirenSettings` id must resolve. Returns the ids used.
fn check_variations(file: &str, doc: &Document, defined: &[Defined], out: &mut Vec<Issue>) -> HashSet<i64> {
    let mut used = HashSet::new();
    for v in items(child(doc.root_element(), "variationData")) {
        let Some(sn) = child(v, "sirenSettings") else { continue };
        let at = line(doc, sn);
        let model = text(child(v, "modelName"));
        let id = match sn.attribute("value").map(|t| t.trim().parse::<i64>()) {
            Some(Ok(id)) if id >= 0 => id,
            _ => {
                push(out, 'E', "siren.var-bad", file, format!("{model}: sirenSettings is not a whole number >= 0{at}"), "use <sirenSettings value=\"0\" /> for no lights, or the id of a siren setting");
                continue;
            }
        };
        used.insert(id);
        if id == 0 || defined.iter().any(|d| d.id == id) {
            continue;
        }
        let who = match vanilla_name(id) {
            Some(n) => format!("vanilla setting '{n}'"),
            None => "not a vanilla id (vanilla ends at 20); another add-on must define it".to_string(),
        };
        push(out, 'I', "siren.var-external", file, format!("{model}: sirenSettings {id} is not defined in this pack's carcols.meta: {who}{at}"), "fine if that is intended; otherwise add the setting to carcols.meta or fix the id");
    }
    used
}

/// Check carcols.meta / carvariations.meta texts (label, content). Lets the GUI and tests skip the file system.
pub fn check_texts(carcols: &[(String, String)], variations: &[(String, String)]) -> Report {
    let mut out: Vec<Issue> = vec![];
    let parse = |label: &str, t: &str, out: &mut Vec<Issue>| -> Option<()> { Document::parse(t.trim_start_matches('\u{feff}')).map(|_| ()).map_err(|e| push(out, 'E', "siren.xml", label, format!("not valid XML: {e}"), "fix the XML; the game ignores or crashes on broken metas")).ok() };
    let mut defined: Vec<Defined> = vec![];
    let mut settings = 0;
    for (label, t) in carcols {
        if parse(label, t, &mut out).is_none() {
            continue;
        }
        let doc = Document::parse(t.trim_start_matches('\u{feff}')).unwrap();
        let d = check_carcols(label, &doc, &mut out);
        settings += d.len();
        defined.extend(d);
    }
    let mut by_id: BTreeMap<i64, Vec<&str>> = BTreeMap::new();
    for d in &defined {
        by_id.entry(d.id).or_default().push(&d.file);
    }
    for (id, files) in by_id {
        let distinct: HashSet<&str> = files.into_iter().collect();
        if distinct.len() > 1 {
            let mut names: Vec<&str> = distinct.into_iter().collect();
            names.sort();
            push(&mut out, 'W', "siren.id-files", names[0], format!("siren setting id {id} is defined in {} carcols.meta files ({}); only one takes effect (unverified which)", names.len(), names.join(", ")), "give each setting a pack-wide unique id");
        }
    }
    let mut used: HashSet<i64> = HashSet::new();
    for (label, t) in variations {
        if parse(label, t, &mut out).is_none() {
            continue;
        }
        let doc = Document::parse(t.trim_start_matches('\u{feff}')).unwrap();
        used.extend(check_variations(label, &doc, &defined, &mut out));
    }
    if !variations.is_empty() {
        for d in defined.iter().filter(|d| !used.contains(&d.id)) {
            push(&mut out, 'I', "siren.unused", &d.file, format!("siren setting {} is not used by any carvariations.meta in this pack", d.id), "fine if another resource or script uses it");
        }
    }
    if settings == 0 && out.iter().all(|i| i.sev != 'E') {
        push(&mut out, 'I', "siren.none", carcols.first().map(|c| c.0.as_str()).unwrap_or("carcols.meta"), "no <Sirens> settings found".into(), "siren settings live in <Sirens> of carcols.meta");
    }
    let rank = |c: char| "EWI".find(c).unwrap_or(3);
    out.sort_by(|a, b| rank(a.sev).cmp(&rank(b.sev)).then_with(|| a.file.cmp(&b.file)));
    let n = |c: char| out.iter().filter(|i| i.sev == c).count();
    let (e, w, i) = (n('E'), n('W'), n('I'));
    Report { summary: format!("{settings} siren setting(s) in {} carcols.meta, {} carvariations.meta: {e} error(s), {w} warning(s), {i} info", carcols.len(), variations.len()), errors: e, issues: out }
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    walk_at(dir, out, 0)
}

fn walk_at(dir: &Path, out: &mut Vec<PathBuf>, depth: usize) {
    let mut es: Vec<_> = fs::read_dir(dir).map(|r| r.filter_map(|e| e.ok()).collect()).unwrap_or_default();
    es.sort_by_key(|e| e.file_name());
    for e in es {
        let p = e.path();
        if p.is_dir() {
            if depth < crate::search::MAX_FOLDER_DEPTH {
                walk_at(&p, out, depth + 1);
            }
        } else {
            out.push(p);
        }
    }
}

/// `path` = a carcols.meta (its carvariations.meta next to it is checked too) or a pack folder (every carcols.meta and
/// carvariations.meta in it, any depth)
pub fn check(path: &Path) -> Result<Report> {
    let (root, files): (PathBuf, Vec<PathBuf>) = if path.is_dir() {
        let mut all = vec![];
        walk(path, &mut all);
        (path.to_path_buf(), all)
    } else if path.is_file() {
        let dir = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new(".")).to_path_buf();
        let mut all = vec![path.to_path_buf()];
        all.extend(fs::read_dir(&dir)?.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.file_name().map(|n| n.eq_ignore_ascii_case("carvariations.meta")).unwrap_or(false)));
        (dir, all)
    } else {
        bail!("{} is not a file or folder", path.display());
    };
    let read = |name: &str| -> Vec<(String, String)> {
        files
            .iter()
            .filter(|p| p.file_name().map(|n| n.eq_ignore_ascii_case(name)).unwrap_or(false))
            .map(|p| (p.strip_prefix(&root).unwrap_or(p).to_string_lossy().replace('\\', "/"), fs::read_to_string(p).unwrap_or_default()))
            .collect()
    };
    let (carcols, variations) = (read("carcols.meta"), read("carvariations.meta"));
    if carcols.is_empty() && variations.is_empty() {
        bail!("no carcols.meta or carvariations.meta in {}", path.display());
    }
    Ok(check_texts(&carcols, &variations))
}

// ---- calculators -----------------------------------------------------------------------------------------------

pub fn deg2rad(d: f64) -> f64 {
    d.to_radians()
}

pub fn rad2deg(r: f64) -> f64 {
    r.to_degrees()
}

/// radians the way the game writes them in carcols.meta: float32, 6 decimals
pub fn fmt_rad(r: f64) -> String {
    format!("{:.6}", r as f32)
}

/// Sequencer from a binary pattern, first digit = highest bit (the guides write 10101010...1010 = 2863311530). 32 digits,
/// or a shorter pattern whose length divides 32 (1, 2, 4, 8, 16), which repeats to fill 32 bits. `0b` prefix, `_` and spaces ignored.
pub fn seq2dec(bits: &str) -> Result<u32> {
    let b: String = bits.trim().trim_start_matches("0b").chars().filter(|c| !matches!(c, '_' | ' ')).collect();
    if b.is_empty() || !b.chars().all(|c| c == '0' || c == '1') {
        bail!("'{bits}' is not a binary pattern of 0 and 1");
    }
    if b.len() > 32 || 32 % b.len() != 0 {
        bail!("pattern has {} digits; give 32, or 1/2/4/8/16 digits to repeat across all 32 beats", b.len());
    }
    let full = b.repeat(32 / b.len());
    Ok(u32::from_str_radix(&full, 2)?)
}

pub fn dec2seq(n: u32) -> String {
    format!("{n:032b}")
}

/// a decimal sequencer, or a binary pattern after `0b`
pub fn parse_sequencer(s: &str) -> Result<u32> {
    let s = s.trim();
    if s.starts_with("0b") {
        seq2dec(s)
    } else {
        s.parse::<u32>().map_err(|_| anyhow!("'{s}' is not a sequencer: whole number 0..4294967295, or 0b followed by binary digits"))
    }
}

/// The guide's speed multiplier for a sequencer whose ones are evenly spaced (period p beats): 32/p, i.e. alternate
/// 1010... = 16, 1000... = 8, 10000000... = 4, 1 + 15 zeros = 2. A single set bit (p = 32, x1) is extrapolated, not in the guide.
pub fn flash_multiplier(seq: u32) -> Option<u32> {
    [2u32, 4, 8, 16, 32].into_iter().find(|p| (0..*p).any(|off| seq == (0..32).filter(|i| i % p == off).fold(0u32, |a, i| a | 1 << i))).map(|p| 32 / p)
}

/// speed value for `syncToBpm=false`: BPM / 100 x multiplier (guide 18; unverified in game; the one vanilla item that
/// fits, STD Lightbar at 300 BPM with an alternating pattern, uses 45 where this gives 48)
pub fn speed_value(bpm: f64, seq: u32) -> Result<f64> {
    if !(bpm > 0.0 && bpm.is_finite()) {
        bail!("BPM must be above 0");
    }
    let m = flash_multiplier(seq).ok_or_else(|| anyhow!("{} is not an evenly spaced pattern; the guide gives multipliers only for 1010.. (x16), 1000.. (x8), 10000000.. (x4) and 1 + 15 zeros (x2)", dec2seq(seq)))?;
    Ok(bpm / 100.0 * m as f64)
}

pub struct StartValue {
    /// the formula's raw result in degrees (large and negative for large deltas)
    pub raw_deg: f64,
    /// same angle, 0..360
    pub deg: f64,
    pub rad: f64,
}

/// Flashiness `start` from the guide's recipe `(X - 1) * Y - (Y / 11.25) * 360` degrees (X = siren number 1..20, Y = delta
/// in degrees; +180 for a steady corona). The second term is Y / 11.25 deg-per-beat (360 / 32) beats, converted to
/// degrees at 360 per beat, i.e. 32 * Y: a whole number of turns, so it vanishes whenever Y is a multiple of 11.25 and
/// otherwise only shifts the result by an arbitrary fraction of a turn. **Unverified, and Rockstar's data does not
/// follow it**: of the 73 vanilla flashiness items with a delta, 64 have start 0 and the other 9 are all delta 90 deg with
/// start 270 deg; the formula reproduces the start of just 24 of the 73 (23 of the zeros, 1 of the 9), while "always 0"
/// reproduces 64. No rule in the vanilla numbers fits siren number and delta together.
pub fn start_value(siren: u32, delta_deg: f64, steady: bool) -> StartValue {
    let raw = (siren as f64 - 1.0) * delta_deg - delta_deg / 11.25 * 360.0 + if steady { 180.0 } else { 0.0 };
    let mut deg = ((raw * 1e9).round() / 1e9).rem_euclid(360.0);
    if deg >= 360.0 {
        deg = 0.0;
    }
    StartValue { raw_deg: raw, deg, rad: deg.to_radians() }
}

fn arg_num(args: &[String], i: usize, what: &str) -> Result<f64> {
    let t = args.get(i).ok_or_else(|| anyhow!("missing <{what}>"))?;
    t.parse::<f64>().ok().filter(|v| v.is_finite()).ok_or_else(|| anyhow!("<{what}> '{t}' is not a number"))
}

/// `crck-explorer siren calc <op> ...`: returns the text to print
pub fn calc(op: &str, args: &[String]) -> Result<String> {
    Ok(match op {
        "deg2rad" => {
            let d = arg_num(args, 0, "degrees")?;
            format!("{d} deg = {} rad", fmt_rad(deg2rad(d)))
        }
        "rad2deg" => {
            let r = arg_num(args, 0, "radians")?;
            format!("{r} rad = {:.4} deg", rad2deg(r))
        }
        "seq2dec" => {
            let b = args.first().ok_or_else(|| anyhow!("missing <binary pattern>"))?;
            let n = seq2dec(b)?;
            format!("{} = {n}", dec2seq(n))
        }
        "dec2seq" => {
            let t = args.first().ok_or_else(|| anyhow!("missing <sequencer>"))?;
            let n = t.trim().parse::<u32>().map_err(|_| anyhow!("<sequencer> '{t}' must be a whole number in 0..4294967295"))?;
            format!("{n} = {}", dec2seq(n))
        }
        "speed" => {
            let bpm = arg_num(args, 0, "bpm")?;
            let seq = parse_sequencer(args.get(1).ok_or_else(|| anyhow!("missing <sequencer> (decimal, or 0b + binary pattern)"))?)?;
            let v = speed_value(bpm, seq)?;
            let m = flash_multiplier(seq).unwrap_or(0);
            format!(
                "speed {v} for syncToBpm=false (BPM {bpm} / 100 x {m}, pattern {}{})\nstrobe (guide: at least double the base speed): {}\nunverified in game; the guides' formula",
                dec2seq(seq),
                if m == 1 { ", x1 extrapolated" } else { "" },
                v * 2.0
            )
        }
        "start" => {
            let siren = arg_num(args, 0, "siren number 1-20")?;
            if siren.fract() != 0.0 || siren < 1.0 {
                bail!("<siren number> must be a whole number from 1 (siren1) up");
            }
            let delta = arg_num(args, 1, "delta in degrees")?;
            let r = start_value(siren as u32, delta, args.iter().any(|a| a == "--steady"));
            format!(
                "start {} rad = {:.4} deg (formula result {:.4} deg)\nthe guide's formula, unverified: vanilla carcols data does not follow it, so test in game",
                fmt_rad(r.rad),
                r.deg,
                r.raw_deg
            )
        }
        o => bail!("unknown calculator '{o}': deg2rad, rad2deg, seq2dec, dec2seq, speed, start"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn angles() {
        assert!(close(deg2rad(180.0), std::f64::consts::PI));
        assert!(close(rad2deg(deg2rad(37.5)), 37.5));
        assert_eq!(fmt_rad(deg2rad(90.0)), "1.570796");
        assert_eq!(fmt_rad(deg2rad(-45.0)), "-0.785398");
    }

    #[test]
    fn sequencer_both_ways() {
        // the guide's example
        assert_eq!(seq2dec("10101010101010101010101010101010").unwrap(), 2863311530);
        assert_eq!(dec2seq(2863311530), "10101010101010101010101010101010");
        assert_eq!(seq2dec("0b10").unwrap(), 2863311530);
        assert_eq!(seq2dec("1000").unwrap(), 2290649224);
        assert_eq!(seq2dec("1").unwrap(), u32::MAX);
        assert_eq!(seq2dec("0000_0000 0000_0000 0000_0000 0000_0001").unwrap(), 1);
        for n in [0, 1, 1431655765, u32::MAX] {
            assert_eq!(seq2dec(&dec2seq(n)).unwrap(), n);
        }
        assert!(seq2dec("101").is_err(), "3 digits do not tile 32 beats");
        assert!(seq2dec("12").is_err());
        assert!(seq2dec(&"1".repeat(33)).is_err());
        assert_eq!(parse_sequencer("4294967295").unwrap(), u32::MAX);
        assert!(parse_sequencer("4294967296").is_err());
        assert!(parse_sequencer("-1").is_err());
    }

    #[test]
    fn speed_from_bpm() {
        // multipliers and sequencer values from the guide
        assert_eq!(flash_multiplier(2863311530), Some(16));
        assert_eq!(flash_multiplier(1431655765), Some(16), "shifted alternate pattern");
        assert_eq!(flash_multiplier(2290649224), Some(8));
        assert_eq!(flash_multiplier(2155905152), Some(4));
        assert_eq!(flash_multiplier(2147516416), Some(2));
        assert_eq!(flash_multiplier(0x8000_0000), Some(1));
        assert_eq!(flash_multiplier(u32::MAX), None);
        assert_eq!(flash_multiplier(0), None);
        assert_eq!(flash_multiplier(0b1011), None);
        assert!(close(speed_value(220.0, 2863311530).unwrap(), 35.2));
        assert!(close(speed_value(220.0, 2290649224).unwrap(), 17.6));
        assert!(close(speed_value(220.0, 2155905152).unwrap(), 8.8));
        assert!(close(speed_value(220.0, 2147516416).unwrap(), 4.4));
        assert!(speed_value(0.0, 2863311530).is_err());
        assert!(speed_value(220.0, 7).is_err());
    }

    #[test]
    fn start_from_siren_and_delta() {
        let s = |x, d, st| start_value(x, d, st);
        // delta 0: every siren starts at 0 (180 for a steady corona)
        assert!(close(s(5, 0.0, false).deg, 0.0));
        assert!(close(s(5, 0.0, true).deg, 180.0));
        // delta a multiple of 11.25 deg: the second term is whole turns, result = (X-1)*Y mod 360
        assert!(close(s(1, 90.0, false).deg, 0.0));
        assert!(close(s(2, 90.0, false).deg, 90.0));
        assert!(close(s(3, 90.0, false).deg, 180.0));
        assert!(close(s(5, 90.0, false).deg, 0.0));
        assert!(close(s(3, 90.0, false).raw_deg, 180.0 - 2880.0));
        assert!(close(s(2, 45.0, true).deg, 45.0 + 180.0));
        // delta not a multiple of 11.25: the second term matters (here 10 deg is 0.889 turns of 11.25)
        assert!(close(s(1, 10.0, false).deg, (-10.0f64 / 11.25 * 360.0).rem_euclid(360.0)));
        assert!(s(1, 10.0, false).deg > 0.0);
        assert!(close(s(2, 90.0, false).rad, std::f64::consts::FRAC_PI_2));
    }

    #[test]
    fn start_formula_is_not_what_vanilla_uses() {
        // (siren number, delta deg, start deg) from the game's own carcols (riot2 id 14, police/sheriff id 1): the formula
        // does not reproduce them. This documents why `start_value` is labelled unverified.
        for (x, d, vanilla_start) in [(1, 90.0, 270.0), (2, 90.0, 270.0), (3, 90.0, 270.0)] {
            assert!(!close(start_value(x, d, false).deg, vanilla_start), "siren {x}");
        }
    }

    #[test]
    fn calc_text() {
        let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(calc("deg2rad", &a(&["90"])).unwrap(), "90 deg = 1.570796 rad");
        assert_eq!(calc("seq2dec", &a(&["10"])).unwrap(), "10101010101010101010101010101010 = 2863311530");
        assert!(calc("speed", &a(&["220", "0b10"])).unwrap().starts_with("speed 35.2 "));
        assert!(calc("start", &a(&["3", "-90", "--steady"])).is_ok());
        assert!(calc("start", &a(&["0", "90"])).is_err());
        assert!(calc("nope", &[]).is_err());
        assert!(calc("deg2rad", &[]).is_err());
    }

    fn setting(id: &str, name: &str, count: usize, item: &str) -> String {
        format!("<Item><id value=\"{id}\"/><name>{name}</name><sequencerBpm value=\"200\"/><sirens>{}</sirens></Item>", item.repeat(count))
    }

    const OK_ITEM: &str = "<Item><rotation><delta value=\"0\"/><start value=\"0\"/><speed value=\"3\"/><sequencer value=\"4294967295\"/></rotation><flashiness><delta value=\"1.5708\"/><start value=\"0\"/><speed value=\"1\"/><sequencer value=\"2863311530\"/><syncToBpm value=\"true\"/></flashiness><corona><intensity value=\"50\"/><size value=\"1\"/></corona><color value=\"0xFFFF0300\"/><light value=\"true\"/></Item>";

    fn run(carcols: &str) -> Report {
        check_texts(&[("carcols.meta".into(), format!("<CVehicleModelInfoVarGlobal><Sirens>{carcols}</Sirens></CVehicleModelInfoVarGlobal>"))], &[])
    }

    fn codes(r: &Report, sev: char) -> Vec<&'static str> {
        r.issues.iter().filter(|i| i.sev == sev).map(|i| i.code).collect()
    }

    #[test]
    fn a_clean_setting_has_no_errors_or_warnings() {
        let r = run(&setting("100", "mine", 20, OK_ITEM));
        assert!(r.issues.is_empty(), "{:?}", r.issues.iter().map(|i| (&i.code, &i.msg)).collect::<Vec<_>>());
    }

    #[test]
    fn each_rule_fires() {
        let bad = OK_ITEM.replace("1.5708", "90").replace("0xFFFF0300", "FF0300").replace("2863311530", "-1");
        let r = run(&format!("{}{}{}", setting("7", "mine", 19, &bad), setting("7", "other", 21, OK_ITEM), setting("300", "x", 20, OK_ITEM)));
        let (e, w) = (codes(&r, 'E'), codes(&r, 'W'));
        assert!(e.contains(&"siren.id-dup") && e.contains(&"siren.sequencer"), "{e:?}");
        for c in ["siren.radians", "siren.color", "siren.count", "siren.id-vanilla", "siren.id-range"] {
            assert!(w.contains(&c), "{c} missing from {w:?}");
        }
        // the vanilla copy of a setting is not a conflict
        assert!(!codes(&run(&setting("3", "fbiold", 20, OK_ITEM)), 'W').contains(&"siren.id-vanilla"));
    }

    #[test]
    fn speed_is_not_an_angle() {
        // vanilla rotation/flashiness speeds reach 60; only delta and start are checked as radians
        let r = run(&setting("100", "m", 20, &OK_ITEM.replace("<speed value=\"3\"/>", "<speed value=\"60\"/>")));
        assert!(r.issues.is_empty());
    }

    #[test]
    fn corona_dark_and_steady_are_only_info() {
        let dark = OK_ITEM.replace("<light value=\"true\"/>", "<light value=\"false\"/>").replace("<intensity value=\"50\"/>", "<intensity value=\"0\"/>");
        let steady = OK_ITEM.replace("<speed value=\"1\"/>", "<speed value=\"0\"/>").replace("<syncToBpm value=\"true\"/>", "<syncToBpm value=\"false\"/>");
        let r = run(&setting("100", "m", 19, &steady) .replace("</sirens>", &format!("{dark}</sirens>")));
        assert_eq!(r.errors, 0);
        assert!(r.issues.iter().all(|i| i.sev == 'I'), "{:?}", r.issues.iter().map(|i| (&i.code, &i.msg)).collect::<Vec<_>>());
        let c = codes(&r, 'I');
        assert!(c.contains(&"siren.steady") && c.contains(&"siren.dark"), "{c:?}");
    }

    #[test]
    fn variations_resolve_against_the_pack() {
        let var = |ids: &[&str]| format!("<CVehicleModelInfoVariation><variationData>{}</variationData></CVehicleModelInfoVariation>", ids.iter().map(|i| format!("<Item><modelName>car</modelName><sirenSettings value=\"{i}\"/></Item>")).collect::<String>());
        let cc = format!("<CVehicleModelInfoVarGlobal><Sirens>{}</Sirens></CVehicleModelInfoVarGlobal>", setting("100", "mine", 20, OK_ITEM));
        let r = check_texts(&[("carcols.meta".into(), cc)], &[("carvariations.meta".into(), var(&["0", "100", "3", "77"]))]);
        let ext: Vec<&str> = r.issues.iter().filter(|i| i.code == "siren.var-external").map(|i| i.msg.as_str()).collect();
        assert_eq!(ext.len(), 2, "{ext:?}");
        assert!(ext.iter().any(|m| m.contains("fbiold")) && ext.iter().any(|m| m.contains("another add-on")));
        assert_eq!(r.errors, 0, "ids absent from the pack are info only");
        let r = check_texts(&[("carcols.meta".into(), String::new())], &[("carvariations.meta".into(), var(&["x"]))]);
        assert!(r.errors >= 1);
    }

    #[test]
    fn broken_xml_and_wrong_root() {
        assert_eq!(check_texts(&[("carcols.meta".into(), "<a>".into())], &[]).errors, 1);
        assert_eq!(check_texts(&[("carcols.meta".into(), "<Other/>".into())], &[]).errors, 1);
    }
}
