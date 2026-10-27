//! Vertex and index buffers, ported from the reference tool's `Drawable.cs`
//! (`VertexBuffer`, `VertexData`, `VertexDeclaration`, `IndexBuffer`) and
//! `VertexType.cs` (component types, declaration types, semantics). Only the
//! legacy PC layout is handled.

use anyhow::{bail, Result};

use super::base::{read_struct_array, StructArray};
use super::xml::{attr_str, child, child_attr_u32, f16_to_f32, f32_to_f16, float, raw_u16s, Node, XmlOut};
use super::*;

/// `VertexComponentType`: the nibble of a declaration's `types` for one semantic.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ComponentType { Nothing, Half2, Float, Half4, FloatUnk, Float2, Float3, Float4, UByte4, Colour, Rgba8Snorm, Unk(u8) }

impl ComponentType {
    pub fn from_nibble(n: u8) -> Self {
        match n & 0xF {
            0 => Self::Nothing, 1 => Self::Half2, 2 => Self::Float, 3 => Self::Half4, 4 => Self::FloatUnk,
            5 => Self::Float2, 6 => Self::Float3, 7 => Self::Float4, 8 => Self::UByte4, 9 => Self::Colour,
            10 => Self::Rgba8Snorm, n => Self::Unk(n),
        }
    }
    /// `VertexComponentTypes.GetSizeInBytes`.
    pub fn size(self) -> usize {
        match self {
            Self::Half2 | Self::Float | Self::UByte4 | Self::Colour | Self::Rgba8Snorm => 4,
            Self::Half4 | Self::Float2 => 8,
            Self::Float3 => 12,
            Self::Float4 => 16,
            Self::Nothing | Self::FloatUnk | Self::Unk(_) => 0,
        }
    }
    /// `VertexComponentTypes.GetComponentCount`: how many values a row holds for it.
    pub fn count(self) -> usize {
        match self {
            Self::Float => 1,
            Self::Half2 | Self::Float2 => 2,
            Self::Float3 => 3,
            Self::Half4 | Self::Float4 | Self::UByte4 | Self::Colour | Self::Rgba8Snorm => 4,
            Self::Nothing | Self::FloatUnk | Self::Unk(_) => 0,
        }
    }
}

/// `VertexSemantics`, indexed by flag bit.
pub const SEMANTICS: [&str; 16] = [
    "Position", "BlendWeights", "BlendIndices", "Normal", "Colour0", "Colour1",
    "TexCoord0", "TexCoord1", "TexCoord2", "TexCoord3", "TexCoord4", "TexCoord5", "TexCoord6", "TexCoord7",
    "Tangent", "Binormal",
];
/// `VertexDeclarationTypes`.
pub const DECLARATION_TYPES: [(&str, u64); 4] = [
    ("GTAV1", 0x7755555555996996), ("GTAV2", 0x030000000199A006), ("GTAV3", 0x0300000001996006), ("GTAV4", 0x7655555555996996),
];

/// `VertexDeclaration` (16 bytes): which semantics a vertex has (`flags`) and how each is stored (`types`).
pub struct VertexDeclaration { pub flags: u32, pub stride: u16, pub unknown_6h: u8, pub count: u8, pub types: u64 }

impl VertexDeclaration {
    /// A declaration with `count` and `stride` derived from its flags (`UpdateCountAndStride`).
    pub fn from_flags_types(flags: u32, types: u64) -> Self {
        let mut d = Self { flags, stride: 0, unknown_6h: 0, count: 0, types };
        let (mut count, mut stride) = (0usize, 0usize);
        for k in 0..16 {
            if (flags >> k) & 1 == 1 { stride += d.component_type(k).size(); count += 1; }
        }
        d.count = count as u8;
        d.stride = stride as u16;
        d
    }
    /// `GetComponentType`: `bit` is the flags bit index.
    pub fn component_type(&self, bit: usize) -> ComponentType { ComponentType::from_nibble(((self.types >> (bit * 4)) & 0xF) as u8) }
    /// `GetComponentOffset`: the bytes taken by the set components before `bit`.
    pub fn component_offset(&self, bit: usize) -> usize {
        (0..bit).filter(|k| (self.flags >> k) & 1 == 1).map(|k| self.component_type(k).size()).sum()
    }
    /// `GetDeclarationId`: the type nibbles of the set components. The reference tool builds each
    /// mask as `0xFu << (i * 4)`, a 32-bit shift, so for bits 8 and up the mask wraps
    /// onto the low nibbles; that is reproduced here.
    pub fn declaration_id(&self) -> u64 {
        let mut id = 0u64;
        for i in 0..16u32 {
            if (self.flags >> i) & 1 == 1 { id += self.types & u64::from(0xFu32 << ((i * 4) & 31)); }
        }
        id
    }
    /// `WriteXml`: `<name type="GTAV1">` with a self-closing element per set semantic.
    pub fn write_xml(&self, x: &mut XmlOut, name: &str) {
        let ty = DECLARATION_TYPES.iter().find(|(_, v)| *v == self.types).map_or_else(|| self.types.to_string(), |(n, _)| (*n).to_owned());
        x.open_attrs(name, &format!("type=\"{ty}\""));
        for (k, s) in SEMANTICS.iter().enumerate() {
            if (self.flags >> k) & 1 == 1 { x.self_closing(s); }
        }
        x.close(name);
    }
    /// `ReadXml`: the type by name (or number) and the flags from the semantic elements.
    pub fn read_xml(n: Node) -> Self {
        let t = attr_str(n, "type");
        let types = DECLARATION_TYPES.iter().find(|(name, _)| name.eq_ignore_ascii_case(t)).map(|(_, v)| *v).or_else(|| t.trim().parse().ok()).unwrap_or(0);
        let mut flags = 0u32;
        for c in n.children().filter(|c| c.is_element()) {
            if let Some(k) = SEMANTICS.iter().position(|s| s.eq_ignore_ascii_case(c.tag_name().name())) { flags |= 1 << k; }
        }
        Self::from_flags_types(flags, types)
    }
    pub fn read(r: &mut Reader, g: &mut Graph, va: u64) -> Result<Option<BlockId>> {
        if va == 0 { return Ok(None); }
        if let Some(id) = r.cached_as::<Self>(va)? { return Ok(Some(id)); }
        let mut c = r.cursor(va)?;
        let d = Self { flags: c.u32(), stride: c.u16(), unknown_6h: c.u8(), count: c.u8(), types: c.u64() };
        c.check()?;
        let id = g.add(d); r.cache::<Self>(va, id); Ok(Some(id))
    }
}
impl Block for VertexDeclaration {
    fn length(&self) -> usize { 16 }
    fn write(&self, w: &mut Writer, _g: &Graph) -> Result<()> {
        w.u32(self.flags); w.u16(self.stride); w.u8(self.unknown_6h); w.u8(self.count); w.u64(self.types); Ok(())
    }
    fn as_any(&self) -> &dyn Any { self } fn as_any_mut(&mut self) -> &mut dyn Any { self }
}

/// `VertexData`: the raw vertex bytes, `count` vertices of `stride` bytes.
pub struct VertexData { pub bytes: Vec<u8>, pub stride: usize, pub count: usize }

impl VertexData {
    /// `n` bytes at `at`, zeros when the range is past the data.
    fn cell(&self, at: usize, n: usize) -> Vec<u8> {
        self.bytes.get(at..at + n).map_or_else(|| vec![0; n], <[u8]>::to_vec)
    }
    /// `GetString`: vertex `v`'s component at flag bit `bit`, values joined by a space.
    fn get_string(&self, decl: &VertexDeclaration, v: usize, bit: usize) -> String {
        let ct = decl.component_type(bit);
        let b = self.cell(v * decl.stride as usize + decl.component_offset(bit), ct.size());
        let f32s = |n: usize| (0..n).map(|i| float(f32::from_le_bytes(b[i * 4..i * 4 + 4].try_into().unwrap()))).collect::<Vec<_>>();
        let halves = |n: usize| (0..n).map(|i| float(f16_to_f32(u16::from_le_bytes([b[i * 2], b[i * 2 + 1]])))).collect::<Vec<_>>();
        let parts = match ct {
            ComponentType::Float => f32s(1),
            ComponentType::Float2 => f32s(2),
            ComponentType::Float3 => f32s(3),
            ComponentType::Float4 => f32s(4),
            ComponentType::Half2 => halves(2),
            ComponentType::Half4 => halves(4),
            ComponentType::Rgba8Snorm => b.iter().map(|&v| float(f32::from(v as i8) / 127.0)).collect(),
            ComponentType::Colour | ComponentType::UByte4 => b.iter().map(|v| v.to_string()).collect(),
            _ => Vec::new(),
        };
        parts.join(" ")
    }
    /// `WriteXml`: one line per vertex, components joined by three spaces.
    pub fn write_xml_rows(&self, x: &mut XmlOut, decl: &VertexDeclaration) {
        for v in 0..self.count {
            let mut row = String::new();
            for k in 0..16 {
                if (decl.flags >> k) & 1 == 1 {
                    if !row.is_empty() { row.push_str("   "); }
                    row.push_str(&self.get_string(decl, v, k));
                }
            }
            x.line(&row);
        }
    }
    /// `ReadXml` + `SetString`: parse the rows into vertex bytes.
    pub fn read_xml_rows(text: &str, decl: &VertexDeclaration) -> Result<Self> {
        let rows: Vec<Vec<&str>> = text.lines().map(|l| l.split_whitespace().collect::<Vec<_>>()).filter(|c| !c.is_empty()).collect();
        let stride = decl.stride as usize;
        let needed: usize = (0..16).filter(|k| (decl.flags >> k) & 1 == 1).map(|k| decl.component_type(k).count()).sum();
        let mut vd = VertexData { bytes: vec![0; rows.len() * stride], stride, count: rows.len() };
        for (v, cols) in rows.iter().enumerate() {
            if cols.len() < needed { bail!("vertex {v}: expected {needed} values, found {}", cols.len()); }
            let mut at = 0;
            for k in 0..16 {
                if (decl.flags >> k) & 1 == 0 { continue; }
                let ct = decl.component_type(k);
                let cc = ct.count();
                let f = |i: usize| cols[at + i].trim().parse::<f32>().unwrap_or(0.0);
                let byte = |i: usize| cols[at + i].trim().parse::<u8>().unwrap_or(0);
                let mut out = Vec::with_capacity(16);
                match ct {
                    ComponentType::Float | ComponentType::Float2 | ComponentType::Float3 | ComponentType::Float4 => {
                        for i in 0..cc { out.extend_from_slice(&f(i).to_le_bytes()); }
                    }
                    ComponentType::Half2 | ComponentType::Half4 => {
                        for i in 0..cc { out.extend_from_slice(&f32_to_f16(f(i)).to_le_bytes()); }
                    }
                    ComponentType::Rgba8Snorm => {
                        for i in 0..cc { out.push(((f(i) * 127.0).clamp(-127.0, 127.0) as i8) as u8); }
                    }
                    ComponentType::Colour | ComponentType::UByte4 => {
                        for i in 0..cc { out.push(byte(i)); }
                    }
                    _ => {}
                }
                let o = v * stride + decl.component_offset(k);
                if let Some(dst) = vd.bytes.get_mut(o..o + out.len()) { dst.copy_from_slice(&out); }
                at += cc;
            }
        }
        Ok(vd)
    }
    /// `ReadBlockAt<VertexData>`: `count * stride` bytes, one block per address.
    /// Plain bytes, so at an address already read as a block of another type (a stray pointer some modded
    /// files carry) they are read again on their own and left out of the pool, as `ResourceDataReader.ReadBlock`
    /// does, rather than refused.
    fn read(r: &mut Reader, g: &mut Graph, va: u64, stride: usize, count: usize) -> Result<Option<BlockId>> {
        if va == 0 { return Ok(None); }
        let pool = match r.cached_as::<Self>(va) {
            Ok(Some(id)) => return Ok(Some(id)),
            Ok(None) => true,
            Err(_) => false,
        };
        let len = stride.checked_mul(count).ok_or_else(|| anyhow::anyhow!("{count} vertices of {stride} bytes overflow"))?;
        let bytes = r.bytes(va, len)?;
        let id = g.add(VertexData { bytes, stride, count });
        if pool { r.cache::<Self>(va, id); }
        Ok(Some(id))
    }
}
impl Block for VertexData {
    fn length(&self) -> usize { self.bytes.len() }
    fn write(&self, w: &mut Writer, _g: &Graph) -> Result<()> { w.bytes(&self.bytes); Ok(()) }
    fn as_any(&self) -> &dyn Any { self } fn as_any_mut(&mut self) -> &mut dyn Any { self }
}

/// `VertexBuffer` (128 bytes).
pub struct VertexBuffer { pub vft: u32, pub stride: u16, pub flags: u16, pub data1: Option<BlockId>, pub data2: Option<BlockId>, pub count: u32, pub info: Option<BlockId> }

impl VertexBuffer {
    pub fn write_xml(&self, x: &mut XmlOut, g: &Graph) {
        x.value("Flags", self.flags);
        let info = self.info.map(|i| g.get::<VertexDeclaration>(i));
        if let Some(info) = info { info.write_xml(x, "Layout"); }
        let none = VertexDeclaration::from_flags_types(0, 0);
        let decl = info.unwrap_or(&none);
        if let Some(d1) = self.data1 {
            x.open("Data"); g.get::<VertexData>(d1).write_xml_rows(x, decl); x.close("Data");
        }
        if let Some(d2) = self.data2.filter(|d| Some(*d) != self.data1) {
            x.open("Data2"); g.get::<VertexData>(d2).write_xml_rows(x, decl); x.close("Data2");
        }
    }
    /// `ReadXml`: `Data2` is `Data` unless a `<Data2>` element is present.
    pub fn read_xml(n: Node, g: &mut Graph) -> Result<BlockId> {
        let flags = child_attr_u32(n, "Flags", "value") as u16;
        let decl = child(n, "Layout").map(VertexDeclaration::read_xml);
        let stride = decl.as_ref().map_or(0, |d| d.stride);
        let rows = |d: Node| match &decl {
            Some(decl) => VertexData::read_xml_rows(d.text().unwrap_or(""), decl),
            None => Ok(VertexData { bytes: Vec::new(), stride: 0, count: 0 }),
        };
        let mut count = 0;
        let data1 = match child(n, "Data") { Some(d) => { let vd = rows(d)?; count = vd.count as u32; Some(g.add(vd)) } None => None };
        let mut data2 = data1;
        if let Some(d) = child(n, "Data2") { data2 = Some(g.add(rows(d)?)); }
        let info = decl.map(|d| g.add(d));
        Ok(g.add(VertexBuffer { vft: 1080153080, stride, flags, data1, data2, count, info }))
    }
    pub fn read(r: &mut Reader, g: &mut Graph, va: u64) -> Result<Option<BlockId>> {
        if va == 0 { return Ok(None); }
        if let Some(id) = r.cached_as::<Self>(va)? { return Ok(Some(id)); }
        let mut c = r.cursor(va)?;
        let vft = c.u32(); c.skip(4);
        let stride = c.u16(); let flags = c.u16(); c.skip(4);
        let p1 = c.u64(); let count = c.u32(); c.skip(4);
        let p2 = c.u64(); c.skip(8);
        let pinfo = c.u64();
        c.check()?;
        let info = VertexDeclaration::read(r, g, pinfo)?;
        let data1 = VertexData::read(r, g, p1, stride as usize, count as usize)?;
        let data2 = VertexData::read(r, g, p2, stride as usize, count as usize)?;
        let id = g.add(VertexBuffer { vft, stride, flags, data1, data2, count, info }); r.cache::<Self>(va, id); Ok(Some(id))
    }
}
impl Block for VertexBuffer {
    fn length(&self) -> usize { 128 }
    fn references(&self, _g: &Graph) -> Vec<BlockId> {
        let mut v: Vec<BlockId> = self.data1.into_iter().collect();
        if self.data2 != self.data1 { v.extend(self.data2); }
        v.extend(self.info);
        v
    }
    fn write(&self, w: &mut Writer, g: &Graph) -> Result<()> {
        let count = self.data1.or(self.data2).map_or(0, |d| g.get::<VertexData>(d).count as u32);
        w.u32(self.vft); w.u32(1);
        w.u16(self.stride); w.u16(self.flags); w.u32(0);
        w.u64(g.ptr(self.data1)); w.u32(count); w.u32(0);
        w.u64(g.ptr(self.data2)); w.u64(0);
        w.u64(g.ptr(self.info));
        w.zeros(9 * 8);
        Ok(())
    }
    fn as_any(&self) -> &dyn Any { self } fn as_any_mut(&mut self) -> &mut dyn Any { self }
}

/// `IndexBuffer` (96 bytes): `indices` is a `StructArray<u16>`.
pub struct IndexBuffer { pub vft: u32, pub indices: Option<BlockId>, pub count: u32 }

impl IndexBuffer {
    pub fn write_xml(&self, x: &mut XmlOut, g: &Graph) {
        if let Some(i) = self.indices {
            x.raw_array("Data", &g.get::<StructArray<u16>>(i).items, 24, |v| v.to_string());
        }
    }
    pub fn read_xml(n: Node, g: &mut Graph) -> Result<BlockId> {
        let items = child(n, "Data").map(raw_u16s);
        let count = items.as_ref().map_or(0, Vec::len) as u32;
        let indices = items.map(|items| g.add(StructArray { items }));
        Ok(g.add(IndexBuffer { vft: 1080152408, indices, count }))
    }
    pub fn read(r: &mut Reader, g: &mut Graph, va: u64) -> Result<Option<BlockId>> {
        if va == 0 { return Ok(None); }
        if let Some(id) = r.cached_as::<Self>(va)? { return Ok(Some(id)); }
        let mut c = r.cursor(va)?;
        let vft = c.u32(); c.skip(4);
        let count = c.u32(); c.skip(4);
        let p = c.u64();
        c.check()?;
        let indices = read_struct_array::<u16>(r, g, p, count as usize)?;
        let id = g.add(IndexBuffer { vft, indices, count }); r.cache::<Self>(va, id); Ok(Some(id))
    }
}
impl Block for IndexBuffer {
    fn length(&self) -> usize { 96 }
    fn references(&self, _g: &Graph) -> Vec<BlockId> { self.indices.into_iter().collect() }
    fn write(&self, w: &mut Writer, g: &Graph) -> Result<()> {
        let count = self.indices.map_or(0, |i| g.get::<StructArray<u16>>(i).items.len() as u32);
        w.u32(self.vft); w.u32(1); w.u32(count); w.u32(0);
        w.u64(g.ptr(self.indices));
        w.zeros(9 * 8);
        Ok(())
    }
    fn as_any(&self) -> &dyn Any { self } fn as_any_mut(&mut self) -> &mut dyn Any { self }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blocks::base::{write_file_base, PagesInfo};
    use crate::resource::SYSTEM_BASE;
    use std::any::Any;

    #[test]
    fn declaration_derives_stride_and_count_from_flags() {
        // PNCT = 89 = Position|Normal|Colour0|TexCoord0 with GTAV1 types
        let d = VertexDeclaration::from_flags_types(89, 0x7755555555996996);
        assert_eq!((d.count, d.stride), (4, 36));          // 12 + 12 + 4 + 8
        assert_eq!(d.component_type(0), ComponentType::Float3);
        assert_eq!(d.component_type(6), ComponentType::Float2);
        assert_eq!(d.component_offset(6), 28);
        assert_eq!(d.declaration_id(), 0x0000000005096006);
    }
    #[test]
    fn rows_round_trip_every_component_type() {
        // flags bits 0,3,4,6,14 : Position Float3, Normal Float3, Colour0 Colour, TexCoord0 Float2, Tangent Float4 (GTAV1)
        let d = VertexDeclaration::from_flags_types(0b100_0000_0101_1001, 0x7755555555996996);
        let text = "1 2 3   0 0 1   255 128 0 255   0.5 0.25   1 0 0 1\n-1 -2 -3   0 1 0   0 0 0 0   0 1   0 0 1 -1\n";
        let vd = VertexData::read_xml_rows(text, &d).unwrap();
        assert_eq!((vd.count, vd.stride, vd.bytes.len()), (2, 52, 104));
        let mut x = XmlOut::new(); x.indent = 0; x.out.clear();
        vd.write_xml_rows(&mut x, &d);
        assert_eq!(x.out, text);
    }
    #[test]
    fn a_zero_size_component_is_skipped_not_misaligned() {
        // bit 1 BlendWeights with a types nibble of 0 (Nothing), bit 2 BlendIndices UByte4, bit 3 Normal
        let d = VertexDeclaration::from_flags_types(0b1111, 0x7755555555996806);
        assert_eq!(d.stride, 28);   // Position Float3, BlendIndices UByte4, Normal Float3
        let vd = VertexData::read_xml_rows("1 2 3   0 0 0 0   0 0 1\n", &d).unwrap();
        assert_eq!(vd.count, 1);
    }
    #[test]
    fn halves_and_snorm_encode_like_reference() {
        let d = VertexDeclaration { flags: 0b1, stride: 4, unknown_6h: 0, count: 1, types: 1 }; // Position as Half2
        let vd = VertexData::read_xml_rows("1 -2\n", &d).unwrap();
        assert_eq!(vd.bytes, vec![0x00, 0x3C, 0x00, 0xC0]);
        let d = VertexDeclaration { flags: 0b1, stride: 4, unknown_6h: 0, count: 1, types: 10 }; // Rgba8Snorm
        let vd = VertexData::read_xml_rows("1 -1 0.5 0\n", &d).unwrap();
        assert_eq!(vd.bytes, vec![127, 0x81, 63, 0]);
    }
    #[test]
    fn buffers_have_reference_lengths_and_round_trip() {
        let mut g = Graph::new();
        let pages = g.add(PagesInfo::default());
        let d = VertexDeclaration::from_flags_types(89, 0x7755555555996996);
        let data = VertexData::read_xml_rows("0 0 0   0 0 1   255 255 255 255   0 0\n", &d).unwrap();
        let info = g.add(d); let data = g.add(data);
        let vb = g.add(VertexBuffer { vft: 1080153080, stride: 36, flags: 0, data1: Some(data), data2: Some(data), count: 1, info: Some(info) });
        let idx = g.add(StructArray { items: vec![0u16, 0, 0] });
        let ib = g.add(IndexBuffer { vft: 1080152408, indices: Some(idx), count: 3 });
        assert_eq!((g.length(vb), g.length(ib), g.length(info)), (128, 96, 16));
        struct Root(BlockId, BlockId, BlockId);
        impl Block for Root { fn length(&self) -> usize { 32 } fn references(&self, _: &Graph) -> Vec<BlockId> { vec![self.0, self.1, self.2] }
            fn write(&self, w: &mut Writer, g: &Graph) -> Result<()> { write_file_base(w, g, 1, Some(self.0)); w.u64(g.position(self.1)); w.u64(g.position(self.2)); Ok(()) }
            fn as_any(&self) -> &dyn Any { self } fn as_any_mut(&mut self) -> &mut dyn Any { self } }
        let root = g.add(Root(pages, vb, ib));
        let file = g.build(root, pages, 1).unwrap();
        let mut r = Reader::open(&file).unwrap(); let mut g2 = Graph::new();
        let mut c = r.cursor(SYSTEM_BASE).unwrap(); c.skip(16); let vb_va = c.u64(); let ib_va = c.u64();
        let vb2 = VertexBuffer::read(&mut r, &mut g2, vb_va).unwrap().unwrap();
        let ib2 = IndexBuffer::read(&mut r, &mut g2, ib_va).unwrap().unwrap();
        let vb2 = g2.get::<VertexBuffer>(vb2);
        assert_eq!((vb2.stride, vb2.count), (36, 1));
        assert_eq!(vb2.data1, vb2.data2, "one data block when both pointers match");
        assert_eq!(g2.get::<StructArray<u16>>(g2.get::<IndexBuffer>(ib2).indices.unwrap()).items, vec![0, 0, 0]);
    }
}
