//! A fragment (`.yft`) read into a block graph and written back (resource version 162).
//!
//! The drawables (`fragDrawable`, 0x150 bytes) are typed blocks, so their meshes, shaders and textures can be
//! edited. Everything else in the system section (the `FragType` header, the physics LOD group and LOD, the
//! archetype, bounds, articulated body, groups, children, transforms, names and cloth) is **not decoded**: it is
//! carried as [`Chunk`]s, byte runs between the typed blocks with every pointer found in them replaced by a
//! link (a block and an offset into it) that the writer resolves again when the file is laid out. Nothing in a
//! chunk is zeroed or invented, so what the game gets for those structures is what the retail file had, moved.
//! That is only sound for pointers: a structure that holds an offset or a size computed from another block's
//! address would break, and a 64-bit word that is not a pointer but looks like one is refused, not guessed at.
//!
//! Chunk boundaries: a chunk starts at a block that something points at and runs to the next typed block, so
//! blocks that were next to each other stay next to each other.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{bail, Context, Result};

use super::base::{read_pages_info, PagesInfo};
use super::drawable::{Drawable, FRAG_DRAWABLE_SIZE, FRAG_NAME_SLOT};
use super::*;
use crate::resource::SYSTEM_BASE;

/// A run of the system section kept as bytes. `links` are (offset, target, offset into the target); the 8 bytes at
/// each offset are zero in `data`. A chunk that was stored directly after another one has the [`Run`] that
/// keeps them together as `owner`.
pub struct Chunk { pub data: Vec<u8>, pub links: Vec<(usize, BlockId, u32)>, pub owner: Option<BlockId> }

impl Block for Chunk {
    fn length(&self) -> usize { self.data.len() }
    fn references(&self, _g: &Graph) -> Vec<BlockId> { self.links.iter().map(|l| l.1).collect() }
    fn owner(&self) -> Option<BlockId> { self.owner }
    fn write(&self, w: &mut Writer, g: &Graph) -> Result<()> {
        let mut data = self.data.clone();
        for &(at, to, add) in &self.links {
            data.get_mut(at..at + 8).context("a link past the end of its chunk")?
                .copy_from_slice(&(g.position(to) + u64::from(add)).to_le_bytes());
        }
        w.bytes(&data);
        Ok(())
    }
    fn as_any(&self) -> &dyn Any { self }
    fn as_any_mut(&mut self) -> &mut dyn Any { self }
}

/// Chunks laid out one directly after the other, as they were in the file. Each member must be a multiple of 16
/// bytes long so the next starts aligned.
pub struct Run { pub members: Vec<BlockId>, lengths: Vec<usize> }

impl Run {
    /// A run over `members` (already in `g`); `g` is told they belong to it.
    pub fn new(g: &mut Graph, members: Vec<BlockId>) -> BlockId {
        let lengths = members.iter().map(|&m| g.length(m)).collect();
        let id = g.add(Run { members: members.clone(), lengths });
        for m in members { g.get_mut::<Chunk>(m).owner = Some(id); }
        id
    }
}
impl Block for Run {
    fn length(&self) -> usize { self.lengths.iter().sum() }
    fn parts(&self) -> Vec<(usize, BlockId)> {
        let mut at = 0;
        self.members.iter().zip(&self.lengths).map(|(&m, &l)| { let p = (at, m); at += l; p }).collect()
    }
    fn write(&self, w: &mut Writer, g: &Graph) -> Result<()> {
        for &m in &self.members { g.get::<Chunk>(m).write(w, g)?; }
        Ok(())
    }
    fn as_any(&self) -> &dyn Any { self }
    fn as_any_mut(&mut self) -> &mut dyn Any { self }
}

fn u64_at(b: &[u8], o: usize) -> u64 { u64::from_le_bytes(b[o..o + 8].try_into().unwrap()) }
fn u32_at(b: &[u8], o: usize) -> u32 { u32::from_le_bytes(b[o..o + 4].try_into().unwrap()) }

/// A `.yft` read: the typed drawables and the chunks around them.
pub struct ReadFrag {
    pub r: Reader,
    pub g: Graph,
    pub pages: BlockId,
    /// The chunk at the start of the system section, which holds the `FragType` header.
    pub root: BlockId,
    /// Every `fragDrawable`: its address in the file and its block.
    pub drawables: Vec<(u64, BlockId)>,
    /// (start, end) in the system section, and the block, in address order; the first is the root.
    pub chunks: Vec<(usize, usize, BlockId)>,
}

/// The physics LOD's children (addresses of the `FragPhysTypeChild` structs), `None` without a physics LOD.
pub fn lod_children(r: &Reader, header: &[u8]) -> Result<Option<Vec<u64>>> {
    let group = u64_at(header, 0xF0);
    if group == 0 { return Ok(None); }
    let lod = u64_at(r.slice(group, 0x30)?, 0x10);
    if lod == 0 { return Ok(None); }
    let lod = r.slice(lod, 0x130)?;
    Ok(Some(r.u64s(u64_at(lod, 0xD0), lod[0x11D] as usize)?))
}

/// The address `v` read from a chunk as an offset into the system section, if it is one.
fn sys_offset(v: u64, sys_len: usize) -> Option<usize> {
    (v >> 32 == 0 && (SYSTEM_BASE..SYSTEM_BASE + sys_len as u64).contains(&v)).then(|| (v - SYSTEM_BASE) as usize)
}

/// The chunks for `targets`: each runs from a target to the next target or typed block (or the end of the section).
/// Cutting at every target keeps the cut the same wherever the writer puts the chunks.
fn group(targets: &BTreeSet<usize>, typed: &BTreeMap<usize, BlockId>, sys_len: usize) -> Vec<(usize, usize)> {
    let starts: Vec<usize> = targets.iter().copied().filter(|t| !typed.contains_key(t)).collect();
    starts.iter().enumerate().map(|(i, &t)| {
        let next_target = starts.get(i + 1).copied().unwrap_or(sys_len);
        let next_typed = typed.range(t + 1..).next().map_or(sys_len, |(&s, _)| s);
        (t, next_target.min(next_typed))
    }).collect()
}

/// A `fragDrawable` at `t`? It has `1` after its vftable, the file's pages pointer and a name.
fn looks_like_frag_drawable(sys: &[u8], t: usize, pages_va: u64) -> bool {
    t != 0 && t % 16 == 0 && t + FRAG_DRAWABLE_SIZE <= sys.len()
        && (0x4000_0000..0x4100_0000).contains(&u32_at(sys, t)) && u32_at(sys, t + 4) == 1
        && u64_at(sys, t + 8) == pages_va && u64_at(sys, t + 0x130) != 0
}

/// Reads the typed blocks (drawables) and cuts the rest of the system section into chunks.
pub fn read_fragment(file: &[u8]) -> Result<ReadFrag> {
    let mut r = Reader::open(file)?;
    let mut g = Graph::new();
    let sys_len = r.sys.len();
    if sys_len < 0x130 { bail!("the system section is too small for a fragment"); }
    let header = r.sys[..0x130].to_vec();
    let pages_va = u64_at(&header, 8);
    let pages = read_pages_info(&mut r, &mut g, pages_va)?.context("the fragment has no pages info")?;
    let mut drawables: Vec<(u64, BlockId)> = Vec::new();
    fn read_drawable(r: &mut Reader, g: &mut Graph, drawables: &mut Vec<(u64, BlockId)>, va: u64, what: &str) -> Result<()> {
        if va == 0 || drawables.iter().any(|d| d.0 == va) { return Ok(()); }
        let id = Drawable::read_frag(r, g, va).with_context(|| format!("the {what} drawable at {va:#x}"))?.context("null drawable")?;
        drawables.push((va, id));
        Ok(())
    }
    read_drawable(&mut r, &mut g, &mut drawables, u64_at(&header, 0x30), "main")?;
    let (array, count) = (u64_at(&header, 0x38), u32_at(&header, 0x48) as usize);
    if array != 0 { for va in r.u64s(array, count)? { read_drawable(&mut r, &mut g, &mut drawables, va, "extra")?; } }
    if let Some(children) = lod_children(&r, &header).context("the physics LOD")? {
        for (i, va) in children.into_iter().enumerate() {
            let child = r.slice(va, 0x100).with_context(|| format!("physics child {i}"))?.to_vec();
            read_drawable(&mut r, &mut g, &mut drawables, u64_at(&child, 0xA0), "child")?;
            read_drawable(&mut r, &mut g, &mut drawables, u64_at(&child, 0xA8), "damaged child")?;
        }
    }

    // Targets of pointers found in chunks are chunk starts; a drawable found that way is read as one and the cut
    // is made again, until nothing new turns up.
    let (chunks, typed) = loop {
        let typed: BTreeMap<usize, BlockId> = r.pool.iter()
            .filter_map(|(&va, p)| sys_offset(va, sys_len).map(|o| (o, p.id))).collect();
        let ranges: Vec<(usize, usize)> = typed.iter().map(|(&s, &id)| (s, s + if id == pages { 16 } else { g.length(id) })).collect();
        let inside_typed = |t: usize| ranges.iter().any(|&(s, e)| t > s && t < e);
        let which = |t: usize| ranges.iter().find(|&&(s, e)| t > s && t < e).map(|&(s, e)| format!("{s:#x}..{e:#x}, {}", typed.get(&s).map_or("?".into(), |&i| type_name(&g, i)))).unwrap_or_default();
        let mut targets: BTreeSet<usize> = BTreeSet::from([0]);
        let consider = |v: u64, targets: &mut BTreeSet<usize>, from: &str| -> Result<()> {
            let Some(t) = sys_offset(v, sys_len) else { return Ok(()) };
            if inside_typed(t) { bail!("{from} points to {v:#x}, inside a typed block ({})", which(t)); }
            let a = t & !15;
            if !typed.contains_key(&a) { targets.insert(a); }
            Ok(())
        };
        for &(va, id) in &drawables {
            let raw = &g.get::<Drawable>(id).frag.as_ref().context("a fragDrawable without its tail")?.raw;
            for o in (0..raw.len() - 7).step_by(8).filter(|&o| o != FRAG_NAME_SLOT) {
                consider(u64_at(raw, o), &mut targets, &format!("the drawable at {va:#x}"))?;
            }
        }
        let mut chunks;
        loop {
            chunks = group(&targets, &typed, sys_len);
            let before = targets.len();
            for &(s, e) in &chunks {
                for o in (s..e.saturating_sub(7)).step_by(8) {
                    consider(u64_at(&r.sys, o), &mut targets, &format!("the data at {:#x}", SYSTEM_BASE + o as u64))?;
                }
            }
            if targets.len() == before { break; }
        }
        let known: BTreeSet<u64> = drawables.iter().map(|d| d.0).collect();
        let found: Vec<usize> = targets.iter().copied()
            .filter(|&t| !typed.contains_key(&t) && !known.contains(&(SYSTEM_BASE + t as u64)) && looks_like_frag_drawable(&r.sys, t, pages_va))
            .collect();
        if found.is_empty() { break (chunks, typed); }
        for t in found { read_drawable(&mut r, &mut g, &mut drawables, SYSTEM_BASE + t as u64, "other")?; }
    };

    // The chunk blocks first, so links can name them.
    let ids: Vec<BlockId> = chunks.iter().map(|&(s, e)| g.add(Chunk { data: r.sys[s..e].to_vec(), links: Vec::new(), owner: None })).collect();
    let gfx_len = r.gfx.len() as u64;
    let type_names: BTreeMap<u32, String> = typed.values().map(|&i| (i.0, type_name(&g, i))).collect();
    let shared_ids: BTreeSet<u32> = drawables.iter().flat_map(|d| { let d = g.get::<Drawable>(d.1); [d.shader_group, d.skeleton, d.joints] }).flatten().map(|b| b.0).collect();
    let drawable_ids: BTreeSet<u32> = drawables.iter().map(|d| d.1.0).collect();
    let resolve = |v: u64, from: &str| -> Result<(BlockId, u32)> {
        if v >> 32 == 0 && (0x6000_0000..0x6000_0000 + gfx_len).contains(&v) {
            bail!("{from} points into the graphics section ({v:#x}), which fragment chunks do not support");
        }
        let t = sys_offset(v, sys_len).with_context(|| format!("{from} holds {v:#x}"))?;
        if let Some(&id) = typed.get(&t) {
            let ok = id == pages || drawable_ids.contains(&id.0) || shared_ids.contains(&id.0);
            if !ok { bail!("{from} points to {v:#x}, a {} (only the pages info, drawables and their shader groups, skeletons and joints can be linked to)", type_names[&id.0]); }
            return Ok((id, 0));
        }
        let at = chunks.iter().position(|&(s, e)| t >= s && t < e)
            .with_context(|| format!("{from} points to {v:#x}, which is in no chunk"))?;
        Ok((ids[at], (t - chunks[at].0) as u32))
    };
    for (n, &(s, e)) in chunks.iter().enumerate() {
        let mut links = Vec::new();
        let mut data = r.sys[s..e].to_vec();
        for o in (0..data.len().saturating_sub(7)).step_by(8) {
            let v = u64_at(&data, o);
            if sys_offset(v, sys_len).is_none() && !(v >> 32 == 0 && (0x6000_0000..0x6000_0000 + gfx_len).contains(&v)) { continue; }
            let (id, add) = resolve(v, &format!("the data at {:#x}", SYSTEM_BASE + (s + o) as u64))?;
            links.push((o, id, add));
            data[o..o + 8].fill(0);
        }
        let c = g.get_mut::<Chunk>(ids[n]);
        c.data = data;
        c.links = links;
    }
    for &(va, id) in &drawables {
        let mut raw = g.get::<Drawable>(id).frag.as_ref().unwrap().raw.clone();
        let mut links = Vec::new();
        raw[FRAG_NAME_SLOT..FRAG_NAME_SLOT + 8].fill(0);
        for o in (0..raw.len() - 7).step_by(8) {
            let v = u64_at(&raw, o);
            if sys_offset(v, sys_len).is_none() && !(v >> 32 == 0 && (0x6000_0000..0x6000_0000 + gfx_len).contains(&v)) { continue; }
            let (to, add) = resolve(v, &format!("the drawable at {va:#x} (+{:#x})", 0xA8 + o))?;
            links.push((o, to, add));
            raw[o..o + 8].fill(0);
        }
        let tail = g.get_mut::<Drawable>(id).frag.as_mut().unwrap();
        tail.raw = raw;
        tail.links = links;
    }
    let root = ids[0];
    let chunks = chunks.into_iter().zip(ids).map(|((s, e), id)| (s, e, id)).collect();
    Ok(ReadFrag { r, g, pages, root, drawables, chunks })
}

/// Lays a graph of [`Chunk`]s and fragDrawables out as a `.yft` (resource version 162).
pub fn write_fragment(g: &mut Graph, root: BlockId, pages: BlockId) -> Result<Vec<u8>> {
    g.get::<PagesInfo>(pages);
    g.build(root, pages, 162)
}

/// The name of a typed block's type, for messages.
fn type_name(g: &Graph, id: BlockId) -> String {
    use super::{base::StringBlock, drawable::*, shader::ShaderGroup, skeleton::{Joints, Skeleton}};
    macro_rules! names { ($($t:ty),*) => { $( if g.try_get::<$t>(id).is_some() { return stringify!($t).into(); } )* } }
    names!(Drawable, ShaderGroup, Skeleton, Joints, DrawableModelsBlock, DrawableModel, DrawableGeometry, PagesInfo, StringBlock);
    "another block".into()
}
