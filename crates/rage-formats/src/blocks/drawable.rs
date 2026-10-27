//! The drawable root and what hangs off it, ported from the reference tool's `Drawable.cs`:
//! `DrawableGeometry`, `DrawableModel`, `DrawableModelsBlock`, `DrawableBase` and
//! `Drawable` (the `.ydr` root, 208 bytes). Only the legacy PC layout is handled.
//!
//! The models block owns every model as a part and each model owns its geometries
//! as parts, so [`Graph::build`] positions them from the owner's position. A block
//! cannot see the graph in [`Block::length`] and [`Block::parts`], so a model keeps
//! the lengths of its geometries and the models block the lengths of its models,
//! captured when they are built ([`DrawableModel::new`], [`DrawableModelsBlock::new`]).

use std::path::Path;

use anyhow::{bail, Result};

use super::bounds::BoundBlock;
use super::base::{read_pages_info, read_simple_list64, read_string_block, read_struct_array, write_file_base, write_simple_list64, PagesInfo, StringBlock, StructArray};
use super::light::Light;
use super::shader::ShaderGroup;
use super::skeleton::{Joints, Skeleton};
use super::vertex::{IndexBuffer, VertexBuffer, VertexData};
use super::xml::{child, child_attr_f32, child_attr_u32, child_text, child_vec3, child_vec4, float, items, write_items, Node, XmlOut};
use super::*;
use crate::names::NameTable;

/// `DrawableGeometry.Unknown_62h`-style constant: 3 indices per primitive.
const INDICES_PER_PRIMITIVE: u16 = 3;
/// `Unknown_3Ch` / `Unknown_4Ch` of `DrawableBase`.
const BOUNDS_PAD: u32 = 0x7f80_0001;

fn pad16(off: usize) -> usize { (16 - off % 16) % 16 }

/// `DrawableGeometry` (152 bytes, plus the bone ids embedded after it: 8 more bytes of
/// padding when there are more than 4, then 2 bytes per id).
pub struct DrawableGeometry {
    /// 1080133528.
    pub vft: u32,
    pub vertex_buffer: Option<BlockId>,
    pub index_buffer: Option<BlockId>,
    /// Empty for none.
    pub bone_ids: Vec<u16>,
    /// Written by the parent model as its shader mapping.
    pub shader_id: u16,
    /// Written by the parent model as its bounds data.
    pub aabb_min: Vec4,
    pub aabb_max: Vec4,
}

impl DrawableGeometry {
    /// The vertex data geometry points at: `VertexBuffer.Data1 ?? Data2`.
    fn vertex_data(&self, g: &Graph) -> Option<BlockId> {
        self.vertex_buffer.and_then(|v| { let vb = g.get::<VertexBuffer>(v); vb.data1.or(vb.data2) })
    }

    /// `DrawableGeometry.WriteXml`.
    pub fn write_xml(&self, x: &mut XmlOut, g: &Graph) {
        x.value("ShaderIndex", self.shader_id);
        x.vec4("BoundingBoxMin", self.aabb_min);
        x.vec4("BoundingBoxMax", self.aabb_max);
        if !self.bone_ids.is_empty() {
            let ids: Vec<String> = self.bone_ids.iter().map(u16::to_string).collect();
            x.string("BoneIDs", &ids.join(", "));
        }
        if let Some(vb) = self.vertex_buffer {
            x.open("VertexBuffer");
            g.get::<VertexBuffer>(vb).write_xml(x, g);
            x.close("VertexBuffer");
        }
        if let Some(ib) = self.index_buffer {
            x.open("IndexBuffer");
            g.get::<IndexBuffer>(ib).write_xml(x, g);
            x.close("IndexBuffer");
        }
    }

    /// `DrawableGeometry.ReadXml`.
    pub fn read_xml(n: Node, g: &mut Graph) -> Result<BlockId> {
        let bone_ids = child(n, "BoneIDs").and_then(|b| b.text()).map_or_else(Vec::new, |t| {
            t.split(',').filter_map(|w| w.trim().parse::<u16>().ok()).collect()
        });
        let vertex_buffer = match child(n, "VertexBuffer") { Some(v) => Some(VertexBuffer::read_xml(v, g)?), None => None };
        let index_buffer = match child(n, "IndexBuffer") { Some(i) => Some(IndexBuffer::read_xml(i, g)?), None => None };
        Ok(g.add(DrawableGeometry {
            vft: 1080133528, vertex_buffer, index_buffer, bone_ids,
            shader_id: child_attr_u32(n, "ShaderIndex", "value") as u16,
            aabb_min: child_vec4(n, "BoundingBoxMin"),
            aabb_max: child_vec4(n, "BoundingBoxMax"),
        }))
    }

    /// `DrawableGeometry.Read`; the shader id and bounds are the parent model's to fill in.
    pub fn read(r: &mut Reader, g: &mut Graph, va: u64) -> Result<Option<BlockId>> {
        if va == 0 { return Ok(None); }
        if let Some(id) = r.cached_as::<Self>(va)? { return Ok(Some(id)); }
        let mut c = r.cursor(va)?;
        let vft = c.u32(); c.skip(4 + 8 + 8);
        let vb_ptr = c.u64(); c.skip(3 * 8);
        let ib_ptr = c.u64(); c.skip(3 * 8);
        c.skip(4 + 4 + 2 + 2 + 4);
        let bone_ptr = c.u64();
        c.skip(2);
        let bone_count = c.u16(); c.skip(4 + 8 + 3 * 8);
        c.check()?;
        let vertex_buffer = VertexBuffer::read(r, g, vb_ptr)?;
        let index_buffer = IndexBuffer::read(r, g, ib_ptr)?;
        let bone_ids = r.u16s(bone_ptr, bone_count as usize)?;
        let id = g.add(DrawableGeometry { vft, vertex_buffer, index_buffer, bone_ids, shader_id: 0, aabb_min: Vec4::default(), aabb_max: Vec4::default() });
        r.cache::<Self>(va, id); Ok(Some(id))
    }
}
impl Block for DrawableGeometry {
    fn length(&self) -> usize {
        152 + if self.bone_ids.is_empty() { 0 } else { (if self.bone_ids.len() > 4 { 8 } else { 0 }) + 2 * self.bone_ids.len() }
    }
    fn references(&self, g: &Graph) -> Vec<BlockId> {
        self.vertex_buffer.into_iter().chain(self.index_buffer).chain(self.vertex_data(g)).collect()
    }
    fn write(&self, w: &mut Writer, g: &Graph) -> Result<()> {
        let indices = self.index_buffer.and_then(|i| g.get::<IndexBuffer>(i).indices)
            .map_or(0, |i| g.get::<StructArray<u16>>(i).items.len() as u32);
        let data = self.vertex_data(g);
        let vertices = count_u16(data.map_or(0, |d| g.get::<VertexData>(d).count), "vertices in a geometry")?;
        let stride = self.vertex_buffer.map_or(0, |v| g.get::<VertexBuffer>(v).stride);
        let ids = self.bone_ids.len();
        let ids16 = count_u16(ids, "bone ids in a geometry")?;
        let bone_ptr = if ids == 0 { 0 } else { w.position() + 152 + if ids > 4 { 8 } else { 0 } };
        w.u32(self.vft); w.u32(1); w.u64(0); w.u64(0);
        w.u64(g.ptr(self.vertex_buffer)); w.zeros(3 * 8);
        w.u64(g.ptr(self.index_buffer)); w.zeros(3 * 8);
        w.u32(indices); w.u32(indices / 3); w.u16(vertices); w.u16(INDICES_PER_PRIMITIVE); w.u32(0);
        w.u64(bone_ptr); w.u16(stride); w.u16(ids16); w.u32(0);
        w.u64(g.ptr(data)); w.zeros(3 * 8);
        if ids > 4 { w.u64(0); }
        for &b in &self.bone_ids { w.u16(b); }
        Ok(())
    }
    fn as_any(&self) -> &dyn Any { self } fn as_any_mut(&mut self) -> &mut dyn Any { self }
}

/// `DrawableModel` (48-byte header, then the shader mapping, geometry pointers, bounds
/// data and the geometries themselves, each 16-aligned).
pub struct DrawableModel {
    /// 1080101528.
    pub vft: u32,
    pub geometries: Vec<BlockId>,
    /// Byte 3 is the bone index, byte 1 `HasSkin`, byte 0 `Unknown1`.
    pub skeleton_binding: u32,
    /// Byte 0 is the render mask, byte 1 the flags.
    pub render_mask_flags: u16,
    /// `Block::length` of each geometry, captured at construction.
    geometry_lengths: Vec<usize>,
}

impl DrawableModel {
    /// A model over `geometries`, which must already be in `g`.
    pub fn new(g: &Graph, geometries: Vec<BlockId>, skeleton_binding: u32, render_mask_flags: u16) -> Self {
        let geometry_lengths = geometries.iter().map(|&i| g.length(i)).collect();
        Self { vft: 1080101528, geometries, skeleton_binding, render_mask_flags, geometry_lengths }
    }

    /// The offset of the geometry pointers, the bounds data and each geometry, and the total length.
    fn layout(&self) -> (usize, usize, Vec<usize>, usize) {
        let n = self.geometries.len();
        let mut off = 48 + 2 * n;
        off += if n == 1 { 6 } else { pad16(off) };
        let pointers = off;
        off += 8 * n;
        off += pad16(off);
        let bounds = off;
        off += (if n > 1 { n + 1 } else { n }) * 32;
        let mut at = Vec::with_capacity(n);
        for &len in &self.geometry_lengths {
            off += pad16(off);
            at.push(off);
            off += len;
        }
        (pointers, bounds, at, off)
    }

    /// The shader index of each geometry.
    pub fn shader_mapping(&self, g: &Graph) -> Vec<u16> {
        self.geometries.iter().map(|&i| g.get::<DrawableGeometry>(i).shader_id).collect()
    }
    /// The geometries' boxes, with the box around all of them first when there are several
    /// (`DrawableModel.ReadXml`).
    pub fn bounds_data(&self, g: &Graph) -> Vec<(Vec4, Vec4)> {
        let boxes: Vec<(Vec4, Vec4)> = self.geometries.iter().map(|&i| { let d = g.get::<DrawableGeometry>(i); (d.aabb_min, d.aabb_max) }).collect();
        if boxes.len() < 2 { return boxes; }
        let lo = |a: Vec4, b: Vec4| Vec4::new(a.x.min(b.x), a.y.min(b.y), a.z.min(b.z), a.w.min(b.w));
        let hi = |a: Vec4, b: Vec4| Vec4::new(a.x.max(b.x), a.y.max(b.y), a.z.max(b.z), a.w.max(b.w));
        let outer = boxes.iter().fold(
            (Vec4::new(f32::MAX, f32::MAX, f32::MAX, f32::MAX), Vec4::new(f32::MIN, f32::MIN, f32::MIN, f32::MIN)),
            |(mn, mx), &(a, b)| (lo(mn, a), hi(mx, b)),
        );
        std::iter::once(outer).chain(boxes).collect()
    }

    pub fn render_mask(&self) -> u8 { self.render_mask_flags as u8 }
    pub fn flags(&self) -> u8 { (self.render_mask_flags >> 8) as u8 }
    pub fn has_skin(&self) -> u8 { (self.skeleton_binding >> 8) as u8 }
    pub fn bone_index(&self) -> u8 { (self.skeleton_binding >> 24) as u8 }
    pub fn unknown1(&self) -> u8 { self.skeleton_binding as u8 }

    /// `DrawableModel.WriteXml`.
    pub fn write_xml(&self, x: &mut XmlOut, g: &Graph) {
        x.value("RenderMask", self.render_mask());
        x.value("Flags", self.flags());
        x.value("HasSkin", self.has_skin());
        x.value("BoneIndex", self.bone_index());
        x.value("Unknown1", self.unknown1());
        if !self.geometries.is_empty() {
            write_items(x, "Geometries", &self.geometries, |x, &i| g.get::<DrawableGeometry>(i).write_xml(x, g));
        }
    }

    /// `DrawableModel.ReadXml`; the shader mapping and bounds data are derived from the geometries.
    pub fn read_xml(n: Node, g: &mut Graph) -> Result<BlockId> {
        let byte = |name: &str| child_attr_u32(n, name, "value") as u8 as u32;
        let skeleton_binding = byte("Unknown1") | byte("HasSkin") << 8 | byte("BoneIndex") << 24;
        let render_mask_flags = (byte("RenderMask") | byte("Flags") << 8) as u16;
        let geometries = items(n, "Geometries").into_iter().map(|i| DrawableGeometry::read_xml(i, g)).collect::<Result<Vec<_>>>()?;
        Ok(g.add(Self::new(g, geometries, skeleton_binding, render_mask_flags)))
    }

    /// `DrawableModel.Read`: the geometries take their shader index and box from the model.
    pub fn read(r: &mut Reader, g: &mut Graph, va: u64) -> Result<Option<BlockId>> {
        if va == 0 { return Ok(None); }
        if let Some(id) = r.cached_as::<Self>(va)? { return Ok(Some(id)); }
        let mut c = r.cursor(va)?;
        let vft = c.u32(); c.skip(4);
        let geoms_ptr = c.u64();
        let count = c.u16() as usize; c.skip(2 + 4);
        let bounds_ptr = c.u64();
        let mapping_ptr = c.u64();
        let skeleton_binding = c.u32();
        let render_mask_flags = c.u16(); c.skip(2);
        c.check()?;
        let mapping = r.u16s(mapping_ptr, count)?;
        let pointers = r.u64s(geoms_ptr, count)?;
        let bounds_count = if count > 1 { count + 1 } else { count };
        let bounds = r.structs::<Vec4>(bounds_ptr, 2 * bounds_count)?;
        let mut geometries = Vec::new();
        for (i, &p) in pointers.iter().enumerate() {
            let Some(geom) = DrawableGeometry::read(r, g, p)? else { continue };
            let (lo, hi) = match bounds.len() / 2 {
                0 => (Vec4::default(), Vec4::default()),
                len if len > 1 && i + 1 < len => (bounds[2 * (i + 1)], bounds[2 * (i + 1) + 1]),
                _ => (bounds[0], bounds[1]),
            };
            let geom_mut = g.get_mut::<DrawableGeometry>(geom);
            geom_mut.shader_id = mapping.get(i).copied().unwrap_or(0);
            geom_mut.aabb_min = lo;
            geom_mut.aabb_max = hi;
            geometries.push(geom);
        }
        let mut model = Self::new(g, geometries, skeleton_binding, render_mask_flags);
        model.vft = vft;
        let id = g.add(model);
        r.cache::<Self>(va, id); Ok(Some(id))
    }
}
impl Block for DrawableModel {
    fn length(&self) -> usize { self.layout().3 }
    fn parts(&self) -> Vec<(usize, BlockId)> { self.layout().2.into_iter().zip(self.geometries.iter().copied()).collect() }
    fn write(&self, w: &mut Writer, g: &Graph) -> Result<()> {
        let n = self.geometries.len();
        let n16 = count_u16(n, "geometries in a model")?;
        let base = w.position();
        let (pointers, bounds, _, _) = self.layout();
        w.u32(self.vft); w.u32(1);
        w.u64(base + pointers as u64);
        w.u16(n16); w.u16(n16); w.u32(0);
        w.u64(base + bounds as u64);
        w.u64(base + 48);
        w.u32(self.skeleton_binding); w.u16(self.render_mask_flags); w.u16(n16);
        for s in self.shader_mapping(g) { w.u16(s); }
        if n == 1 { w.zeros(6); } else { w.pad16(); }
        for &geom in &self.geometries { w.u64(g.position(geom)); }
        w.pad16();
        for (lo, hi) in self.bounds_data(g) { w.vec4(lo); w.vec4(hi); }
        for &geom in &self.geometries {
            w.pad16();
            g.get::<DrawableGeometry>(geom).write(w, g)?;
        }
        Ok(())
    }
    fn as_any(&self) -> &dyn Any { self } fn as_any_mut(&mut self) -> &mut dyn Any { self }
}

/// The five LODs of a models block, in file order.
const LOD_COUNT: usize = 5;

/// `DrawableModelsBlock`: per LOD list a 16-byte pointer-list header, the model pointers,
/// then the models inline (each one a part). The root's LOD pointers point at the headers.
pub struct DrawableModelsBlock {
    pub high: Option<Vec<BlockId>>,
    pub med: Option<Vec<BlockId>>,
    pub low: Option<Vec<BlockId>>,
    pub vlow: Option<Vec<BlockId>>,
    /// `Extra`: the reference tool says it "shouldn't be used".
    pub extra: Option<Vec<BlockId>>,
    /// `Block::length` of each model, in list order, captured at construction.
    model_lengths: [Vec<usize>; LOD_COUNT],
}

impl DrawableModelsBlock {
    /// A block over the model lists, whose models must already be in `g`.
    pub fn new(g: &Graph, high: Option<Vec<BlockId>>, med: Option<Vec<BlockId>>, low: Option<Vec<BlockId>>, vlow: Option<Vec<BlockId>>, extra: Option<Vec<BlockId>>) -> Self {
        let mut block = Self { high, med, low, vlow, extra, model_lengths: Default::default() };
        let lens: Vec<Vec<usize>> = block.lists().iter().map(|l| l.map_or_else(Vec::new, |l| l.iter().map(|&m| g.length(m)).collect())).collect();
        for (slot, l) in block.model_lengths.iter_mut().zip(lens) { *slot = l; }
        block
    }

    fn lists(&self) -> [Option<&Vec<BlockId>>; LOD_COUNT] {
        [self.high.as_ref(), self.med.as_ref(), self.low.as_ref(), self.vlow.as_ref(), self.extra.as_ref()]
    }

    /// Per list: the header's offset and the offset of each model; and the total length.
    fn layout(&self) -> ([Option<usize>; LOD_COUNT], Vec<Vec<usize>>, usize) {
        let mut off = 0;
        let mut headers = [None; LOD_COUNT];
        let mut models = vec![Vec::new(); LOD_COUNT];
        for (k, list) in self.lists().into_iter().enumerate() {
            let Some(list) = list else { continue };
            off += pad16(off);
            headers[k] = Some(off);
            off += 16 + 8 * list.len();
            for &len in &self.model_lengths[k] {
                off += pad16(off);
                models[k].push(off);
                off += len;
            }
        }
        (headers, models, off)
    }

    /// The address of LOD list `k`'s header (`GetHighPointer` and its siblings), 0 for an absent list.
    /// `own` is this block's id, which the graph positions.
    fn lod_ptr(&self, g: &Graph, own: BlockId, k: usize) -> u64 {
        self.layout().0[k].map_or(0, |o| g.position(own) + o as u64)
    }
    pub fn high_ptr(&self, g: &Graph, own: BlockId) -> u64 { self.lod_ptr(g, own, 0) }
    pub fn med_ptr(&self, g: &Graph, own: BlockId) -> u64 { self.lod_ptr(g, own, 1) }
    pub fn low_ptr(&self, g: &Graph, own: BlockId) -> u64 { self.lod_ptr(g, own, 2) }
    pub fn vlow_ptr(&self, g: &Graph, own: BlockId) -> u64 { self.lod_ptr(g, own, 3) }

    pub fn all_models(&self) -> Vec<BlockId> { self.lists().into_iter().flatten().flatten().copied().collect() }

    /// `Read` of `DrawableModelsBlock`: the lists at the root's LOD pointers, and an extra
    /// list when the block itself starts elsewhere. A block that starts at a LOD pointer
    /// other than the high one (no high models) is that LOD's list, not an extra one.
    pub fn read(r: &mut Reader, g: &mut Graph, va: u64, lods: [u64; 4]) -> Result<Option<BlockId>> {
        if va == 0 { return Ok(None); }
        if let Some(id) = r.cached_as::<Self>(va)? { return Ok(Some(id)); }
        let read_list = |r: &mut Reader, g: &mut Graph, ptr: u64| -> Result<Option<Vec<BlockId>>> {
            if ptr == 0 { return Ok(None); }
            let mut c = r.cursor(ptr)?;
            let (list_ptr, _count, capacity) = read_simple_list64(&mut c);
            c.check()?;
            let pointers = r.u64s(list_ptr, capacity as usize)?;
            let mut models = Vec::new();
            for p in pointers { if let Some(m) = DrawableModel::read(r, g, p)? { models.push(m); } }
            Ok(Some(models))
        };
        let high = read_list(r, g, lods[0])?;
        let med = read_list(r, g, lods[1])?;
        let low = read_list(r, g, lods[2])?;
        let vlow = read_list(r, g, lods[3])?;
        let extra = if lods.contains(&va) { None } else { read_list(r, g, va)? };
        let id = g.add(Self::new(g, high, med, low, vlow, extra));
        r.cache::<Self>(va, id); Ok(Some(id))
    }
}
impl Block for DrawableModelsBlock {
    fn length(&self) -> usize { self.layout().2 }
    fn parts(&self) -> Vec<(usize, BlockId)> {
        let (_, offsets, _) = self.layout();
        self.lists().into_iter().zip(offsets).flat_map(|(l, o)| l.into_iter().flatten().copied().zip(o).map(|(m, o)| (o, m))).collect()
    }
    fn write(&self, w: &mut Writer, g: &Graph) -> Result<()> {
        for list in self.lists().into_iter().flatten() {
            w.pad16();
            // `ResourcePointerListHeader`: the pointer array right after it, count, capacity
            let n = count_u16(list.len(), "models in a LOD list")?;
            w.u64(w.position() + 16); w.u16(n); w.u16(n); w.u32(0);
            for &m in list { w.u64(g.position(m)); }
            for &m in list {
                w.pad16();
                g.get::<DrawableModel>(m).write(w, g)?;
            }
        }
        Ok(())
    }
    fn as_any(&self) -> &dyn Any { self } fn as_any_mut(&mut self) -> &mut dyn Any { self }
}

/// `Drawable` (208 bytes): `ResourceFileBase`, the `DrawableBase` fields and the name, light
/// list and bound pointers. The light list's 16-byte header is written inline, pointing at
/// a `StructArray<Light>`.
pub struct Drawable {
    /// 1079456120.
    pub vft: u32,
    pub pages: Option<BlockId>,
    pub shader_group: Option<BlockId>,
    pub skeleton: Option<BlockId>,
    pub joints: Option<BlockId>,
    pub bounding_center: Vec3,
    pub bounding_sphere_radius: f32,
    pub bounding_box_min: Vec3,
    pub bounding_box_max: Vec3,
    /// High, medium, low, very low.
    pub lod_dist: [f32; 4],
    /// Per LOD: byte 0 the flags, byte 1 the render mask.
    pub render_mask_flags: [u32; 4],
    pub models: Option<BlockId>,
    pub name: Option<BlockId>,
    /// A `StructArray<Light>`.
    pub lights: Option<BlockId>,
    pub lights_count: usize,
    /// A `BoundBlock`; a bound in a drawable has no pages of its own.
    pub bound: Option<BlockId>,
}

impl Drawable {
    /// `FlagsHigh` and its siblings (`lod`: 0 high .. 3 very low).
    pub fn flags(&self, lod: usize) -> u8 { self.render_mask_flags[lod] as u8 }
    pub fn set_flags(&mut self, lod: usize, v: u8) { self.render_mask_flags[lod] = self.render_mask_flags[lod] & 0xFFFF_FF00 | u32::from(v); }
    /// `RenderMaskHigh` and its siblings.
    pub fn render_mask(&self, lod: usize) -> u8 { (self.render_mask_flags[lod] >> 8) as u8 }
    pub fn set_render_mask(&mut self, lod: usize, v: u8) { self.render_mask_flags[lod] = self.render_mask_flags[lod] & 0xFFFF_00FF | u32::from(v) << 8; }

    /// `BuildRenderMasks`: each LOD's mask is the OR of its models' masks, 0 for an absent LOD.
    pub fn build_render_masks(&mut self, g: &Graph) {
        let masks: [u8; 4] = match self.models {
            Some(m) => {
                let block = g.get::<DrawableModelsBlock>(m);
                [&block.high, &block.med, &block.low, &block.vlow].map(|l| {
                    l.as_ref().map_or(0, |l| l.iter().fold(0, |mask, &i| mask | g.get::<DrawableModel>(i).render_mask()))
                })
            }
            None => [0; 4],
        };
        for (lod, mask) in masks.into_iter().enumerate() { self.set_render_mask(lod, mask); }
    }

    /// `Drawable.WriteXml` for the `<Drawable>` element's children: `Name`, the base fields,
    /// then the lights.
    pub fn write_xml(&self, x: &mut XmlOut, g: &Graph, names: &NameTable, dds_dir: Option<&Path>) -> Result<()> {
        x.string("Name", self.name.map_or("", |n| g.get::<StringBlock>(n).0.as_str()));
        x.vec3("BoundingSphereCenter", self.bounding_center);
        x.value("BoundingSphereRadius", float(self.bounding_sphere_radius));
        x.vec3("BoundingBoxMin", self.bounding_box_min);
        x.vec3("BoundingBoxMax", self.bounding_box_max);
        for (tag, d) in ["LodDistHigh", "LodDistMed", "LodDistLow", "LodDistVlow"].into_iter().zip(self.lod_dist) { x.value(tag, float(d)); }
        for (lod, tag) in ["FlagsHigh", "FlagsMed", "FlagsLow", "FlagsVlow"].into_iter().enumerate() { x.value(tag, self.flags(lod)); }
        if let Some(sg) = self.shader_group {
            x.open("ShaderGroup");
            g.get::<ShaderGroup>(sg).write_xml(x, g, names, dds_dir)?;
            x.close("ShaderGroup");
        }
        if let Some(s) = self.skeleton {
            x.open("Skeleton");
            g.get::<Skeleton>(s).write_xml(x, g);
            x.close("Skeleton");
        }
        if let Some(j) = self.joints {
            x.open("Joints");
            g.get::<Joints>(j).write_xml(x, g);
            x.close("Joints");
        }
        if let Some(m) = self.models {
            let block = g.get::<DrawableModelsBlock>(m);
            for (tag, list) in [("DrawableModelsHigh", &block.high), ("DrawableModelsMedium", &block.med), ("DrawableModelsLow", &block.low),
                                ("DrawableModelsVeryLow", &block.vlow), ("DrawableModelsX", &block.extra)] {
                if let Some(list) = list {
                    write_items(x, tag, list, |x, &i| g.get::<DrawableModel>(i).write_xml(x, g));
                }
            }
        }
        if let Some(b) = self.bound { g.get::<BoundBlock>(b).write_xml(x, g, None); }
        // `LightAttributes` is read inline, so a file's drawable always has a (maybe empty) list
        // and `WriteXml` always writes `<Lights />` for one read from a file.
        let lights = self.lights.map_or(&[][..], |l| &g.get::<StructArray<Light>>(l).items[..]);
        write_items(x, "Lights", lights, |x, l| l.write_xml(x, names));
        Ok(())
    }

    /// `Drawable.ReadXml`: `n` is the `<Drawable>` element. What the reference tool accepts silently but the
    /// game may not (a texture that is not embedded, a bone without a name) is added to `warnings`.
    pub fn read_xml(n: Node, g: &mut Graph, dds_dir: Option<&Path>, warnings: &mut Vec<String>) -> Result<BlockId> {
        let shader_group = match child(n, "ShaderGroup") { Some(s) => Some(ShaderGroup::read_xml(s, g, dds_dir, warnings)?), None => None };
        let skeleton = match child(n, "Skeleton") { Some(s) => Some(Skeleton::read_xml(s, g, warnings)?), None => None };
        let joints = match child(n, "Joints") { Some(j) => Some(Joints::read_xml(j, g)?), None => None };
        // An absent or empty list is `None` (`XmlMeta.ReadItemArray`).
        let mut list = |tag: &str| -> Result<Option<Vec<BlockId>>> {
            let nodes = items(n, tag);
            if nodes.is_empty() { return Ok(None); }
            nodes.into_iter().map(|i| DrawableModel::read_xml(i, g)).collect::<Result<Vec<_>>>().map(Some)
        };
        let high = list("DrawableModelsHigh")?;
        let med = list("DrawableModelsMedium")?;
        let low = list("DrawableModelsLow")?;
        let vlow = list("DrawableModelsVeryLow")?;
        let extra = list("DrawableModelsX")?;
        // The extra list is read from the models block's start, which is where the first LOD list is written:
        // next to any LOD list it would be written but never read back.
        let lods = [("DrawableModelsHigh", &high), ("DrawableModelsMedium", &med), ("DrawableModelsLow", &low), ("DrawableModelsVeryLow", &vlow)];
        if let (Some(_), Some((tag, _))) = (&extra, lods.iter().find(|(_, l)| l.is_some())) {
            bail!("<DrawableModelsX> cannot be built next to <{tag}>: the extra model list is only read back when it is the drawable's only model list");
        }
        let models = if [&high, &med, &low, &vlow, &extra].iter().all(|l| l.is_none()) { None } else { Some(g.add(DrawableModelsBlock::new(g, high, med, low, vlow, extra))) };
        let pages = Some(g.add(PagesInfo::default()));
        let name = Some(g.add(StringBlock(child_text(n, "Name"))));
        let lights: Vec<Light> = items(n, "Lights").into_iter().map(Light::read_xml).collect();
        let lights_count = lights.len();
        let lights = if lights.is_empty() { None } else { Some(g.add(StructArray { items: lights })) };
        let bound = match child(n, "Bounds") { Some(b) => BoundBlock::read_xml(b, g, None)?, None => None };
        let mut d = Drawable {
            vft: 1079456120, pages, shader_group, skeleton, joints,
            bounding_center: child_vec3(n, "BoundingSphereCenter"),
            bounding_sphere_radius: child_attr_f32(n, "BoundingSphereRadius", "value"),
            bounding_box_min: child_vec3(n, "BoundingBoxMin"),
            bounding_box_max: child_vec3(n, "BoundingBoxMax"),
            lod_dist: ["LodDistHigh", "LodDistMed", "LodDistLow", "LodDistVlow"].map(|t| child_attr_f32(n, t, "value")),
            render_mask_flags: [0; 4], models, name, lights, lights_count, bound,
        };
        for (lod, tag) in ["FlagsHigh", "FlagsMed", "FlagsLow", "FlagsVlow"].into_iter().enumerate() {
            d.set_flags(lod, child_attr_u32(n, tag, "value") as u8);
        }
        d.build_render_masks(g);
        Ok(g.add(d))
    }

    /// `Drawable.Read` (and `DrawableBase.Read`).
    pub fn read(r: &mut Reader, g: &mut Graph, va: u64) -> Result<Option<BlockId>> {
        if va == 0 { return Ok(None); }
        if let Some(id) = r.cached_as::<Self>(va)? { return Ok(Some(id)); }
        let mut c = r.cursor(va)?;
        let vft = c.u32(); c.skip(4);
        let pages_ptr = c.u64();
        let (sg_ptr, skeleton_ptr) = (c.u64(), c.u64());
        let bounding_center = c.vec3(); let bounding_sphere_radius = c.f32();
        let bounding_box_min = c.vec3(); c.skip(4);
        let bounding_box_max = c.vec3(); c.skip(4);
        let lod_ptrs = [c.u64(), c.u64(), c.u64(), c.u64()];
        let lod_dist = [c.f32(), c.f32(), c.f32(), c.f32()];
        let render_mask_flags = [c.u32(), c.u32(), c.u32(), c.u32()];
        let joints_ptr = c.u64(); c.skip(2 + 2 + 4);
        let models_ptr = c.u64();
        let name_ptr = c.u64();
        let (lights_ptr, lights_count, _) = read_simple_list64(&mut c);
        c.skip(8);
        let bound_ptr = c.u64();
        c.check()?;

        let pages = read_pages_info(r, g, pages_ptr)?;
        let shader_group = ShaderGroup::read(r, g, sg_ptr)?;
        let skeleton = Skeleton::read(r, g, skeleton_ptr)?;
        let joints = Joints::read(r, g, joints_ptr)?;
        let models = DrawableModelsBlock::read(r, g, if models_ptr == 0 { lod_ptrs[0] } else { models_ptr }, lod_ptrs)?;
        let name = read_string_block(r, g, name_ptr)?;
        let lights = read_struct_array::<Light>(r, g, lights_ptr, lights_count as usize)?;
        let bound = BoundBlock::read(r, g, bound_ptr, None)?;
        let id = g.add(Drawable {
            vft, pages, shader_group, skeleton, joints, bounding_center, bounding_sphere_radius, bounding_box_min, bounding_box_max,
            lod_dist, render_mask_flags, models, name, lights, lights_count: lights_count as usize, bound,
        });
        r.cache::<Self>(va, id); Ok(Some(id))
    }
}
impl Block for Drawable {
    fn length(&self) -> usize { 208 }
    fn references(&self, _g: &Graph) -> Vec<BlockId> {
        [self.pages, self.shader_group, self.skeleton, self.joints, self.models, self.name, self.lights, self.bound].into_iter().flatten().collect()
    }
    fn write(&self, w: &mut Writer, g: &Graph) -> Result<()> {
        let lod = |f: fn(&DrawableModelsBlock, &Graph, BlockId) -> u64| self.models.map_or(0, |m| f(g.get::<DrawableModelsBlock>(m), g, m));
        let lights = self.lights.map_or(0, |l| g.get::<StructArray<Light>>(l).items.len());
        write_file_base(w, g, self.vft, self.pages);
        w.u64(g.ptr(self.shader_group)); w.u64(g.ptr(self.skeleton));
        w.vec3(self.bounding_center); w.f32(self.bounding_sphere_radius);
        w.vec3(self.bounding_box_min); w.u32(BOUNDS_PAD);
        w.vec3(self.bounding_box_max); w.u32(BOUNDS_PAD);
        w.u64(lod(DrawableModelsBlock::high_ptr)); w.u64(lod(DrawableModelsBlock::med_ptr));
        w.u64(lod(DrawableModelsBlock::low_ptr)); w.u64(lod(DrawableModelsBlock::vlow_ptr));
        for d in self.lod_dist { w.f32(d); }
        for m in self.render_mask_flags { w.u32(m); }
        w.u64(g.ptr(self.joints));
        w.u16(0); w.u16(self.models.map_or(0, |m| g.length(m).div_ceil(16)) as u16); w.u32(0);
        w.u64(g.ptr(self.models));
        w.u64(g.ptr(self.name));
        write_simple_list64(w, g, self.lights, lights, "lights")?;
        w.u64(0);
        w.u64(g.ptr(self.bound));
        Ok(())
    }
    fn as_any(&self) -> &dyn Any { self } fn as_any_mut(&mut self) -> &mut dyn Any { self }
}
