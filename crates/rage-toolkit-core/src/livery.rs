//! Modkit liveries (`VMT_LIVERY_MOD`): add one to a FiveM vehicle resource and keep carcols / carvariations / label Lua /
//! manifest in step. Rules and layout follow the livery guide the user supplied. Existing livery entries are never
//! reordered or rewritten (their position is their index; saved vehicles store it): new entries are only appended.
//! Edits are text splices, so the rest of each file keeps its formatting.
//! The livery YFT itself cannot be generated here (`rage-formats` has no YFT writer): it stays a Blender step.
use crate::{fivem, pack::sanitize};
use anyhow::{bail, Context, Result};
use roxmltree::{Document, Node};
use std::{
    fs,
    path::{Path, PathBuf},
};

pub struct AddParams<'a> {
    pub pack: &'a Path,
    pub vehicle: &'a str,
    pub name: &'a str,
    /// text shown in the mod shop (default: the name)
    pub display: Option<&'a str>,
    /// label key (default: livery model name, upper case)
    pub label: Option<&'a str>,
    /// needed only when the vehicle has no livery kit yet; must be unique across the whole server
    pub kit_id: Option<u32>,
    /// existing livery fragment to copy into stream/
    pub yft: Option<&'a Path>,
}

pub struct KitInfo {
    pub kit: String,
    pub id: String,
    pub kit_type: String,
    /// (index among VMT_LIVERY_MOD entries, livery model name, label, yft present in the pack)
    pub liveries: Vec<(usize, String, String, bool)>,
    pub linked_to: Vec<String>,
}

// ---- small XML helpers -----------------------------------------------------------------------------------------

fn strip_bom(s: &str) -> &str {
    s.trim_start_matches('\u{feff}')
}

fn el<'a, 'b>(n: Node<'a, 'b>, name: &str) -> Option<Node<'a, 'b>> {
    n.children().find(|c| c.is_element() && c.tag_name().name() == name)
}

fn items<'a, 'b>(n: Node<'a, 'b>) -> Vec<Node<'a, 'b>> {
    n.children().filter(|c| c.is_element() && c.tag_name().name() == "Item").collect()
}

fn text(n: Node) -> String {
    n.descendants().filter(|d| d.is_text()).map(|d| d.text().unwrap_or("")).collect::<String>().trim().to_string()
}

fn val(n: Node, name: &str) -> String {
    el(n, name).map(text).unwrap_or_default()
}

fn attr(n: Node, name: &str) -> String {
    el(n, name).and_then(|c| c.attribute("value")).unwrap_or("").to_string()
}

fn eqi(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

fn indent_before(t: &str, pos: usize) -> String {
    let line_start = t[..pos].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let s = &t[line_start..pos];
    if s.chars().all(|c| c == ' ' || c == '\t') {
        s.to_string()
    } else {
        String::new()
    }
}

/// `block` = lines at the child indentation, each ending in "\n". Returns the text with it appended inside `parent`.
fn append_child(t: &str, parent: Node, block: &str) -> String {
    let r = parent.range();
    let tag = parent.tag_name().name();
    let pindent = indent_before(t, r.start);
    if t[r.clone()].trim_end().ends_with("/>") && parent.children().all(|c| !c.is_element()) {
        // <tag /> -> <tag>\n block pindent</tag>
        return format!("{}<{tag}>\n{block}{pindent}</{tag}>{}", &t[..r.start], &t[r.end..]);
    }
    let close = t[..r.end].rfind(&format!("</{tag}")).unwrap_or(r.end);
    let line_start = t[..close].rfind('\n').map(|i| i + 1).unwrap_or(0);
    if t[line_start..close].chars().all(|c| c == ' ' || c == '\t') {
        format!("{}{block}{}", &t[..line_start], &t[line_start..])
    } else {
        format!("{}\n{block}{pindent}{}", &t[..close], &t[close..])
    }
}

/// indent every line of `xml` (no trailing newline needed) by `indent`
fn indented(xml: &str, indent: &str) -> String {
    xml.lines().map(|l| if l.is_empty() { "\n".to_string() } else { format!("{indent}{l}\n") }).collect()
}

fn child_indent(t: &str, parent: Node) -> String {
    match items(parent).first() {
        Some(first) => indent_before(t, first.range().start),
        None => format!("{}  ", indent_before(t, parent.range().start)),
    }
}

fn livery_item_xml(model: &str, label: &str) -> String {
    format!(
        "<Item>\n  <modelName>{model}</modelName>\n  <modShopLabel>{label}</modShopLabel>\n  <linkedModels />\n  <turnOffBones />\n  <type>VMT_LIVERY_MOD</type>\n  <bone>chassis</bone>\n  <collisionBone>chassis</collisionBone>\n  <cameraPos>VMCP_DEFAULT</cameraPos>\n  <audioApply value=\"1.000000\" />\n  <weight value=\"20\" />\n  <turnOffExtra value=\"false\" />\n  <disableBonnetCamera value=\"false\" />\n  <allowBonnetSlide value=\"true\" />\n</Item>"
    )
}

fn kit_xml(kit: &str, id: u32, first: &str) -> String {
    format!(
        "<Item>\n  <kitName>{kit}</kitName>\n  <id value=\"{id}\" />\n  <kitType>MKT_SPECIAL</kitType>\n  <visibleMods>\n{}  </visibleMods>\n  <linkMods />\n  <statMods />\n  <slotNames />\n  <liveryNames />\n  <livery2Names />\n</Item>",
        indented(first, "    ")
    )
}

const CARCOLS_TEMPLATE: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<CVehicleModelInfoVarGlobal>\n  <Kits />\n  <Lights />\n</CVehicleModelInfoVarGlobal>\n";

fn variations_template(model: &str, kit: &str) -> String {
    format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<CVehicleModelInfoVariation>\n  <variationData>\n    <Item>\n      <modelName>{model}</modelName>\n      <colors />\n      <kits>\n        <Item>{kit}</Item>\n      </kits>\n    </Item>\n  </variationData>\n</CVehicleModelInfoVariation>\n")
}

fn find_file(dir: &Path, name: &str) -> Option<PathBuf> {
    let mut stack = vec![dir.to_path_buf()];
    let mut found: Vec<PathBuf> = vec![];
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.file_name().map(|n| eqi(&n.to_string_lossy(), name)).unwrap_or(false) {
                found.push(p);
            }
        }
    }
    found.sort();
    found.into_iter().next()
}

fn lua_str(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\'', "\\'")
}

// ---- add -------------------------------------------------------------------------------------------------------

pub fn add(p: &AddParams) -> Result<Vec<String>> {
    let mut log: Vec<String> = vec![];
    let model = sanitize(p.vehicle);
    let lname = sanitize(p.name);
    if model.is_empty() || lname.is_empty() {
        bail!("vehicle and livery names must contain letters or digits");
    }
    let livery_model = format!("{model}_liv_{lname}");
    let label = p.label.map(str::to_string).unwrap_or_else(|| livery_model.to_uppercase());
    if !label.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        bail!("label '{label}' may only contain letters, digits and underscores");
    }
    let display = p.display.unwrap_or(p.name);

    let cc_path = find_file(p.pack, "carcols.meta").unwrap_or_else(|| p.pack.join("data/carcols.meta"));
    let cv_path = find_file(p.pack, "carvariations.meta").unwrap_or_else(|| p.pack.join("data/carvariations.meta"));
    let mut cc = if cc_path.exists() { strip_bom(&fs::read_to_string(&cc_path)?).to_string() } else { CARCOLS_TEMPLATE.to_string() };
    let cv_existing = if cv_path.exists() { Some(strip_bom(&fs::read_to_string(&cv_path)?).to_string()) } else { None };

    // which kits does the vehicle already use?
    let vehicle_kits: Vec<String> = match &cv_existing {
        Some(t) => {
            let d = Document::parse(t).with_context(|| format!("{} is not valid XML", cv_path.display()))?;
            el(d.root_element(), "variationData")
                .into_iter()
                .flat_map(items)
                .find(|v| eqi(&val(*v, "modelName"), &model))
                .and_then(|v| el(v, "kits"))
                .map(|k| items(k).into_iter().map(text).collect())
                .unwrap_or_default()
        }
        None => vec![],
    };

    // pick the vehicle's livery kit, or create one
    let (kit_name, created_kit) = {
        let d = Document::parse(&cc).with_context(|| format!("{} is not valid XML", cc_path.display()))?;
        let kits: Vec<Node> = el(d.root_element(), "Kits").map(items).unwrap_or_default();
        let existing = kits.iter().find(|k| vehicle_kits.iter().any(|vk| eqi(vk, &val(**k, "kitName"))) && val(**k, "kitType") == "MKT_SPECIAL");
        match (existing, p.kit_id) {
            (Some(k), _) => (val(*k, "kitName"), false),
            (None, Some(id)) => {
                let wanted = format!("{id}_{model}_modkit");
                match kits.iter().find(|k| attr(**k, "id") == id.to_string()) {
                    // the vehicle's own kit exists but is not linked yet: reuse it
                    Some(k) if eqi(&val(*k, "kitName"), &wanted) => (wanted, false),
                    Some(_) => bail!("kit id {id} is already used in carcols.meta; kit ids must be unique across the whole server"),
                    None => (wanted, true),
                }
            }
            (None, None) => bail!("{model} has no livery kit yet: pass --kit-id <unique number> to create one"),
        }
    };

    // 1. carcols.meta
    if created_kit {
        let id: u32 = kit_name.split('_').next().and_then(|s| s.parse().ok()).context("kit id")?;
        let block = {
            let d = Document::parse(&cc)?;
            let kits = el(d.root_element(), "Kits").context("carcols.meta has no <Kits>")?;
            indented(&kit_xml(&kit_name, id, &livery_item_xml(&livery_model, &label)), &child_indent(&cc, kits))
        };
        let new = {
            let d = Document::parse(&cc)?;
            append_child(&cc, el(d.root_element(), "Kits").unwrap(), &block)
        };
        cc = new;
        log.push(format!("carcols.meta: new kit {kit_name} (MKT_SPECIAL) with livery #0 {livery_model}"));
    } else {
        let (block, already) = {
            let d = Document::parse(&cc)?;
            let kit = el(d.root_element(), "Kits").map(items).unwrap_or_default().into_iter().find(|k| eqi(&val(*k, "kitName"), &kit_name)).unwrap();
            let vm = el(kit, "visibleMods").context("kit has no <visibleMods>")?;
            let present = items(vm).iter().any(|i| val(*i, "type") == "VMT_LIVERY_MOD" && eqi(&val(*i, "modelName"), &livery_model));
            (indented(&livery_item_xml(&livery_model, &label), &child_indent(&cc, vm)), present)
        };
        if already {
            log.push(format!("carcols.meta: {livery_model} is already in {kit_name}; left unchanged"));
        } else {
            let new = {
                let d = Document::parse(&cc)?;
                let kit = el(d.root_element(), "Kits").map(items).unwrap_or_default().into_iter().find(|k| eqi(&val(*k, "kitName"), &kit_name)).unwrap();
                append_child(&cc, el(kit, "visibleMods").unwrap(), &block)
            };
            cc = new;
            let d = Document::parse(&cc)?;
            let kit = el(d.root_element(), "Kits").map(items).unwrap_or_default().into_iter().find(|k| eqi(&val(*k, "kitName"), &kit_name)).unwrap();
            let idx = items(el(kit, "visibleMods").unwrap()).iter().filter(|i| val(**i, "type") == "VMT_LIVERY_MOD").count() - 1;
            log.push(format!("carcols.meta: appended {livery_model} to {kit_name} as livery #{idx}"));
        }
    }
    if !cc_path.exists() {
        fs::create_dir_all(cc_path.parent().unwrap())?;
    }
    fs::write(&cc_path, &cc)?;

    // 2. carvariations.meta: the vehicle must list the kit
    let linked = vehicle_kits.iter().any(|k| eqi(k, &kit_name));
    match (&cv_existing, linked) {
        (Some(_), true) => {}
        (None, _) => {
            fs::create_dir_all(cv_path.parent().unwrap())?;
            fs::write(&cv_path, variations_template(&model, &kit_name))?;
            log.push(format!("carvariations.meta: created with {model} -> {kit_name} (minimal; merge into the vehicle's real entry if it has one elsewhere)"));
        }
        (Some(t), false) => {
            let d = Document::parse(t)?;
            let vd = el(d.root_element(), "variationData").context("carvariations.meta has no <variationData>")?;
            let new = match items(vd).into_iter().find(|v| eqi(&val(*v, "modelName"), &model)) {
                Some(v) => match el(v, "kits") {
                    Some(k) => append_child(t, k, &indented(&format!("<Item>{kit_name}</Item>"), &child_indent(t, k))),
                    None => append_child(t, v, &indented(&format!("<kits>\n  <Item>{kit_name}</Item>\n</kits>"), &child_indent(t, v))),
                },
                None => {
                    let b = indented(&format!("<Item>\n  <modelName>{model}</modelName>\n  <colors />\n  <kits>\n    <Item>{kit_name}</Item>\n  </kits>\n</Item>"), &child_indent(t, vd));
                    append_child(t, vd, &b)
                }
            };
            fs::write(&cv_path, new)?;
            log.push(format!("carvariations.meta: {model} now lists kit {kit_name}"));
        }
    }

    // 3. label Lua
    let lua_path = p.pack.join("client.lua");
    let lua = fs::read_to_string(&lua_path).unwrap_or_default();
    let all_lua: String = fs::read_dir(p.pack)?.flatten().map(|e| e.path()).filter(|p| p.extension().map(|e| e == "lua").unwrap_or(false)).map(|p| fs::read_to_string(p).unwrap_or_default()).collect();
    if all_lua.contains(&format!("'{label}'")) || all_lua.contains(&format!("\"{label}\"")) {
        log.push(format!("label {label}: already defined in the pack's Lua"));
    } else {
        let line = format!("    AddTextEntry('{label}', '{}')\n", lua_str(display));
        let (b, e) = ("-- crck-explorer:livery-labels begin", "-- crck-explorer:livery-labels end");
        let new = match (lua.find(b), lua.find(e)) {
            (Some(_), Some(ei)) => {
                // insert before the `end)` that closes the thread, i.e. the last line before the end marker
                let at = lua[..ei].rfind("end)").unwrap_or(ei);
                format!("{}{line}{}", &lua[..at], &lua[at..])
            }
            _ => format!("{lua}{}{b}\nCreateThread(function()\n{line}end)\n{e}\n", if lua.is_empty() || lua.ends_with('\n') { "" } else { "\n" }),
        };
        fs::write(&lua_path, new)?;
        log.push(format!("client.lua: AddTextEntry('{label}', '{display}')"));
    }

    // 4. manifest
    log.extend(ensure_manifest(p.pack, &cc_path, &cv_path)?);

    // 5. livery fragment
    let stream_yft = p.pack.join("stream").join(format!("{livery_model}.yft"));
    if let Some(src) = p.yft {
        fs::create_dir_all(stream_yft.parent().unwrap())?;
        fs::copy(src, &stream_yft).with_context(|| format!("copying {}", src.display()))?;
        let kb = fs::metadata(&stream_yft)?.len() / 1024;
        log.push(format!("stream/{livery_model}.yft copied ({kb} KB){}", if kb > 2000 { "; that is far larger than a correct livery fragment (~70-250 KB): it probably contains the whole body" } else { "" }));
        log.push("NOTE: the fragment's internal name must equal the file name".into());
    } else if !stream_yft.exists() {
        log.push(format!("TODO: build stream/{livery_model}.yft (1 bone `chassis`, 1 tiny mesh, paint material with DiffuseSampler2 = your livery DDS); re-run with --yft to copy it in"));
    }
    Ok(log)
}

fn ensure_manifest(pack: &Path, cc: &Path, cv: &Path) -> Result<Vec<String>> {
    let rel = |p: &Path| p.strip_prefix(pack).unwrap_or(p).to_string_lossy().replace('\\', "/");
    let (rcc, rcv) = (rel(cc), rel(cv));
    let mp = pack.join("fxmanifest.lua");
    let mut text = fs::read_to_string(&mp).unwrap_or_else(|_| "fx_version 'cerulean'\ngame 'gta5'\n".to_string());
    let clean = fivem::strip_comments(&text);
    let files = fivem::block_strings(&clean, "files").unwrap_or_default();
    let declared = fivem::data_files(&clean);
    let mut added: Vec<String> = vec![];
    let mut add_line = |s: String| {
        if !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&s);
        text.push('\n');
        added.push(s);
    };
    let missing_files: Vec<&String> = [&rcc, &rcv].into_iter().filter(|r| !files.iter().any(|g| fivem::glob(g.as_bytes(), r.as_bytes()))).collect();
    if !missing_files.is_empty() {
        add_line(format!("files {{ {} }}", missing_files.iter().map(|r| format!("'{r}'")).collect::<Vec<_>>().join(", ")));
    }
    for (ty, path) in [("CARCOLS_FILE", &rcc), ("VEHICLE_VARIATION_FILE", &rcv)] {
        if !declared.iter().any(|(t, p)| t == ty && p == path) {
            add_line(format!("data_file '{ty}' '{path}'"));
        }
    }
    if !clean.contains("client.lua") {
        add_line("client_script 'client.lua'".into());
    }
    if added.is_empty() {
        return Ok(vec!["fxmanifest.lua: already complete".into()]);
    }
    fs::write(&mp, &text)?;
    Ok(added.into_iter().map(|a| format!("fxmanifest.lua: + {a}")).collect())
}

// ---- list ------------------------------------------------------------------------------------------------------

pub fn list(pack: &Path) -> Result<Vec<KitInfo>> {
    let cc_path = find_file(pack, "carcols.meta").context("no carcols.meta in the pack")?;
    let t = fs::read_to_string(&cc_path)?;
    let d = Document::parse(strip_bom(&t))?;
    let cv = find_file(pack, "carvariations.meta").and_then(|p| fs::read_to_string(p).ok());
    let cvdoc = cv.as_ref().and_then(|t| Document::parse(strip_bom(t)).ok());
    let mut out = vec![];
    for k in el(d.root_element(), "Kits").map(items).unwrap_or_default() {
        let kit = val(k, "kitName");
        let mut idx = 0;
        let mut liveries = vec![];
        for m in el(k, "visibleMods").map(items).unwrap_or_default().into_iter().filter(|i| val(*i, "type") == "VMT_LIVERY_MOD") {
            let model = val(m, "modelName");
            let present = find_file(pack, &format!("{model}.yft")).is_some();
            liveries.push((idx, model, val(m, "modShopLabel"), present));
            idx += 1;
        }
        if liveries.is_empty() {
            continue;
        }
        let linked_to = cvdoc
            .as_ref()
            .and_then(|d| el(d.root_element(), "variationData").map(|v| items(v)))
            .unwrap_or_default()
            .into_iter()
            .filter(|v| el(*v, "kits").map(|ks| items(ks).iter().any(|i| eqi(&text(*i), &kit))).unwrap_or(false))
            .map(|v| val(v, "modelName"))
            .collect();
        out.push(KitInfo { kit, id: attr(k, "id"), kit_type: val(k, "kitType"), liveries, linked_to });
    }
    Ok(out)
}

// ---- append-only check -----------------------------------------------------------------------------------------

fn liveries_by_kit(t: &str) -> Result<Vec<(String, String, Vec<String>)>> {
    let d = Document::parse(strip_bom(t))?;
    Ok(el(d.root_element(), "Kits")
        .map(items)
        .unwrap_or_default()
        .into_iter()
        .map(|k| {
            let ls = el(k, "visibleMods").map(items).unwrap_or_default().into_iter().filter(|i| val(*i, "type") == "VMT_LIVERY_MOD").map(|i| val(i, "modelName")).collect();
            (val(k, "kitName"), attr(k, "id"), ls)
        })
        .collect())
}

/// Problems that would give saved vehicles the wrong livery when `new` replaces `old` on a live server. Empty = safe.
pub fn append_only(old: &str, new: &str) -> Result<Vec<String>> {
    let (o, n) = (liveries_by_kit(old)?, liveries_by_kit(new)?);
    let mut bad = vec![];
    for (kit, id, ls) in &o {
        let Some((_, nid, nls)) = n.iter().find(|(k, _, _)| eqi(k, kit)) else {
            if !ls.is_empty() {
                bad.push(format!("kit {kit} was removed ({} livery/ies lost)", ls.len()));
            }
            continue;
        };
        if id != nid {
            bad.push(format!("kit {kit}: id changed {id} -> {nid}"));
        }
        for (i, l) in ls.iter().enumerate() {
            match nls.get(i) {
                None => bad.push(format!("kit {kit}: livery #{i} {l} was removed")),
                Some(x) if !eqi(x, l) => bad.push(format!("kit {kit}: livery #{i} was {l}, now {x} (reordered, replaced or an entry was inserted above it)")),
                _ => {}
            }
        }
    }
    for (kit, id, _) in &n {
        if !o.iter().any(|(k, _, _)| eqi(k, kit)) {
            if let Some((ok, _, _)) = o.iter().find(|(_, oid, _)| oid == id) {
                bad.push(format!("kit {kit} reuses id {id} that belonged to {ok}"));
            }
        }
    }
    Ok(bad)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("rage_toolkit_liv_{tag}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("data")).unwrap();
        d
    }

    fn params<'a>(pack: &'a Path, name: &'a str, kit_id: Option<u32>) -> AddParams<'a> {
        AddParams { pack, vehicle: "MyVeh", name, display: Some("Some Name"), label: None, kit_id, yft: None }
    }

    #[test]
    fn creates_everything_then_appends_and_is_idempotent() {
        let d = scratch("full");
        // no kit yet: needs an id
        assert!(add(&params(&d, "lspd1", None)).is_err());
        add(&params(&d, "lspd1", Some(4800))).unwrap();
        add(&params(&d, "lspd2", None)).unwrap();
        let cc = fs::read_to_string(d.join("data/carcols.meta")).unwrap();
        let kits = liveries_by_kit(&cc).unwrap();
        assert_eq!(kits, vec![("4800_myveh_modkit".into(), "4800".into(), vec!["myveh_liv_lspd1".into(), "myveh_liv_lspd2".into()])]);
        // second add of the same livery changes nothing
        let before = fs::read_to_string(d.join("data/carcols.meta")).unwrap();
        let log = add(&params(&d, "lspd2", None)).unwrap();
        assert!(log.iter().any(|l| l.contains("already in")));
        assert_eq!(before, fs::read_to_string(d.join("data/carcols.meta")).unwrap());
        // linked, labelled, in the manifest
        assert!(fs::read_to_string(d.join("data/carvariations.meta")).unwrap().contains("<Item>4800_myveh_modkit</Item>"));
        let lua = fs::read_to_string(d.join("client.lua")).unwrap();
        assert!(lua.contains("AddTextEntry('MYVEH_LIV_LSPD1'") && lua.contains("AddTextEntry('MYVEH_LIV_LSPD2'"), "{lua}");
        assert_eq!(lua.matches("CreateThread").count(), 1);
        let mf = fs::read_to_string(d.join("fxmanifest.lua")).unwrap();
        for want in ["data_file 'CARCOLS_FILE' 'data/carcols.meta'", "data_file 'VEHICLE_VARIATION_FILE' 'data/carvariations.meta'", "client_script 'client.lua'"] {
            assert_eq!(mf.matches(want).count(), 1, "{want} in {mf}");
        }
        // the result passes the validator's livery rules (only the missing yft files remain)
        let r = crate::validate::run(&d, crate::validate::Profile::Core);
        let msgs: Vec<_> = r.issues.iter().map(|i| i.msg.clone()).collect();
        assert!(msgs.iter().all(|m| m.contains(".yft not found") || m.contains("vehicles.meta") || m.contains("no variation") || m.contains("missing or unreadable")), "{msgs:?}");
        // list reports indices
        let l = list(&d).unwrap();
        assert_eq!(l[0].liveries.iter().map(|x| x.0).collect::<Vec<_>>(), vec![0, 1]);
        assert_eq!(l[0].linked_to, vec!["myveh"]);
        fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn existing_files_keep_their_formatting() {
        let d = scratch("existing");
        fs::write(d.join("data/carcols.meta"), "<CVehicleModelInfoVarGlobal>\n\t<Kits>\n\t\t<Item>\n\t\t\t<kitName>10_myveh_modkit</kitName>\n\t\t\t<id value=\"10\" />\n\t\t\t<kitType>MKT_SPECIAL</kitType>\n\t\t\t<visibleMods />\n\t\t</Item>\n\t</Kits>\n</CVehicleModelInfoVarGlobal>\n").unwrap();
        fs::write(d.join("data/carvariations.meta"), "<CVehicleModelInfoVariation>\n\t<variationData>\n\t\t<Item>\n\t\t\t<modelName>myveh</modelName>\n\t\t\t<kits />\n\t\t</Item>\n\t</variationData>\n</CVehicleModelInfoVariation>\n").unwrap();
        add(&params(&d, "a", Some(10))).unwrap();
        let cc = fs::read_to_string(d.join("data/carcols.meta")).unwrap();
        assert!(cc.starts_with("<CVehicleModelInfoVarGlobal>\n\t<Kits>\n\t\t<Item>\n\t\t\t<kitName>10_myveh_modkit</kitName>"), "{cc}");
        assert_eq!(liveries_by_kit(&cc).unwrap()[0].2, vec!["myveh_liv_a"]);
        assert!(fs::read_to_string(d.join("data/carvariations.meta")).unwrap().contains("<Item>10_myveh_modkit</Item>"));
        fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn kit_id_must_be_unique() {
        let d = scratch("dup");
        add(&AddParams { pack: &d, vehicle: "aaa", name: "x", display: None, label: None, kit_id: Some(7), yft: None }).unwrap();
        let e = add(&AddParams { pack: &d, vehicle: "bbb", name: "x", display: None, label: None, kit_id: Some(7), yft: None });
        assert!(e.unwrap_err().to_string().contains("already used"));
        fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn append_only_rules() {
        let mk = |ls: &[&str], id: &str| format!("<C><Kits><Item><kitName>k</kitName><id value=\"{id}\"/><visibleMods>{}</visibleMods></Item></Kits></C>", ls.iter().map(|l| format!("<Item><modelName>{l}</modelName><type>VMT_LIVERY_MOD</type></Item>")).collect::<String>());
        let old = mk(&["a", "b"], "1");
        assert!(append_only(&old, &mk(&["a", "b", "c"], "1")).unwrap().is_empty());
        assert!(append_only(&old, &mk(&["b", "a"], "1")).unwrap().iter().any(|m| m.contains("reordered")));
        assert!(append_only(&old, &mk(&["x", "a", "b"], "1")).unwrap().len() >= 2);
        assert!(append_only(&old, &mk(&["a"], "1")).unwrap().iter().any(|m| m.contains("removed")));
        assert!(append_only(&old, &mk(&["a", "b"], "2")).unwrap().iter().any(|m| m.contains("id changed")));
    }
}
