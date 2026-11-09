//! The files a clothing DLC needs besides its YMT and shop meta, both ways (written at build, read when a pack is opened):
//!
//! - **creature metadata** (`mp_creaturemetadata_<dlc>.ymt`, RBF0): turns on the expression tracks that make high heels lift the
//!   ped and hats squash the hair. The heel height and squash themselves are the drawables' YMT expression values; this file
//!   only says which feet and hats have the track. Packs (GTA World's, Rockstar's) list every feet and every `p_head` number of
//!   the DLC, for both sexes in one file, and so does the build.
//! - **pedalternatevariations** (`ALTERNATE_VARIATIONS_FILE`): while one of some drawables is worn, another shows an alternate
//!   model (hands under a jacket, hair under a mask). Switches read from a pack are written back, following their drawables
//!   through renumbering; masks marked "hides hair" add the hair switches.
//! - **first_person_alternates** (`PED_FIRST_PERSON_ALTERNATE_DATA`, as Rockstar's content.xml registers it): the alternate
//!   model a drawable shows in first person.
use super::{parse_name, vanilla_alts, AltSwitch, AssetRef, Kind, OtherFirstPerson, Project, Role, Sex};
use anyhow::{bail, Context, Result};
use rage_formats::rbf::{self, RbfStructure, RbfValue};

// ---- creature metadata -------------------------------------------------------------------------------------------------

/// One expression entry of a creature metadata file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expression {
    /// in `pedPropExpressions` (a prop) or `pedCompExpressions` (a component)
    pub prop: bool,
    /// `pedCompID` / `pedPropID`: the component or anchor
    pub slot: u32,
    /// `pedCompVarIndex` / `pedPropVarIndex`: the drawable number
    pub var: u32,
    /// `pedCompExpressionIndex` / `pedPropExpressionIndex`: which of the five expression values drives it
    pub index: u32,
    pub tracks: Vec<u8>,
    /// little-endian 16-bit ids
    pub ids: Vec<u8>,
    pub types: Vec<u8>,
    pub components: Vec<u8>,
}

impl Expression {
    /// high heels: feet (component 6), driven by expression value 4, bone track 33 / id 28462
    pub fn heels(var: u32) -> Self {
        Self { prop: false, slot: 6, var, index: 4, tracks: vec![33], ids: 28462u16.to_le_bytes().to_vec(), types: vec![2], components: vec![1] }
    }

    /// a hat squashing the hair: `p_head` (anchor 0), driven by expression value 0, track 33 / id 13201
    pub fn hat(var: u32) -> Self {
        Self { prop: true, slot: 0, var, index: 0, tracks: vec![33], ids: 13201u16.to_le_bytes().to_vec(), types: vec![2], components: vec![1] }
    }
}

/// The creature metadata of DLC `dlc`: every feet number and every `p_head` number it has, over both sexes. Empty when it has
/// neither (then no file is written; the shop meta still names it, as packs do).
pub fn creature_expressions(p: &Project, dlc: usize) -> Vec<Expression> {
    if let Some(game) = p.dlcs.get(dlc).and_then(|d| d.game_creature.as_ref()) {
        // a Rockstar DLC from the game: its own entries as they are, then one for each feet / p_head of ours it has none for
        let mut out = game.clone();
        let mut ours: Vec<(Kind, u32)> = p.drawables.iter().filter(|d| d.dlc == dlc && !d.ydd.as_os_str().is_empty() && matches!(d.kind, Kind::Component(6) | Kind::Prop(0))).map(|d| (d.kind, d.number)).collect();
        ours.sort();
        ours.dedup();
        for (kind, n) in ours {
            let e = if kind == Kind::Prop(0) { Expression::hat(n) } else { Expression::heels(n) };
            if !game.iter().any(|g| g.prop == e.prop && g.slot == e.slot && g.var == e.var) {
                out.push(e);
            }
        }
        return out;
    }
    let upto = |kind: Kind| p.drawables.iter().filter(|d| d.dlc == dlc && d.kind == kind).map(|d| d.number + 1).max().unwrap_or(0);
    let feet = (0..upto(Kind::Component(6))).map(Expression::heels);
    let hats = (0..upto(Kind::Prop(0))).map(Expression::hat);
    feet.chain(hats).collect()
}

fn array(name: &str, content: &str, bytes: &[u8]) -> (String, RbfValue) {
    (name.to_string(), RbfValue::Structure(RbfStructure { name: name.to_string(), attributes: vec![("content".into(), RbfValue::Str(content.into()))], children: vec![(String::new(), RbfValue::Bytes(bytes.to_vec()))] }))
}

/// The creature metadata file (RBF0), as the game's and GTA World's are laid out.
pub fn creature_bytes(list: &[Expression]) -> Result<Vec<u8>> {
    let section = |prop: bool| {
        let (id, var, index) = if prop { ("pedPropID", "pedPropVarIndex", "pedPropExpressionIndex") } else { ("pedCompID", "pedCompVarIndex", "pedCompExpressionIndex") };
        let items = list.iter().filter(|e| e.prop == prop).map(|e| {
            let s = RbfStructure {
                name: "Item".into(),
                attributes: vec![],
                children: vec![(id.into(), RbfValue::Uint(e.slot)), (var.into(), RbfValue::Uint(e.var)), (index.into(), RbfValue::Uint(e.index)), array("tracks", "char_array", &e.tracks), array("ids", "short_array", &e.ids), array("types", "char_array", &e.types), array("components", "char_array", &e.components)],
            };
            ("Item".to_string(), RbfValue::Structure(s))
        });
        let name = if prop { "pedPropExpressions" } else { "pedCompExpressions" };
        (name.to_string(), RbfValue::Structure(RbfStructure { name: name.into(), attributes: vec![], children: items.collect() }))
    };
    let root = RbfStructure { name: "CCreatureMetaData".into(), attributes: vec![], children: vec![section(false), section(true)] };
    let bytes = rbf::write(&root)?;
    // read back: what was meant
    if read_creature(&bytes)? != list {
        bail!("the creature metadata reads back differently");
    }
    Ok(bytes)
}

/// Read a creature metadata file.
pub fn read_creature(bytes: &[u8]) -> Result<Vec<Expression>> {
    let root = rbf::parse(bytes).context("reading the creature metadata")?;
    if root.name != "CCreatureMetaData" {
        bail!("not creature metadata (the root is {})", root.name);
    }
    let mut out = vec![];
    for (section, prop) in [("pedCompExpressions", false), ("pedPropExpressions", true)] {
        let Some(s) = root.child(section) else { continue };
        let (id, var, index) = if prop { ("pedPropID", "pedPropVarIndex", "pedPropExpressionIndex") } else { ("pedCompID", "pedCompVarIndex", "pedCompExpressionIndex") };
        for item in s.children_named("Item") {
            let uint = |n: &str| item.children.iter().find_map(|(k, v)| if k == n { if let RbfValue::Uint(x) = v { Some(*x) } else { None } } else { None }).with_context(|| format!("an expression without {n}"));
            let bytes = |n: &str| item.child(n).and_then(|c| c.children.iter().find_map(|(_, v)| if let RbfValue::Bytes(b) = v { Some(b.clone()) } else { None })).unwrap_or_default();
            out.push(Expression { prop, slot: uint(id)?, var: uint(var)?, index: uint(index)?, tracks: bytes("tracks"), ids: bytes("ids"), types: bytes("types"), components: bytes("components") });
        }
    }
    Ok(out)
}

// ---- alternate variations ----------------------------------------------------------------------------------------------

/// A drawable as an alternate-variations file names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawRef {
    pub dlc: String,
    pub component: u8,
    pub index: u32,
}

/// A switch as the file has it, before it is matched with the project's drawables.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawSwitch {
    pub sex: Sex,
    pub target: RawRef,
    pub alt: u32,
    pub sources: Vec<RawRef>,
}

fn ped_sex(name: &str) -> Option<Sex> {
    match name.trim().to_lowercase().as_str() {
        "mp_m_freemode_01" => Some(Sex::Male),
        "mp_f_freemode_01" => Some(Sex::Female),
        _ => None,
    }
}

fn child<'a, 'i>(n: roxmltree::Node<'a, 'i>, name: &str) -> Option<roxmltree::Node<'a, 'i>> {
    n.children().find(|c| c.has_tag_name(name))
}

fn value(n: roxmltree::Node, name: &str) -> Option<u32> {
    child(n, name)?.attribute("value")?.trim().parse().ok()
}

fn raw_ref(n: roxmltree::Node) -> Option<RawRef> {
    Some(RawRef { dlc: child(n, "dlcNameHash").and_then(|d| d.text()).unwrap_or("").trim().to_string(), component: value(n, "component")? as u8, index: value(n, "index")? })
}

/// Read a pedalternatevariations file. Switches of peds other than the freemode ones, and entries that cannot be read, are
/// reported.
pub fn read_alt_variations(xml: &str) -> Result<(Vec<RawSwitch>, Vec<String>)> {
    let doc = roxmltree::Document::parse(xml).context("pedalternatevariations is not valid XML")?;
    let root = doc.root_element();
    if !root.has_tag_name("CAlternateVariations") {
        bail!("not an alternate variations file (the root is {})", root.tag_name().name());
    }
    let (mut out, mut odd) = (vec![], vec![]);
    for ped in child(root, "peds").into_iter().flat_map(|p| p.children().filter(|c| c.has_tag_name("Item"))) {
        let name = child(ped, "name").and_then(|n| n.text()).unwrap_or("").to_string();
        let Some(sex) = ped_sex(&name) else {
            odd.push(format!("switches for ped {name:?} (only the freemode peds are kept)"));
            continue;
        };
        for sw in child(ped, "switches").into_iter().flat_map(|s| s.children().filter(|c| c.has_tag_name("Item"))) {
            let (Some(target), Some(alt)) = (raw_ref(sw), value(sw, "alt")) else {
                odd.push(format!("a {name} switch that cannot be read"));
                continue;
            };
            let sources = child(sw, "sourceAssets").into_iter().flat_map(|s| s.children().filter(|c| c.has_tag_name("Item"))).filter_map(raw_ref).collect();
            out.push(RawSwitch { sex, target, alt, sources });
        }
    }
    Ok((out, odd))
}

/// a reference matched with the project: one of its components when the DLC name (in any case), component and number fit
pub fn resolve(p: &Project, sex: Sex, r: &RawRef) -> AssetRef {
    let ours = p.drawables.iter().find(|d| d.sex == sex && d.kind == Kind::Component(r.component) && d.number == r.index && p.dlc_name(d.dlc, sex).eq_ignore_ascii_case(&r.dlc));
    match ours {
        Some(d) => AssetRef::Ours(d.id),
        None => AssetRef::Other { dlc: r.dlc.clone(), component: r.component, index: r.index },
    }
}

/// Add switches read from a file to the project, as the file of DLC `file`.
pub fn add_switches(p: &mut Project, file: usize, raws: &[RawSwitch]) {
    let switches: Vec<AltSwitch> = raws.iter().map(|w| AltSwitch { file, sex: w.sex, target: resolve(p, w.sex, &w.target), alt: w.alt, sources: w.sources.iter().map(|s| resolve(p, w.sex, s)).collect() }).collect();
    p.alt_switches.extend(switches);
}

/// the reference as the file writes it, or `None` when it points at a drawable the project no longer has
fn unresolve(p: &Project, r: &AssetRef) -> Option<RawRef> {
    match r {
        AssetRef::Other { dlc, component, index } => Some(RawRef { dlc: dlc.clone(), component: *component, index: *index }),
        AssetRef::Ours(id) => {
            let d = p.find(*id)?;
            let Kind::Component(c) = d.kind else { return None };
            Some(RawRef { dlc: p.dlc_name(d.dlc, d.sex), component: c, index: d.number })
        }
    }
}

/// The hide-hair switches of DLC `dlc`: while one of its masks marked "hides hair" is worn, every hair of the game (and every
/// hair of this DLC that has an `_1` model) shows its alternate 1.
pub fn hide_hair_switches(p: &Project, dlc: usize) -> Vec<AltSwitch> {
    let mut out = vec![];
    for sex in [Sex::Male, Sex::Female] {
        let masks: Vec<AssetRef> = p.drawables.iter().filter(|d| d.dlc == dlc && d.sex == sex && d.kind == Kind::Component(1) && d.meta.hides_hair).map(|d| AssetRef::Ours(d.id)).collect();
        if masks.is_empty() {
            continue;
        }
        let hairs = if sex == Sex::Male { vanilla_alts::MALE_HAIRS } else { vanilla_alts::FEMALE_HAIRS };
        for (name, indices) in hairs {
            for i in *indices {
                out.push(AltSwitch { file: dlc, sex, target: AssetRef::Other { dlc: name.to_string(), component: 2, index: *i }, alt: 1, sources: masks.clone() });
            }
        }
        for h in p.drawables.iter().filter(|d| d.dlc == dlc && d.sex == sex && d.kind == Kind::Component(2) && d.alternates.iter().any(|(n, _)| *n == 1)) {
            out.push(AltSwitch { file: dlc, sex, target: AssetRef::Ours(h.id), alt: 1, sources: masks.clone() });
        }
    }
    out
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// The switches the pedalternatevariations file of DLC `dlc` holds, as written: the ones read from a pack, then the hide-hair
/// ones. Sources that point at drawables the project no longer has are left out, and switches with no target too; each is
/// one line of the returned problems.
pub fn alt_switches_of(p: &Project, dlc: usize) -> (Vec<RawSwitch>, Vec<String>) {
    let (mut out, mut problems) = (vec![], vec![]);
    for w in p.alt_switches.iter().filter(|w| w.file == dlc).cloned().chain(hide_hair_switches(p, dlc)) {
        let Some(target) = unresolve(p, &w.target) else {
            problems.push(format!("an alternate variation of a removed drawable was left out ({} {})", w.sex.label(), p.dlc_label(dlc)));
            continue;
        };
        let sources: Vec<RawRef> = w.sources.iter().filter_map(|s| unresolve(p, s)).collect();
        if sources.len() != w.sources.len() {
            problems.push(format!("{} removed drawable(s) were left out of the switch of {} {} {:03}", w.sources.len() - sources.len(), target.dlc, target.component, target.index));
        }
        out.push(RawSwitch { sex: w.sex, target, alt: w.alt, sources });
    }
    (out, problems)
}

/// The pedalternatevariations file of DLC `dlc`, or `None` when it has no switches.
pub fn alt_variations_xml(switches: &[RawSwitch]) -> Option<String> {
    if switches.is_empty() {
        return None;
    }
    let mut x = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<CAlternateVariations>\n    <peds>\n");
    let r = |x: &mut String, pad: &str, r: &RawRef| {
        x.push_str(&format!("{pad}<dlcNameHash>{}</dlcNameHash>\n{pad}<component value=\"{}\" />\n{pad}<index value=\"{}\" />\n", esc(&r.dlc), r.component, r.index));
    };
    for sex in [Sex::Male, Sex::Female] {
        let mine: Vec<&RawSwitch> = switches.iter().filter(|w| w.sex == sex).collect();
        if mine.is_empty() {
            continue;
        }
        x.push_str(&format!("        <Item>\n            <name>{}</name>\n            <switches>\n", sex.ped()));
        for w in mine {
            x.push_str("                <Item>\n");
            r(&mut x, "                    ", &w.target);
            x.push_str(&format!("                    <alt value=\"{}\" />\n", w.alt));
            x.push_str("                    <sourceAssets>\n");
            for s in &w.sources {
                x.push_str("                        <Item>\n");
                r(&mut x, "                            ", s);
                x.push_str("                        </Item>\n");
            }
            x.push_str("                    </sourceAssets>\n                </Item>\n");
        }
        x.push_str("            </switches>\n        </Item>\n");
    }
    x.push_str("    </peds>\n</CAlternateVariations>\n");
    Some(x)
}

// ---- first-person alternates -------------------------------------------------------------------------------------------

/// Read a first_person_alternates file: (asset name, alternate number).
pub fn read_first_person(xml: &str) -> Result<Vec<(String, u32)>> {
    let doc = roxmltree::Document::parse(xml).context("first_person_alternates is not valid XML")?;
    let root = doc.root_element();
    if !root.has_tag_name("FirstPersonAlternateData") {
        bail!("not a first-person alternates file (the root is {})", root.tag_name().name());
    }
    Ok(child(root, "alternates")
        .into_iter()
        .flat_map(|a| a.children().filter(|c| c.has_tag_name("Item")))
        .filter_map(|i| Some((child(i, "assetName")?.text()?.trim().to_string(), value(i, "alternate")?)))
        .collect())
}

/// The game's asset name of a drawable: `MP_M_Freemode_01_<dlc>/jbib_061_u` (props `..._p_<dlc>/p_head_003`).
pub fn asset_name(p: &Project, d: &super::Drawable) -> String {
    let ped = if d.sex == Sex::Male { "MP_M_Freemode_01" } else { "MP_F_Freemode_01" };
    let file = d.kind.drawable_file(d.number, &d.race);
    format!("{ped}_{}{}/{}", if d.kind.is_prop() { "p_" } else { "" }, p.dlc_name(d.dlc, d.sex), file.trim_end_matches(".ydd"))
}

/// Put first-person entries read from the file of DLC `file` into the project: on the drawable they name, or kept as they are.
pub fn add_first_person(p: &mut Project, file: usize, entries: &[(String, u32)]) {
    for (asset, alt) in entries {
        let found = (|| {
            let (left, right) = asset.split_once('/')?;
            let left = left.to_lowercase();
            let (sex, rest) = if let Some(r) = left.strip_prefix("mp_m_freemode_01_") { (Sex::Male, r) } else { (Sex::Female, left.strip_prefix("mp_f_freemode_01_")?) };
            let (kind, number) = match parse_name(&format!("{right}.ydd"))? {
                Role::Drawable { kind, number, .. } => (kind, number),
                _ => return None,
            };
            let dlc = if kind.is_prop() { rest.strip_prefix("p_")? } else { rest };
            p.drawables.iter().position(|d| d.sex == sex && d.kind == kind && d.number == number && p.dlc_name(d.dlc, sex).eq_ignore_ascii_case(dlc))
        })();
        match found {
            Some(i) if p.drawables[i].meta.first_person.is_none() => p.drawables[i].meta.first_person = Some(*alt),
            _ => p.other_first_person.push(OtherFirstPerson { file, asset: asset.clone(), alt: *alt }),
        }
    }
}

/// The first-person entries of DLC `dlc`'s file, as written: its drawables', then the ones kept from a pack.
pub fn first_person_of(p: &Project, dlc: usize) -> Vec<(String, u32)> {
    let mut mine: Vec<&super::Drawable> = p.drawables.iter().filter(|d| d.dlc == dlc && d.meta.first_person.is_some()).collect();
    mine.sort_by_key(|d| d.slot());
    mine.iter().map(|d| (asset_name(p, d), d.meta.first_person.unwrap_or(1))).chain(p.other_first_person.iter().filter(|o| o.file == dlc).map(|o| (o.asset.clone(), o.alt))).collect()
}

/// The first_person_alternates file, or `None` when there is nothing in it.
pub fn first_person_xml(entries: &[(String, u32)]) -> Option<String> {
    if entries.is_empty() {
        return None;
    }
    let mut x = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<FirstPersonAlternateData>\n  <alternates>\n");
    for (asset, alt) in entries {
        x.push_str(&format!("    <Item>\n      <assetName>{}</assetName>\n      <alternate value=\"{alt}\" />\n    </Item>\n", esc(asset)));
    }
    x.push_str("  </alternates>\n</FirstPersonAlternateData>\n");
    Some(x)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clothing::{Dlc, Drawable, DrawableMeta};

    fn dr(sex: Sex, kind: Kind, number: u32) -> Drawable {
        Drawable { sex, kind, number, race: if kind.is_prop() { String::new() } else { "u".into() }, ..Default::default() }
    }

    #[test]
    fn creature_metadata_lists_every_feet_and_hat_of_both_sexes_and_reads_back() {
        let mut p = Project::default();
        p.drawables = vec![dr(Sex::Male, Kind::Component(6), 0), dr(Sex::Male, Kind::Component(6), 1), dr(Sex::Female, Kind::Component(6), 0), dr(Sex::Female, Kind::Prop(0), 2), dr(Sex::Male, Kind::Component(11), 5)];
        let e = creature_expressions(&p, 0);
        assert_eq!(e.iter().map(|x| (x.prop, x.slot, x.var)).collect::<Vec<_>>(), [(false, 6, 0), (false, 6, 1), (true, 0, 0), (true, 0, 1), (true, 0, 2)]);
        assert_eq!((e[0].ids.clone(), e[2].ids.clone()), (vec![0x2E, 0x6F], vec![0x91, 0x33]), "the ids GTA World's and Rockstar's files use");
        let bytes = creature_bytes(&e).unwrap();
        assert_eq!(&bytes[..4], b"RBF0");
        assert_eq!(read_creature(&bytes).unwrap(), e);
        assert!(creature_expressions(&Project::default(), 0).is_empty());
    }

    #[test]
    fn switches_follow_their_drawables_and_masks_that_hide_hair_add_the_hair_switches() {
        let mut p = Project { dlcs: vec![Dlc { names: Some(("mp_m_x1".into(), "mp_f_x1".into())), ..Default::default() }], ..Default::default() };
        let jacket = dr(Sex::Male, Kind::Component(11), 3);
        let jid = jacket.id;
        p.drawables = vec![jacket, dr(Sex::Male, Kind::Component(11), 4)];
        let raws = vec![RawSwitch { sex: Sex::Male, target: RawRef { dlc: "male_heist".into(), component: 5, index: 0 }, alt: 19, sources: vec![RawRef { dlc: "MP_M_X1".into(), component: 11, index: 3 }, RawRef { dlc: "mp_m_x1".into(), component: 11, index: 99 }] }];
        add_switches(&mut p, 0, &raws);
        assert_eq!(p.alt_switches[0].sources, [AssetRef::Ours(jid), AssetRef::Other { dlc: "mp_m_x1".into(), component: 11, index: 99 }], "ours by id (any case), unknown kept as written");
        // jbib 003 moves to 000: the switch follows
        let i = p.drawables.iter().position(|d| d.id == jid).unwrap();
        p.move_to(i, (0, Sex::Male, Kind::Component(11), 0)).unwrap();
        let (written, problems) = alt_switches_of(&p, 0);
        assert!(problems.is_empty());
        assert_eq!(written[0].sources[0], RawRef { dlc: "mp_m_x1".into(), component: 11, index: 0 });
        let xml = alt_variations_xml(&written).unwrap();
        assert_eq!(read_alt_variations(&xml).unwrap(), (written.clone(), vec![]));
        // removed: left out, and said
        p.drawables.retain(|d| d.id != jid);
        assert_eq!(alt_switches_of(&p, 0).1.len(), 1);
        // a mask that hides hair: every game hair, and our hair with an _1 model
        let mut mask = dr(Sex::Female, Kind::Component(1), 0);
        mask.meta = DrawableMeta { hides_hair: true, ..Default::default() };
        let mut hair = dr(Sex::Female, Kind::Component(2), 0);
        hair.alternates = vec![(1, "h1.ydd".into())];
        p.drawables.extend([mask, hair, dr(Sex::Female, Kind::Component(2), 1)]);
        let hh = hide_hair_switches(&p, 0);
        let game: usize = vanilla_alts::FEMALE_HAIRS.iter().map(|x| x.1.len()).sum();
        assert_eq!(hh.len(), game + 1, "the game's hairs and the one hair with an _1 model");
        assert!(hh.iter().all(|w| w.alt == 1 && w.sources.len() == 1));
    }

    #[test]
    fn first_person_entries_land_on_their_drawable_or_are_kept() {
        let mut p = Project { dlcs: vec![Dlc { names: Some(("mp_m_x3".into(), "mp_f_x3".into())), ..Default::default() }], ..Default::default() };
        let mut j = dr(Sex::Male, Kind::Component(11), 61);
        j.alternates = vec![(1, "a.ydd".into())];
        p.drawables = vec![j, dr(Sex::Male, Kind::Prop(0), 2)];
        let read = vec![("MP_M_Freemode_01_mp_m_x3/jbib_061_u".to_string(), 1), ("MP_M_Freemode_01_p_mp_m_x3/p_head_002".to_string(), 1), ("MP_F_Freemode_01_mp_m_x3/jbib_009_u".to_string(), 1)];
        add_first_person(&mut p, 0, &read);
        assert_eq!((p.drawables[0].meta.first_person, p.drawables[1].meta.first_person, p.other_first_person.len()), (Some(1), Some(1), 1));
        let written = first_person_of(&p, 0);
        let mut a = written.clone();
        a.sort();
        let mut b = read.clone();
        b.sort();
        assert_eq!(a, b);
        assert_eq!(read_first_person(&first_person_xml(&written).unwrap()).unwrap(), written);
    }

    /// GTA World's control resource (set CRCK_CLOTHING_CONTROL to it): every creature metadata file is what the build writes for
    /// its DLC's YMTs, every alternate variations and first-person file reads and writes back the same.
    #[test]
    #[ignore = "needs a pack: set CRCK_CLOTHING_CONTROL=<control resource folder> and run with --ignored"]
    fn real_pack_extra_files_are_written_again_as_they_were() {
        use crate::clothing::ymt::read_ymt;
        let dir = std::path::PathBuf::from(std::env::var("CRCK_CLOTHING_CONTROL").expect("CRCK_CLOTHING_CONTROL"));
        let stream = dir.join("stream");
        let mut checked = 0;
        for e in std::fs::read_dir(&stream).unwrap().flatten() {
            let name = e.file_name().to_string_lossy().to_lowercase();
            let Some(label) = name.strip_prefix("mp_creaturemetadata_").and_then(|n| n.strip_suffix(".ymt")) else { continue };
            let bytes = std::fs::read(e.path()).unwrap();
            let list = read_creature(&bytes).unwrap();
            assert_eq!(creature_bytes(&list).unwrap(), bytes, "{name}: written back byte for byte");
            // the counts of the DLC's two YMTs give the same file (mp2023_02 is the YMTs' 2023_02)
            let short = label.strip_prefix("mp").filter(|s| s.starts_with("20")).unwrap_or(label);
            let mut p = Project::default();
            for (sex, s) in [(Sex::Male, "m"), (Sex::Female, "f")] {
                let ymt = stream.join(format!("mp_{s}_freemode_01_mp_{s}_{short}.ymt"));
                let Ok(b) = std::fs::read(&ymt) else { continue };
                let (y, _, _) = read_ymt(&b).unwrap();
                for (kind, n, _) in y.entries() {
                    p.drawables.push(dr(sex, kind, n));
                }
            }
            assert_eq!(creature_bytes(&creature_expressions(&p, 0)).unwrap(), bytes, "{name}: the build writes the same from the YMTs");
            checked += 1;
        }
        for e in std::fs::read_dir(dir.join("data")).unwrap().flatten() {
            let name = e.file_name().to_string_lossy().to_lowercase();
            let text = std::fs::read_to_string(e.path()).unwrap();
            if name.starts_with("pedalternatevariations") {
                let (raws, odd) = read_alt_variations(&text).unwrap();
                assert!(odd.is_empty(), "{name}: {odd:?}");
                let mut p = Project::default();
                add_switches(&mut p, 0, &raws);
                let (written, problems) = alt_switches_of(&p, 0);
                assert!(problems.is_empty());
                assert_eq!(read_alt_variations(&alt_variations_xml(&written).unwrap()).unwrap().0, raws, "{name}");
                println!("{name}: {} switches", raws.len());
                checked += 1;
            } else if name.starts_with("first_person_alternates") {
                let read = read_first_person(&text).unwrap();
                let mut p = Project::default();
                add_first_person(&mut p, 0, &read);
                assert_eq!(read_first_person(&first_person_xml(&first_person_of(&p, 0)).unwrap()).unwrap(), read, "{name}");
                checked += 1;
            }
        }
        println!("{checked} file(s) written again as they were");
        assert!(checked > 0);
    }
}
