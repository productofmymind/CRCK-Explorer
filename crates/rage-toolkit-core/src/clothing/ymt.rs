//! The YMT (`CPedVariationInfo`) of one DLC and sex, both ways: written from the project's drawables and read from a pack, through
//! one [`Ymt`] value, so writing can be checked by reading back and an existing pack can be opened with every setting it had.
//!
//! What is modelled is what freemode clothing packs use (checked against GTA World's packs and grzyClothTool's output): per
//! drawable the texture ids, `propMask`, the alternate count, cloth, audio ids, expression values (high heels, hair under a hat),
//! flags, and for props render flags and stickyness. Anything else that is not at its usual value (inclusions, selection sets,
//! texture distributions) is reported when reading, so nothing is lost without a word.
use super::{Drawable, Kind, ANCHORS};
use anyhow::{bail, Context, Result};
use rage_formats::{build_meta, dump_meta, from_xml, rage_joaat, to_xml, MetaValue, NameTable, Schema};
use std::collections::BTreeMap;

/// the game's names for the prop anchor points, in anchor order
pub const ANCHOR_ENUM: [&str; 13] = ["ANCHOR_HEAD", "ANCHOR_EYES", "ANCHOR_EARS", "ANCHOR_MOUTH", "ANCHOR_LEFT_HAND", "ANCHOR_RIGHT_HAND", "ANCHOR_LEFT_WRIST", "ANCHOR_RIGHT_WRIST", "ANCHOR_HIP", "ANCHOR_LEFT_FOOT", "ANCHOR_RIGHT_FOOT", "ANCHOR_PH_L_HAND", "ANCHOR_PH_R_HAND"];

/// One drawable's entry in a YMT. Components use `prop_mask`, `alternates`, `cloth`, `audio2`; props use `render_flags`, `stickyness`.
/// `flags` is a component's `flags` and a prop's `propFlags`; `flags2` a component's `pedXml_flags` and a prop's `flags`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Entry {
    /// `texId` of each texture
    pub textures: Vec<u8>,
    pub prop_mask: u8,
    pub alternates: u8,
    pub cloth: bool,
    /// audio id hashes (0 = empty)
    pub audio: u32,
    pub audio2: u32,
    pub expression: [f32; 5],
    pub flags: u32,
    pub flags2: u32,
    pub render_flags: u32,
    pub stickyness: u8,
}

/// the hash of an audio name as the project stores it: empty is 0, `hash_XXXXXXXX` is that hash, a name is hashed
pub fn audio_hash(name: &str) -> u32 {
    let n = name.trim();
    if n.is_empty() {
        0
    } else if let Some(h) = n.strip_prefix("hash_").and_then(|h| u32::from_str_radix(h, 16).ok()) {
        h
    } else {
        rage_joaat(&n.to_lowercase())
    }
}

/// The prop render flags the game knows (ePropRenderFlags), with their bits: `PRF_ALPHA`, `PRF_DECAL`, `PRF_CUTOUT`.
pub fn render_flags() -> Vec<(String, u32)> {
    // looked up once: the YMT writer asks for every prop
    static FLAGS: std::sync::OnceLock<Vec<(String, u32)>> = std::sync::OnceLock::new();
    FLAGS.get_or_init(|| {
        let names = NameTable::core_shared();
        let s = Schema::builtin();
        let Some(e) = s.meta_enums.values().find(|e| e.entries.iter().any(|(h, _)| *h == rage_joaat("PRF_ALPHA"))) else { return vec![] };
        let mut v: Vec<(String, u32)> = e.entries.iter().filter(|(_, b)| (0..32).contains(b)).map(|(h, b)| (names.resolve(*h).into_owned(), 1u32 << b)).collect();
        v.sort_by_key(|x| x.1);
        v
    })
    .clone()
}

/// render flag bits as the XML writes them (names joined by `, `; bits the game does not name as a number)
fn render_flag_text(bits: u32) -> String {
    if bits == 0 {
        return String::new();
    }
    let known = render_flags();
    if known.iter().fold(0, |a, (_, b)| a | b) & bits != bits {
        return bits.to_string();
    }
    known.iter().filter(|(_, b)| bits & b != 0).map(|(n, _)| n.as_str()).collect::<Vec<_>>().join(", ")
}

/// a hash as the project stores it: its name when known, else `hash_XXXXXXXX`, empty for 0
pub fn audio_name(hash: u32) -> String {
    if hash == 0 {
        String::new()
    } else {
        NameTable::core_shared().resolve(hash).into_owned()
    }
}

impl Entry {
    /// what the YMT says about drawable `d`
    pub fn of(d: &Drawable) -> Entry {
        let m = &d.meta;
        let comp = !d.kind.is_prop();
        Entry {
            textures: (0..d.textures.len()).map(|n| d.tex_id(n)).collect(),
            prop_mask: if comp { d.prop_mask() } else { 0 },
            alternates: if comp { d.alternates.len().min(255) as u8 } else { 0 },
            cloth: comp && d.cloth.is_some(),
            audio: audio_hash(m.audio.as_deref().unwrap_or(if comp { "none" } else { "" })),
            audio2: if comp { audio_hash(m.audio2.as_deref().unwrap_or("")) } else { 0 },
            expression: m.expression_mods,
            flags: m.flags,
            flags2: m.flags2,
            render_flags: if comp { 0 } else { m.render_flags },
            stickyness: if comp { 0 } else { m.stickyness },
        }
    }

    /// Put what a YMT said into drawable `d` (whose kind, race and texture count are already set): only what differs from the
    /// usual values is stored, so a project file stays short.
    pub fn apply(&self, d: &mut Drawable) {
        let plain = Entry::of(&Drawable { meta: Default::default(), textures: d.textures.iter().map(|t| super::Texture { tex_id: None, ..t.clone() }).collect(), ..d.clone() });
        for (n, id) in self.textures.iter().enumerate() {
            if let Some(t) = d.textures.get_mut(n) {
                t.tex_id = (plain.textures.get(n) != Some(id)).then_some(*id);
            }
        }
        let m = &mut d.meta;
        m.prop_mask = (self.prop_mask != plain.prop_mask).then_some(self.prop_mask);
        m.audio = (self.audio != plain.audio).then(|| audio_name(self.audio));
        m.audio2 = (self.audio2 != plain.audio2).then(|| audio_name(self.audio2));
        m.expression_mods = self.expression;
        m.flags = self.flags;
        m.flags2 = self.flags2;
        m.render_flags = self.render_flags;
        m.stickyness = self.stickyness;
    }
}

/// The drawables of one DLC and sex as its YMT has them: per component index and per prop anchor, the entries in number order.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Ymt {
    pub components: BTreeMap<u8, Vec<Entry>>,
    pub props: BTreeMap<u8, Vec<Entry>>,
}

impl Ymt {
    /// from drawables with consecutive numbers `0..n` per kind (one DLC and one sex)
    pub fn of(drawables: &[&Drawable]) -> Ymt {
        let mut y = Ymt::default();
        let mut sorted: Vec<&&Drawable> = drawables.iter().collect();
        sorted.sort_by_key(|d| (d.kind, d.number));
        for d in sorted {
            match d.kind {
                Kind::Component(i) => y.components.entry(i).or_default().push(Entry::of(d)),
                Kind::Prop(i) => y.props.entry(i).or_default().push(Entry::of(d)),
            }
        }
        y
    }

    /// every (kind, number, entry)
    pub fn entries(&self) -> impl Iterator<Item = (Kind, u32, &Entry)> {
        let comps = self.components.iter().flat_map(|(c, v)| v.iter().enumerate().map(move |(n, e)| (Kind::Component(*c), n as u32, e)));
        let props = self.props.iter().flat_map(|(a, v)| v.iter().enumerate().map(move |(n, e)| (Kind::Prop(*a), n as u32, e)));
        comps.chain(props)
    }

    /// The XML of the YMT. `dlc_name` is hashed in lowercase.
    pub fn to_xml(&self, dlc_name: &str) -> String {
        let mut x = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<CPedVariationInfo>\n");
        let mut line = |depth: usize, text: &str| {
            x.push_str(&"  ".repeat(depth));
            x.push_str(text);
            x.push('\n');
        };
        // a known hash by its name, as the reader prints it, so the two compare as text
        let hash = audio_name;
        let tag = |name: &str, text: String| if text.is_empty() { format!("<{name}/>") } else { format!("<{name}>{text}</{name}>") };
        let floats = |f: &[f32; 5]| f.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(" ");
        line(1, "<bHasTexVariations value=\"true\"/>");
        line(1, "<bHasDrawblVariations value=\"true\"/>");
        line(1, "<bHasLowLODs value=\"false\"/>");
        line(1, "<bIsSuperLOD value=\"false\"/>");
        // position of each present component in aComponentData3, FF when absent
        let mut avail = [255u8; 12];
        for (pos, comp) in self.components.keys().enumerate() {
            avail[*comp as usize] = pos as u8;
        }
        line(1, "<availComp itemType=\"ByteArray\">");
        line(2, &avail.iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(" "));
        line(1, "</availComp>");
        if self.components.is_empty() {
            line(1, "<aComponentData3 itemType=\"CPVComponentData\"/>");
        } else {
            line(1, "<aComponentData3 itemType=\"CPVComponentData\">");
            for drawables in self.components.values() {
                line(2, "<Item>");
                // a byte: the game's own files store the sum modulo 256 when a component has more than 255 textures
                line(3, &format!("<numAvailTex value=\"{}\"/>", drawables.iter().map(|d| d.textures.len()).sum::<usize>() % 256));
                line(3, "<aDrawblData3 itemType=\"CPVDrawblData\">");
                for e in drawables {
                    line(4, "<Item>");
                    line(5, &format!("<propMask value=\"{}\"/>", e.prop_mask));
                    line(5, &format!("<numAlternatives value=\"{}\"/>", e.alternates));
                    if e.textures.is_empty() {
                        line(5, "<aTexData itemType=\"CPVTextureData\"/>");
                    } else {
                        line(5, "<aTexData itemType=\"CPVTextureData\">");
                        for id in &e.textures {
                            line(6, "<Item>");
                            line(7, &format!("<texId value=\"{id}\"/>"));
                            line(7, "<distribution value=\"255\"/>");
                            line(6, "</Item>");
                        }
                        line(5, "</aTexData>");
                    }
                    line(5, "<clothData>");
                    line(6, &format!("<ownsCloth value=\"{}\"/>", e.cloth));
                    line(5, "</clothData>");
                    line(4, "</Item>");
                }
                line(3, "</aDrawblData3>");
                line(2, "</Item>");
            }
            line(1, "</aComponentData3>");
        }
        line(1, "<aSelectionSets itemType=\"CPedSelectionSet\"/>");
        if self.components.is_empty() {
            line(1, "<compInfos itemType=\"CComponentInfo\"/>");
        } else {
            line(1, "<compInfos itemType=\"CComponentInfo\">");
            for (comp, drawables) in &self.components {
                for (n, e) in drawables.iter().enumerate() {
                    line(2, "<Item>");
                    line(3, &tag("pedXml_audioID", hash(e.audio)));
                    line(3, &tag("pedXml_audioID2", hash(e.audio2)));
                    line(3, "<pedXml_expressionMods itemType=\"float\">");
                    line(4, &floats(&e.expression));
                    line(3, "</pedXml_expressionMods>");
                    line(3, &format!("<flags value=\"{}\"/>", e.flags));
                    line(3, "<inclusions/>");
                    line(3, "<exclusions/>");
                    line(3, "<pedXml_vfxComps/>");
                    line(3, &format!("<pedXml_flags value=\"{}\"/>", e.flags2));
                    line(3, &format!("<pedXml_compIdx value=\"{comp}\"/>"));
                    line(3, &format!("<pedXml_drawblIdx value=\"{n}\"/>"));
                    line(2, "</Item>");
                }
            }
            line(1, "</compInfos>");
        }
        line(1, "<propInfo>");
        let n_props: usize = self.props.values().map(Vec::len).sum();
        line(2, &format!("<numAvailProps value=\"{n_props}\"/>"));
        if n_props == 0 {
            line(2, "<aPropMetaData itemType=\"CPedPropMetaData\"/>");
        } else {
            line(2, "<aPropMetaData itemType=\"CPedPropMetaData\">");
            for (anchor, props) in &self.props {
                for (n, e) in props.iter().enumerate() {
                    line(3, "<Item>");
                    line(4, &tag("audioId", hash(e.audio)));
                    line(4, "<expressionMods itemType=\"float\">");
                    line(5, &floats(&e.expression));
                    line(4, "</expressionMods>");
                    if e.textures.is_empty() {
                        line(4, "<texData itemType=\"CPedPropTexData\"/>");
                    } else {
                        line(4, "<texData itemType=\"CPedPropTexData\">");
                        for id in &e.textures {
                            line(5, "<Item>");
                            line(6, "<inclusions/>");
                            line(6, "<exclusions/>");
                            line(6, &format!("<texId value=\"{id}\"/>"));
                            line(6, "<inclusionId value=\"0\"/>");
                            line(6, "<exclusionId value=\"0\"/>");
                            line(6, "<distribution value=\"255\"/>");
                            line(5, "</Item>");
                        }
                        line(4, "</texData>");
                    }
                    line(4, &tag("renderFlags", render_flag_text(e.render_flags)));
                    line(4, &format!("<propFlags value=\"{}\"/>", e.flags));
                    line(4, &format!("<flags value=\"{}\"/>", e.flags2));
                    line(4, &format!("<anchorId value=\"{anchor}\"/>"));
                    line(4, &format!("<propId value=\"{n}\"/>"));
                    line(4, &format!("<stickyness value=\"{}\"/>", e.stickyness));
                    line(3, "</Item>");
                }
            }
            line(2, "</aPropMetaData>");
        }
        if self.props.is_empty() {
            line(2, "<aAnchors itemType=\"CAnchorProps\"/>");
        } else {
            line(2, "<aAnchors itemType=\"CAnchorProps\">");
            for (anchor, props) in &self.props {
                line(3, "<Item>");
                line(4, "<props itemType=\"byte\">");
                line(5, &props.iter().map(|e| e.textures.len().to_string()).collect::<Vec<_>>().join(" "));
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

    /// Read a YMT tree (ours or anyone's). Returns the entries, the DLC name hash, and what was found that is not modelled (and
    /// would be written at its usual value).
    pub fn read(root: &MetaValue) -> Result<(Ymt, u32, Vec<String>)> {
        let r = root.as_struct().context("the YMT has no CPedVariationInfo")?;
        let mut y = Ymt::default();
        let mut odd: Vec<String> = vec![];
        let int = |v: &MetaValue, name: &str| v.as_struct().and_then(|s| s.field(name)).and_then(|f| f.as_u32()).unwrap_or(0);
        let hash = |v: &MetaValue, name: &str| v.as_struct().and_then(|s| s.field(name)).and_then(|f| f.as_hash()).unwrap_or(0);
        let floats = |v: &MetaValue, name: &str| -> [f32; 5] {
            let mut out = [0.0; 5];
            match v.as_struct().and_then(|s| s.field(name)) {
                Some(MetaValue::Array(a)) => {
                    for (i, x) in a.items.iter().take(5).enumerate() {
                        if let MetaValue::F32(f) = x {
                            out[i] = *f;
                        }
                    }
                }
                Some(MetaValue::Struct(s)) => {
                    for (i, (_, x)) in s.fields.iter().take(5).enumerate() {
                        if let MetaValue::F32(f) = x {
                            out[i] = *f;
                        }
                    }
                }
                _ => {}
            }
            out
        };
        let not_empty = |v: Option<&MetaValue>| match v {
            None | Some(MetaValue::Null) => false,
            Some(MetaValue::Array(a)) => !a.items.is_empty(),
            Some(MetaValue::Bytes(b)) => b.iter().any(|x| *x != 0),
            Some(MetaValue::Struct(s)) => s.fields.iter().any(|(_, f)| !matches!(f, MetaValue::Null) && f.as_i64() != Some(0) && !(matches!(f, MetaValue::Array(a) if a.items.is_empty()))),
            Some(x) => x.as_i64().is_some_and(|n| n != 0) || x.as_hash().is_some_and(|h| h != 0),
        };
        for name in ["bHasLowLODs", "bIsSuperLOD"] {
            if matches!(r.field(name), Some(MetaValue::Bool(true))) {
                odd.push(format!("{name} is true"));
            }
        }
        if not_empty(r.field("aSelectionSets")) {
            odd.push("selection sets".into());
        }
        let avail: Vec<u8> = match r.field("availComp") {
            Some(MetaValue::Bytes(b)) => b.clone(),
            _ => vec![],
        };
        let comps = r.field("aComponentData3").map(|v| v.items()).unwrap_or(&[]);
        for (i, a) in avail.iter().enumerate() {
            if *a == 255 {
                continue;
            }
            let Some(c) = comps.get(*a as usize) else {
                odd.push(format!("availComp points component {i} at entry {a}, which is not there"));
                continue;
            };
            let mut entries = vec![];
            for d in c.as_struct().and_then(|s| s.field("aDrawblData3")).map(|v| v.items()).unwrap_or(&[]) {
                let tex = d.as_struct().and_then(|s| s.field("aTexData")).map(|t| t.items()).unwrap_or(&[]);
                for t in tex {
                    if int(t, "distribution") != 255 {
                        odd.push(format!("component {i}: a texture distribution of {}", int(t, "distribution")));
                    }
                }
                let cloth = matches!(d.as_struct().and_then(|s| s.field("clothData")).and_then(|c| c.as_struct()).and_then(|c| c.field("ownsCloth")), Some(MetaValue::Bool(true)));
                entries.push(Entry { textures: tex.iter().map(|t| int(t, "texId") as u8).collect(), prop_mask: int(d, "propMask") as u8, alternates: int(d, "numAlternatives") as u8, cloth, ..Default::default() });
            }
            y.components.insert(i as u8, entries);
        }
        for info in r.field("compInfos").map(|v| v.items()).unwrap_or(&[]) {
            let (c, n) = (int(info, "pedXml_compIdx") as u8, int(info, "pedXml_drawblIdx") as usize);
            let Some(e) = y.components.get_mut(&c).and_then(|v| v.get_mut(n)) else {
                odd.push(format!("compInfo for component {c} drawable {n}, which the YMT does not have"));
                continue;
            };
            e.audio = hash(info, "pedXml_audioID");
            e.audio2 = hash(info, "pedXml_audioID2");
            e.expression = floats(info, "pedXml_expressionMods");
            e.flags = int(info, "flags");
            e.flags2 = int(info, "pedXml_flags");
            let s = info.as_struct();
            for f in ["inclusions", "exclusions", "pedXml_vfxComps"] {
                if not_empty(s.and_then(|s| s.field(f))) {
                    odd.push(format!("component {c} drawable {n}: {f}"));
                }
            }
        }
        if let Some(info) = r.field("propInfo").and_then(|p| p.as_struct()) {
            let mut props: BTreeMap<u8, BTreeMap<u32, Entry>> = BTreeMap::new();
            for p in info.field("aPropMetaData").map(|v| v.items()).unwrap_or(&[]) {
                let tex = p.as_struct().and_then(|s| s.field("texData")).map(|t| t.items()).unwrap_or(&[]);
                for t in tex {
                    let s = t.as_struct();
                    if ["inclusions", "exclusions", "inclusionId", "exclusionId"].iter().any(|f| not_empty(s.and_then(|s| s.field(f)))) || int(t, "distribution") != 255 {
                        odd.push(format!("prop anchor {} prop {}: texture inclusions, exclusions or distribution", int(p, "anchorId"), int(p, "propId")));
                    }
                }
                let e = Entry {
                    textures: tex.iter().map(|t| int(t, "texId") as u8).collect(),
                    audio: hash(p, "audioId"),
                    expression: floats(p, "expressionMods"),
                    render_flags: int(p, "renderFlags"),
                    flags: int(p, "propFlags"),
                    flags2: int(p, "flags"),
                    stickyness: int(p, "stickyness") as u8,
                    ..Default::default()
                };
                let anchor = int(p, "anchorId");
                if anchor as usize >= ANCHORS.len() {
                    odd.push(format!("a prop on anchor {anchor}, which is not one of the {} anchors", ANCHORS.len()));
                    continue;
                }
                props.entry(anchor as u8).or_default().insert(int(p, "propId"), e);
            }
            for (anchor, list) in props {
                if list.keys().copied().ne(0..list.len() as u32) {
                    odd.push(format!("anchor {anchor}: the prop ids are not 0, 1, 2, ..."));
                }
                y.props.insert(anchor, list.into_values().collect());
            }
        }
        let dlc = r.field("dlcName").and_then(|v| v.as_hash()).unwrap_or(0);
        odd.sort();
        odd.dedup();
        Ok((y, dlc, odd))
    }
}

fn normalised(xml: &str) -> String {
    // the DLC name is a hash that may print as a known name; it is compared as a number below
    crate::archetype::fidelity::normalised_xml(xml, &["<dlcName>"])
}

/// The YMT file; the bytes are read back and compared with what was meant before they are returned.
pub fn ymt_bytes(ymt: &Ymt, dlc_name: &str) -> Result<Vec<u8>> {
    let xml = ymt.to_xml(dlc_name);
    let tree = from_xml(&xml).context("the generated YMT XML does not parse")?;
    let built = build_meta(&tree, Schema::builtin()).context("building the YMT")?;
    if let Some(w) = built.warnings.first() {
        bail!("the YMT writer left something out: {w}");
    }
    let back = dump_meta(&built.bytes).context("reading the YMT back")?;
    let names = NameTable::core_shared();
    let (want, got) = (normalised(&to_xml(&tree, names)), normalised(&to_xml(&back.root, names)));
    if want != got {
        let at = want.lines().zip(got.lines()).position(|(a, b)| a != b).unwrap_or(0);
        bail!("the YMT reads back differently from what was meant (first difference near line {}: {:?} vs {:?})", at + 1, want.lines().nth(at).unwrap_or(""), got.lines().nth(at).unwrap_or(""));
    }
    let (read, dlc, odd) = Ymt::read(&back.root)?;
    if dlc != rage_joaat(&dlc_name.to_lowercase()) {
        bail!("the YMT's DLC name hash is wrong ({dlc:08X})");
    }
    if let Some(o) = odd.first() {
        bail!("the YMT reads back with something unexpected: {o}");
    }
    if read != *ymt {
        bail!("the YMT reads back with other settings than the project's");
    }
    Ok(built.bytes)
}

/// Read a YMT file (binary) into entries: the DLC name hash and what is not modelled come with it.
pub fn read_ymt(bytes: &[u8]) -> Result<(Ymt, u32, Vec<String>)> {
    let dump = dump_meta(bytes).context("reading the YMT")?;
    let (y, dlc, mut odd) = Ymt::read(&dump.root)?;
    odd.extend(dump.warnings);
    Ok((y, dlc, odd))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clothing::{DrawableMeta, Sex, Texture};

    #[test]
    fn the_anchor_names_are_the_games() {
        let s = Schema::builtin();
        let e = s.meta_enums.values().find(|e| e.entries.iter().any(|(h, _)| *h == rage_joaat("ANCHOR_HEAD"))).expect("the anchor enum");
        for (i, name) in ANCHOR_ENUM.iter().enumerate() {
            assert_eq!(e.entries.iter().find(|(h, _)| *h == rage_joaat(name)).map(|x| x.1), Some(i as i32), "{name} is anchor {i}");
        }
        assert_eq!(ANCHORS.len(), ANCHOR_ENUM.len());
    }

    fn dr(kind: Kind, number: u32, textures: usize, meta: DrawableMeta) -> Drawable {
        Drawable { sex: Sex::Male, kind, number, race: if kind.is_prop() { String::new() } else { "u".into() }, textures: (0..textures).map(|k| Texture { letter: (b'a' + k as u8) as char, ..Default::default() }).collect(), meta, ..Default::default() }
    }

    #[test]
    fn every_setting_is_written_and_reads_back_the_same() {
        let heels = DrawableMeta { expression_mods: [0.0, 0.0, 0.0, 0.0, 0.9], prop_mask: Some(65), flags: 65536, flags2: 3, audio: Some("hash_1234ABCD".into()), audio2: Some("none".into()), ..Default::default() };
        let hat = DrawableMeta { expression_mods: [-0.34999996, 0.0, 0.0, 0.0, 0.0], render_flags: 1, flags: 2, flags2: 4, stickyness: 1, audio: Some("none".into()), ..Default::default() };
        let mut jacket = dr(Kind::Component(11), 0, 3, DrawableMeta::default());
        jacket.alternates = vec![(1, "a.ydd".into()), (2, "b.ydd".into())];
        jacket.cloth = Some("c.yld".into());
        jacket.race = "r".into();
        jacket.textures[2].tex_id = Some(5);
        let ds = [jacket, dr(Kind::Component(11), 1, 1, DrawableMeta::default()), dr(Kind::Component(6), 0, 2, heels), dr(Kind::Prop(0), 0, 4, hat), dr(Kind::Prop(11), 0, 1, DrawableMeta::default())];
        let refs: Vec<&Drawable> = ds.iter().collect();
        let y = Ymt::of(&refs);
        let jbib = &y.components[&11][0];
        assert_eq!((jbib.prop_mask, jbib.alternates, jbib.cloth, jbib.textures.clone()), (17, 2, true, vec![1, 1, 5]), "a _r drawable: propMask 17, texId 1");
        assert_eq!(y.props[&0][0].textures, [0, 1, 2, 3], "a prop's texture ids are its indices");
        assert_eq!(render_flags().iter().map(|x| x.0.as_str()).collect::<Vec<_>>(), ["PRF_ALPHA", "PRF_DECAL", "PRF_CUTOUT"]);
        let bytes = ymt_bytes(&y, "mp_m_MyPack_1").unwrap();
        assert_eq!(&bytes[..4], b"RSC7");
        let (back, dlc, odd) = read_ymt(&bytes).unwrap();
        assert_eq!((back, dlc, odd), (y.clone(), rage_joaat("mp_m_mypack_1"), vec![]));
        // and into drawables again: only what differs from the usual is stored
        let mut again = dr(Kind::Component(11), 0, 3, DrawableMeta::default());
        again.race = "r".into();
        y.components[&11][0].apply(&mut again);
        assert_eq!((again.meta.prop_mask, again.textures.iter().map(|t| t.tex_id).collect::<Vec<_>>()), (None, vec![None, None, Some(5)]));
        let mut feet = dr(Kind::Component(6), 0, 2, DrawableMeta::default());
        y.components[&6][0].apply(&mut feet);
        assert_eq!((feet.meta.heels(), feet.meta.prop_mask, feet.meta.audio.as_deref(), feet.meta.audio2.as_deref()), (Some(0.9), Some(65), Some("hash_1234ABCD"), Some("none")));
        // the old reader of rage-formats reads it too
        let (info, _, _) = rage_formats::parse_ymt(&bytes).expect("parse_ymt");
        assert_eq!(info.component_data.len(), 2);
        // empty
        ymt_bytes(&Ymt::default(), "x").unwrap();
    }

    /// Every YMT of a real pack (set CRCK_CLOTHING_PACK to a folder with .ymt files): read, written again, and the XML compared.
    #[test]
    #[ignore = "needs a pack: set CRCK_CLOTHING_PACK=<folder with .ymt files> and run with --ignored"]
    fn real_pack_ymts_are_written_again_as_they_were() {
        let dir = std::path::PathBuf::from(std::env::var("CRCK_CLOTHING_PACK").expect("CRCK_CLOTHING_PACK"));
        let mut done = 0;
        for f in walk(&dir) {
            if !f.extension().is_some_and(|e| e.eq_ignore_ascii_case("ymt")) || f.file_name().is_some_and(|n| n.to_string_lossy().to_lowercase().contains("creaturemetadata")) {
                continue;
            }
            let bytes = std::fs::read(&f).unwrap();
            let Ok(orig) = dump_meta(&bytes) else { continue };
            if !orig.warnings.is_empty() {
                println!("skipped (reader warnings) {}", f.display());
                continue;
            }
            let (y, dlc, odd) = Ymt::read(&orig.root).unwrap();
            assert!(odd.is_empty(), "{}: {odd:?}", f.display());
            // the DLC name: from the file name (<ped>_<dlc>.ymt)
            let stem = f.file_stem().unwrap().to_string_lossy().to_lowercase();
            let name = stem.split_once("freemode_01_").map(|x| x.1.to_string()).unwrap();
            assert_eq!(dlc, rage_joaat(&name), "{}: the DLC name hash is the file's", f.display());
            let ours = ymt_bytes(&y, &name).unwrap_or_else(|e| panic!("{}: {e:#}", f.display()));
            let names = NameTable::core();
            let back = dump_meta(&ours).unwrap();
            // every modelled setting, props included
            assert_eq!(Ymt::read(&back.root).unwrap().0, y, "{}", f.display());
            // and the text of the drawables exactly. After them, some packs list compInfos and props in their own order (by
            // anchor name, components shuffled); ours go by number. Each entry names its component / anchor and index, so the
            // order means nothing to the game, and the settings in them are compared above.
            let head = |x: String| x.split("<compInfos").next().unwrap_or_default().to_string();
            let (a, b) = (head(normalised(&to_xml(&orig.root, &names))), head(normalised(&to_xml(&back.root, &names))));
            if a != b {
                let at = a.lines().zip(b.lines()).position(|(x, y)| x != y).unwrap_or(0);
                panic!("{}: differs near line {}: {:?} vs {:?}", f.display(), at + 1, a.lines().nth(at), b.lines().nth(at));
            }
            done += 1;
        }
        println!("{done} YMT(s) written again identically");
        assert!(done > 0);
    }

    fn walk(d: &std::path::Path) -> Vec<std::path::PathBuf> {
        let mut out = vec![];
        for e in std::fs::read_dir(d).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                out.extend(walk(&p));
            } else {
                out.push(p);
            }
        }
        out
    }
}
