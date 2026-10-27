//! Textures and the embedded texture dictionary, ported from the reference tool's `Texture.cs`
//! (`TextureDictionary`, `TextureBase`, `Texture`, `TextureData`). Only the legacy PC
//! layout is handled.

use std::path::Path;

use anyhow::{bail, Context, Result};

use super::base::{
    read_pages_info, read_pointer_array64, read_simple_list64, read_string_block, read_struct_array,
    write_file_base, write_pointer_list64, write_simple_list64, PointerArray64, StringBlock, StructArray,
};
use super::xml::{child, child_attr_u32, child_text, format_flags, parse_flags, Node, XmlOut};
use super::*;
use crate::gen9::Edition;
use crate::rage_joaat;
use crate::resource::SYSTEM_BASE;
use crate::ytd::{mip_chain_size, stride_for, to_ytd_layout, TextureFormat, YtdTexture, YTD_VERSION};

/// A `.ytd` of either edition as a legacy `.ytd` (resource version 13), the Gen9 → Legacy conversion for
/// texture dictionaries. A legacy file comes back unchanged. See [`Texture::read_gen9`] for what a Gen9
/// texture becomes and what stops a conversion.
///
/// A Gen9 texture's `UsageData` low bits (the texture usage) carry over; the legacy usage *flags* and
/// extra flags have no Gen9 source and are written as 0, as `serialize_ytd` writes them.
pub fn convert_ytd_to_legacy(file: &[u8]) -> Result<Vec<u8>> {
    match Edition::of_file("ytd", file)? {
        Some(Edition::Legacy) => return Ok(file.to_vec()),
        Some(Edition::Gen9) => {}
        None => bail!("not a texture dictionary of a known edition"),
    }
    let mut r = Reader::open(file)?;
    let mut g = Graph::new();
    let dict = TextureDictionary::read_edition(&mut r, &mut g, SYSTEM_BASE, Edition::Gen9)?.context("the resource has no root block")?;
    let pages = g.get::<TextureDictionary>(dict).pages.context("the dictionary has no pages info")?;
    g.build(dict, pages, YTD_VERSION)
}

/// `TextureUsage`, indexed by value (`Texture.cs:1247`).
pub const USAGE_NAMES: [&str; 30] = [
    "UNKNOWN", "DEFAULT", "TERRAIN", "CLOUDDENSITY", "CLOUDNORMAL", "CABLE", "FENCE", "ENVEFF", "SCRIPT",
    "WATERFLOW", "WATERFOAM", "WATERFOG", "WATEROCEAN", "WATER", "FOAMOPACITY", "FOAM", "DIFFUSEMIPSHARPEN",
    "DIFFUSEDETAIL", "DIFFUSEDARK", "DIFFUSEALPHAOPAQUE", "DIFFUSE", "DETAIL", "NORMAL", "SPECULAR", "EMISSIVE",
    "TINTPALETTE", "SKIPPROCESSING", "DONOTOPTIMIZE", "TEST", "COUNT",
];

/// `TextureUsageFlags` (`Texture.cs:1273`).
pub const USAGE_FLAG_NAMES: [(&str, u32); 25] = [
    ("NOT_HALF", 1), ("HD_SPLIT", 1 << 1), ("X2", 1 << 2), ("X4", 1 << 3), ("Y4", 1 << 4), ("X8", 1 << 5),
    ("X16", 1 << 6), ("X32", 1 << 7), ("X64", 1 << 8), ("Y64", 1 << 9), ("X128", 1 << 10), ("X256", 1 << 11),
    ("X512", 1 << 12), ("Y512", 1 << 13), ("X1024", 1 << 14), ("Y1024", 1 << 15), ("X2048", 1 << 16),
    ("Y2048", 1 << 17), ("EMBEDDEDSCRIPTRT", 1 << 18), ("UNK19", 1 << 19), ("UNK20", 1 << 20), ("UNK21", 1 << 21),
    ("FLAG_FULL", 1 << 22), ("MAPS_HALF", 1 << 23), ("UNK24", 1 << 24),
];

/// `TextureUsage.ToString()`: the member name, or the number for a value with none.
fn usage_name(u: u8) -> String {
    USAGE_NAMES.get(u as usize).map_or_else(|| u.to_string(), |n| (*n).to_owned())
}
/// `Xml.GetChildEnumInnerText<TextureUsage>`: a name or a number; anything else is 0.
fn parse_usage(s: &str) -> u8 {
    let s = s.trim();
    if let Some(i) = USAGE_NAMES.iter().position(|n| n.eq_ignore_ascii_case(s)) { return i as u8; }
    s.parse::<u8>().map_or(0, |v| v & 0x1F)
}
/// `TextureUsageFlags.ToString()`: the set names joined by `, `, `0` for none, the number when a bit has no name.
fn usage_flags_name(f: u32) -> String {
    let known = USAGE_FLAG_NAMES.iter().fold(0, |a, (_, v)| a | v);
    if f & !known != 0 { return f.to_string(); }
    format_flags(f, &USAGE_FLAG_NAMES, |v, b| v & b == b, 0, "0")
}
fn parse_usage_flags(s: &str) -> u32 {
    match s.trim().parse::<u32>() { Ok(v) => v, Err(_) => parse_flags(s, &USAGE_FLAG_NAMES, |a, b| a | b, 0) }
}

/// The `.dds` file name for a texture called `name` (`null.dds` for none, as the reference tool writes): one plain
/// file name inside the dump folder whatever the name holds. Path separators, `: * ? " < > |` and control
/// characters become `_`, a leading `..` is dropped, a name left empty or `.` / `..` becomes `_`, and a
/// Windows device name (`CON`, `NUL`, `COM1`, ...) gets a `_` in front.
pub fn dds_file_name(name: &str) -> String {
    if name.is_empty() { return "null.dds".to_owned(); }
    let mut s: String = name.chars()
        .map(|c| if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control() { '_' } else { c })
        .collect();
    while let Some(rest) = s.strip_prefix("..") { s = rest.to_owned(); }
    if s.is_empty() || s == "." { s = "_".to_owned(); }
    let stem = s.split('.').next().unwrap_or("").trim_end().to_ascii_uppercase();
    let numbered = (stem.starts_with("COM") || stem.starts_with("LPT")) && stem.len() == 4 && stem.as_bytes()[3].is_ascii_digit();
    if ["CON", "PRN", "AUX", "NUL"].contains(&stem.as_str()) || numbered { s.insert(0, '_'); }
    format!("{s}.dds")
}

/// `TextureData`: the pixel bytes of every mip level in the game's layout (graphics section).
pub struct TextureData { pub bytes: Vec<u8> }

impl TextureData {
    /// `TextureData.Read`: `stride * height` bytes for the first level, a quarter of that for each next.
    pub fn read(r: &mut Reader, g: &mut Graph, va: u64, stride: u16, height: u16, levels: u8) -> Result<Option<BlockId>> {
        if va == 0 { return Ok(None); }
        if let Some(id) = r.cached_as::<Self>(va)? { return Ok(Some(id)); }
        let (mut len, mut total) = (stride as usize * height as usize, 0usize);
        for _ in 0..levels { total += len; len /= 4; }
        let bytes = r.bytes(va, total)?;
        let id = g.add(TextureData { bytes }); r.cache::<Self>(va, id); Ok(Some(id))
    }
}
impl Block for TextureData {
    fn length(&self) -> usize { self.bytes.len() }
    fn section(&self) -> Section { Section::Graphics }
    fn write(&self, w: &mut Writer, _g: &Graph) -> Result<()> { w.bytes(&self.bytes); Ok(()) }
    fn as_any(&self) -> &dyn Any { self } fn as_any_mut(&mut self) -> &mut dyn Any { self }
}

/// `Texture` (`TextureBase` 80 bytes + 64): one texture; `usage`/`usage_flags` are the two halves of `UsageData`.
pub struct Texture {
    pub vft: u32,
    pub name: Option<BlockId>,
    pub name_hash: u32,
    pub unknown_32h: u16,
    pub usage: u8,
    pub usage_flags: u32,
    pub extra_flags: u32,
    pub width: u16,
    pub height: u16,
    pub depth: u16,
    pub stride: u16,
    pub format: TextureFormat,
    pub levels: u8,
    pub data: Option<BlockId>,
}

impl Texture {
    fn name_str<'a>(&self, g: &'a Graph) -> &'a str { self.name.map_or("", |n| g.get::<StringBlock>(n).0.as_str()) }

    /// A texture from a decoded one; `t.pixel_data` must already be in the game's layout.
    pub fn from_ytd_texture(g: &mut Graph, t: &YtdTexture, usage: u8, usage_flags: u32, extra_flags: u32, unknown_32h: u16) -> BlockId {
        let name = if t.name.is_empty() { None } else { Some(g.add(StringBlock(t.name.clone()))) };
        let data = if t.pixel_data.is_empty() { None } else { Some(g.add(TextureData { bytes: t.pixel_data.clone() })) };
        let name_hash = if t.name_hash != 0 { t.name_hash } else { rage_joaat(&t.name.to_lowercase()) };
        g.add(Texture {
            vft: 0, name, name_hash, unknown_32h, usage, usage_flags, extra_flags,
            width: t.width, height: t.height, depth: t.depth.max(1), stride: t.stride, format: t.format, levels: t.levels, data,
        })
    }

    pub fn to_ytd_texture(&self, g: &Graph) -> YtdTexture {
        YtdTexture {
            name: self.name_str(g).to_owned(),
            name_hash: self.name_hash,
            width: self.width,
            height: self.height,
            depth: self.depth,
            format: self.format,
            levels: self.levels,
            stride: self.stride,
            pixel_data: self.data.map_or_else(Vec::new, |d| g.get::<TextureData>(d).bytes.clone()),
        }
    }

    /// `Texture.WriteXml`; with `dds_dir` the texture is also saved there as `<name>.dds` (made safe by
    /// [`dds_file_name`], which also gives the `<FileName>` text).
    pub fn write_xml(&self, x: &mut XmlOut, g: &Graph, dds_dir: Option<&Path>) -> Result<()> {
        let name = self.name_str(g);
        x.string("Name", name);
        x.value("Unk32", self.unknown_32h);
        x.string("Usage", &usage_name(self.usage));
        x.string("UsageFlags", &usage_flags_name(self.usage_flags));
        x.value("ExtraFlags", self.extra_flags);
        x.value("Width", self.width);
        x.value("Height", self.height);
        x.value("MipLevels", self.levels);
        x.string("Format", self.format.reference_name());
        let file = dds_file_name(name);
        x.string("FileName", &file);
        if let Some(dir) = dds_dir {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
            let path = dir.join(&file);
            std::fs::write(&path, self.to_ytd_texture(g).to_dds()).with_context(|| format!("writing {}", path.display()))?;
        }
        Ok(())
    }

    /// `Texture.ReadXml`: the XML's values, overridden by the DDS beside it when `dds_dir` is given.
    pub fn read_xml(n: Node, g: &mut Graph, dds_dir: Option<&Path>) -> Result<BlockId> {
        let name_text = child_text(n, "Name");
        let mut t = Texture {
            vft: 0,
            name: None,
            name_hash: if name_text.is_empty() { 0 } else { rage_joaat(&name_text.to_lowercase()) },
            unknown_32h: child_attr_u32(n, "Unk32", "value") as u16,
            usage: parse_usage(&child_text(n, "Usage")),
            usage_flags: parse_usage_flags(&child_text(n, "UsageFlags")),
            extra_flags: child_attr_u32(n, "ExtraFlags", "value"),
            width: child_attr_u32(n, "Width", "value") as u16,
            height: child_attr_u32(n, "Height", "value") as u16,
            depth: 1,
            stride: 0,
            format: TextureFormat::from_reference_name(&child_text(n, "Format")).unwrap_or(TextureFormat::Unknown),
            levels: child_attr_u32(n, "MipLevels", "value") as u8,
            data: None,
        };
        if !name_text.is_empty() { t.name = Some(g.add(StringBlock(name_text))); }
        let file = child(n, "FileName").and_then(|c| c.text()).unwrap_or("").trim().to_owned();
        if let (false, Some(dir)) = (file.is_empty(), dds_dir) {
            let path = dir.join(&file);
            if !path.exists() { bail!("texture file not found: {}", path.display()); }
            let bytes = std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
            let d = crate::parse_dds(&bytes).with_context(|| format!("texture file format not supported: {}", path.display()))?;
            t.width = d.width; t.height = d.height; t.depth = d.depth.max(1); t.levels = d.levels; t.format = d.format; t.stride = d.stride;
            t.data = Some(g.add(TextureData { bytes: d.pixel_data }));
        }
        Ok(g.add(t))
    }

    /// A Gen9 (Enhanced) texture, read into the legacy form: `TextureBase.Read` with `IsGen9`
    /// (`Texture.cs:305`). The header is 80 bytes (width, height, format and the data pointer live in the
    /// base, with no stride) followed by 48 bytes holding a shader resource view, which a legacy texture
    /// has no use for.
    ///
    /// The format becomes its legacy twin ([`crate::gen9::legacy_texture_format`]), the stride is the
    /// legacy one ([`stride_for`]) and the pixels, laid out by the slice pitch of every mip, are re-laid
    /// out the way the legacy game stores a chain. A texture the legacy game cannot hold (cube map, 3D
    /// texture, a format it lacks, a non-linear tile mode) is an error.
    pub fn read_gen9(r: &mut Reader, g: &mut Graph, va: u64) -> Result<Option<BlockId>> {
        if va == 0 { return Ok(None); }
        if let Some(id) = r.cached_as::<Self>(va)? { return Ok(Some(id)); }
        let mut c = r.cursor(va)?;
        c.skip(0x18);
        let (width, height, depth) = (c.u16(), c.u16(), c.u16());
        let (dimension, format, tile_mode, _aa, levels) = (c.u8(), c.u8(), c.u8(), c.u8(), c.u8());
        c.skip(5);
        let name_ptr = c.u64(); c.skip(8);
        let data_ptr = c.u64();
        let usage_data = c.u32();
        c.check()?;
        let name = read_string_block(r, g, name_ptr)?;
        let name_hash = name.map_or(0, |n| rage_joaat(&g.get::<StringBlock>(n).0.to_lowercase()));
        let label = name.map_or_else(|| "(unnamed)".to_owned(), |n| g.get::<StringBlock>(n).0.clone());
        if dimension != 1 { bail!("texture '{label}': dimension {dimension} (2 is a cube map, 3 a volume); only 2D textures convert"); }
        if tile_mode != 255 { bail!("texture '{label}': tile mode {tile_mode}; only linear (auto) textures convert"); }
        let fmt = crate::gen9::legacy_texture_format(format).with_context(|| format!("texture '{label}'"))?;
        let stride = stride_for(fmt, width, height);
        let depth = depth.max(1);
        let len = mip_chain_size(fmt, width, height, levels) * depth as usize;
        let pixels = if data_ptr == 0 || len == 0 { Vec::new() } else {
            let raw = r.bytes(data_ptr, len).with_context(|| format!("texture '{label}': pixel data"))?;
            to_ytd_layout(fmt, width, height, stride, levels, &raw)
        };
        let data = if pixels.is_empty() { None } else { Some(g.add(TextureData { bytes: pixels })) };
        let id = g.add(Texture {
            vft: 0, name, name_hash, unknown_32h: 0, usage: (usage_data & 0x1F) as u8, usage_flags: 0, extra_flags: 0,
            width, height, depth, stride, format: fmt, levels, data,
        });
        r.cache::<Self>(va, id); Ok(Some(id))
    }

    pub fn read(r: &mut Reader, g: &mut Graph, va: u64) -> Result<Option<BlockId>> {
        if va == 0 { return Ok(None); }
        if let Some(id) = r.cached_as::<Self>(va)? { return Ok(Some(id)); }
        let mut c = r.cursor(va)?;
        let vft = c.u32(); c.skip(4 + 32);
        let name_ptr = c.u64();
        c.skip(2);
        let unknown_32h = c.u16(); c.skip(12);
        let usage_data = c.u32(); c.skip(4);
        let extra_flags = c.u32(); c.skip(4);
        let (width, height, depth, stride) = (c.u16(), c.u16(), c.u16(), c.u16());
        let format = TextureFormat::from_u32(c.u32()); c.skip(1);
        let levels = c.u8(); c.skip(2 + 16);
        let data_ptr = c.u64();
        c.skip(24);
        c.check()?;
        let name = read_string_block(r, g, name_ptr)?;
        let name_hash = name.map_or(0, |n| rage_joaat(&g.get::<StringBlock>(n).0.to_lowercase()));
        let data = TextureData::read(r, g, data_ptr, stride, height, levels)?;
        let id = g.add(Texture {
            vft, name, name_hash, unknown_32h, usage: (usage_data & 0x1F) as u8, usage_flags: usage_data >> 5, extra_flags,
            width, height, depth, stride, format, levels, data,
        });
        r.cache::<Self>(va, id); Ok(Some(id))
    }
}
impl Block for Texture {
    fn length(&self) -> usize { 144 }
    fn references(&self, _g: &Graph) -> Vec<BlockId> { self.name.into_iter().chain(self.data).collect() }
    fn write(&self, w: &mut Writer, g: &Graph) -> Result<()> {
        w.u32(self.vft); w.u32(1); w.zeros(32);
        w.u64(g.ptr(self.name));
        w.u16(1); w.u16(self.unknown_32h); w.zeros(12);
        w.u32((self.usage as u32 & 0x1F) | (self.usage_flags << 5)); w.u32(0);
        w.u32(self.extra_flags); w.u32(0);
        w.u16(self.width); w.u16(self.height); w.u16(self.depth); w.u16(self.stride);
        w.u32(self.format as u32); w.u8(0); w.u8(self.levels); w.u16(0); w.zeros(16);
        w.u64(g.ptr(self.data));
        w.zeros(24);
        Ok(())
    }
    fn as_any(&self) -> &dyn Any { self } fn as_any_mut(&mut self) -> &mut dyn Any { self }
}

/// `TextureDictionary` (64 bytes): textures sorted by name hash, with the hash list in the same order.
pub struct TextureDictionary {
    pub vft: u32,
    pub pages: Option<BlockId>,
    pub hashes: Option<BlockId>,
    /// A `PointerArray64` of [`Texture`]; its length is the count written for both lists.
    pub textures: Option<BlockId>,
}

impl TextureDictionary {
    /// `BuildFromTextureList`: sort by name hash, then the hash array and the pointer array.
    pub fn from_textures(g: &mut Graph, mut textures: Vec<BlockId>) -> BlockId {
        textures.sort_by_key(|&t| g.get::<Texture>(t).name_hash);
        let hashes: Vec<u32> = textures.iter().map(|&t| g.get::<Texture>(t).name_hash).collect();
        let (hashes, textures) = if textures.is_empty() {
            (None, None)
        } else {
            (Some(g.add(StructArray { items: hashes })), Some(g.add(PointerArray64 { items: textures.into_iter().map(Some).collect() })))
        };
        g.add(TextureDictionary { vft: 0, pages: None, hashes, textures })
    }

    /// The number of textures: the length of the pointer array (null entries included).
    pub fn count(&self, g: &Graph) -> usize { self.textures.map_or(0, |t| g.get::<PointerArray64>(t).items.len()) }

    fn texture_ids(&self, g: &Graph) -> Vec<Option<BlockId>> {
        self.textures.map_or_else(Vec::new, |t| g.get::<PointerArray64>(t).items.clone())
    }

    /// `Lookup`: the texture stored under `hash` (the last, when a hash repeats).
    pub fn lookup(&self, g: &Graph, hash: u32) -> Option<BlockId> {
        let hashes = self.hashes.map_or(&[][..], |h| g.get::<StructArray<u32>>(h).items.as_slice());
        let textures = self.texture_ids(g);
        (0..hashes.len().min(textures.len())).rev().find(|&i| hashes[i] == hash).and_then(|i| textures[i])
    }

    /// `TextureDictionary.WriteXml`: an `<Item>` per texture.
    pub fn write_xml(&self, x: &mut XmlOut, g: &Graph, dds_dir: Option<&Path>) -> Result<()> {
        for t in self.texture_ids(g).into_iter().flatten() {
            x.open("Item");
            g.get::<Texture>(t).write_xml(x, g, dds_dir)?;
            x.close("Item");
        }
        Ok(())
    }

    /// `WriteXmlNode`: `<name>` around the items, or `<name />` for a dictionary with none.
    pub fn write_xml_node(&self, x: &mut XmlOut, g: &Graph, name: &str, dds_dir: Option<&Path>) -> Result<()> {
        if self.texture_ids(g).is_empty() { x.self_closing(name); return Ok(()); }
        x.open(name);
        self.write_xml(x, g, dds_dir)?;
        x.close(name);
        Ok(())
    }

    /// `ReadXml`: the `<Item>` children of `n`, their DDS files read from `dds_dir`.
    pub fn read_xml(n: Node, g: &mut Graph, dds_dir: &Path) -> Result<BlockId> {
        let mut textures = Vec::new();
        for item in n.children().filter(|c| c.is_element() && c.tag_name().name() == "Item") {
            textures.push(Texture::read_xml(item, g, Some(dds_dir))?);
        }
        Ok(Self::from_textures(g, textures))
    }

    pub fn read(r: &mut Reader, g: &mut Graph, va: u64) -> Result<Option<BlockId>> { Self::read_edition(r, g, va, Edition::Legacy) }

    /// `read` for a dictionary of either edition: the dictionary struct is the same in both, only its
    /// textures differ ([`Texture::read_gen9`]).
    pub fn read_edition(r: &mut Reader, g: &mut Graph, va: u64, edition: Edition) -> Result<Option<BlockId>> {
        if va == 0 { return Ok(None); }
        if let Some(id) = r.cached_as::<Self>(va)? { return Ok(Some(id)); }
        let mut c = r.cursor(va)?;
        let vft = c.u32(); c.skip(4);
        let pages_ptr = c.u64(); c.skip(16);
        let (hashes_ptr, hashes_count, _) = read_simple_list64(&mut c);
        let (textures_ptr, textures_count, _) = read_simple_list64(&mut c);
        c.check()?;
        let pages = read_pages_info(r, g, pages_ptr)?;
        let hashes = read_struct_array::<u32>(r, g, hashes_ptr, hashes_count as usize)?;
        let textures = if textures_ptr == 0 {
            None
        } else if let Some(id) = r.cached_as::<PointerArray64>(textures_ptr)? {
            Some(id)
        } else {
            let ptrs = read_pointer_array64(r, g, textures_ptr, textures_count as usize)?;
            let items = ptrs.into_iter().map(|p| match edition {
                Edition::Legacy => Texture::read(r, g, p),
                Edition::Gen9 => Texture::read_gen9(r, g, p),
            }).collect::<Result<Vec<_>>>()?;
            let id = g.add(PointerArray64 { items });
            r.cache::<PointerArray64>(textures_ptr, id);
            Some(id)
        };
        let id = g.add(TextureDictionary { vft, pages, hashes, textures });
        r.cache::<Self>(va, id); Ok(Some(id))
    }
}
impl Block for TextureDictionary {
    fn length(&self) -> usize { 64 }
    fn references(&self, _g: &Graph) -> Vec<BlockId> { self.pages.into_iter().chain(self.hashes).chain(self.textures).collect() }
    fn write(&self, w: &mut Writer, g: &Graph) -> Result<()> {
        write_file_base(w, g, self.vft, self.pages);
        w.u32(0); w.u32(0); w.u32(1); w.u32(0);
        let count = self.count(g);
        write_simple_list64(w, g, self.hashes, count, "textures")?;
        write_pointer_list64(w, g, self.textures, count, "textures")?;
        Ok(())
    }
    fn as_any(&self) -> &dyn Any { self } fn as_any_mut(&mut self) -> &mut dyn Any { self }
}

#[cfg(test)]
mod tests {
    use crate::blocks::base::PagesInfo;
    use super::*;
    use crate::blocks::xml::XmlOut;
    use crate::blocks::Graph;
    use crate::ytd::TextureFormat;

    #[test]
    fn dictionary_builds_a_ytd_the_old_parser_reads_and_round_trips_xml() {
        let dir = tempfile::tempdir().unwrap();
        let tex = crate::ytd::tests::sample_dxt1_4x4("wall_a");
        let mut g = Graph::new();
        let pages = g.add(PagesInfo::default());
        let t = Texture::from_ytd_texture(&mut g, &tex, 20, 0, 0, 0);
        let dict = TextureDictionary::from_textures(&mut g, vec![t]);
        g.get_mut::<TextureDictionary>(dict).pages = Some(pages);
        let ytd = g.build(dict, pages, 13).unwrap();
        let parsed = crate::parse_ytd(&ytd).unwrap();
        assert_eq!((parsed.len(), parsed[0].name.as_str(), parsed[0].width), (1, "wall_a", 4));

        let mut x = XmlOut::new(); x.open("TextureDictionary");
        g.get::<TextureDictionary>(dict).write_xml(&mut x, &g, Some(dir.path())).unwrap();
        x.close("TextureDictionary");
        assert!(dir.path().join("wall_a.dds").exists());
        assert!(x.out.contains("<Format>D3DFMT_DXT1</Format>"));
        let doc = roxmltree::Document::parse(&x.out).unwrap();
        let mut g2 = Graph::new();
        let d2 = TextureDictionary::read_xml(doc.root_element(), &mut g2, dir.path()).unwrap();
        let t2 = g2.get::<TextureDictionary>(d2).lookup(&g2, crate::rage_joaat("wall_a")).unwrap();
        assert_eq!(g2.get::<Texture>(t2).width, 4);
    }

    /// A Gen9 texture header (128 bytes) as the Enhanced game writes it, for the conversion tests.
    struct Gen9Tex { name: BlockId, data: BlockId, width: u16, height: u16, levels: u8, format: u8, dimension: u8, tile: u8 }
    impl Block for Gen9Tex {
        fn length(&self) -> usize { 128 }
        fn references(&self, _g: &Graph) -> Vec<BlockId> { vec![self.name, self.data] }
        fn write(&self, w: &mut Writer, g: &Graph) -> Result<()> {
            w.u32(0); w.u32(1); w.u32(1); w.u32(0); w.u32(0x0026_0208); w.u32(0);
            w.u16(self.width); w.u16(self.height); w.u16(1);
            w.u8(self.dimension); w.u8(self.format); w.u8(self.tile); w.u8(0); w.u8(self.levels);
            w.u8(0x28); w.u8(0); w.u8(0); w.u16(1);
            w.u64(g.position(self.name)); w.u64(0); w.u64(g.position(self.data));
            w.u32(20); w.u32(2); w.u64(0);
            w.zeros(48);
            Ok(())
        }
        fn as_any(&self) -> &dyn Any { self } fn as_any_mut(&mut self) -> &mut dyn Any { self }
    }

    fn gen9_ytd(width: u16, height: u16, levels: u8, format: u8, dimension: u8, tile: u8, pixels: Vec<u8>) -> Vec<u8> {
        let mut g = Graph::new();
        let pages = g.add(PagesInfo::default());
        let name = g.add(StringBlock("wall_a".into()));
        let data = g.add(TextureData { bytes: pixels });
        let tex = g.add(Gen9Tex { name, data, width, height, levels, format, dimension, tile });
        let hashes = g.add(StructArray { items: vec![crate::rage_joaat("wall_a")] });
        let textures = g.add(PointerArray64 { items: vec![Some(tex)] });
        let dict = g.add(TextureDictionary { vft: 0, pages: Some(pages), hashes: Some(hashes), textures: Some(textures) });
        g.build(dict, pages, 5).unwrap()
    }

    #[test]
    fn a_gen9_dictionary_converts_to_a_legacy_one() {
        // 8x8 BC1 with two mips: 2x2 blocks (32 bytes) then one block (8 bytes)
        let pixels: Vec<u8> = (0..40).collect();
        let file = gen9_ytd(8, 8, 2, 0x47, 1, 255, pixels.clone());
        assert_eq!(Edition::of_file("ytd", &file).unwrap(), Some(Edition::Gen9));
        let legacy = convert_ytd_to_legacy(&file).unwrap();
        assert_eq!(Edition::of_file("ytd", &legacy).unwrap(), Some(Edition::Legacy));
        let t = crate::parse_ytd(&legacy).unwrap();
        assert_eq!(t.len(), 1);
        let t = &t[0];
        assert_eq!((t.name.as_str(), t.width, t.height, t.levels, t.format, t.stride), ("wall_a", 8, 8, 2, TextureFormat::DXT1, 4));
        assert_eq!(t.pixel_data, pixels);
        // converting a legacy file again changes nothing
        assert_eq!(convert_ytd_to_legacy(&legacy).unwrap(), legacy);
    }

    #[test]
    fn a_gen9_texture_the_legacy_game_cannot_hold_is_refused() {
        let e = convert_ytd_to_legacy(&gen9_ytd(4, 4, 1, 0x5F, 1, 255, vec![0; 16])).unwrap_err();
        assert!(format!("{e:#}").contains("BC6H_UF16"), "{e:#}");
        let e = convert_ytd_to_legacy(&gen9_ytd(4, 4, 1, 0x47, 2, 255, vec![0; 8])).unwrap_err();
        assert!(format!("{e:#}").contains("cube map"), "{e:#}");
        let e = convert_ytd_to_legacy(&gen9_ytd(4, 4, 1, 0x47, 1, 7, vec![0; 8])).unwrap_err();
        assert!(format!("{e:#}").contains("tile mode 7"), "{e:#}");
    }

    #[test]
    fn dds_dimensions_win_over_the_xml() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("t.dds"), crate::ytd::tests::sample_dxt1_4x4("t").to_dds()).unwrap();
        let doc = roxmltree::Document::parse("<Item><Name>t</Name><Unk32 value=\"0\"/><Usage>DEFAULT</Usage><UsageFlags/><ExtraFlags value=\"0\"/><Width value=\"64\"/><Height value=\"64\"/><MipLevels value=\"7\"/><Format>D3DFMT_DXT5</Format><FileName>t.dds</FileName></Item>").unwrap();
        let mut g = Graph::new();
        let t = Texture::read_xml(doc.root_element(), &mut g, Some(dir.path())).unwrap();
        let t = g.get::<Texture>(t);
        assert_eq!((t.width, t.height, t.levels, t.format), (4, 4, 1, TextureFormat::DXT1));
    }

    #[test]
    fn reads_back_a_built_dictionary_sorted_by_hash() {
        let mut g = Graph::new();
        let pages = g.add(PagesInfo::default());
        let ts: Vec<_> = ["zz_tex", "aa_tex", "mm_tex"].iter()
            .map(|n| Texture::from_ytd_texture(&mut g, &crate::ytd::tests::sample_dxt1_4x4(n), 1, 0x0100_0003, 1, 0x20)).collect();
        let dict = TextureDictionary::from_textures(&mut g, ts);
        g.get_mut::<TextureDictionary>(dict).pages = Some(pages);
        let file = g.build(dict, pages, 13).unwrap();

        let mut r = Reader::open(&file).unwrap();
        let mut g2 = Graph::new();
        let d2 = TextureDictionary::read(&mut r, &mut g2, crate::resource::SYSTEM_BASE).unwrap().unwrap();
        let d = g2.get::<TextureDictionary>(d2);
        let hashes = &g2.get::<StructArray<u32>>(d.hashes.unwrap()).items;
        assert!(hashes.windows(2).all(|w| w[0] < w[1]) && hashes.len() == 3);
        let t = g2.get::<Texture>(d.lookup(&g2, crate::rage_joaat("mm_tex")).unwrap());
        assert_eq!((t.usage, t.usage_flags, t.extra_flags, t.unknown_32h, t.stride), (1, 0x0100_0003, 1, 0x20, 2));
        assert_eq!(g2.get::<TextureData>(t.data.unwrap()).bytes.len(), 8);
        assert!(crate::parse_ytd(&file).unwrap().iter().all(|t| t.pixel_data.len() == 8));
    }

    #[test]
    fn usage_flags_and_an_empty_dictionary_are_written_like_reference() {
        assert_eq!(usage_flags_name(0x0100_0003), "NOT_HALF, HD_SPLIT, UNK24");
        assert_eq!(usage_flags_name(0), "0");
        assert_eq!(parse_usage_flags("NOT_HALF, X2"), 5);
        assert_eq!((parse_usage("diffuse"), parse_usage("22"), usage_name(31)), (20, 22, "31".to_string()));
        let mut g = Graph::new();
        let d = TextureDictionary::from_textures(&mut g, vec![]);
        let mut x = XmlOut::new(); x.out.clear();
        g.get::<TextureDictionary>(d).write_xml_node(&mut x, &g, "TextureDictionary", None).unwrap();
        assert_eq!(x.out, "<TextureDictionary />\n");
    }

    #[test]
    fn dds_file_names_stay_inside_the_folder() {
        assert_eq!(dds_file_name("wall_a"), "wall_a.dds");
        assert_eq!(dds_file_name(""), "null.dds");
        assert_eq!(dds_file_name("../escaped"), "_escaped.dds");
        assert_eq!(dds_file_name(r"..\..\escaped"), "_.._escaped.dds");
        assert_eq!(dds_file_name(r"C:\x"), "C__x.dds");
        assert_eq!(dds_file_name("/abs/x"), "_abs_x.dds");
        assert_eq!(dds_file_name("a:b"), "a_b.dds");
        assert_eq!(dds_file_name("a*b?c\"<>|\u{1}"), "a_b_c_____.dds");
        assert_eq!(dds_file_name(".."), "_.dds");
        assert_eq!(dds_file_name("."), "_.dds");
        assert_eq!(dds_file_name("...."), "_.dds");
        assert_eq!(dds_file_name("con"), "_con.dds");
        assert_eq!(dds_file_name("com1.x"), "_com1.x.dds");
        assert_eq!(dds_file_name("console"), "console.dds");
        for n in ["../escaped", r"C:\x", "a:b", "..", "/abs/x", r"..\..\escaped"] {
            let f = dds_file_name(n);
            assert!(!f.contains(['/', '\\']) && std::path::Path::new(&f).components().count() == 1, "{n} -> {f}");
        }

        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("dump");
        let mut g = Graph::new();
        let t = Texture::from_ytd_texture(&mut g, &crate::ytd::tests::sample_dxt1_4x4("../escaped"), 1, 0, 0, 0);
        let mut x = XmlOut::new(); x.out.clear();
        g.get::<Texture>(t).write_xml(&mut x, &g, Some(&dir)).unwrap();
        assert!(dir.join("_escaped.dds").exists());
        assert!(!root.path().join("escaped.dds").exists());
        assert!(x.out.contains("<FileName>_escaped.dds</FileName>"), "{}", x.out);
    }

    #[test]
    fn a_missing_dds_names_its_path() {
        let dir = tempfile::tempdir().unwrap();
        let doc = roxmltree::Document::parse("<Item><Name>gone</Name><FileName>gone.dds</FileName></Item>").unwrap();
        let mut g = Graph::new();
        let err = Texture::read_xml(doc.root_element(), &mut g, Some(dir.path())).err().unwrap();
        assert!(err.to_string().contains("texture file not found") && err.to_string().contains("gone.dds"));
    }
}
