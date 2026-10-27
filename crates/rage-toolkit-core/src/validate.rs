//! Meta cross-checks for an add-on vehicle pack folder (source folder, FiveM resource or loose dlc.rpf tree).
//! Severity: E = game/server will likely fail, W = likely a mistake, I = worth knowing.
//! Names compare case-insensitively (the game hashes lowercase). Items the pack doesn't define may be vanilla, so those are I.
use roxmltree::{Document, Node};
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
};

/// `None` when `text` is well-formed XML, otherwise the parser's message (with line and column).
pub fn xml_problem(text: &str) -> Option<String> {
    Document::parse(text.trim_start_matches('\u{feff}')).err().map(|e| e.to_string())
}

pub struct Issue {
    pub sev: char,
    /// stable id for scripts (v2 rules only; empty for the original meta rules)
    pub code: &'static str,
    pub file: String,
    pub msg: String,
    pub fix: String,
}

pub struct Report {
    pub issues: Vec<Issue>,
    pub summary: String,
    pub errors: usize,
}

const META_NAMES: [&str; 5] = ["vehicles.meta", "carvariations.meta", "handling.meta", "carcols.meta", "vehiclelayouts.meta"];

fn walk_files(dir: &Path, out: &mut Vec<PathBuf>) {
    walk_files_at(dir, out, 0)
}

fn walk_files_at(dir: &Path, out: &mut Vec<PathBuf>, depth: usize) {
    let mut es: Vec<_> = fs::read_dir(dir).map(|r| r.filter_map(|e| e.ok()).collect()).unwrap_or_default();
    es.sort_by_key(|e| e.file_name());
    for e in es {
        let p = e.path();
        if p.is_dir() {
            if depth < crate::search::MAX_FOLDER_DEPTH {
                walk_files_at(&p, out, depth + 1);
            }
        } else {
            out.push(p);
        }
    }
}

fn fname(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

fn lc(s: &str) -> String {
    s.to_lowercase()
}

fn eqi(a: &str, b: &str) -> bool {
    lc(a) == lc(b)
}

fn text_of(n: Node) -> String {
    n.descendants().filter(|d| d.is_text()).map(|d| d.text().unwrap_or("")).collect()
}

fn child<'a, 'b>(n: Node<'a, 'b>, name: &str) -> Option<Node<'a, 'b>> {
    n.children().find(|c| c.is_element() && c.tag_name().name() == name)
}

fn val(n: Node, name: &str) -> String {
    child(n, name).map(|c| text_of(c).trim().to_string()).unwrap_or_default()
}

fn attr_value(n: Node, name: &str) -> Option<String> {
    child(n, name).and_then(|c| c.attribute("value")).map(str::to_string)
}

fn items<'a, 'b>(doc: Option<&'b Document<'a>>, container: &str) -> Vec<Node<'b, 'a>> {
    doc.and_then(|d| child(d.root_element(), container))
        .map(|c| c.children().filter(|x| x.is_element() && x.tag_name().name() == "Item").collect())
        .unwrap_or_default()
}

fn line(doc: &Document, n: Node) -> String {
    format!(" (line {})", doc.text_pos_at(n.range().start).row)
}

/// groups with count > 1, key = first spelling, in order of first appearance
fn dups<'a>(keys: impl Iterator<Item = &'a str>) -> Vec<(String, usize)> {
    let mut order: Vec<String> = vec![];
    let mut map: HashMap<String, (String, usize)> = HashMap::new();
    for k in keys {
        let e = map.entry(lc(k)).or_insert_with(|| {
            order.push(lc(k));
            (k.to_string(), 0)
        });
        e.1 += 1;
    }
    order.iter().filter_map(|k| map.get(k)).filter(|(k, c)| !k.is_empty() && *c > 1).cloned().collect()
}

/// Core = meta cross-checks only. Fivem adds manifest, stream size and folder-structure rules.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    Core,
    Fivem,
}

pub fn run(dir: &Path, profile: Profile) -> Report {
    let mut all = vec![];
    walk_files(dir, &mut all);
    let mut texts: HashMap<&str, String> = HashMap::new();
    let mut issues: Vec<Issue> = vec![];
    let mut add = |s: char, f: &str, m: String, fix: &str| issues.push(Issue { sev: s, code: "", file: f.into(), msg: m, fix: fix.into() });

    for n in META_NAMES {
        let paths: Vec<&PathBuf> = all.iter().filter(|p| eqi(&fname(p), n)).collect();
        if paths.len() > 1 {
            add('W', n, format!("found {} copies", paths.len()), "keep one copy per pack");
        }
        let Some(p) = paths.first() else { continue };
        let raw = fs::read_to_string(p).unwrap_or_default();
        let t = raw.trim_start_matches('\u{feff}').to_string();
        match Document::parse(&t) {
            Ok(_) => {
                texts.insert(n, t);
            }
            Err(e) => add('E', n, format!("not valid XML: {e}"), "fix the XML; the game ignores or crashes on broken metas"),
        }
    }
    let parsed: HashMap<&str, Document> = texts.iter().map(|(k, t)| (*k, Document::parse(t).unwrap())).collect();
    let doc = |n: &str| parsed.get(n);
    if doc("vehicles.meta").is_none() {
        add('E', "vehicles.meta", "missing or unreadable".into(), "every add-on vehicle needs a vehicles.meta entry");
    }
    for (f, expect) in [
        ("vehicles.meta", "CVehicleModelInfo__InitDataList"),
        ("carvariations.meta", "CVehicleModelInfoVariation"),
        ("handling.meta", "CHandlingDataMgr"),
        ("carcols.meta", "CVehicleModelInfoVarGlobal"),
        ("vehiclelayouts.meta", "CVehicleMetadataMgr"),
    ] {
        if let Some(d) = doc(f) {
            let got = d.root_element().tag_name().name();
            if got != expect {
                add('E', f, format!("root element is <{got}>, expected <{expect}>"), "wrong template or wrong file type");
            }
        }
    }

    // --- gather
    let models: Vec<(Node, String)> = items(doc("vehicles.meta"), "InitDatas").into_iter().map(|v| (v, val(v, "modelName"))).collect();
    let handling: Vec<(Node, String)> = items(doc("handling.meta"), "HandlingData").into_iter().map(|h| (h, val(h, "handlingName"))).collect();
    let variations: Vec<(Node, String)> = items(doc("carvariations.meta"), "variationData").into_iter().map(|v| (v, val(v, "modelName"))).collect();
    let kits: Vec<(Node, String)> = items(doc("carcols.meta"), "Kits").into_iter().map(|k| (k, val(k, "kitName"))).collect();
    let layouts: HashSet<String> = doc("vehiclelayouts.meta")
        .map(|d| {
            d.descendants()
                .filter(|n| n.is_element() && n.tag_name().name() == "Name")
                .map(|n| text_of(n).trim().to_string())
                .filter(|x| lc(x).starts_with("layout_"))
                .map(|x| lc(&x))
                .collect()
        })
        .unwrap_or_default();
    let files: HashSet<String> = all.iter().map(|p| lc(&fname(p))).collect();
    let dv = doc("vehicles.meta");
    let dh = doc("handling.meta");
    let dcv = doc("carvariations.meta");
    let dcc = doc("carcols.meta");

    // --- vehicles.meta
    for (k, c) in dups(models.iter().map(|m| m.1.as_str())) {
        add('E', "vehicles.meta", format!("modelName '{k}' defined {c} times"), "remove the duplicate InitData");
    }
    for (v, name) in &models {
        let d = dv.unwrap();
        if name.is_empty() {
            add('E', "vehicles.meta", format!("InitData without modelName{}", line(d, *v)), "add <modelName>");
            continue;
        }
        if *name != lc(name) {
            add('I', "vehicles.meta", format!("modelName '{name}' has capitals; spawn names are case-insensitive but lowercase is the convention"), "lowercase it everywhere");
        }
        if !files.contains(&format!("{}.yft", lc(name))) {
            add('E', "model", format!("{name}.yft not found{}", line(d, *v)), "add the model, or fix modelName");
        }
        if !files.contains(&format!("{}_hi.yft", lc(name))) {
            add('I', "model", format!("{name}_hi.yft not found; vanilla vehicles ship a _hi fragment"), "optional; add for close-up detail");
        }
        let txd = val(*v, "txdName");
        if txd.is_empty() {
            add('W', "vehicles.meta", format!("{name}: txdName empty"), "set txdName (normally = modelName)");
        } else {
            if !eqi(&txd, name) {
                add('W', "vehicles.meta", format!("{name}: txdName '{txd}' differs from modelName"), "keep them equal unless sharing a texture dictionary on purpose");
            }
            if !files.contains(&format!("{}.ytd", lc(&txd))) {
                add('E', "model", format!("{txd}.ytd not found (txdName of {name}){}", line(d, *v)), "add the texture dictionary or fix txdName");
            }
        }
        let h = val(*v, "handlingId");
        if h.is_empty() {
            add('E', "vehicles.meta", format!("{name}: handlingId empty"), "set handlingId");
        } else if handling.is_empty() {
            add('I', "vehicles.meta", format!("{name}: no handling.meta in pack, handlingId '{h}' must be vanilla"), "include handling.meta for custom handling");
        } else if !handling.iter().any(|x| eqi(&x.1, &h)) {
            add('I', "vehicles.meta", format!("{name}: handlingId '{h}' not in this pack's handling.meta; fine only if it is a vanilla handling"), "add it to handling.meta or use a vanilla name");
        }
        let lay = val(*v, "layout");
        if lay.is_empty() {
            add('W', "vehicles.meta", format!("{name}: layout empty"), "set a LAYOUT_* name");
        } else if !layouts.is_empty() && !layouts.contains(&lc(&lay)) {
            add('I', "vehicles.meta", format!("{name}: layout '{lay}' not in this pack's vehiclelayouts.meta; fine only if vanilla"), "add it or use a vanilla layout");
        }
        if val(*v, "gameName").is_empty() {
            add('W', "vehicles.meta", format!("{name}: gameName empty (label key for the display name)"), "set gameName");
        }
    }

    // --- handling.meta
    for (k, c) in dups(handling.iter().map(|x| x.1.as_str())) {
        add('E', "handling.meta", format!("handlingName '{k}' defined {c} times"), "keep one; duplicates silently override each other");
    }
    for (h, name) in &handling {
        let d = dh.unwrap();
        if name.is_empty() {
            add('E', "handling.meta", format!("HandlingData without handlingName{}", line(d, *h)), "add <handlingName>");
        }
        match child(*h, "fMass").and_then(|c| c.attribute("value")).and_then(|v| v.trim().parse::<f64>().ok()) {
            None => add('E', "handling.meta", format!("{name}: fMass missing or not a number{}", line(d, *h)), "set fMass value=\"...\""),
            Some(m) if m <= 0.0 => add('E', "handling.meta", format!("{name}: fMass {m} <= 0{}", line(d, *h)), "mass must be positive"),
            Some(m) if !(100.0..=100000.0).contains(&m) => add('W', "handling.meta", format!("{name}: fMass {m} looks wrong"), "normal cars are ~800-3000"),
            _ => {}
        }
    }
    for (_, hn) in handling.iter().filter(|(_, n)| !n.is_empty()) {
        if !models.is_empty() && !models.iter().any(|(m, _)| eqi(&val(*m, "handlingId"), hn)) {
            add('W', "handling.meta", format!("handling '{hn}' is not used by any vehicle in vehicles.meta"), "remove it or point a vehicle at it");
        }
    }

    // --- carvariations.meta
    for (v, name) in &variations {
        let d = dcv.unwrap();
        if !models.iter().any(|m| eqi(&m.1, name)) {
            add('E', "carvariations.meta", format!("variation for '{name}' has no matching vehicles.meta modelName{}", line(d, *v)), "fix the name; unmatched variations are ignored");
        }
        for k in child(*v, "kits").into_iter().flat_map(|k| k.children().filter(|c| c.is_element() && c.tag_name().name() == "Item")) {
            let kn = text_of(k).trim().to_string();
            if !kits.is_empty() && !kits.iter().any(|x| eqi(&x.1, &kn)) {
                add('I', "carvariations.meta", format!("{name}: kit '{kn}' not in this pack's carcols.meta; fine only if vanilla"), "add the kit to carcols.meta");
            }
        }
        for c in child(*v, "colors").into_iter().flat_map(|k| k.children().filter(|c| c.is_element() && c.tag_name().name() == "Item")) {
            let t = text_of(c);
            let parts: Vec<&str> = t.split(',').map(str::trim).filter(|p| !p.is_empty()).collect();
            if parts.len() < 4 || parts.iter().any(|p| !p.parse::<i32>().map(|n| (0..=255).contains(&n)).unwrap_or(false)) {
                add('W', "carvariations.meta", format!("{name}: colour row '{}' is not 4-6 ints in 0..255{}", t.trim(), line(d, c)), "use palette indices, e.g. 134, 134, 0, 156, 0, 0");
            }
        }
    }
    if dcv.is_some() {
        for (_, name) in models.iter().filter(|m| !m.1.is_empty() && !variations.iter().any(|x| eqi(&x.1, &m.1))) {
            add('W', "carvariations.meta", format!("{name} has no variation entry (spawns with default colours, no kits)"), "add a variationData entry");
        }
    }

    // --- carcols.meta
    for (k, c) in dups(kits.iter().map(|x| x.1.as_str())) {
        add('E', "carcols.meta", format!("kitName '{k}' defined {c} times"), "duplicate kits are a classic cause of black wheels / missing mods");
    }
    let ids: Vec<String> = kits.iter().filter_map(|(k, _)| attr_value(*k, "id")).collect();
    for (k, c) in dups_exact(ids.iter().map(String::as_str)) {
        add('E', "carcols.meta", format!("kit id {k} used {c} times"), "each kit needs a unique id");
    }
    if let Some(d) = dcc {
        let sirens: Vec<String> = d
            .descendants()
            .filter(|n| n.is_element() && n.tag_name().name() == "Sirens")
            .flat_map(|s| s.children().filter(|c| c.is_element() && c.tag_name().name() == "Item"))
            .filter_map(|s| attr_value(s, "id"))
            .collect();
        for (k, c) in dups_exact(sirens.iter().map(String::as_str)) {
            add('E', "carcols.meta", format!("siren id {k} used {c} times"), "siren ids must be unique");
        }
    }

    // --- livery kits (VMT_LIVERY_MOD)
    let labels: Vec<String> = all
        .iter()
        .filter(|p| p.extension().map(|e| e.eq_ignore_ascii_case("lua")).unwrap_or(false))
        .flat_map(|p| fs::read_to_string(p).unwrap_or_default().split('\n').map(str::to_string).collect::<Vec<_>>())
        .filter(|l| l.contains("AddTextEntry"))
        .collect();
    let linked: HashSet<String> = variations
        .iter()
        .flat_map(|(v, _)| child(*v, "kits").into_iter().flat_map(|k| k.children().filter(|c| c.is_element() && c.tag_name().name() == "Item")))
        .map(|i| lc(text_of(i).trim()))
        .collect();
    for (k, kname) in &kits {
        let d = dcc.unwrap();
        let liveries: Vec<Node> = k
            .descendants()
            .filter(|n| n.is_element() && n.tag_name().name() == "visibleMods")
            .flat_map(|vm| vm.children().filter(|c| c.is_element() && c.tag_name().name() == "Item"))
            .filter(|i| val(*i, "type") == "VMT_LIVERY_MOD")
            .collect();
        if liveries.is_empty() {
            continue;
        }
        if val(*k, "kitType") != "MKT_SPECIAL" {
            add('W', "carcols.meta", format!("{kname}: kit has liveries but kitType is '{}'", val(*k, "kitType")), "livery kits use MKT_SPECIAL");
        }
        if dcv.is_some() && !linked.contains(&lc(kname)) {
            add('E', "carcols.meta", format!("{kname}: livery kit not listed in any carvariations <kits>{}", line(d, *k)), &format!("add <Item>{kname}</Item> to the vehicle's kits"));
        }
        for (i, lv) in liveries.iter().enumerate() {
            let mn = val(*lv, "modelName");
            let tag = format!("{kname} livery #{i}");
            if mn.is_empty() {
                add('E', "carcols.meta", format!("{tag}: modelName empty{}", line(d, *lv)), "set it to the livery YFT name without .yft");
                continue;
            }
            if mn != lc(&mn) {
                add('W', "carcols.meta", format!("{tag}: modelName '{mn}' has capitals"), "livery YFT names should be lowercase");
            }
            let yft = all.iter().find(|p| eqi(&fname(p), &format!("{mn}.yft")));
            match yft {
                None => add('E', "model", format!("{tag}: {mn}.yft not found"), "add the livery fragment to stream/"),
                Some(p) => {
                    let len = fs::metadata(p).map(|m| m.len()).unwrap_or(0);
                    if len > 2_000_000 {
                        add('W', "model", format!("{mn}.yft is {} KB; a correct livery fragment is ~70-250 KB", len / 1024), "likely contains the whole body/skeleton; rebuild from the blank template (1 bone, 1 tiny mesh)");
                    }
                }
            }
            let label = val(*lv, "modShopLabel");
            if !label.is_empty() && !labels.is_empty() && !labels.iter().any(|l| lc(l).contains(&lc(&label))) {
                add('W', "carcols.meta", format!("{tag}: modShopLabel '{label}' has no AddTextEntry in the pack's Lua"), &format!("add AddTextEntry('{label}', 'Name') or the mod shop shows NULL"));
            } else if !label.is_empty() && labels.is_empty() {
                add('I', "carcols.meta", format!("{tag}: modShopLabel '{label}' needs an AddTextEntry (no Lua in this pack)"), "define the label in client.lua");
            }
        }
        for (k2, c) in dups(liveries.iter().map(|i| val(*i, "modelName")).collect::<Vec<_>>().iter().map(String::as_str)) {
            add('E', "carcols.meta", format!("{kname}: livery '{k2}' listed {c} times"), "duplicate entries shift livery indices");
        }
    }
    for (v, name) in &models {
        if let Some(lod) = child(*v, "lodDistances") {
            let c = lod.attribute("content");
            if c != Some("float_array") {
                add('E', "vehicles.meta", format!("{name}: lodDistances content=\"{}\", must be \"float_array\"{}", c.unwrap_or(""), line(dv.unwrap(), lod)), "with the wrong type every LOD distance is 0 and the vehicle disappears at range");
            }
        }
    }

    if profile == Profile::Fivem {
        crate::fivem::rules(dir, &all, &mut issues);
    }

    // --- report order: severity, then file (stable)
    let rank = |c: char| "EWI".find(c).unwrap_or(3);
    issues.sort_by(|a, b| rank(a.sev).cmp(&rank(b.sev)).then_with(|| a.file.cmp(&b.file)));
    let n = |c: char| issues.iter().filter(|i| i.sev == c).count();
    let (e, w, i) = (n('E'), n('W'), n('I'));
    Report { summary: format!("{} vehicle(s), {} handling, {} kit(s): {e} error(s), {w} warning(s), {i} info", models.len(), handling.len(), kits.len()), errors: e, issues }
}

/// exact-string duplicate groups (ids), first appearance order
fn dups_exact<'a>(keys: impl Iterator<Item = &'a str>) -> Vec<(String, usize)> {
    let mut order: Vec<&str> = vec![];
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for k in keys {
        if !counts.contains_key(k) {
            order.push(k);
        }
        *counts.entry(k).or_default() += 1;
    }
    order.into_iter().filter(|k| counts[k] > 1).map(|k| (k.to_string(), counts[k])).collect()
}

pub fn print(r: &Report) {
    for g in &r.issues {
        let code = if g.code.is_empty() { String::new() } else { format!(" [{}]", g.code) };
        println!("[{}] {}: {}{code}\n      fix: {}", g.sev, g.file, g.msg, g.fix);
    }
    println!("\n{}", r.summary);
}

fn esc(s: &str) -> String {
    let mut o = String::new();
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o
}

/// machine-readable report for scripts / CI
pub fn to_json(r: &Report) -> String {
    let items: Vec<String> = r
        .issues
        .iter()
        .map(|g| {
            format!(
                "    {{\"severity\": \"{}\", \"code\": \"{}\", \"file\": \"{}\", \"message\": \"{}\", \"fix\": \"{}\"}}",
                g.sev,
                g.code,
                esc(&g.file),
                esc(&g.msg),
                esc(&g.fix)
            )
        })
        .collect();
    format!("{{\n  \"summary\": \"{}\",\n  \"errors\": {},\n  \"issues\": [\n{}\n  ]\n}}\n", esc(&r.summary), r.errors, items.join(",\n"))
}
