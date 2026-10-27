//! Shaders, ported from the reference tool's `Drawable.cs` (`ShaderGroup`, `ShaderFX`,
//! `ShaderParameter`, `ShaderParametersBlock`). Only the legacy PC layout is handled.
//!
//! A texture parameter points at a `TextureBase`: either a [`Texture`] of the group's
//! dictionary or a name-only [`TextureRef`] (the 80-byte `TextureBase` layout). Parameter
//! names are stored as the hash of the lowercased name, the way `ShaderParamNames` lists them.

use std::path::Path;

use anyhow::Result;

use super::base::{read_pointer_array64, read_string_block, PointerArray64, StringBlock, StructArray};
use super::texture::{Texture, TextureDictionary};
use super::xml::{attr_f32, attr_str, child, child_attr_u32, child_text, escape, float, hash_string, items, parse_hash, parse_hash_lower, Node, XmlOut};
use super::*;
use crate::names::NameTable;
use crate::rage_joaat;

/// `TextureBase` (80 bytes) that is not part of any dictionary: a shader parameter's texture named in XML.
pub struct TextureRef {
    pub name: Option<BlockId>,
    pub name_hash: u32,
    /// 2 when made from XML (`Drawable.cs:1167`).
    pub unknown_32h: u16,
}

impl TextureRef {
    /// A texture parameter's pointer: a block already in the pool (a dictionary texture) or a new `TextureRef`.
    pub fn read(r: &mut Reader, g: &mut Graph, va: u64) -> Result<Option<BlockId>> {
        if va == 0 { return Ok(None); }
        if r.cached_is::<Texture>(va) { return r.cached_as::<Texture>(va); }
        if let Some(id) = r.cached_as::<Self>(va)? { return Ok(Some(id)); }
        let mut c = r.cursor(va)?;
        c.skip(40);
        let name_ptr = c.u64();
        c.skip(2);
        let unknown_32h = c.u16();
        c.skip(28);
        c.check()?;
        let name = read_string_block(r, g, name_ptr)?;
        let name_hash = name.map_or(0, |n| rage_joaat(&g.get::<StringBlock>(n).0.to_lowercase()));
        let id = g.add(TextureRef { name, name_hash, unknown_32h });
        r.cache::<Self>(va, id); Ok(Some(id))
    }
}
impl Block for TextureRef {
    fn length(&self) -> usize { 80 }
    fn references(&self, _g: &Graph) -> Vec<BlockId> { self.name.into_iter().collect() }
    fn write(&self, w: &mut Writer, g: &Graph) -> Result<()> {
        // the first 80 bytes of `Texture::write`
        w.u32(0); w.u32(1); w.zeros(32);
        w.u64(g.ptr(self.name));
        w.u16(1); w.u16(self.unknown_32h); w.zeros(12);
        w.u32(0); w.u32(0);
        w.u32(0); w.u32(0);
        Ok(())
    }
    fn as_any(&self) -> &dyn Any { self } fn as_any_mut(&mut self) -> &mut dyn Any { self }
}

/// The name and hash of a texture parameter's block, a [`Texture`] or a [`TextureRef`].
fn texture_info(g: &Graph, id: BlockId) -> (&str, u32) {
    let (name, hash) = match (g.try_get::<Texture>(id), g.try_get::<TextureRef>(id)) {
        (Some(t), _) => (t.name, t.name_hash),
        (None, Some(t)) => (t.name, t.name_hash),
        (None, None) => (None, 0),
    };
    (name.map_or("", |n| g.get::<StringBlock>(n).0.as_str()), hash)
}

/// `ShaderParameter.Data`: a texture (`data_type` 0) or `data_type` vectors.
#[derive(Debug, Clone, PartialEq)]
pub enum ParamData {
    /// A [`Texture`] or a name-only [`TextureRef`]; `None` for a texture parameter with no name.
    Texture(Option<BlockId>),
    Vectors(Vec<Vec4>),
}

/// `ShaderParameter` plus its entry of `ShaderParametersBlock.Hashes`.
#[derive(Debug, Clone, PartialEq)]
pub struct ShaderParam { pub data_type: u8, pub unknown_1h: u8, pub name_hash: u32, pub data: ParamData }

/// `ShaderParametersBlock`: the parameter records, the vector data embedded after them
/// (the parts), the name hashes and padding.
pub struct ShaderParametersBlock {
    pub params: Vec<ShaderParam>,
    /// The vector data of each parameter as a `StructArray<Vec4>` part; `None` for textures.
    pub vec_blocks: Vec<Option<BlockId>>,
}

fn vec_attrs(v: Vec4) -> String {
    format!("x=\"{}\" y=\"{}\" z=\"{}\" w=\"{}\"", float(v.x), float(v.y), float(v.z), float(v.w))
}

impl ShaderParametersBlock {
    pub fn base_size(&self) -> usize {
        32 + self.params.iter().map(|p| 16 + 16 * p.data_type as usize).sum::<usize>() + 4 * self.params.len()
    }
    pub fn parameters_size(&self) -> u16 {
        self.params.iter().fold(self.params.len() as u16 * 16, |s, p| s.wrapping_add(16 * p.data_type as u16))
    }
    /// `BaseSize` rounded up to 16.
    pub fn parameters_data_size(&self) -> u16 { self.base_size().div_ceil(16).wrapping_mul(16) as u16 }
    pub fn texture_count(&self) -> u8 { self.params.iter().filter(|p| p.data_type == 0).count() as u8 }

    /// The parameters as `ReadXml` finishes them: textures numbered `i + 2` in `unknown_1h`, then,
    /// from the last parameter backwards, the vector parameters from 160 up by their `data_type`.
    pub fn from_params(g: &mut Graph, mut params: Vec<ShaderParam>) -> Self {
        for (i, p) in params.iter_mut().enumerate() {
            if p.data_type == 0 { p.unknown_1h = (i + 2) as u8; }
        }
        let mut offset = 160u32;
        for p in params.iter_mut().rev() {
            if p.data_type != 0 { p.unknown_1h = offset as u8; offset += p.data_type as u32; }
        }
        let vec_blocks = params.iter().map(|p| match &p.data {
            ParamData::Vectors(v) if p.data_type != 0 => Some(g.add(StructArray { items: v.clone() })),
            _ => None,
        }).collect();
        Self { params, vec_blocks }
    }

    /// `ShaderParametersBlock.WriteXml`: an `<Item>` per parameter.
    pub fn write_xml(&self, x: &mut XmlOut, g: &Graph, names: &NameTable) {
        for p in &self.params {
            let name = escape(&hash_string(p.name_hash, names));
            let kind = match p.data_type { 0 => "Texture", 1 => "Vector", _ => "Array" };
            let attrs = format!("name=\"{name}\" type=\"{kind}\"");
            match (&p.data, p.data_type) {
                (ParamData::Texture(Some(id)), 0) => {
                    x.open_attrs("Item", &attrs);
                    x.string("Name", texture_info(g, *id).0);
                    x.close("Item");
                }
                (ParamData::Vectors(v), 1) if !v.is_empty() => x.self_closing(&format!("Item {attrs} {}", vec_attrs(v[0]))),
                (ParamData::Vectors(v), t) if t > 1 => {
                    x.open_attrs("Item", &attrs);
                    for &vec in v { x.self_closing(&format!("Value {}", vec_attrs(vec))); }
                    x.close("Item");
                }
                _ => x.self_closing(&format!("Item {attrs}")),
            }
        }
    }

    /// `ShaderParametersBlock.ReadXml`: `n` is the `<Parameters>` node.
    pub fn read_xml(n: Node, g: &mut Graph) -> Result<Self> {
        let mut params = Vec::new();
        for item in n.children().filter(|c| c.is_element() && c.tag_name().name() == "Item") {
            let name_hash = parse_hash_lower(attr_str(item, "name"));
            let (data_type, data) = match attr_str(item, "type") {
                "Texture" => {
                    let data = child(item, "Name").map(|_| {
                        let text = child_text(item, "Name");
                        let name_hash = if text.is_empty() { 0 } else { rage_joaat(&text.to_lowercase()) };
                        let name = if text.is_empty() { None } else { Some(g.add(StringBlock(text))) };
                        g.add(TextureRef { name, name_hash, unknown_32h: 2 })
                    });
                    (0, ParamData::Texture(data))
                }
                "Vector" => (1, ParamData::Vectors(vec![Vec4::new(attr_f32(item, "x"), attr_f32(item, "y"), attr_f32(item, "z"), attr_f32(item, "w"))])),
                "Array" => {
                    let v: Vec<Vec4> = item.children().filter(|c| c.is_element() && c.tag_name().name() == "Value")
                        .map(|c| Vec4::new(attr_f32(c, "x"), attr_f32(c, "y"), attr_f32(c, "z"), attr_f32(c, "w"))).collect();
                    // an array with no values has data type 0, like a texture
                    if v.is_empty() { (0, ParamData::Texture(None)) } else {
                        let n = count_u8(v.len(), "values in an array parameter")
                            .with_context(|| format!("shader parameter {}", attr_str(item, "name")))?;
                        (n, ParamData::Vectors(v))
                    }
                }
                _ => (0, ParamData::Texture(None)),
            };
            params.push(ShaderParam { data_type, unknown_1h: 0, name_hash, data });
        }
        Ok(Self::from_params(g, params))
    }

    pub fn read(r: &mut Reader, g: &mut Graph, va: u64, count: usize) -> Result<Option<BlockId>> {
        if va == 0 { return Ok(None); }
        if let Some(id) = r.cached_as::<Self>(va)? { return Ok(Some(id)); }
        let mut c = r.cursor(va)?;
        let mut recs = Vec::with_capacity(count);
        for _ in 0..count {
            let data_type = c.u8(); let unknown_1h = c.u8(); c.skip(2 + 4);
            recs.push((data_type, unknown_1h, c.u64()));
        }
        c.check()?;
        let mut data = Vec::with_capacity(count);
        let mut vec_blocks = Vec::with_capacity(count);
        let mut vector_bytes = 0u64;
        for &(data_type, _, ptr) in &recs {
            if data_type == 0 {
                data.push(ParamData::Texture(TextureRef::read(r, g, ptr)?));
                vec_blocks.push(None);
            } else {
                vector_bytes += 16 * data_type as u64;
                let v = r.structs::<Vec4>(ptr, data_type as usize)?;
                vec_blocks.push(Some(g.add(StructArray { items: v.clone() })));
                data.push(ParamData::Vectors(v));
            }
        }
        // the vector data is embedded between the records and the hashes
        let hashes = r.u32s(va + 16 * count as u64 + vector_bytes, count)?;
        let params = recs.into_iter().zip(data).zip(hashes)
            .map(|(((data_type, unknown_1h, _), data), name_hash)| ShaderParam { data_type, unknown_1h, name_hash, data })
            .collect();
        let id = g.add(ShaderParametersBlock { params, vec_blocks });
        r.cache::<Self>(va, id); Ok(Some(id))
    }
}

impl Block for ShaderParametersBlock {
    fn length(&self) -> usize { self.base_size() + self.parameters_data_size() as usize * 4 }
    fn references(&self, _g: &Graph) -> Vec<BlockId> {
        self.params.iter().filter_map(|p| match p.data { ParamData::Texture(id) => id, _ => None }).collect()
    }
    fn parts(&self) -> Vec<(usize, BlockId)> {
        let mut offset = self.params.len() * 16;
        let mut parts = Vec::new();
        for (p, b) in self.params.iter().zip(&self.vec_blocks) {
            if let Some(b) = b { parts.push((offset, *b)); }
            offset += 16 * p.data_type as usize;
        }
        parts
    }
    fn write(&self, w: &mut Writer, g: &Graph) -> Result<()> {
        for (i, p) in self.params.iter().enumerate() {
            let ptr = match &p.data {
                ParamData::Texture(id) => g.ptr(*id),
                ParamData::Vectors(_) => g.ptr(self.vec_blocks.get(i).copied().flatten()),
            };
            w.u8(p.data_type); w.u8(p.unknown_1h); w.u16(0); w.u32(0); w.u64(ptr);
        }
        for p in &self.params {
            if let ParamData::Vectors(v) = &p.data {
                for k in 0..p.data_type as usize { w.vec4(v.get(k).copied().unwrap_or_default()); }
            }
        }
        for p in &self.params { w.u32(p.name_hash); }
        w.zeros(32 + 4 * self.parameters_data_size() as usize);
        Ok(())
    }
    fn as_any(&self) -> &dyn Any { self } fn as_any_mut(&mut self) -> &mut dyn Any { self }
}

/// `ShaderFX` (48 bytes): one shader with its parameters.
pub struct ShaderFx {
    pub name_hash: u32,
    pub file_name_hash: u32,
    pub render_bucket: u8,
    pub render_bucket_mask: u32,
    pub params: Option<BlockId>,
    /// 32768 unless read otherwise.
    pub unknown_12h: u16,
}

impl ShaderFx {
    /// `ShaderFX.WriteXml`.
    pub fn write_xml(&self, x: &mut XmlOut, g: &Graph, names: &NameTable) {
        x.string("Name", &hash_string(self.name_hash, names));
        x.string("FileName", &hash_string(self.file_name_hash, names));
        x.value("RenderBucket", self.render_bucket);
        if let Some(p) = self.params {
            x.open("Parameters");
            g.get::<ShaderParametersBlock>(p).write_xml(x, g, names);
            x.close("Parameters");
        }
    }

    /// `ShaderFX.ReadXml`; the mask is `(1 << bucket) | 0xFF00`.
    pub fn read_xml(n: Node, g: &mut Graph) -> Result<BlockId> {
        let render_bucket = child_attr_u32(n, "RenderBucket", "value") as u8;
        let params = match child(n, "Parameters") {
            Some(p) => { let b = ShaderParametersBlock::read_xml(p, g)?; Some(g.add(b)) }
            None => None,
        };
        Ok(g.add(ShaderFx {
            name_hash: parse_hash(&child_text(n, "Name")),
            file_name_hash: parse_hash(&child_text(n, "FileName")),
            render_bucket,
            render_bucket_mask: 1u32.wrapping_shl(render_bucket as u32) | 0xFF00,
            params,
            unknown_12h: 32768,
        }))
    }

    pub fn read(r: &mut Reader, g: &mut Graph, va: u64) -> Result<Option<BlockId>> {
        if va == 0 { return Ok(None); }
        if let Some(id) = r.cached_as::<Self>(va)? { return Ok(Some(id)); }
        let mut c = r.cursor(va)?;
        let params_ptr = c.u64();
        let name_hash = c.u32(); c.skip(4);
        let count = c.u8(); let render_bucket = c.u8(); let unknown_12h = c.u16();
        c.skip(2 + 2);
        let file_name_hash = c.u32(); c.skip(4);
        let render_bucket_mask = c.u32(); c.skip(12);
        c.check()?;
        let params = ShaderParametersBlock::read(r, g, params_ptr, count as usize)?;
        let id = g.add(ShaderFx { name_hash, file_name_hash, render_bucket, render_bucket_mask, params, unknown_12h });
        r.cache::<Self>(va, id); Ok(Some(id))
    }
}
impl Block for ShaderFx {
    fn length(&self) -> usize { 48 }
    fn references(&self, _g: &Graph) -> Vec<BlockId> { self.params.into_iter().collect() }
    fn write(&self, w: &mut Writer, g: &Graph) -> Result<()> {
        let (count, size, data_size, textures) = self.params.map_or((0, 0, 0, 0), |p| {
            let p = g.get::<ShaderParametersBlock>(p);
            (p.params.len(), p.parameters_size(), p.parameters_data_size(), p.texture_count())
        });
        let count = count_u8(count, "parameters in a shader")?;
        w.u64(g.ptr(self.params));
        w.u32(self.name_hash); w.u32(0);
        w.u8(count); w.u8(self.render_bucket); w.u16(self.unknown_12h);
        w.u16(size); w.u16(data_size);
        w.u32(self.file_name_hash); w.u32(0);
        w.u32(self.render_bucket_mask);
        w.u16(0); w.u8(0); w.u8(textures);
        w.u64(0);
        Ok(())
    }
    fn as_any(&self) -> &dyn Any { self } fn as_any_mut(&mut self) -> &mut dyn Any { self }
}

/// `ShaderGroup` (64 bytes): the embedded texture dictionary and the shaders.
pub struct ShaderGroup {
    /// 1080113136.
    pub vft: u32,
    pub dictionary: Option<BlockId>,
    /// A `PointerArray64` of [`ShaderFx`]; its length is the count written.
    pub shaders: Option<BlockId>,
}

impl ShaderGroup {
    /// The number of shaders: the length of the pointer array (null entries included).
    pub fn count(&self, g: &Graph) -> usize { self.shaders.map_or(0, |s| g.get::<PointerArray64>(s).items.len()) }

    /// `ShaderGroup.WriteXml`: the dictionary (with its DDS files in `dds_dir`), then the shaders.
    pub fn write_xml(&self, x: &mut XmlOut, g: &Graph, names: &NameTable, dds_dir: Option<&Path>) -> Result<()> {
        if let Some(d) = self.dictionary {
            g.get::<TextureDictionary>(d).write_xml_node(x, g, "TextureDictionary", dds_dir)?;
        }
        let fxs: Vec<BlockId> = self.shaders.map_or_else(Vec::new, |s| g.get::<PointerArray64>(s).items.iter().flatten().copied().collect());
        if fxs.is_empty() {
            x.self_closing("Shaders");
        } else {
            x.open("Shaders");
            for fx in fxs {
                x.open("Item");
                g.get::<ShaderFx>(fx).write_xml(x, g, names);
                x.close("Item");
            }
            x.close("Shaders");
        }
        Ok(())
    }

    /// `ShaderGroup.ReadXml`: `n` is the `<ShaderGroup>` node. A texture parameter naming a texture
    /// that is not in the dictionary adds a line to `warnings` (the reference tool accepts it silently).
    pub fn read_xml(n: Node, g: &mut Graph, dds_dir: Option<&Path>, warnings: &mut Vec<String>) -> Result<BlockId> {
        let dictionary = match child(n, "TextureDictionary") {
            Some(d) => {
                let mut textures = Vec::new();
                for item in d.children().filter(|c| c.is_element() && c.tag_name().name() == "Item") {
                    textures.push(Texture::read_xml(item, g, dds_dir)?);
                }
                Some(TextureDictionary::from_textures(g, textures))
            }
            None => None,
        };
        let fxs = items(n, "Shaders").into_iter().map(|i| ShaderFx::read_xml(i, g)).collect::<Result<Vec<_>>>()?;
        let shaders = if fxs.is_empty() { None } else { Some(g.add(PointerArray64 { items: fxs.into_iter().map(Some).collect() })) };
        let sg = ShaderGroup { vft: 1080113136, dictionary, shaders };
        warnings.extend(sg.resolve_textures(g));
        Ok(g.add(sg))
    }

    pub fn read(r: &mut Reader, g: &mut Graph, va: u64) -> Result<Option<BlockId>> {
        if va == 0 { return Ok(None); }
        if let Some(id) = r.cached_as::<Self>(va)? { return Ok(Some(id)); }
        let mut c = r.cursor(va)?;
        let vft = c.u32(); c.skip(4);
        let dict_ptr = c.u64();
        let shaders_ptr = c.u64();
        let count = c.u16() as usize; c.skip(2 + 36);
        c.check()?;
        // the dictionary first, so its textures are in the pool when the parameters point at them
        let dictionary = TextureDictionary::read(r, g, dict_ptr)?;
        let shaders = if shaders_ptr == 0 {
            None
        } else if let Some(id) = r.cached_as::<PointerArray64>(shaders_ptr)? {
            Some(id)
        } else {
            let ptrs = read_pointer_array64(r, g, shaders_ptr, count)?;
            let items = ptrs.into_iter().map(|p| ShaderFx::read(r, g, p)).collect::<Result<Vec<_>>>()?;
            let id = g.add(PointerArray64 { items });
            r.cache::<PointerArray64>(shaders_ptr, id);
            Some(id)
        };
        let id = g.add(ShaderGroup { vft, dictionary, shaders });
        r.cache::<Self>(va, id); Ok(Some(id))
    }

    /// `ShaderGroup.ReadXml`'s swap: every texture parameter whose name hash is in the dictionary
    /// points at the dictionary's texture. Returns a warning for each parameter left naming a texture
    /// that is not embedded (the game looks it up in the archetype's texture dictionary instead).
    pub fn resolve_textures(&self, g: &mut Graph) -> Vec<String> {
        let Some(shaders) = self.shaders else { return Vec::new() };
        let fxs: Vec<Option<BlockId>> = g.get::<PointerArray64>(shaders).items.clone();
        let mut swaps = Vec::new();
        let mut missing = Vec::new();
        for (s, fx) in fxs.into_iter().enumerate() {
            let Some(pb) = fx.and_then(|fx| g.get::<ShaderFx>(fx).params) else { continue };
            for (i, p) in g.get::<ShaderParametersBlock>(pb).params.iter().enumerate() {
                let ParamData::Texture(Some(id)) = p.data else { continue };
                let (name, hash) = texture_info(g, id);
                match self.dictionary.and_then(|d| g.get::<TextureDictionary>(d).lookup(g, hash)) {
                    Some(t) => swaps.push((pb, i, t)),
                    None if !name.is_empty() => missing.push((s, p.name_hash, name.to_owned())),
                    None => {}
                }
            }
        }
        for (pb, i, t) in swaps { g.get_mut::<ShaderParametersBlock>(pb).params[i].data = ParamData::Texture(Some(t)); }
        if missing.is_empty() { return Vec::new(); }
        let names = NameTable::core();
        missing.into_iter().map(|(s, param, tex)| format!(
            "shader {s} parameter {}: texture '{tex}' is not embedded (resolved at runtime from the archetype's txd)",
            hash_string(param, &names),
        )).collect()
    }
}
impl Block for ShaderGroup {
    fn length(&self) -> usize { 64 }
    fn references(&self, _g: &Graph) -> Vec<BlockId> { self.dictionary.into_iter().chain(self.shaders).collect() }
    fn write(&self, w: &mut Writer, g: &Graph) -> Result<()> {
        w.u32(self.vft); w.u32(1);
        w.u64(g.ptr(self.dictionary));
        w.u64(g.ptr(self.shaders));
        let count = count_u16(self.count(g), "shaders")?;
        w.u16(count); w.u16(count);
        w.u32(0); w.u64(0); w.u64(0);
        w.u32(64 / 16); w.u32(0); w.u64(0);
        Ok(())
    }
    fn as_any(&self) -> &dyn Any { self } fn as_any_mut(&mut self) -> &mut dyn Any { self }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blocks::base::PagesInfo;

    #[test]
    fn parameter_block_sizes_and_numbering_match_reference() {
        let mut g = Graph::new();
        let p = ShaderParametersBlock::from_params(&mut g, vec![
            ShaderParam { data_type: 0, unknown_1h: 0, name_hash: 1, data: ParamData::Texture(None) },
            ShaderParam { data_type: 1, unknown_1h: 0, name_hash: 2, data: ParamData::Vectors(vec![Vec4::new(1.0,0.0,0.0,0.0)]) },
            ShaderParam { data_type: 0, unknown_1h: 0, name_hash: 3, data: ParamData::Texture(None) },
            ShaderParam { data_type: 2, unknown_1h: 0, name_hash: 4, data: ParamData::Vectors(vec![Vec4::default(); 2]) },
        ]);
        assert_eq!(p.params.iter().map(|q| q.unknown_1h).collect::<Vec<_>>(), vec![2, 162, 4, 160]);
        assert_eq!(p.base_size(), 32 + (16 + 32 + 16 + 48) + 16);
        assert_eq!(p.parameters_size(), 112);
        assert_eq!(p.parameters_data_size(), 160);
        assert_eq!(p.texture_count(), 2);
        assert_eq!(p.length(), 160 + 160 * 4);
    }

    const XML: &str = r#"<ShaderGroup><TextureDictionary><Item><Name>wall_a</Name><Unk32 value="0"/><Usage>DEFAULT</Usage><UsageFlags/><ExtraFlags value="0"/><Width value="4"/><Height value="4"/><MipLevels value="1"/><Format>D3DFMT_DXT1</Format><FileName>wall_a.dds</FileName></Item></TextureDictionary>
<Shaders><Item><Name>default</Name><FileName>default.sps</FileName><RenderBucket value="0" /><Parameters><Item name="DiffuseSampler" type="Texture"><Name>wall_a</Name></Item><Item name="specularFactor" type="Vector" x="1" y="2" z="3" w="4" /></Parameters></Item></Shaders></ShaderGroup>"#;

    fn dds_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("wall_a.dds"), crate::ytd::tests::sample_dxt1_4x4("wall_a").to_dds()).unwrap();
        dir
    }

    #[test]
    fn shader_xml_round_trips_and_binds_embedded_textures() {
        let dir = dds_dir();
        let doc = roxmltree::Document::parse(XML).unwrap();
        let mut g = Graph::new();
        let mut warnings = Vec::new();
        let sg = ShaderGroup::read_xml(doc.root_element(), &mut g, Some(dir.path()), &mut warnings).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        let sg_ref = g.get::<ShaderGroup>(sg);
        let shaders = g.get::<PointerArray64>(sg_ref.shaders.unwrap());
        let fx = g.get::<ShaderFx>(shaders.items[0].unwrap());
        assert_eq!(fx.render_bucket_mask, 0xFF01);
        let params = g.get::<ShaderParametersBlock>(fx.params.unwrap());
        let dict_tex = g.get::<TextureDictionary>(sg_ref.dictionary.unwrap()).lookup(&g, crate::rage_joaat("wall_a")).unwrap();
        assert!(matches!(params.params[0].data, ParamData::Texture(Some(id)) if id == dict_tex), "parameter points at the embedded texture");
        let mut x = XmlOut::new(); x.out.clear();
        x.open("ShaderGroup"); sg_ref.write_xml(&mut x, &g, &NameTable::core(), None).unwrap(); x.close("ShaderGroup");
        assert!(x.out.contains(r#"<Item name="specularFactor" type="Vector" x="1" y="2" z="3" w="4" />"#));
        assert!(x.out.contains(r#"<Item name="DiffuseSampler" type="Texture">"#));
    }

    #[test]
    fn built_shaders_read_back_sharing_the_dictionary_texture() {
        let dir = dds_dir();
        let xml = XML.replace("</Parameters>", r#"<Item name="unknownParam" type="Texture" /><Item name="colours" type="Array"><Value x="1" y="0" z="0" w="0"/><Value x="0" y="1" z="0" w="0"/></Item><Item name="BumpSampler" type="Texture"><Name>other</Name></Item></Parameters>"#);
        let doc = roxmltree::Document::parse(&xml).unwrap();
        let mut g = Graph::new();
        let mut warnings = Vec::new();
        let sg = ShaderGroup::read_xml(doc.root_element(), &mut g, Some(dir.path()), &mut warnings).unwrap();
        assert_eq!(warnings, ["shader 0 parameter BumpSampler: texture 'other' is not embedded (resolved at runtime from the archetype's txd)"]);
        let pages = g.add(PagesInfo::default());
        let file = g.build(sg, pages, 13).unwrap();

        let mut r = Reader::open(&file).unwrap();
        let mut g2 = Graph::new();
        let sg2 = ShaderGroup::read(&mut r, &mut g2, crate::resource::SYSTEM_BASE).unwrap().unwrap();
        let s = g2.get::<ShaderGroup>(sg2);
        assert_eq!((s.vft, s.count(&g2)), (1080113136, 1));
        let fx = g2.get::<ShaderFx>(g2.get::<PointerArray64>(s.shaders.unwrap()).items[0].unwrap());
        assert_eq!((fx.name_hash, fx.render_bucket_mask, fx.unknown_12h), (crate::rage_joaat("default"), 0xFF01, 32768));
        let orig = g.get::<ShaderParametersBlock>(g.get::<ShaderFx>(g.get::<PointerArray64>(g.get::<ShaderGroup>(sg).shaders.unwrap()).items[0].unwrap()).params.unwrap());
        let back = g2.get::<ShaderParametersBlock>(fx.params.unwrap());
        assert_eq!(back.params.len(), 5);
        for (a, b) in orig.params.iter().zip(&back.params) {
            assert_eq!((a.data_type, a.unknown_1h, a.name_hash), (b.data_type, b.unknown_1h, b.name_hash));
        }
        let dict_tex = g2.get::<TextureDictionary>(s.dictionary.unwrap()).lookup(&g2, crate::rage_joaat("wall_a")).unwrap();
        assert_eq!(back.params[0].data, ParamData::Texture(Some(dict_tex)), "the pool yields the dictionary's texture");
        assert_eq!(back.params[1].data, orig.params[1].data);
        assert_eq!(back.params[2].data, ParamData::Texture(None));
        assert_eq!(back.params[3].data, orig.params[3].data);
        let loose = match back.params[4].data { ParamData::Texture(Some(id)) => g2.get::<TextureRef>(id), _ => panic!("a texture ref") };
        assert_eq!((loose.name_hash, loose.unknown_32h), (crate::rage_joaat("other"), 2));

        // a second dump of the read-back graph matches the first
        let dump = |g: &Graph, id| { let mut x = XmlOut::new(); x.open("ShaderGroup"); g.get::<ShaderGroup>(id).write_xml(&mut x, g, &NameTable::core(), None).unwrap(); x.out };
        assert_eq!(dump(&g, sg), dump(&g2, sg2));
    }

    #[test]
    fn a_truncated_parameter_block_is_an_error() {
        let mut g = Graph::new();
        let pages = g.add(PagesInfo::default());
        let p = ShaderParametersBlock::from_params(&mut g, vec![ShaderParam { data_type: 1, unknown_1h: 0, name_hash: 2, data: ParamData::Vectors(vec![Vec4::default()]) }]);
        let pid = g.add(p);
        let file = g.build(pid, pages, 13).unwrap();
        let (sys, _) = crate::resource::prepare_rsc7(&file).unwrap();
        let mut r = Reader::open(&file).unwrap();
        let va = crate::resource::SYSTEM_BASE + sys.len() as u64 - 8;
        assert!(ShaderParametersBlock::read(&mut r, &mut Graph::new(), va, 2).is_err());
    }
}
