//! The reference tool's resource block graph, ported: a resource is an arena of
//! blocks that point at each other by [`BlockId`]; [`Graph::build`] lays
//! them into RSC7 pages the way `ResourceBuilder.Build` does and
//! [`Reader`] reads them back with `ResourceDataReader`'s position-keyed block pool.
//!
//! The entry points are [`ydr::read_ydr`], [`ydr::write_ydr`], [`ydr::dump_ydr_xml`],
//! [`ydr::build_ydr_from_xml`] and their [`ybn`] counterparts (drawables and bounds to and from
//! The reference tool's XML), also re-exported from the crate root.

pub mod base;
pub mod bounds;
pub mod bvh;
pub mod drawable;
pub mod light;
pub mod shader;
pub mod skeleton;
pub mod texture;
pub mod vertex;
pub mod xml;
pub mod ybn;
pub mod ydr;
pub mod yft;
pub mod yft_xml;

use std::any::{Any, TypeId};
use std::collections::{HashMap, HashSet};

use anyhow::{bail, Context, Result};

use crate::math::{Vec3, Vec4, Mat4};
use crate::resource::{build_rsc7_with_flags, pack_pages, prepare_rsc7, rsc7_page_count, GRAPHICS_BASE, SYSTEM_BASE};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BlockId(pub u32);

/// What [`ydr::build_ydr_from_xml_checked`] and [`ybn::build_ybn_from_xml_checked`] return: the file, the XML
/// it reads back as, and a line for each thing the reference tool would accept silently but the game may not.
#[derive(Debug, Clone)]
pub struct Built { pub bytes: Vec<u8>, pub xml: String, pub warnings: Vec<String> }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section { System, Graphics }

pub trait Block: Any {
    fn length(&self) -> usize;
    fn section(&self) -> Section { Section::System }
    fn references(&self, _g: &Graph) -> Vec<BlockId> { Vec::new() }
    fn parts(&self) -> Vec<(usize, BlockId)> { Vec::new() }
    /// The block that lays this one out as a part of itself: a reference to this block is a reference to that one.
    fn owner(&self) -> Option<BlockId> { None }
    fn write(&self, w: &mut Writer, g: &Graph) -> Result<()>;
    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;
}

#[derive(Default)]
pub struct Graph { blocks: Vec<Box<dyn Block>>, positions: Vec<u64> }

impl Graph {
    pub fn new() -> Self { Self::default() }
    pub fn add<B: Block>(&mut self, b: B) -> BlockId {
        self.blocks.push(Box::new(b));
        self.positions.push(0);
        BlockId(self.blocks.len() as u32 - 1)
    }
    pub fn get<B: Block>(&self, id: BlockId) -> &B {
        self.blocks[id.0 as usize].as_any().downcast_ref().expect("block type")
    }
    /// `get`, or `None` when the block is of another type.
    pub fn try_get<B: Block>(&self, id: BlockId) -> Option<&B> { self.blocks[id.0 as usize].as_any().downcast_ref() }
    pub fn get_mut<B: Block>(&mut self, id: BlockId) -> &mut B {
        self.blocks[id.0 as usize].as_any_mut().downcast_mut().expect("block type")
    }
    pub fn length(&self, id: BlockId) -> usize { self.blocks[id.0 as usize].length() }
    pub fn position(&self, id: BlockId) -> u64 { self.positions[id.0 as usize] }
    pub fn ptr(&self, id: Option<BlockId>) -> u64 { id.map_or(0, |i| self.position(i)) }

    /// `ResourceBuilder.GetBlocks`: every block reachable through references
    /// (parts are walked for their references but are not top-level).
    fn collect(&self, root: BlockId) -> (Vec<BlockId>, Vec<BlockId>) {
        let (mut sys, mut gfx, mut seen) = (Vec::new(), Vec::new(), HashSet::new());
        fn add(g: &Graph, id: BlockId, top: bool, sys: &mut Vec<BlockId>, gfx: &mut Vec<BlockId>, seen: &mut HashSet<BlockId>) {
            let b = &g.blocks[id.0 as usize];
            if top {
                if let Some(o) = b.owner() { return add(g, o, true, sys, gfx, seen); }
                if !seen.insert(id) { return; }
                match b.section() { Section::System => sys.push(id), Section::Graphics => gfx.push(id) }
            }
            for r in b.references(g) { add(g, r, true, sys, gfx, seen); }
            for (_, p) in b.parts() { add(g, p, false, sys, gfx, seen); }
        }
        add(self, root, true, &mut sys, &mut gfx, &mut seen);
        (sys, gfx)
    }

    fn assign_parts(&mut self, id: BlockId) {
        let base = self.positions[id.0 as usize];
        for (off, p) in self.blocks[id.0 as usize].parts() {
            self.positions[p.0 as usize] = base + off as u64;
            self.assign_parts(p);
        }
    }

    pub fn build(&mut self, root: BlockId, pages_info: BlockId, version: u32) -> Result<Vec<u8>> {
        let (sys, gfx) = self.collect(root);
        let sys_sizes: Vec<usize> = sys.iter().map(|&i| self.length(i)).collect();
        let gfx_sizes: Vec<usize> = gfx.iter().map(|&i| self.length(i)).collect();
        let sys_layout = pack_pages(&sys_sizes, true, 128)?;
        let sys_pages = rsc7_page_count(sys_layout.flags);
        let gfx_layout = pack_pages(&gfx_sizes, false, 128 - sys_pages)?;
        let gfx_pages = rsc7_page_count(gfx_layout.flags);
        {
            let pi = self.get_mut::<base::PagesInfo>(pages_info);
            pi.system_pages = sys_pages as u8;
            pi.graphics_pages = gfx_pages as u8;
        }
        for (i, &id) in sys.iter().enumerate() { self.positions[id.0 as usize] = SYSTEM_BASE + sys_layout.offsets[i] as u64; }
        for (i, &id) in gfx.iter().enumerate() { self.positions[id.0 as usize] = GRAPHICS_BASE + gfx_layout.offsets[i] as u64; }
        for &id in sys.iter().chain(&gfx) { self.assign_parts(id); }

        let mut ws = Writer::new(SYSTEM_BASE, sys_layout.size);
        let mut wg = Writer::new(GRAPHICS_BASE, gfx_layout.size);
        for (ids, w) in [(&sys, &mut ws), (&gfx, &mut wg)] {
            for &id in ids.iter() {
                let b = &self.blocks[id.0 as usize];
                let at = self.positions[id.0 as usize];
                w.seek(at);
                b.write(w, self).with_context(|| format!("writing block {}", id.0))?;
                let wrote = (w.position() - at) as usize;
                if wrote != b.length() { bail!("block {} wrote {wrote} bytes, declared {}", id.0, b.length()); }
            }
        }
        Ok(build_rsc7_with_flags(version, sys_layout.flags, &ws.into_inner(), gfx_layout.flags, &wg.into_inner()))
    }
}

/// A count written in a 16-bit field; an error naming `what` when it does not fit.
pub fn count_u16(n: usize, what: &str) -> Result<u16> {
    u16::try_from(n).map_err(|_| anyhow::anyhow!("{n} {what} exceeds 65535, the most a 16-bit count holds"))
}
/// A count written in an 8-bit field; an error naming `what` when it does not fit.
pub fn count_u8(n: usize, what: &str) -> Result<u8> {
    u8::try_from(n).map_err(|_| anyhow::anyhow!("{n} {what} exceeds 255, the most an 8-bit count holds"))
}

pub struct Writer { data: Vec<u8>, base: u64, pos: usize }

impl Writer {
    pub fn new(base: u64, size: usize) -> Self { Self { data: vec![0; size], base, pos: 0 } }
    pub fn seek(&mut self, va: u64) { self.pos = (va - self.base) as usize; }
    pub fn position(&self) -> u64 { self.base + self.pos as u64 }
    pub fn bytes(&mut self, b: &[u8]) {
        if self.pos + b.len() > self.data.len() { self.data.resize(self.pos + b.len(), 0); }
        self.data[self.pos..self.pos + b.len()].copy_from_slice(b);
        self.pos += b.len();
    }
    pub fn zeros(&mut self, n: usize) { let z = vec![0u8; n]; self.bytes(&z); }
    pub fn u8(&mut self, v: u8) { self.bytes(&[v]); }
    pub fn u16(&mut self, v: u16) { self.bytes(&v.to_le_bytes()); }
    pub fn i16(&mut self, v: i16) { self.bytes(&v.to_le_bytes()); }
    pub fn u32(&mut self, v: u32) { self.bytes(&v.to_le_bytes()); }
    pub fn u64(&mut self, v: u64) { self.bytes(&v.to_le_bytes()); }
    pub fn f32(&mut self, v: f32) { self.bytes(&v.to_le_bytes()); }
    pub fn vec3(&mut self, v: Vec3) { self.f32(v.x); self.f32(v.y); self.f32(v.z); }
    pub fn vec4(&mut self, v: Vec4) { self.f32(v.x); self.f32(v.y); self.f32(v.z); self.f32(v.w); }
    /// `writer.WritePadding(16)`: zero bytes up to the next 16-byte boundary of the virtual address.
    pub fn pad16(&mut self) { let n = (16 - (self.position() % 16)) % 16; self.zeros(n as usize); }
    pub fn into_inner(self) -> Vec<u8> { self.data }
}

/// A pooled block: its id and the type it was read as.
#[derive(Clone, Copy)]
struct Pooled { id: BlockId, ty: TypeId, name: fn() -> &'static str }

/// The name of a block type without module paths: `Texture`, `StructArray<Mat4>`.
fn short_type_name(full: &str) -> String {
    let mut out = String::with_capacity(full.len());
    let mut word = String::new();
    for c in full.chars().chain(std::iter::once('\0')) {
        if c.is_alphanumeric() || c == '_' || c == ':' { word.push(c); continue; }
        out.push_str(word.rsplit("::").next().unwrap_or(""));
        word.clear();
        if c != '\0' { out.push(c); }
    }
    out
}

/// `ResourceDataReader`: the two decoded sections plus the block pool keyed by
/// virtual address, so an address read twice yields the same [`BlockId`]. The pool
/// remembers the type each address was read as: a pointer that lands on a block of
/// another type (a corrupt or crafted file) is an error, never a block of the wrong type
/// (a plain array or vertex data there is read again on its own, see [`base::read_struct_array`]).
pub struct Reader { sys: Vec<u8>, gfx: Vec<u8>, pool: HashMap<u64, Pooled> }

impl Reader {
    pub fn open(file: &[u8]) -> Result<Reader> { let (sys, gfx) = prepare_rsc7(file)?; Ok(Reader { sys, gfx, pool: HashMap::new() }) }
    fn section(&self, va: u64) -> Result<(&[u8], u64)> {
        if va == 0 { bail!("null pointer") }
        if va >> 32 != 0 { bail!("pointer {va:#x} is in neither section") }
        match va & 0xF000_0000 {
            SYSTEM_BASE => Ok((&self.sys, SYSTEM_BASE)),
            GRAPHICS_BASE => Ok((&self.gfx, GRAPHICS_BASE)),
            _ => bail!("pointer {va:#x} is in neither section"),
        }
    }
    pub fn slice(&self, va: u64, len: usize) -> Result<&[u8]> {
        let (d, b) = self.section(va)?; let off = (va - b) as usize;
        let end = off.checked_add(len).with_context(|| format!("{len} bytes at {va:#x} overflow"))?;
        d.get(off..end).with_context(|| format!("{len} bytes at {va:#x} run past the section"))
    }
    /// A cursor from `va` to the end of its section.
    pub fn cursor(&self, va: u64) -> Result<Cursor<'_>> {
        let (d, b) = self.section(va)?; let pos = (va - b) as usize;
        if pos > d.len() { bail!("pointer {va:#x} is outside its section"); }
        Ok(Cursor { data: d, pos, base: b, overrun: false })
    }
    /// The NUL-terminated string at `va`; `None` for a null pointer.
    pub fn string(&self, va: u64) -> Result<Option<String>> {
        if va == 0 { return Ok(None); }
        let (d, b) = self.section(va)?; let off = (va - b) as usize;
        if off > d.len() { bail!("pointer {va:#x} is outside its section"); }
        let end = d[off..].iter().position(|&c| c == 0).map_or(d.len(), |n| off + n);
        Ok(Some(String::from_utf8_lossy(&d[off..end]).into_owned()))
    }
    /// The block already read at `va` as a `B`; `None` when nothing was read there, an error
    /// when the address was read as another type (a pointer into a different block).
    pub fn cached_as<B: Block>(&self, va: u64) -> Result<Option<BlockId>> {
        match self.pool.get(&va) {
            None => Ok(None),
            Some(p) if p.ty == TypeId::of::<B>() => Ok(Some(p.id)),
            Some(p) => bail!(
                "block at {va:#x} was already read as {} — a pointer to a {} points into a different block",
                short_type_name((p.name)()), short_type_name(std::any::type_name::<B>())
            ),
        }
    }
    /// Whether the block read at `va` is a `B`.
    pub fn cached_is<B: Block>(&self, va: u64) -> bool { self.pool.get(&va).is_some_and(|p| p.ty == TypeId::of::<B>()) }
    /// Records that the block at `va` was read as the `B` `id`.
    pub fn cache<B: Block>(&mut self, va: u64, id: BlockId) {
        self.pool.insert(va, Pooled { id, ty: TypeId::of::<B>(), name: std::any::type_name::<B> });
    }
    pub fn structs<T: Pod>(&self, va: u64, count: usize) -> Result<Vec<T>> {
        if va == 0 || count == 0 { return Ok(Vec::new()); }
        let len = count.checked_mul(T::SIZE).with_context(|| format!("{count} items at {va:#x} overflow"))?;
        let b = self.slice(va, len)?;
        Ok((0..count).map(|i| T::read(&b[i * T::SIZE..])).collect())
    }
    pub fn u16s(&self, va: u64, n: usize) -> Result<Vec<u16>> { self.structs(va, n) }
    pub fn i16s(&self, va: u64, n: usize) -> Result<Vec<i16>> { self.structs(va, n) }
    pub fn u32s(&self, va: u64, n: usize) -> Result<Vec<u32>> { self.structs(va, n) }
    pub fn u64s(&self, va: u64, n: usize) -> Result<Vec<u64>> { self.structs(va, n) }
    pub fn bytes(&self, va: u64, n: usize) -> Result<Vec<u8>> { self.structs(va, n) }
}

static ZEROS: [u8; 64] = [0; 64];

/// A reader over the rest of a section. Reads never panic: one that runs past
/// the end yields zeros and sets `overrun`, so block readers call [`Cursor::check`]
/// (`c.check()?`) after reading their fixed fields.
pub struct Cursor<'a> { data: &'a [u8], pub pos: usize, base: u64, overrun: bool }
impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> &'a [u8] {
        assert!(n <= ZEROS.len());
        let end = self.pos.checked_add(n).filter(|&e| e <= self.data.len());
        let s = match end { Some(e) => &self.data[self.pos..e], None => { self.overrun = true; &ZEROS[..n] } };
        self.pos = self.pos.saturating_add(n);
        s
    }
    /// An error if any read so far ran past the end of the section.
    pub fn check(&self) -> Result<()> {
        if self.overrun { bail!("read past the end of the section at {:#x}", self.va()) } else { Ok(()) }
    }
    pub fn u8(&mut self) -> u8 { self.take(1)[0] }
    pub fn u16(&mut self) -> u16 { u16::read(self.take(2)) }
    pub fn i16(&mut self) -> i16 { i16::read(self.take(2)) }
    pub fn u32(&mut self) -> u32 { u32::read(self.take(4)) }
    pub fn u64(&mut self) -> u64 { u64::read(self.take(8)) }
    pub fn f32(&mut self) -> f32 { f32::read(self.take(4)) }
    pub fn vec3(&mut self) -> Vec3 { Vec3::read(self.take(12)) }
    pub fn vec4(&mut self) -> Vec4 { Vec4::read(self.take(16)) }
    pub fn skip(&mut self, n: usize) { self.pos += n; }
    /// The virtual address the cursor is at.
    pub fn va(&self) -> u64 { self.base + self.pos as u64 }
}

pub trait Pod: Copy {
    const SIZE: usize;
    fn write(&self, w: &mut Writer);
    fn read(b: &[u8]) -> Self;
}
macro_rules! pod_num { ($t:ty, $n:expr) => { impl Pod for $t {
    const SIZE: usize = $n;
    fn write(&self, w: &mut Writer) { w.bytes(&self.to_le_bytes()); }
    fn read(b: &[u8]) -> Self { <$t>::from_le_bytes(b[..$n].try_into().unwrap()) }
} } }
pod_num!(u8, 1); pod_num!(u16, 2); pod_num!(i16, 2); pod_num!(u32, 4); pod_num!(u64, 8); pod_num!(f32, 4);
impl Pod for Vec3 { const SIZE: usize = 12; fn write(&self, w: &mut Writer) { w.vec3(*self) } fn read(b: &[u8]) -> Self { crate::resource::vec3_le(b, 0) } }
impl Pod for Vec4 { const SIZE: usize = 16; fn write(&self, w: &mut Writer) { w.vec4(*self) } fn read(b: &[u8]) -> Self { crate::resource::vec4_le(b, 0) } }
impl Pod for Mat4 { const SIZE: usize = 64;
    fn write(&self, w: &mut Writer) { for r in self.0.chunks(4) { w.vec4(Vec4::new(r[0], r[1], r[2], r[3])) } }
    fn read(b: &[u8]) -> Self { Mat4(std::array::from_fn(|i| f32::read(&b[i * 4..]))) } }

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blocks::base::{PagesInfo, StringBlock, StructArray};
    use crate::resource::{prepare_rsc7, u64_le, SYSTEM_BASE};

    /// A 32-byte root: FileBase(16) + name pointer(8) + floats pointer(8).
    struct Root { pages: BlockId, name: BlockId, floats: BlockId }
    impl Block for Root {
        fn length(&self) -> usize { 32 }
        fn references(&self, _g: &Graph) -> Vec<BlockId> { vec![self.pages, self.name, self.floats] }
        fn write(&self, w: &mut Writer, g: &Graph) -> anyhow::Result<()> {
            base::write_file_base(w, g, 0x1234, Some(self.pages));
            w.u64(g.position(self.name));
            w.u64(g.position(self.floats));
            Ok(())
        }
        fn as_any(&self) -> &dyn std::any::Any { self }
        fn as_any_mut(&mut self) -> &mut dyn std::any::Any { self }
    }

    #[test]
    fn builds_a_resource_with_resolved_pointers() {
        let mut g = Graph::new();
        let pages = g.add(PagesInfo::default());
        let name = g.add(StringBlock("hello".into()));
        let floats = g.add(StructArray { items: vec![1.0f32, 2.0] });
        let root = g.add(Root { pages, name, floats });
        let file = g.build(root, pages, 7).unwrap();
        let (sys, gfx) = prepare_rsc7(&file).unwrap();
        assert!(gfx.is_empty());
        assert_eq!(&sys[0..4], &0x1234u32.to_le_bytes());
        let name_va = u64_le(&sys, 16);
        let off = (name_va - SYSTEM_BASE) as usize;
        assert_eq!(&sys[off..off + 6], b"hello\0");
        let floats_va = u64_le(&sys, 24);
        let off = (floats_va - SYSTEM_BASE) as usize;
        assert_eq!(f32::from_le_bytes(sys[off + 4..off + 8].try_into().unwrap()), 2.0);
        // pages info was shrunk to the real counts
        let pi = (u64_le(&sys, 8) - SYSTEM_BASE) as usize;
        assert_eq!(sys[pi + 8], 1); // system pages
        assert_eq!(sys[pi + 9], 0); // graphics pages
        assert_eq!(g.position(root), SYSTEM_BASE);
    }

    #[test]
    fn reads_back_what_it_built_sharing_blocks_by_address() {
        let mut g = Graph::new();
        let pages = g.add(PagesInfo::default());
        let name = g.add(StringBlock("hello".into()));
        let floats = g.add(StructArray { items: vec![1.0f32, 2.0] });
        let root = g.add(Root { pages, name, floats });
        let file = g.build(root, pages, 7).unwrap();

        let mut r = Reader::open(&file).unwrap();
        let mut g2 = Graph::new();
        let mut c = r.cursor(SYSTEM_BASE).unwrap();
        assert_eq!(c.u32(), 0x1234); assert_eq!(c.u32(), 1);
        let pages_va = c.u64(); let name_va = c.u64(); let floats_va = c.u64();
        let pi = base::read_pages_info(&mut r, &mut g2, pages_va).unwrap().unwrap();
        assert_eq!(g2.get::<PagesInfo>(pi).system_pages, 1);
        assert_eq!(g2.get::<PagesInfo>(pi).graphics_pages, 0);
        let s1 = base::read_string_block(&mut r, &mut g2, name_va).unwrap().unwrap();
        let s2 = base::read_string_block(&mut r, &mut g2, name_va).unwrap().unwrap();
        assert_eq!(s1, s2, "the pool returns one block per address");
        assert_eq!(g2.get::<StringBlock>(s1).0, "hello");
        let fa = base::read_struct_array::<f32>(&mut r, &mut g2, floats_va, 2).unwrap().unwrap();
        assert_eq!(g2.get::<StructArray<f32>>(fa).items, vec![1.0, 2.0]);
        assert!(base::read_string_block(&mut r, &mut g2, 0).unwrap().is_none());
    }

    #[test]
    fn bad_pointers_are_errors_not_panics() {
        let mut g = Graph::new();
        let pages = g.add(PagesInfo::default());
        let name = g.add(StringBlock("hello".into()));
        let floats = g.add(StructArray { items: vec![1.0f32, 2.0] });
        let root = g.add(Root { pages, name, floats });
        let file = g.build(root, pages, 7).unwrap();
        let r = Reader::open(&file).unwrap();
        let (sys, _) = prepare_rsc7(&file).unwrap();

        assert!(r.slice(0, 4).unwrap_err().to_string().contains("null pointer"));
        assert!(r.slice(0x7000_0000, 4).is_err());
        assert!(r.slice(0x1_5000_0000, 4).is_err());
        assert!(r.string(0x7000_0000).is_err());
        assert!(r.cursor(SYSTEM_BASE + sys.len() as u64 + 16).is_err());
        assert!(r.string(SYSTEM_BASE + sys.len() as u64 + 16).is_err());
        assert!(r.slice(SYSTEM_BASE, usize::MAX).is_err());
        assert!(r.structs::<u64>(SYSTEM_BASE, usize::MAX / 4).is_err());

        let mut c = r.cursor(SYSTEM_BASE + sys.len() as u64 - 2).unwrap();
        assert!(c.check().is_ok());
        c.u64();
        assert!(c.check().unwrap_err().to_string().contains("read past the end"));
    }

    /// The triangle fixture built, with the root's pointer at `to` copied over the one at `from`
    /// (offsets into the system section).
    pub(crate) fn triangle_with_pointer_copied(from: usize, to: usize) -> Vec<u8> {
        let file = ydr::build_ydr_from_xml(include_str!("../../tests/fixtures/one_triangle.ydr.xml"), None).unwrap();
        let (mut sys, gfx) = prepare_rsc7(&file).unwrap();
        let flags = |at: usize| u32::from_le_bytes(file[at..at + 4].try_into().unwrap());
        let ptr = sys[from..from + 8].to_vec();
        sys[to..to + 8].copy_from_slice(&ptr);
        crate::resource::build_rsc7_with_flags(flags(4), flags(8), &sys, flags(12), &gfx)
    }

    #[test]
    fn a_pointer_into_a_block_of_another_type_is_an_error_not_a_panic() {
        // the skeleton pointer (0x18) made equal to the shader group pointer (0x10)
        let file = triangle_with_pointer_copied(0x10, 0x18);
        let err = match std::panic::catch_unwind(|| ydr::read_ydr(&file)) {
            Ok(r) => r.err().expect("a skeleton pointing at the shader group is refused"),
            Err(_) => panic!("read_ydr panicked"),
        };
        let msg = format!("{err:#}");
        assert!(msg.contains("already read as ShaderGroup") && msg.contains("Skeleton"), "{msg}");
        assert!(ydr::dump_ydr_xml(&file, &crate::names::NameTable::core(), None).is_err());
    }

    #[test]
    fn the_pool_checks_the_type_an_address_was_read_as() {
        let mut g = Graph::new();
        let pages = g.add(PagesInfo::default());
        let name = g.add(StringBlock("hello".into()));
        let floats = g.add(StructArray { items: vec![1.0f32, 2.0] });
        let root = g.add(Root { pages, name, floats });
        let file = g.build(root, pages, 7).unwrap();
        let mut r = Reader::open(&file).unwrap();
        let mut g2 = Graph::new();
        let s = base::read_string_block(&mut r, &mut g2, SYSTEM_BASE + 64).unwrap().unwrap();
        assert_eq!(r.cached_as::<StringBlock>(SYSTEM_BASE + 64).unwrap(), Some(s));
        assert!(r.cached_is::<StringBlock>(SYSTEM_BASE + 64) && !r.cached_is::<PagesInfo>(SYSTEM_BASE + 64));
        let err = r.cached_as::<StructArray<Mat4>>(SYSTEM_BASE + 64).unwrap_err().to_string();
        assert!(err.contains("already read as StringBlock — a pointer to a StructArray<Mat4> points into a different block"), "{err}");
        assert!(base::read_pages_info(&mut r, &mut g2, SYSTEM_BASE + 64).is_err(), "a typed block is refused");
        // a plain array at that address is read on its own, unpooled, as `ResourceDataReader.ReadBlock` does
        let a = base::read_struct_array::<u16>(&mut r, &mut g2, SYSTEM_BASE + 64, 1).unwrap().unwrap();
        assert_ne!(a, s);
        assert_eq!(g2.get::<StructArray<u16>>(a).items.len(), 1);
        assert!(r.cached_is::<StringBlock>(SYSTEM_BASE + 64));
        assert_eq!(r.cached_as::<PagesInfo>(SYSTEM_BASE + 8).unwrap(), None);
    }

    #[test]
    fn a_block_writing_the_wrong_length_is_an_error() {
        struct Bad;
        impl Block for Bad {
            fn length(&self) -> usize { 8 }
            fn write(&self, w: &mut Writer, _g: &Graph) -> anyhow::Result<()> { w.u32(1); Ok(()) }
            fn as_any(&self) -> &dyn std::any::Any { self }
            fn as_any_mut(&mut self) -> &mut dyn std::any::Any { self }
        }
        let mut g = Graph::new();
        let pages = g.add(PagesInfo::default());
        let bad = g.add(Bad);
        assert!(g.build(bad, pages, 1).unwrap_err().to_string().contains("wrote 4 bytes, declared 8"));
    }
}
