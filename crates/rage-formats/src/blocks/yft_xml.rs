//! `.yft` to XML and back.
//!
//! The XML holds the fragment's name, bounding sphere, drawables (the same `<Drawable>` XML a `.ydr` has, plus the
//! fragDrawable's own bytes), the default bone pose and the physics children as readable views, and `<Chunks>`:
//! the rest of the file as bytes with its pointers as links (see [`super::yft`]). The drawables and the chunks are
//! what [`build_yft_from_xml`] builds from; the name, sphere, bone pose and physics children are views of bytes
//! inside the chunks and the checked build refuses an XML where an edit to one of them does not match the chunks.
//!
//! Drawables are numbered in document order (main, extra, physics children, the others) and links name them
//! `d<n>`; `c<n>+<offset>` is an offset into chunk `n`, `p` the pages info. A drawable whose shader group,
//! skeleton or joints are the ones an earlier drawable has leaves them out and says which drawable has them
//! (`<ShaderGroupFrom value="0" />`), so the blocks stay shared.

use std::path::Path;

use anyhow::{bail, Context, Result};

use super::base::PagesInfo;
use super::drawable::{Drawable, FragTail, Shared, FRAG_DRAWABLE_SIZE};
use super::xml::{attr_str, child, float, Node, XmlOut};
use super::yft::{lod_children, read_fragment, write_fragment, Chunk, ReadFrag, Run};
use super::{BlockId, Built, Graph};
use crate::math::Vec3;
use crate::names::NameTable;

fn f32_at(b: &[u8], o: usize) -> f32 { f32::from_le_bytes(b[o..o + 4].try_into().unwrap()) }
fn u16_at(b: &[u8], o: usize) -> u16 { u16::from_le_bytes(b[o..o + 2].try_into().unwrap()) }
fn u32_at(b: &[u8], o: usize) -> u32 { u32::from_le_bytes(b[o..o + 4].try_into().unwrap()) }
fn u64_at(b: &[u8], o: usize) -> u64 { u64::from_le_bytes(b[o..o + 8].try_into().unwrap()) }
fn vec3_at(b: &[u8], o: usize) -> Vec3 { Vec3::new(f32_at(b, o), f32_at(b, o + 4), f32_at(b, o + 8)) }

fn hex(x: &mut XmlOut, bytes: &[u8]) {
    for row in bytes.chunks(32) { x.line(&row.iter().map(|b| format!("{b:02x}")).collect::<String>()); }
}

struct Ctx<'a> {
    rf: ReadFrag,
    names: &'a NameTable,
    dds_dir: Option<&'a Path>,
    /// Drawables in document order.
    order: Vec<BlockId>,
    emitted: Vec<bool>,
    /// The chunks in the order a walk from the root meets them: the same wherever the writer puts them.
    chunk_order: Vec<BlockId>,
}

impl Ctx<'_> {
    fn id_at(&self, va: u64) -> Option<BlockId> { self.rf.drawables.iter().find(|d| d.0 == va).map(|d| d.1) }
    fn index_of(&self, id: BlockId) -> Option<usize> { self.order.iter().position(|&o| o == id) }

    /// `<Drawable>` for the `fragDrawable` at `va` (nothing for a null pointer or one already written).
    fn drawable(&mut self, x: &mut XmlOut, va: u64) -> Result<()> {
        let Some(id) = self.id_at(va) else { return Ok(()) };
        let k = self.index_of(id).context("a drawable outside the order")?;
        if std::mem::replace(&mut self.emitted[k], true) { return Ok(()); }
        let g = &self.rf.g;
        let d = g.get::<Drawable>(id);
        let earlier = |f: fn(&Drawable) -> Option<BlockId>, mine: Option<BlockId>| -> Option<usize> {
            let mine = mine?;
            (0..k).find(|&j| f(g.get::<Drawable>(self.order[j])) == Some(mine))
        };
        let (sg, sk, jo) = (earlier(|d| d.shader_group, d.shader_group), earlier(|d| d.skeleton, d.skeleton), earlier(|d| d.joints, d.joints));
        x.open("Drawable");
        d.write_xml_ex(x, g, self.names, self.dds_dir, Shared { shader_group: sg.is_some(), skeleton: sk.is_some(), joints: jo.is_some() })?;
        for (tag, from) in [("ShaderGroupFrom", sg), ("SkeletonFrom", sk), ("JointsFrom", jo)] {
            if let Some(j) = from { x.value(tag, j); }
        }
        let tail = d.frag.as_ref().context("a fragDrawable without its tail")?;
        let links: Vec<String> = tail.links.iter().map(|&(at, to, add)| format!("{at}>{}", self.token(to, add))).collect();
        x.open_attrs("FragExtra", &format!("Links=\"{}\"", links.join(",")));
        hex(x, &tail.raw);
        x.close("FragExtra");
        x.close("Drawable");
        Ok(())
    }

    fn token(&self, to: BlockId, add: u32) -> String {
        if to == self.rf.pages { return "p".into(); }
        if let Some(k) = self.index_of(to) { return format!("d{k}"); }
        for (k, &d) in self.order.iter().enumerate() {
            let d = self.rf.g.get::<Drawable>(d);
            if d.shader_group == Some(to) { return format!("sg{k}"); }
            if d.skeleton == Some(to) { return format!("sk{k}"); }
            if d.joints == Some(to) { return format!("jo{k}"); }
        }
        let n = self.chunk_order.iter().position(|&c| c == to).expect("a link to a block that is neither pages, drawable, shared block nor chunk");
        format!("c{n}+{add}")
    }

    /// `count` floats per row of a `FragBoneTransforms`: 12 floats per bone (three rows of four).
    fn bone_pose(&self, x: &mut XmlOut, va: u64) -> Result<()> {
        if va == 0 { return Ok(()); }
        let count = self.rf.r.slice(va, 0x20)?[0x10] as usize;
        let items = self.rf.r.slice(va + 0x20, count * 48)?;
        let floats: Vec<f32> = (0..count * 12).map(|i| f32_at(items, i * 4)).collect();
        x.open("BoneTransforms");
        for bone in floats.chunks(12) {
            x.raw_array("Item", bone, 12, |f| float(*f));
        }
        x.close("BoneTransforms");
        Ok(())
    }

    /// `FragPhysicsLODGroup` -> first LOD -> children, each placed by the LOD's transform of the same index.
    fn physics(&mut self, x: &mut XmlOut, h: &[u8]) -> Result<()> {
        let Some(children) = lod_children(&self.rf.r, h)? else { return Ok(()) };
        let group_va = u64_at(h, 0xF0);
        let lod_va = u64_at(self.rf.r.slice(group_va, 0x30)?, 0x10);
        let lod = self.rf.r.slice(lod_va, 0x130)?.to_vec();
        let transforms_va = u64_at(&lod, 0x100);
        let transforms: Vec<f32> = if transforms_va == 0 { Vec::new() } else {
            let n = u32_at(self.rf.r.slice(transforms_va, 0x20)?, 0x10) as usize;
            let b = self.rf.r.slice(transforms_va + 0x20, n * 64)?;
            (0..n * 16).map(|i| f32_at(b, i * 4)).collect()
        };
        x.open("Physics");
        x.vec3("PositionOffset", vec3_at(&lod, 0x30));
        x.open("Children");
        for (i, va) in children.into_iter().enumerate() {
            let child = self.rf.r.slice(va, 0x100).with_context(|| format!("physics child {i}"))?.to_vec();
            x.open("Item");
            x.value("GroupIndex", u16_at(&child, 0x10));
            x.value("BoneTag", u16_at(&child, 0x12));
            if let Some(m) = transforms.chunks(16).nth(i) { x.raw_array("Transform", m, 16, |f| float(*f)); }
            self.drawable(x, u64_at(&child, 0xA0)).with_context(|| format!("physics child {i} drawable"))?;
            x.close("Item");
        }
        x.close("Children");
        x.close("Physics");
        Ok(())
    }
}

/// A `.yft` as XML (see the module note); embedded textures are written to `dds_dir` when given.
pub fn dump_yft_xml(file: &[u8], names: &NameTable, dds_dir: Option<&Path>) -> Result<String> {
    let rf = read_fragment(file)?;
    let h = rf.r.sys[..0x130].to_vec();
    // The document order of the drawables, which the links name them by.
    let mut order: Vec<BlockId> = Vec::new();
    let id_at = |va: u64| rf.drawables.iter().find(|d| d.0 == va).map(|d| d.1);
    let mut push = |va: u64| { if let Some(id) = id_at(va) { if !order.contains(&id) { order.push(id); } } };
    push(u64_at(&h, 0x30));
    let (array, name_array, count) = (u64_at(&h, 0x38), u64_at(&h, 0x40), u32_at(&h, 0x48) as usize);
    let extras = if array != 0 && count > 0 { rf.r.u64s(array, count)? } else { Vec::new() };
    for &va in &extras { push(va); }
    let children = lod_children(&rf.r, &h)?.unwrap_or_default();
    let child_drawables = children.iter().map(|&va| Ok(u64_at(rf.r.slice(va, 0x100)?, 0xA0))).collect::<Result<Vec<u64>>>()?;
    for &va in &child_drawables { push(va); }
    let (chunk_order, found) = walk(&rf);
    for id in found { if !order.contains(&id) { order.push(id); } }

    let emitted = vec![false; order.len()];
    let mut c = Ctx { rf, names, dds_dir, order, emitted, chunk_order };
    let mut x = XmlOut::new();
    x.open("Fragment");
    x.string("Name", &c.rf.r.string(u64_at(&h, 0x58))?.unwrap_or_default());
    x.vec3("BoundingSphereCenter", vec3_at(&h, 0x20));
    x.value("BoundingSphereRadius", float(f32_at(&h, 0x2C)));
    c.drawable(&mut x, u64_at(&h, 0x30)).context("the main drawable")?;

    if !extras.is_empty() {
        let name_ptrs = if name_array == 0 { Vec::new() } else { c.rf.r.u64s(name_array, count)? };
        x.open("ExtraDrawables");
        for (i, va) in extras.into_iter().enumerate() {
            if va == 0 { continue; }
            x.open("Item");
            x.string("Name", &name_ptrs.get(i).map_or(Ok(None), |&p| c.rf.r.string(p))?.unwrap_or_default());
            c.drawable(&mut x, va).with_context(|| format!("extra drawable {i}"))?;
            x.close("Item");
        }
        x.close("ExtraDrawables");
    }

    c.bone_pose(&mut x, u64_at(&h, 0xA8)).context("the bone transforms")?;
    c.physics(&mut x, &h).context("the physics children")?;

    let rest: Vec<BlockId> = (0..c.order.len()).filter(|&k| !c.emitted[k]).map(|k| c.order[k]).collect();
    if !rest.is_empty() {
        x.open("OtherDrawables");
        for id in rest {
            let va = c.rf.drawables.iter().find(|d| d.1 == id).unwrap().0;
            x.open("Item");
            c.drawable(&mut x, va)?;
            x.close("Item");
        }
        x.close("OtherDrawables");
    }

    x.open("Chunks");
    for (n, &id) in c.chunk_order.clone().iter().enumerate() {
        let chunk = c.rf.g.get::<Chunk>(id);
        let links: Vec<String> = chunk.links.iter().map(|&(at, to, add)| format!("{at}>{}", c.token(to, add))).collect();
        let pred = c.rf.chunks.iter().find(|&&(_, e, _)| Some(e) == c.rf.chunks.iter().find(|k| k.2 == id).map(|k| k.0)).and_then(|p| c.chunk_order.iter().position(|&o| o == p.2));
        let after = pred.map_or(String::new(), |p| format!(" After=\"{p}\""));
        x.open_attrs("Chunk", &format!("Id=\"{n}\" Size=\"{}\"{after} Links=\"{}\"", chunk.data.len(), links.join(",")));
        hex(&mut x, &chunk.data);
        x.close("Chunk");
    }
    x.close("Chunks");
    x.close("Fragment");
    Ok(x.out)
}

fn parse_hex(s: &str) -> Result<Vec<u8>> {
    let digits: Vec<u8> = s.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    if digits.len() % 2 != 0 { bail!("hex text of odd length"); }
    digits.chunks(2).map(|p| {
        let d = |c: u8| (c as char).to_digit(16).map(|v| v as u8).context("a character that is not hex");
        Ok(d(p[0])? << 4 | d(p[1])?)
    }).collect()
}

/// `"12>d3,20>c1+16,8>p"`: (offset, link) pairs; a link is resolved by `to`.
fn parse_links(s: &str, mut to: impl FnMut(&str) -> Result<(BlockId, u32)>) -> Result<Vec<(usize, BlockId, u32)>> {
    s.split(',').map(str::trim).filter(|l| !l.is_empty()).map(|l| {
        let (at, target) = l.split_once('>').with_context(|| format!("link `{l}` has no `>`"))?;
        let (id, add) = to(target).with_context(|| format!("link `{l}`"))?;
        Ok((at.parse().with_context(|| format!("link `{l}` has no offset"))?, id, add))
    }).collect()
}

/// The `<Fragment>` document read into a graph; returns it with the root chunk and the pages info.
fn read_yft_xml(doc: &roxmltree::Document, dds_dir: Option<&Path>, warnings: &mut Vec<String>) -> Result<(Graph, BlockId, BlockId)> {
    let node = doc.root_element();
    let tag = node.tag_name().name();
    if tag != "Fragment" { bail!("the XML's root element is <{tag}>, not <Fragment>: it is not a fragment"); }
    let Some(chunks_node) = child(node, "Chunks") else {
        bail!("the XML has no <Chunks>: it holds no physics data, so it cannot be built into a .yft (export the .yft with this tool to get one that does)");
    };
    let mut g = Graph::new();
    let pages = g.add(PagesInfo::default());
    let nodes: Vec<Node> = node.descendants().filter(|n| n.has_tag_name("Drawable")).collect();
    let mut drawables = Vec::new();
    for n in &nodes {
        let id = Drawable::read_xml(*n, &mut g, dds_dir, warnings).context("a <Drawable>")?;
        g.get_mut::<Drawable>(id).pages = Some(pages);
        drawables.push(id);
    }
    // Blocks an earlier drawable has.
    for (k, dn) in nodes.iter().enumerate() {
        for (tag, which) in [("ShaderGroupFrom", 0), ("SkeletonFrom", 1), ("JointsFrom", 2)] {
            let Some(from) = child(*dn, tag) else { continue };
            let j: usize = attr_str(from, "value").trim().parse().with_context(|| format!("drawable {k}'s <{tag}>"))?;
            if j >= drawables.len() { bail!("drawable {k}'s <{tag}> names drawable {j}, which does not exist"); }
            let donor = g.get::<Drawable>(drawables[j]);
            let block = match which { 0 => donor.shader_group, 1 => donor.skeleton, _ => donor.joints };
            if block.is_none() { bail!("drawable {k}'s <{tag}> names drawable {j}, which has none"); }
            let d = g.get_mut::<Drawable>(drawables[k]);
            match which { 0 => d.shader_group = block, 1 => d.skeleton = block, _ => d.joints = block }
        }
    }
    // Chunk blocks first, then the links that name them.
    let chunk_nodes: Vec<Node> = chunks_node.children().filter(|n| n.has_tag_name("Chunk")).collect();
    if chunk_nodes.is_empty() { bail!("<Chunks> is empty: the fragment has no header"); }
    let mut chunks = Vec::new();
    for (n, cn) in chunk_nodes.iter().enumerate() {
        if attr_str(*cn, "Id") != n.to_string() { bail!("chunk {n} has Id=\"{}\": chunks must be numbered 0, 1, 2 ... in order", attr_str(*cn, "Id")); }
        let data = parse_hex(cn.text().unwrap_or("")).with_context(|| format!("chunk {n}"))?;
        if attr_str(*cn, "Size") != data.len().to_string() { bail!("chunk {n} says Size=\"{}\" but holds {} bytes", attr_str(*cn, "Size"), data.len()); }
        chunks.push(g.add(Chunk { data, links: Vec::new(), owner: None }));
    }
    let shared: Vec<[Option<BlockId>; 3]> = drawables.iter().map(|&i| { let d = g.get::<Drawable>(i); [d.shader_group, d.skeleton, d.joints] }).collect();
    let resolve = |t: &str| -> Result<(BlockId, u32)> {
        if t == "p" { return Ok((pages, 0)); }
        for (prefix, which) in [("sg", 0), ("sk", 1), ("jo", 2)] {
            if let Some(k) = t.strip_prefix(prefix) {
                let k: usize = k.parse().context("a drawable number")?;
                let d = shared.get(k).with_context(|| format!("drawable {k} does not exist"))?;
                return Ok((d[which].with_context(|| format!("drawable {k} has no {prefix} block"))?, 0));
            }
        }
        if let Some(k) = t.strip_prefix('d') {
            let k: usize = k.parse().context("a drawable number")?;
            return Ok((*drawables.get(k).with_context(|| format!("drawable {k} does not exist"))?, 0));
        }
        if let Some(rest) = t.strip_prefix('c') {
            let (k, add) = rest.split_once('+').context("a chunk link is c<n>+<offset>")?;
            let k: usize = k.parse().context("a chunk number")?;
            return Ok((*chunks.get(k).with_context(|| format!("chunk {k} does not exist"))?, add.parse().context("a chunk offset")?));
        }
        bail!("unknown link target `{t}`")
    };
    for (n, cn) in chunk_nodes.iter().enumerate() {
        let links = parse_links(attr_str(*cn, "Links"), resolve).with_context(|| format!("chunk {n}"))?;
        let c = g.get_mut::<Chunk>(chunks[n]);
        if let Some(l) = links.iter().find(|l| l.0 + 8 > c.data.len()) { bail!("chunk {n}: link at {} is past the chunk's end", l.0); }
        c.links = links;
    }
    for (k, dn) in nodes.iter().enumerate() {
        let extra = child(*dn, "FragExtra").with_context(|| format!("drawable {k} has no <FragExtra>"))?;
        let raw = parse_hex(extra.text().unwrap_or("")).with_context(|| format!("drawable {k}'s <FragExtra>"))?;
        if raw.len() != FRAG_DRAWABLE_SIZE - 0xA8 { bail!("drawable {k}'s <FragExtra> holds {} bytes, not {}", raw.len(), FRAG_DRAWABLE_SIZE - 0xA8); }
        let links = parse_links(attr_str(extra, "Links"), resolve).with_context(|| format!("drawable {k}"))?;
        if let Some(l) = links.iter().find(|l| l.0 + 8 > raw.len()) { bail!("drawable {k}: link at {} is past the extra data's end", l.0); }
        g.get_mut::<Drawable>(drawables[k]).frag = Some(FragTail { raw, links });
    }
    // Chunks stored one directly after the other stay so.
    let mut after: Vec<Option<usize>> = Vec::new();
    for (n, cn) in chunk_nodes.iter().enumerate() {
        let a = attr_str(*cn, "After");
        let a = if a.is_empty() { None } else { Some(a.parse::<usize>().with_context(|| format!("chunk {n}'s After"))?) };
        if let Some(p) = a { if p >= chunks.len() || p == n { bail!("chunk {n} says it follows chunk {p}, which cannot be"); } }
        after.push(a);
    }
    let mut next: Vec<Option<usize>> = vec![None; chunks.len()];
    for (n, a) in after.iter().enumerate() {
        if let Some(p) = *a {
            if next[p].replace(n).is_some() { bail!("two chunks say they follow chunk {p}"); }
        }
    }
    let mut seen = 0;
    for head in (0..chunks.len()).filter(|&n| after[n].is_none()) {
        let mut members = vec![chunks[head]];
        let mut at = head;
        while let Some(n) = next[at] { members.push(chunks[n]); at = n; }
        seen += members.len();
        if members.len() > 1 {
            if let Some(m) = members[..members.len() - 1].iter().find(|&&m| g.length(m) % 16 != 0) { bail!("a chunk of {} bytes cannot be followed directly by another: its size is not a multiple of 16", g.length(*m)); }
            Run::new(&mut g, members);
        }
    }
    if seen != chunks.len() { bail!("the chunks' After attributes form a loop"); }
    if after[0].is_some() { bail!("chunk 0 holds the header, so it cannot follow another chunk"); }
    Ok((g, chunks[0], pages))
}

/// The `.yft` for a `<Fragment>` document (see the module note for what it can build).
pub fn build_yft_from_xml(xml: &str, dds_dir: Option<&Path>) -> Result<Vec<u8>> {
    let doc = roxmltree::Document::parse(xml).context("the XML is not well formed")?;
    let (mut g, root, pages) = read_yft_xml(&doc, dds_dir, &mut Vec::new())?;
    write_fragment(&mut g, root, pages)
}

/// The XML as lines to compare: element path, attributes, and text with numbers in one spelling. A chunk's size
/// and trailing zero bytes are left out: where the writer puts a chunk it can leave a gap after it that the
/// next read counts as part of the chunk.
fn flatten(n: Node, path: &str, out: &mut Vec<String>) {
    let path = format!("{path}/{}", n.tag_name().name());
    let chunk = n.tag_name().name() == "Chunk";
    let mut attrs: Vec<String> = n.attributes().filter(|a| !(chunk && (a.name() == "Size" || a.name() == "After"))).map(|a| format!("{}={}", a.name(), norm(a.value()))).collect();
    attrs.sort();
    let text: String = n.children().filter(|c| c.is_text()).filter_map(|c| c.text()).collect();
    let hexy = chunk || n.tag_name().name() == "FragExtra";
    let mut text = if hexy { text.split_whitespace().collect::<String>().to_lowercase() } else { norm(&text) };
    if chunk { while text.ends_with("00") && text.len() % 2 == 0 { text.truncate(text.len() - 2); } }
    out.push(format!("{path} [{}] {text}", attrs.join(" ")));
    for c in n.children().filter(|c| c.is_element()) { flatten(c, &path, out); }
}
fn norm(s: &str) -> String {
    let words: Vec<&str> = s.split_whitespace().collect();
    if !words.is_empty() && words.iter().all(|w| w.parse::<f32>().is_ok_and(f32::is_finite)) {
        return words.iter().map(|w| float(w.parse().unwrap())).collect::<Vec<_>>().join(" ");
    }
    words.join("")
}
fn flat_doc(xml: &str) -> Result<Vec<String>> {
    let doc = roxmltree::Document::parse(xml).context("the XML is not well formed")?;
    let mut out = Vec::new();
    flatten(doc.root_element(), "", &mut out);
    Ok(out)
}

/// The first difference between two documents, compared as [`flatten`] sees them.
fn first_flat_difference(a: &str, b: &str) -> Result<Option<String>> {
    let (a, b) = (flat_doc(a)?, flat_doc(b)?);
    for i in 0..a.len().max(b.len()) {
        if a.get(i) != b.get(i) {
            let cut = |s: Option<&String>| s.map_or("<end>".to_owned(), |s| s.chars().take(160).collect());
            return Ok(Some(format!("expected `{}`, read back `{}`", cut(a.get(i)), cut(b.get(i)))));
        }
    }
    Ok(None)
}

/// [`build_yft_from_xml`], then reads the bytes back and checks them: the XML they read back as must equal the XML
/// that was given (numbers compared as numbers), so an edit the file cannot hold, such as to the name or to a
/// physics child, is an error and not silently dropped; and building that XML again must read back as the same XML.
/// Returns the file, the XML it reads back as and the warnings for what the reference tool accepts silently.
/// The comparison is of XML dumped with no DDS folder, so texture pixels are outside it.
pub fn build_yft_from_xml_checked(xml: &str, dds_dir: Option<&Path>) -> Result<Built> {
    let doc = roxmltree::Document::parse(xml).context("the XML is not well formed")?;
    let mut warnings = Vec::new();
    let (mut g, root, pages) = read_yft_xml(&doc, dds_dir, &mut warnings)?;
    let bytes = write_fragment(&mut g, root, pages)?;
    let names = NameTable::core();
    let back = dump_yft_xml(&bytes, &names, None).context("the written file cannot be read back")?;
    if let Some(msg) = first_flat_difference(xml, &back)? { bail!("the written file does not read back as the given XML: {msg}"); }
    let again = build_yft_from_xml(&back, None).context("the XML the file reads back as cannot be built")?;
    let again = dump_yft_xml(&again, &names, None)?;
    if let Some(msg) = first_flat_difference(&back, &again)? { bail!("building the read-back XML again does not read back identically: {msg}"); }
    Ok(Built { bytes, xml: back, warnings })
}

/// Chunk and drawable counts and the bytes in chunks, for tools that report on a file.
pub fn chunk_summary(file: &[u8]) -> Result<(usize, usize, usize)> {
    let rf = read_fragment(file)?;
    let bytes = rf.chunks.iter().map(|c| c.1 - c.0).sum();
    Ok((rf.chunks.len(), rf.drawables.len(), bytes))
}


/// The chunks and the drawables in the order a depth-first walk from the root chunk meets them. A chunk nothing
/// points to is dead data: the file never reaches it, so the XML leaves it out.
fn walk(rf: &ReadFrag) -> (Vec<BlockId>, Vec<BlockId>) {
    let (mut chunks, mut drawables, mut seen) = (Vec::new(), Vec::new(), std::collections::HashSet::new());
    let mut stack = vec![rf.root];
    while let Some(id) = stack.pop() {
        if !seen.insert(id.0) { continue; }
        if let Some(c) = rf.g.try_get::<Chunk>(id) {
            chunks.push(id);
            stack.extend(c.links.iter().rev().map(|l| l.1));
        } else if let Some(d) = rf.g.try_get::<Drawable>(id) {
            drawables.push(id);
            stack.extend(d.frag.iter().flat_map(|f| f.links.iter().rev().map(|l| l.1)));
        }
    }
    (chunks, drawables)
}

/// A `<Fragment>` for tests: `drawable_xml` (a `.ydr`'s XML) as the only drawable, a zeroed header chunk and `name`.
/// It is not a fragment the game would load: it has no physics.
#[doc(hidden)]
pub fn test_fragment_xml(drawable_xml: &str, name: &str) -> String {
    let inner = drawable_xml.split_once("<Drawable>").and_then(|(_, r)| r.rsplit_once("</Drawable>")).expect("a <Drawable> document").0;
    let mut name_bytes = name.as_bytes().to_vec();
    name_bytes.push(0);
    name_bytes.resize(name_bytes.len().div_ceil(16) * 16, 0);
    let hex = |b: &[u8]| b.iter().map(|b| format!("{b:02x}")).collect::<String>();
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Fragment>\n<Name>{name}</Name>\n<BoundingSphereCenter x=\"0\" y=\"0\" z=\"0\" />\n<BoundingSphereRadius value=\"0\" />\n\
         <Drawable>{inner}<FragExtra Links=\"\">{}</FragExtra>\n</Drawable>\n<Chunks>\n<Chunk Id=\"0\" Size=\"304\" Links=\"8>p,48>d0,88>c1+0\">{}</Chunk>\n<Chunk Id=\"1\" Size=\"{}\" Links=\"\">{}</Chunk>\n</Chunks>\n</Fragment>\n",
        hex(&[0; 0xA8]), hex(&[0; 304]), name_bytes.len(), hex(&name_bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blocks::ydr;

    fn tri_xml() -> String {
        let ydr = ydr::build_ydr_from_xml(include_str!("../../tests/fixtures/one_triangle.ydr.xml"), None).unwrap();
        ydr::dump_ydr_xml(&ydr, &NameTable::core(), None).unwrap()
    }

    #[test]
    fn a_fragment_xml_builds_into_a_yft_that_reads_back_as_the_same_xml() {
        let xml = test_fragment_xml(&tri_xml(), "tri");
        let built = build_yft_from_xml_checked(&xml, None).unwrap();
        let back = dump_yft_xml(&built.bytes, &NameTable::core(), None).unwrap();
        assert!(back.contains("<Name>tri</Name>") && back.contains("<Chunks>"), "{back}");
        assert_eq!(chunk_summary(&built.bytes).unwrap().1, 1);
        // the file builds again from what it reads back as, to the same file
        let again = build_yft_from_xml_checked(&back, None).unwrap();
        assert_eq!(again.xml, back);
        assert_eq!(super::super::yft::read_fragment(&again.bytes).unwrap().chunks.len(), 2);
    }

    #[test]
    fn an_edit_the_chunks_cannot_hold_is_an_error_not_dropped() {
        let xml = test_fragment_xml(&tri_xml(), "tri");
        let renamed = xml.replacen("<Name>tri</Name>", "<Name>other</Name>", 1);
        let err = build_yft_from_xml_checked(&renamed, None).unwrap_err().to_string();
        assert!(err.contains("does not read back as the given XML"), "{err}");
        let moved = xml.replacen("<BoundingSphereRadius value=\"0\" />", "<BoundingSphereRadius value=\"5\" />", 1);
        assert!(build_yft_from_xml_checked(&moved, None).is_err());
    }

    #[test]
    fn broken_links_are_errors() {
        let xml = test_fragment_xml(&tri_xml(), "tri");
        for (from, to, want) in [
            ("Links=\"8>p,48>d0,88>c1+0\"", "Links=\"8>p,48>d3,88>c1+0\"", "drawable 3 does not exist"),
            ("Links=\"8>p,48>d0,88>c1+0\"", "Links=\"8>p,48>d0,88>c7+0\"", "chunk 7 does not exist"),
            ("Links=\"8>p,48>d0,88>c1+0\"", "Links=\"8>p,48>d0,300>c1+0\"", "past the chunk's end"),
            ("Links=\"8>p,48>d0,88>c1+0\"", "Links=\"8>p,48>x0,88>c1+0\"", "unknown link target"),
            ("Id=\"1\"", "Id=\"2\"", "numbered 0, 1, 2"),
            ("Size=\"304\"", "Size=\"300\"", "says Size"),
        ] {
            let err = format!("{:#}", build_yft_from_xml_checked(&xml.replacen(from, to, 1), None).unwrap_err());
            assert!(err.contains(want), "{to}: {err}");
        }
        let no_chunks = xml.split("<Chunks>").next().unwrap().to_owned() + "</Fragment>";
        assert!(build_yft_from_xml_checked(&no_chunks, None).unwrap_err().to_string().contains("no <Chunks>"));
        assert!(build_yft_from_xml_checked("<Drawable/>", None).unwrap_err().to_string().contains("not a fragment"));
    }
}
