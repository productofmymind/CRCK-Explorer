//! Building a clothing pack for FiveM: the YMT (`CPedVariationInfo`), the shop meta (`ShopPedApparel`), the streamed files with
//! their game names, and `fxmanifest.lua`. `plan_build` is the preview (every file that would be written, every problem);
//! `build` writes it and refuses to overwrite anything.
//!
//! The YMT is written as the reference tool-layout XML, turned into a Meta file by `rage-formats`, then **read back and compared** with what
//! was meant, so a file that would read differently is never written. Its shape was checked against the game's own freemode YMTs
//!; it has not been loaded by the game or FiveM.
use super::{file_of, validate, Drawable, Issue, Kind, Project, Sev, Sex};
use anyhow::{bail, Context, Result};
use rage_formats::{build_meta, dump_meta, from_xml, rage_joaat, to_xml, MetaValue, NameTable, Schema};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

// ---- the shape of a YMT ---------------------------------------------------------------------------------------------------

/// What a YMT says about the drawables of one sex: per component the texture count of each drawable, per prop anchor the texture
/// count of each prop. Everything else in the file follows from this.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Shape {
    /// component index -> per drawable: (texture count, owns cloth)
    pub components: BTreeMap<u8, Vec<(usize, bool)>>,
    /// anchor index -> per prop: texture count
    pub props: BTreeMap<u8, Vec<usize>>,
}

impl Shape {
    /// from drawables with consecutive numbers `0..n` per kind (already local to one addon and one sex)
    pub fn of(drawables: &[&Drawable]) -> Shape {
        let mut s = Shape::default();
        let mut sorted: Vec<&&Drawable> = drawables.iter().collect();
        sorted.sort_by_key(|d| (d.kind, d.number));
        for d in sorted {
            match d.kind {
                Kind::Component(i) => s.components.entry(i).or_default().push((d.textures.len(), d.cloth.is_some())),
                Kind::Prop(i) => s.props.entry(i).or_default().push(d.textures.len()),
            }
        }
        s
    }

    /// read it back out of a YMT tree (ours or the game's)
    pub fn of_tree(root: &MetaValue) -> Shape {
        let mut s = Shape::default();
        let Some(r) = root.as_struct() else { return s };
        let avail: Vec<u8> = match r.field("availComp") {
            Some(MetaValue::Bytes(b)) => b.clone(),
            _ => vec![],
        };
        let comps = r.field("aComponentData3").map(|v| v.items()).unwrap_or(&[]);
        for (i, a) in avail.iter().enumerate() {
            if *a == 255 {
                continue;
            }
            let Some(c) = comps.get(*a as usize).and_then(|c| c.as_struct()) else { continue };
            let list = c.field("aDrawblData3").map(|v| v.items()).unwrap_or(&[]);
            let drawables = list
                .iter()
                .filter_map(|d| d.as_struct())
                .map(|d| {
                    let tex = d.field("aTexData").map(|t| t.items().len()).unwrap_or(0);
                    let cloth = matches!(d.field("clothData").and_then(|c| c.as_struct()).and_then(|c| c.field("ownsCloth")), Some(MetaValue::Bool(true)));
                    (tex, cloth)
                })
                .collect();
            s.components.insert(i as u8, drawables);
        }
        if let Some(info) = r.field("propInfo").and_then(|p| p.as_struct()) {
            for a in info.field("aAnchors").map(|v| v.items()).unwrap_or(&[]).iter().filter_map(|a| a.as_struct()) {
                // the anchor is an enum; its index is the order of the anchor names
                let name = match a.field("anchor") {
                    Some(MetaValue::Enum { name: Some(h), .. }) => NameTable::core().resolve(*h).to_string(),
                    Some(MetaValue::Str(t)) => t.clone(),
                    _ => continue,
                };
                let Some(idx) = ANCHOR_ENUM.iter().position(|e| *e == name) else { continue };
                let counts: Vec<usize> = match a.field("props") {
                    Some(MetaValue::Array(arr)) => arr.items.iter().filter_map(|x| x.as_i64()).map(|x| x as usize).collect(),
                    Some(MetaValue::Bytes(b)) => b.iter().map(|x| *x as usize).collect(),
                    _ => vec![],
                };
                s.props.insert(idx as u8, counts);
            }
        }
        s
    }
}

/// the game's names for the prop anchor points, in anchor order
pub const ANCHOR_ENUM: [&str; 9] = ["ANCHOR_HEAD", "ANCHOR_EYES", "ANCHOR_EARS", "ANCHOR_MOUTH", "ANCHOR_LEFT_HAND", "ANCHOR_RIGHT_HAND", "ANCHOR_LEFT_WRIST", "ANCHOR_RIGHT_WRIST", "ANCHOR_HIP"];

/// The YMT as XML in the reference tool's layout. `dlc_name` is the pack name (hashed in lowercase).
pub fn ymt_xml(shape: &Shape, dlc_name: &str) -> String {
    let mut x = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<CPedVariationInfo>\n");
    let mut line = |depth: usize, text: &str| {
        x.push_str(&"  ".repeat(depth));
        x.push_str(text);
        x.push('\n');
    };
    line(1, "<bHasTexVariations value=\"true\"/>");
    line(1, "<bHasDrawblVariations value=\"true\"/>");
    line(1, "<bHasLowLODs value=\"false\"/>");
    line(1, "<bIsSuperLOD value=\"false\"/>");
    // position of each present component in aComponentData3, FF when absent
    let mut avail = [255u8; 12];
    for (pos, comp) in shape.components.keys().enumerate() {
        avail[*comp as usize] = pos as u8;
    }
    line(1, "<availComp itemType=\"ByteArray\">");
    line(2, &avail.iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(" "));
    line(1, "</availComp>");
    if shape.components.is_empty() {
        line(1, "<aComponentData3 itemType=\"CPVComponentData\"/>");
    } else {
        line(1, "<aComponentData3 itemType=\"CPVComponentData\">");
        for drawables in shape.components.values() {
            line(2, "<Item>");
            // a byte: the game's own files store the sum modulo 256 when a component has more than 255 textures (74 of them do)
            line(3, &format!("<numAvailTex value=\"{}\"/>", drawables.iter().map(|d| d.0).sum::<usize>() % 256));
            line(3, "<aDrawblData3 itemType=\"CPVDrawblData\">");
            for (tex, cloth) in drawables {
                line(4, "<Item>");
                line(5, "<propMask value=\"1\"/>");
                line(5, "<numAlternatives value=\"0\"/>");
                if *tex == 0 {
                    line(5, "<aTexData itemType=\"CPVTextureData\"/>");
                } else {
                    line(5, "<aTexData itemType=\"CPVTextureData\">");
                    for _ in 0..*tex {
                        line(6, "<Item>");
                        line(7, "<texId value=\"0\"/>");
                        line(7, "<distribution value=\"255\"/>");
                        line(6, "</Item>");
                    }
                    line(5, "</aTexData>");
                }
                line(5, "<clothData>");
                line(6, &format!("<ownsCloth value=\"{cloth}\"/>"));
                line(5, "</clothData>");
                line(4, "</Item>");
            }
            line(3, "</aDrawblData3>");
            line(2, "</Item>");
        }
        line(1, "</aComponentData3>");
    }
    line(1, "<aSelectionSets itemType=\"CPedSelectionSet\"/>");
    if shape.components.is_empty() {
        line(1, "<compInfos itemType=\"CComponentInfo\"/>");
    } else {
        line(1, "<compInfos itemType=\"CComponentInfo\">");
        for (comp, drawables) in &shape.components {
            for n in 0..drawables.len() {
                line(2, "<Item>");
                line(3, "<pedXml_audioID>none</pedXml_audioID>");
                line(3, "<pedXml_audioID2>none</pedXml_audioID2>");
                line(3, "<pedXml_expressionMods itemType=\"float\">");
                line(4, "0 0 0 0 0");
                line(3, "</pedXml_expressionMods>");
                line(3, "<flags value=\"0\"/>");
                line(3, "<inclusions/>");
                line(3, "<exclusions/>");
                line(3, "<pedXml_vfxComps/>");
                line(3, "<pedXml_flags value=\"0\"/>");
                line(3, &format!("<pedXml_compIdx value=\"{comp}\"/>"));
                line(3, &format!("<pedXml_drawblIdx value=\"{n}\"/>"));
                line(2, "</Item>");
            }
        }
        line(1, "</compInfos>");
    }
    line(1, "<propInfo>");
    let n_props: usize = shape.props.values().map(Vec::len).sum();
    line(2, &format!("<numAvailProps value=\"{n_props}\"/>"));
    if n_props == 0 {
        line(2, "<aPropMetaData itemType=\"CPedPropMetaData\"/>");
    } else {
        line(2, "<aPropMetaData itemType=\"CPedPropMetaData\">");
        for (anchor, props) in &shape.props {
            for (n, tex) in props.iter().enumerate() {
                line(3, "<Item>");
                line(4, "<audioId>none</audioId>");
                line(4, "<expressionMods itemType=\"float\">");
                line(5, "0 0 0 0 0");
                line(4, "</expressionMods>");
                if *tex == 0 {
                    line(4, "<texData itemType=\"CPedPropTexData\"/>");
                } else {
                    line(4, "<texData itemType=\"CPedPropTexData\">");
                    for t in 0..*tex {
                        line(5, "<Item>");
                        line(6, "<inclusions/>");
                        line(6, "<exclusions/>");
                        line(6, &format!("<texId value=\"{t}\"/>"));
                        line(6, "<inclusionId value=\"0\"/>");
                        line(6, "<exclusionId value=\"0\"/>");
                        line(6, "<distribution value=\"255\"/>");
                        line(5, "</Item>");
                    }
                    line(4, "</texData>");
                }
                line(4, "<renderFlags/>");
                line(4, "<propFlags value=\"0\"/>");
                line(4, "<flags value=\"0\"/>");
                line(4, &format!("<anchorId value=\"{anchor}\"/>"));
                line(4, &format!("<propId value=\"{n}\"/>"));
                line(4, "<stickyness value=\"0\"/>");
                line(3, "</Item>");
            }
        }
        line(2, "</aPropMetaData>");
    }
    if shape.props.is_empty() {
        line(2, "<aAnchors itemType=\"CAnchorProps\"/>");
    } else {
        line(2, "<aAnchors itemType=\"CAnchorProps\">");
        for (anchor, props) in &shape.props {
            line(3, "<Item>");
            line(4, "<props itemType=\"byte\">");
            line(5, &props.iter().map(|t| t.to_string()).collect::<Vec<_>>().join(" "));
            line(4, "</props>");
            line(4, &format!("<anchor>{}</anchor>", ANCHOR_ENUM[*anchor as usize]));
            line(3, "</Item>");
        }
        line(2, "</aAnchors>");
    }
    line(1, "</propInfo>");
    line(1, &format!("<dlcName>hash_{:08X}</dlcName>", rage_joaat(&dlc_name.to_lowercase())));
    line(0, "</CPedVariationInfo>");
    x
}

fn normalised(xml: &str) -> String {
    // the DLC name is a hash that may print as a known name; it is compared as a number below
    crate::archetype::fidelity::normalised_xml(xml, &["<dlcName>"])
}

/// the YMT file for a shape; the bytes are read back and compared with the XML before they are returned
pub fn ymt_bytes(shape: &Shape, dlc_name: &str) -> Result<Vec<u8>> {
    let xml = ymt_xml(shape, dlc_name);
    let tree = from_xml(&xml).context("the generated YMT XML does not parse")?;
    let built = build_meta(&tree, Schema::builtin()).context("building the YMT")?;
    if let Some(w) = built.warnings.first() {
        bail!("the YMT writer left something out: {w}");
    }
    let back = dump_meta(&built.bytes).context("reading the YMT back")?;
    let names = NameTable::core();
    let (want, got) = (normalised(&to_xml(&tree, &names)), normalised(&to_xml(&back.root, &names)));
    if want != got {
        let at = want.lines().zip(got.lines()).position(|(a, b)| a != b).unwrap_or(0);
        bail!("the YMT reads back differently from what was meant (first difference near line {}: {:?} vs {:?})", at + 1, want.lines().nth(at).unwrap_or(""), got.lines().nth(at).unwrap_or(""));
    }
    let dlc = back.root.as_struct().and_then(|s| s.field("dlcName")).and_then(|v| v.as_hash());
    if dlc != Some(rage_joaat(&dlc_name.to_lowercase())) {
        bail!("the YMT's DLC name hash is wrong ({dlc:?})");
    }
    if Shape::of_tree(&back.root) != *shape {
        bail!("the YMT reads back with another shape than the project's");
    }
    Ok(built.bytes)
}

// ---- the other files --------------------------------------------------------------------------------------------------------

/// `ShopPedApparel`: what FiveM reads (`SHOP_PED_APPAREL_META_FILE`) to know the addon exists. The shop lists stay empty: the
/// clothes are added to a ped by script, not through the in-game shop.
pub fn shop_meta(sex: Sex, addon: &str) -> String {
    let (ped, character) = (sex.ped(), if sex == Sex::Male { "SCR_CHAR_MULTIPLAYER" } else { "SCR_CHAR_MULTIPLAYER_F" });
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\n<ShopPedApparel>\n  <pedName>{ped}</pedName>\n  <dlcName>{addon}</dlcName>\n  <fullDlcName>{ped}_{addon}</fullDlcName>\n  <eCharacter>{character}</eCharacter>\n  <pedOutfits />\n  <pedComponents />\n  <pedProps />\n</ShopPedApparel>\n"
    )
}

pub fn fxmanifest(project_name: &str, metas: &[String]) -> String {
    let mut s = format!("-- generated by CRCK Explorer: clothing pack {project_name}\nfx_version 'cerulean'\ngame 'gta5'\n\nfiles {{\n");
    s += &metas.iter().map(|m| format!("    '{m}',\n")).collect::<String>();
    s += "}\n\n";
    s += &metas.iter().map(|m| format!("data_file 'SHOP_PED_APPAREL_META_FILE' '{m}'\n")).collect::<String>();
    s
}

// ---- splitting and planning --------------------------------------------------------------------------------------------------

/// The pack cut into addons of at most `limit` drawables per component and sex: one addon (the project itself) when it fits, else
/// `name_01`, `name_02`, ... with the numbers starting again at 0 in each.
pub fn split_addons(p: &Project) -> Vec<Project> {
    let limit = p.limit.max(1) as u32;
    let chunks = p.drawables.iter().map(|d| d.number / limit + 1).max().unwrap_or(1) as usize;
    if chunks <= 1 {
        return vec![p.clone()];
    }
    (0..chunks)
        .map(|k| Project {
            archived: vec![], sorted: p.sorted,
            name: format!("{}_{:02}", p.name, k + 1),
            limit: p.limit,
            drawables: p.drawables.iter().filter(|d| (d.number / limit) as usize == k).map(|d| Drawable { number: d.number - k as u32 * limit, ..d.clone() }).collect(),
        })
        .filter(|a| !a.drawables.is_empty())
        .collect()
}

#[derive(Debug, Clone)]
pub enum Source {
    Copy(PathBuf),
    /// an image (PNG, JPG, DDS, ...) made into a one-texture `.ytd` when the pack is built
    Image(PathBuf),
    Bytes(Vec<u8>),
}

#[derive(Debug, Clone)]
pub struct Output {
    /// relative to the resource folder, with `/`
    pub dest: String,
    pub source: Source,
}

#[derive(Debug, Clone, Default)]
pub struct BuildPlan {
    /// the resource folder name (the pack name)
    pub resource: String,
    pub outputs: Vec<Output>,
    /// addon names, with their drawable and texture counts
    pub addons: Vec<(String, usize, usize)>,
    pub issues: Vec<Issue>,
}

impl BuildPlan {
    pub fn errors(&self) -> usize {
        self.issues.iter().filter(|i| i.sev == Sev::Error).count()
    }
}

/// What building would write and what is wrong. Nothing is written. With errors the output list is empty.
pub fn plan_build(p: &Project) -> BuildPlan {
    // all in stream/, or a folder per sex and category (FiveM streams files from subfolders the same way)
    let sorted = p.sorted;
    let folder = |d: &Drawable| if sorted { format!("stream/{}/{}/", d.sex.label(), d.kind.name()) } else { "stream/".to_string() };
    let mut plan = BuildPlan { resource: p.name.clone(), ..Default::default() };
    plan.issues = validate(p);
    // the YMT stores drawables by position, so a gap in the numbers would make every file name wrong: it must be closed first
    for ((sex, kind), items) in group(p) {
        let mut nums: Vec<u32> = items.iter().map(|d| d.number).collect();
        nums.sort();
        if nums.iter().enumerate().any(|(i, n)| *n != i as u32) && !nums.windows(2).any(|w| w[0] == w[1]) {
            plan.issues.push(Issue { sev: Sev::Error, code: "CLOTH_GAP_BUILD", message: format!("{} {}: the numbers are not 0, 1, 2, ... so the game would pick the wrong drawable", sex.label(), kind.name()), fix: "press Close the gaps before building".into(), drawable: None });
        }
    }
    if plan.errors() > 0 {
        return plan;
    }
    let addons = split_addons(p);
    let mut metas = vec![];
    for addon in &addons {
        plan.addons.push((addon.name.clone(), addon.drawables.len(), addon.drawables.iter().map(|d| d.textures.len()).sum()));
        for sex in [Sex::Male, Sex::Female] {
            let drawables: Vec<&Drawable> = addon.drawables.iter().filter(|d| d.sex == sex).collect();
            if drawables.is_empty() {
                continue;
            }
            let ped = sex.ped();
            let ymt = match ymt_bytes(&Shape::of(&drawables), &addon.name) {
                Ok(b) => b,
                Err(e) => {
                    plan.issues.push(Issue { sev: Sev::Error, code: "CLOTH_YMT", message: format!("{} YMT: {e:#}", sex.label()), fix: "this is a bug in CRCK Explorer; the files were not written".into(), drawable: None });
                    plan.outputs.clear();
                    return plan;
                }
            };
            plan.outputs.push(Output { dest: format!("stream/{ped}_{}.ymt", addon.name), source: Source::Bytes(ymt) });
            let meta = format!("{ped}_{}.meta", addon.name);
            plan.outputs.push(Output { dest: meta.clone(), source: Source::Bytes(shop_meta(sex, &addon.name).into_bytes()) });
            metas.push(meta);
            for d in drawables {
                // components are streamed as <ped>_<addon>^file, props as <ped>_p_<addon>^file
                let prefix = if d.kind.is_prop() { format!("{ped}_p_{}^", addon.name) } else { format!("{ped}_{}^", addon.name) };
                plan.outputs.push(Output { dest: format!("{}{prefix}{}", folder(d), d.kind.drawable_file(d.number, &d.race)), source: Source::Copy(d.ydd.clone()) });
                if let Some(c) = &d.cloth {
                    plan.outputs.push(Output { dest: format!("{}{prefix}{}", folder(d), d.kind.drawable_file(d.number, &d.race).replace(".ydd", ".yld")), source: Source::Copy(c.clone()) });
                }
                // alternate models keep their number after the drawable's name: jbib_007_u_1.ydd
                for (n, a) in &d.alternates {
                    plan.outputs.push(Output { dest: format!("{}{prefix}{}", folder(d), d.kind.drawable_file(d.number, &d.race).replace(".ydd", &format!("_{n}.ydd"))), source: Source::Copy(a.clone()) });
                }
                for t in &d.textures {
                    let is_ytd = t.path.extension().map(|e| e.to_string_lossy().eq_ignore_ascii_case("ytd")).unwrap_or(false);
                    plan.outputs.push(Output { dest: format!("{}{prefix}{}", folder(d), d.kind.texture_file(d.number, t.letter, &t.race)), source: if is_ytd { Source::Copy(t.path.clone()) } else { Source::Image(t.path.clone()) } });
                }
            }
        }
    }
    plan.outputs.push(Output { dest: "fxmanifest.lua".into(), source: Source::Bytes(fxmanifest(&p.name, &metas).into_bytes()) });
    // two sources for one name would silently lose one of them
    let mut seen = std::collections::HashSet::new();
    for o in &plan.outputs {
        if !seen.insert(o.dest.to_lowercase()) {
            plan.issues.push(Issue { sev: Sev::Error, code: "CLOTH_OUTPUT_CLASH", message: format!("two files would be written as {}", o.dest), fix: "give one of the drawables or textures another number or letter".into(), drawable: None });
        }
    }
    if plan.errors() > 0 {
        plan.outputs.clear();
    }
    plan
}

fn group(p: &Project) -> BTreeMap<(Sex, Kind), Vec<&Drawable>> {
    let mut m: BTreeMap<(Sex, Kind), Vec<&Drawable>> = BTreeMap::new();
    for d in &p.drawables {
        m.entry((d.sex, d.kind)).or_default().push(d);
    }
    m
}

/// One image as a `.ytd` at `to`. The texture inside is named like the file it is written as (`jbib_diff_000_a_uni`), which is how the
/// game's own pack files name theirs.
fn image_to_ytd(from: &Path, to: &Path) -> Result<()> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let stem = to.file_stem().map(|s| s.to_string_lossy().rsplit('^').next().unwrap_or("texture").to_string()).unwrap_or_else(|| "texture".into());
    let ext = from.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_else(|| "png".into());
    let tmp = std::env::temp_dir().join(format!("rage_toolkit_cloth_{}_{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir_all(&tmp)?;
    let image = tmp.join(format!("{stem}.{ext}"));
    let result = fs::copy(from, &image).map(|_| ()).map_err(anyhow::Error::from).and_then(|_| crate::pipeline::files_to_ytd(&[image.clone()], to, &crate::pipeline::Options::default()).map(|_| ())).with_context(|| format!("making a texture dictionary of {}", from.display()));
    let _ = fs::remove_dir_all(&tmp);
    result
}

/// Write the plan into `<out_root>/<resource>/`. Refuses when that folder already holds any file the build would write.
pub fn build(plan: &BuildPlan, out_root: &Path) -> Result<PathBuf> {
    if plan.errors() > 0 || plan.outputs.is_empty() {
        bail!("the pack has {} error(s); nothing was written", plan.errors().max(1));
    }
    let dir = out_root.join(&plan.resource);
    if let Some(o) = plan.outputs.iter().find(|o| dir.join(&o.dest).exists()) {
        bail!("{} already exists; nothing was written (choose another folder or remove the old build)", dir.join(&o.dest).display());
    }
    for o in &plan.outputs {
        if let Source::Copy(p) | Source::Image(p) = &o.source {
            if !p.is_file() {
                bail!("{} is missing; nothing was written", p.display());
            }
        }
    }
    let mut written: Vec<PathBuf> = vec![];
    let result = (|| -> Result<()> {
        for o in &plan.outputs {
            let to = dir.join(&o.dest);
            fs::create_dir_all(to.parent().unwrap_or(&dir))?;
            match &o.source {
                Source::Copy(from) => {
                    fs::copy(from, &to).with_context(|| format!("copying {} to {}", file_of(from), to.display()))?;
                }
                Source::Image(from) => image_to_ytd(from, &to)?,
                Source::Bytes(b) => fs::write(&to, b).with_context(|| format!("writing {}", to.display()))?,
            }
            written.push(to);
        }
        Ok(())
    })();
    if result.is_err() {
        // a half-written pack is worse than none: take back what this build wrote
        for w in &written {
            let _ = fs::remove_file(w);
        }
    }
    result?;
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clothing::{Texture, ANCHORS};

    fn d(sex: Sex, kind: Kind, number: u32, ydd: &Path, textures: &[(&Path, char)]) -> Drawable {
        Drawable { sex, kind, number, source_number: number, ydd: ydd.to_path_buf(), race: if kind.is_prop() { String::new() } else { "u".into() }, cloth: None, alternates: vec![], textures: textures.iter().map(|(p, l)| Texture { path: p.to_path_buf(), letter: *l, race: if kind.is_prop() { String::new() } else { "uni".into() } }).collect() }
    }

    #[test]
    fn the_anchor_names_are_the_games() {
        let s = Schema::builtin();
        let e = s.meta_enums.values().find(|e| e.entries.iter().any(|(h, _)| *h == rage_joaat("ANCHOR_HEAD"))).expect("the anchor enum");
        for (i, name) in ANCHOR_ENUM.iter().enumerate() {
            assert_eq!(e.entries.iter().find(|(h, _)| *h == rage_joaat(name)).map(|x| x.1), Some(i as i32), "{name} is anchor {i}");
        }
        assert_eq!(ANCHORS.len(), ANCHOR_ENUM.len());
    }

    #[test]
    fn a_ymt_is_a_valid_meta_file_that_reads_back_as_meant() {
        let mut shape = Shape::default();
        shape.components.insert(3, vec![(2, false), (1, true)]);
        shape.components.insert(11, vec![(3, false)]);
        shape.props.insert(0, vec![1, 2]);
        shape.props.insert(1, vec![4]);
        let bytes = ymt_bytes(&shape, "MyPack").unwrap();
        assert_eq!(&bytes[..4], b"RSC7");
        let (info, _, _) = rage_formats::parse_ymt(&bytes).expect("the older reader of rage-formats reads it too");
        assert_eq!(info.avail_comp, [255, 255, 255, 0, 255, 255, 255, 255, 255, 255, 255, 1]);
        assert_eq!(info.component_data.len(), 2);
        assert_eq!(info.component_data[0].num_avail_tex, 3);
        let back = dump_meta(&bytes).unwrap();
        assert_eq!(Shape::of_tree(&back.root), shape);
        let xml = to_xml(&back.root, &NameTable::core());
        assert!(xml.contains(&format!("hash_{:08X}", rage_joaat("mypack"))), "the DLC name is hashed in lowercase");
        // and it survives a rewrite like every Meta file
        assert!(crate::archetype::fidelity::check(&bytes).unwrap().is_lossless());
        // no props, no components
        ymt_bytes(&Shape::default(), "x").unwrap();
    }

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("rage_toolkit_cloth_build_{name}_{}", std::process::id()));
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
    fn an_image_texture_is_built_into_a_real_ytd() {
        let d = temp("image");
        let y = put(&d, "in/x.ydd", "1");
        let png = d.join("in/skin.png");
        image::RgbaImage::from_pixel(8, 8, image::Rgba([200, 30, 30, 255])).save(&png).unwrap();
        let p = Project { archived: vec![], sorted: false, name: "imgpack".into(), limit: 128, drawables: vec![d_(Sex::Male, Kind::Component(11), 0, &y, &[(&png, 'a')])] };
        let plan = plan_build(&p);
        assert_eq!(plan.errors(), 0, "{:?}", plan.issues);
        let out = build(&plan, &d.join("out")).unwrap();
        let ytd = out.join("stream/mp_m_freemode_01_imgpack^jbib_diff_000_a_uni.ytd");
        let (textures, _) = crate::pipeline::read_ytd(&ytd).expect("the output is a texture dictionary");
        assert_eq!(textures.len(), 1);
        assert_eq!(textures[0].name, "jbib_diff_000_a_uni");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_pack_is_planned_and_built_with_the_names_fivem_expects() {
        let d = temp("pack");
        let (y1, y2, yh) = (put(&d, "in/x1.ydd", "1"), put(&d, "in/x2.ydd", "2"), put(&d, "in/hat.ydd", "h"));
        let (t1, t2, t3, th) = (put(&d, "in/t1.ytd", "a"), put(&d, "in/t2.ytd", "b"), put(&d, "in/t3.ytd", "c"), put(&d, "in/th.ytd", "d"));
        let cloth = put(&d, "in/c.yld", "cloth");
        let mut second = d_(Sex::Male, Kind::Component(11), 1, &y2, &[(&t3, 'a')]);
        second.cloth = Some(cloth);
        let p = Project {
            archived: vec![], sorted: false,
            name: "mypack".into(),
            limit: 128,
            drawables: vec![d_(Sex::Male, Kind::Component(11), 0, &y1, &[(&t1, 'a'), (&t2, 'b')]), second, d_(Sex::Male, Kind::Prop(0), 0, &yh, &[(&th, 'a')]), d_(Sex::Female, Kind::Component(3), 0, &y1, &[(&t1, 'a')])],
        };
        let plan = plan_build(&p);
        assert_eq!(plan.errors(), 0, "{:?}", plan.issues);
        let dests: Vec<&str> = plan.outputs.iter().map(|o| o.dest.as_str()).collect();
        for want in [
            "fxmanifest.lua",
            "mp_m_freemode_01_mypack.meta",
            "mp_f_freemode_01_mypack.meta",
            "stream/mp_m_freemode_01_mypack.ymt",
            "stream/mp_f_freemode_01_mypack.ymt",
            "stream/mp_m_freemode_01_mypack^jbib_000_u.ydd",
            "stream/mp_m_freemode_01_mypack^jbib_diff_000_a_uni.ytd",
            "stream/mp_m_freemode_01_mypack^jbib_diff_000_b_uni.ytd",
            "stream/mp_m_freemode_01_mypack^jbib_001_u.yld",
            "stream/mp_m_freemode_01_p_mypack^p_head_000.ydd",
            "stream/mp_m_freemode_01_p_mypack^p_head_diff_000_a.ytd",
            "stream/mp_f_freemode_01_mypack^uppr_000_u.ydd",
        ] {
            assert!(dests.contains(&want), "{want} missing from {dests:#?}");
        }
        let manifest = plan.outputs.iter().find(|o| o.dest == "fxmanifest.lua").map(|o| if let Source::Bytes(b) = &o.source { String::from_utf8_lossy(b).to_string() } else { String::new() }).unwrap();
        assert!(manifest.contains("data_file 'SHOP_PED_APPAREL_META_FILE' 'mp_m_freemode_01_mypack.meta'") && manifest.contains("'mp_f_freemode_01_mypack.meta',"), "{manifest}");
        // the shop meta passes the metadata validator
        let meta = plan.outputs.iter().find(|o| o.dest == "mp_m_freemode_01_mypack.meta").map(|o| if let Source::Bytes(b) = &o.source { String::from_utf8_lossy(b).to_string() } else { String::new() }).unwrap();
        assert_eq!(crate::metaxml::check(&meta).errors(), 0, "{meta}");
        // build, then refuse to build over it
        let out = build(&plan, &d.join("out")).unwrap();
        assert!(out.join("stream").join("mp_m_freemode_01_mypack^jbib_diff_000_a_uni.ytd").is_file());
        assert_eq!(fs::read_to_string(out.join("stream/mp_m_freemode_01_mypack^jbib_diff_000_a_uni.ytd")).unwrap(), "a");
        let again = build(&plan, &d.join("out"));
        assert!(again.is_err() && again.unwrap_err().to_string().contains("already exists"));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_built_pack_opens_again_with_every_drawable_in_its_slot() {
        let d = temp("reopen");
        let (y1, y2, yh) = (put(&d, "in/x1.ydd", "1"), put(&d, "in/x2.ydd", "2"), put(&d, "in/hat.ydd", "h"));
        let (t1, t2, th) = (put(&d, "in/t1.ytd", "a"), put(&d, "in/t2.ytd", "b"), put(&d, "in/th.ytd", "d"));
        let mut second = d_(Sex::Male, Kind::Component(11), 1, &y2, &[(&t2, 'a')]);
        second.cloth = Some(put(&d, "in/c.yld", "cloth"));
        second.alternates = vec![(1, put(&d, "in/alt.ydd", "alt"))];
        let p = Project { archived: vec![], sorted: false, name: "packa".into(), limit: 128, drawables: vec![d_(Sex::Male, Kind::Component(11), 0, &y1, &[(&t1, 'a'), (&t2, 'b')]), second, d_(Sex::Male, Kind::Prop(0), 0, &yh, &[(&th, 'a')]), d_(Sex::Female, Kind::Component(3), 0, &y1, &[(&t1, 'a')])] };
        let out = build(&plan_build(&p), &d.join("out")).unwrap();
        assert_eq!(fs::read_to_string(out.join("stream/mp_m_freemode_01_packa^jbib_001_u_1.ydd")).unwrap(), "alt", "the alternate keeps its _1");
        let back = crate::clothing::open_pack(&out).unwrap();
        assert_eq!(back.name, "packa");
        assert_eq!(back.drawables.iter().map(|x| x.alternates.len()).sum::<usize>(), 1, "and comes back with its drawable");
        let shape = |p: &Project| {
            let mut v: Vec<(Sex, Kind, u32, usize, bool)> = p.drawables.iter().map(|x| (x.sex, x.kind, x.number, x.textures.len(), x.cloth.is_some())).collect();
            v.sort();
            v
        };
        assert_eq!(shape(&back), shape(&p));
        // and the reopened pack builds again into the same files, from the built files
        let again = build(&plan_build(&Project { name: "packb".into(), ..back }), &d.join("out2")).unwrap();
        assert!(again.join("stream/mp_m_freemode_01_packb^jbib_diff_000_b_uni.ytd").is_file());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn sorted_builds_put_each_category_in_its_own_folder() {
        let d = temp("sorted");
        let (y, t) = (put(&d, "x.ydd", "1"), put(&d, "t.ytd", "a"));
        let mut p = Project { name: "srt".into(), drawables: vec![d_(Sex::Female, Kind::Component(11), 0, &y, &[(&t, 'a')]), d_(Sex::Male, Kind::Component(4), 0, &y, &[(&t, 'a')]), d_(Sex::Male, Kind::Prop(0), 0, &y, &[(&t, 'a')])], ..Default::default() };
        p.sorted = true;
        let plan = plan_build(&p);
        let dests: Vec<&str> = plan.outputs.iter().map(|o| o.dest.as_str()).collect();
        for want in ["stream/female/jbib/mp_f_freemode_01_srt^jbib_000_u.ydd", "stream/female/jbib/mp_f_freemode_01_srt^jbib_diff_000_a_uni.ytd", "stream/male/lowr/mp_m_freemode_01_srt^lowr_000_u.ydd", "stream/male/p_head/mp_m_freemode_01_p_srt^p_head_000.ydd", "stream/mp_f_freemode_01_srt.ymt"] {
            assert!(dests.contains(&want), "{want} missing from {dests:#?}");
        }
        let out = build(&plan, &d.join("out")).unwrap();
        assert!(out.join("stream/male/lowr/mp_m_freemode_01_srt^lowr_000_u.ydd").is_file());
        // a sorted pack opens again like a flat one
        assert_eq!(crate::clothing::open_pack(&out).unwrap().drawables.len(), 3);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn archive_restore_and_slot_moves_never_lose_a_drawable() {
        let d = temp("moves");
        let y = put(&d, "x.ydd", "1");
        let (ta, tb) = (put(&d, "a.ytd", "a"), put(&d, "b.ytd", "b"));
        let mut p = Project { archived: vec![], sorted: false, name: "m".into(), limit: 128, drawables: (0..3).map(|n| d_(Sex::Male, Kind::Component(11), n, &y, &[(if n == 1 { &tb } else { &ta }, 'a')])).collect() };
        // remove 001: archived with its texture, the others keep their numbers
        let gone = p.archive(1).unwrap().clone();
        assert_eq!((p.drawables.len(), p.archived.len(), gone.textures[0].path.clone()), (2, 1, tb.clone()));
        // restored after the last
        assert_eq!(p.restore(0), Some(3));
        assert!(p.archived.is_empty() && p.drawables.iter().any(|x| x.number == 3 && x.textures[0].path == tb));
        // 003 to 000: the two swap
        let i = p.drawables.iter().position(|x| x.number == 3).unwrap();
        p.move_to(i, Sex::Male, Kind::Component(11), 0).unwrap();
        assert_eq!(p.drawables.iter().find(|x| x.number == 0).unwrap().textures[0].path, tb);
        assert_eq!(p.drawables.iter().map(|x| x.number).collect::<Vec<_>>(), [0, 2, 3]);
        // to another component, onto a free number; then female; a prop is refused
        let i = p.drawables.iter().position(|x| x.number == 2).unwrap();
        p.move_to(i, Sex::Female, Kind::Component(8), 0).unwrap();
        assert!(p.drawables.iter().any(|x| (x.sex, x.kind, x.number) == (Sex::Female, Kind::Component(8), 0)));
        assert!(p.move_to(0, Sex::Male, Kind::Prop(0), 0).is_err());
        // a drawable from another pack lands after its group's last
        let mut other = Project { name: "o".into(), ..Default::default() };
        assert_eq!(other.add_next(p.drawables[0].clone()), 0);
        assert_eq!(other.add_next(p.drawables[0].clone()), 1);
        // the archive is saved with the project
        p.archive(0);
        let f = d.join("p.ragetoolkitclothes");
        super::super::project::save(&p, &f).unwrap();
        let back = super::super::project::load(&f).unwrap();
        assert_eq!((back.drawables.len(), back.archived.len()), (p.drawables.len(), 1));
        let _ = fs::remove_dir_all(&d);
    }

    fn d_(sex: Sex, kind: Kind, number: u32, ydd: &Path, textures: &[(&Path, char)]) -> Drawable {
        d(sex, kind, number, ydd, textures)
    }

    #[test]
    fn errors_stop_the_build_and_gaps_must_be_closed_first() {
        let d = temp("errors");
        let y = put(&d, "x.ydd", "1");
        let t = put(&d, "t.ytd", "a");
        let mut p = Project { archived: vec![], sorted: false, name: "mypack".into(), limit: 128, drawables: vec![d_(Sex::Male, Kind::Component(11), 0, &y, &[(&t, 'a')]), d_(Sex::Male, Kind::Component(11), 2, &y, &[(&t, 'a')])] };
        let plan = plan_build(&p);
        assert!(plan.issues.iter().any(|i| i.code == "CLOTH_GAP_BUILD") && plan.outputs.is_empty());
        assert!(build(&plan, &d.join("out")).is_err());
        assert!(!d.join("out").exists(), "nothing was written");
        p.close_gaps();
        assert_eq!(plan_build(&p).errors(), 0);
        p.drawables[0].textures.clear();
        assert!(plan_build(&p).issues.iter().any(|i| i.code == "CLOTH_NO_TEXTURE"));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_big_pack_is_split_into_addons_and_each_numbers_from_zero() {
        let d = temp("split");
        let y = put(&d, "x.ydd", "1");
        let t = put(&d, "t.ytd", "a");
        let p = Project { archived: vec![], sorted: false, name: "big".into(), limit: 3, drawables: (0..7).map(|n| d_(Sex::Male, Kind::Component(3), n, &y, &[(&t, 'a')])).collect() };
        let addons = split_addons(&p);
        assert_eq!(addons.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(), ["big_01", "big_02", "big_03"]);
        assert_eq!(addons.iter().map(|a| a.drawables.len()).collect::<Vec<_>>(), [3, 3, 1]);
        assert_eq!(addons[1].drawables.iter().map(|x| x.number).collect::<Vec<_>>(), [0, 1, 2]);
        let plan = plan_build(&p);
        assert_eq!(plan.errors(), 0, "{:?}", plan.issues.iter().map(|i| &i.message).collect::<Vec<_>>());
        let dests: Vec<&str> = plan.outputs.iter().map(|o| o.dest.as_str()).collect();
        assert!(dests.contains(&"stream/mp_m_freemode_01_big_02^uppr_000_u.ydd") && dests.contains(&"mp_m_freemode_01_big_03.meta"));
        let _ = fs::remove_dir_all(&d);
    }
}
