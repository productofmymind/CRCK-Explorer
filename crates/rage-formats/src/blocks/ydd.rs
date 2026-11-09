//! `.ydd` entry points, ported from the reference tool's `DrawableDictionary` (`Drawable.cs`): read a drawable
//! dictionary into a block graph and write one back (resource version 165), plus the helpers to list and replace
//! the textures embedded in its drawables' shader groups.
//!
//! The dictionary is 64 bytes: `ResourceFileBase`, two words kept as read, the name-hash list and the
//! drawable pointer list. Each drawable is the plain 208-byte [`Drawable`] a `.ydr` holds, so everything that
//! hangs off one (shader group, embedded texture dictionary, skeleton, joints, models, lights, bound) is the
//! same typed block the `.ydr` writer lays out. Blocks shared between drawables (one shader group or skeleton
//! used by several entries) are read once and stay shared.
//!
//! Only the legacy PC layout is handled: a Gen9 (Enhanced) `.ydd` is refused, not converted.

use std::collections::HashSet;

use anyhow::{bail, Context, Result};

use super::base::{read_pages_info, read_pointer_array64, read_simple_list64, read_struct_array, write_file_base, write_pointer_list64, write_simple_list64, PointerArray64, StringBlock, StructArray};
use super::bounds::BoundBlock;
use super::drawable::Drawable;
use super::shader::ShaderGroup;
use super::texture::{Texture, TextureData, TextureDictionary};
use super::*;
use crate::gen9::Edition;
use crate::resource::SYSTEM_BASE;
use crate::ytd::YtdTexture;

/// The resource version of a legacy `.ydd`.
pub const YDD_VERSION: u32 = 165;

/// `DrawableDictionary` (64 bytes).
pub struct DrawableDictionary {
    pub vft: u32,
    pub pages: Option<BlockId>,
    /// `Unknown_10h` and `Unknown_18h` (0 and 1 in the files we have seen), kept as read.
    pub unknown_10h: u64,
    pub unknown_18h: u64,
    /// A `StructArray<u32>`: the name hash of each drawable, in the order of `drawables`.
    pub hashes: Option<BlockId>,
    /// A `PointerArray64` of [`Drawable`].
    pub drawables: Option<BlockId>,
}

impl DrawableDictionary {
    pub fn read(r: &mut Reader, g: &mut Graph, va: u64) -> Result<Option<BlockId>> {
        if va == 0 { return Ok(None); }
        if let Some(id) = r.cached_as::<Self>(va)? { return Ok(Some(id)); }
        let mut c = r.cursor(va)?;
        let vft = c.u32(); c.skip(4);
        let pages_ptr = c.u64();
        let unknown_10h = c.u64();
        let unknown_18h = c.u64();
        let (hashes_ptr, hashes_count, _) = read_simple_list64(&mut c);
        let (drawables_ptr, drawables_count, _) = read_simple_list64(&mut c);
        c.check()?;
        let pages = read_pages_info(r, g, pages_ptr)?;
        let hashes = read_struct_array::<u32>(r, g, hashes_ptr, hashes_count as usize)?;
        let drawables = if drawables_ptr == 0 {
            None
        } else {
            let ptrs = read_pointer_array64(r, g, drawables_ptr, drawables_count as usize)?;
            let mut items = Vec::with_capacity(ptrs.len());
            for (i, p) in ptrs.into_iter().enumerate() {
                items.push(Drawable::read(r, g, p).with_context(|| format!("drawable {i} of the dictionary"))?);
            }
            Some(g.add(PointerArray64 { items }))
        };
        let id = g.add(DrawableDictionary { vft, pages, unknown_10h, unknown_18h, hashes, drawables });
        r.cache::<Self>(va, id); Ok(Some(id))
    }

    /// The name hashes, in drawable order.
    pub fn hash_list<'a>(&self, g: &'a Graph) -> &'a [u32] {
        self.hashes.map_or(&[][..], |h| g.get::<StructArray<u32>>(h).items.as_slice())
    }

    /// The drawable slots, in file order (`None` for a null pointer).
    pub fn drawable_list(&self, g: &Graph) -> Vec<Option<BlockId>> {
        self.drawables.map_or_else(Vec::new, |d| g.get::<PointerArray64>(d).items.clone())
    }
}

impl Block for DrawableDictionary {
    fn length(&self) -> usize { 64 }
    fn references(&self, _g: &Graph) -> Vec<BlockId> { self.pages.into_iter().chain(self.hashes).chain(self.drawables).collect() }
    fn write(&self, w: &mut Writer, g: &Graph) -> Result<()> {
        write_file_base(w, g, self.vft, self.pages);
        w.u64(self.unknown_10h);
        w.u64(self.unknown_18h);
        let hashes = self.hash_list(g).len();
        let drawables = self.drawables.map_or(0, |d| g.get::<PointerArray64>(d).items.len());
        write_simple_list64(w, g, self.hashes, hashes, "drawable hashes")?;
        write_pointer_list64(w, g, self.drawables, drawables, "drawables")?;
        Ok(())
    }
    fn as_any(&self) -> &dyn Any { self } fn as_any_mut(&mut self) -> &mut dyn Any { self }
}

/// Reads a legacy `.ydd` into a graph; returns it with the [`DrawableDictionary`] root.
pub fn read_ydd(file: &[u8]) -> Result<(Graph, BlockId)> {
    match Edition::of_file("ydd", file)? {
        Some(Edition::Legacy) => {}
        Some(Edition::Gen9) => bail!("this .ydd is a Gen9 (Enhanced) resource; only legacy (version {YDD_VERSION}) drawable dictionaries can be rewritten"),
        None => bail!("not a drawable dictionary of a known version (legacy .ydd files are version {YDD_VERSION})"),
    }
    let mut r = Reader::open(file)?;
    let mut g = Graph::new();
    let root = DrawableDictionary::read(&mut r, &mut g, SYSTEM_BASE)?.context("the resource has no root block")?;
    if g.get::<DrawableDictionary>(root).hash_list(&g).len() != g.get::<DrawableDictionary>(root).drawable_list(&g).len() {
        bail!("the dictionary's hash list and drawable list have different lengths");
    }
    Ok((g, root))
}

/// Lays the graph out and writes it as a `.ydd` (resource version 165).
pub fn write_ydd(g: &mut Graph, root: BlockId) -> Result<Vec<u8>> {
    let (pages, drawables) = {
        let d = g.get::<DrawableDictionary>(root);
        (d.pages.context("the dictionary has no pages info")?, d.drawable_list(g))
    };
    let bounds: Vec<BlockId> = drawables.into_iter().flatten().filter_map(|d| g.get::<Drawable>(d).bound).collect();
    for b in bounds { BoundBlock::prepare_tree(g, b); }
    g.build(root, pages, YDD_VERSION)
}

/// One texture embedded in a drawable of a dictionary.
#[derive(Debug, Clone)]
pub struct EmbeddedTexture {
    /// Index of the first drawable whose shader group holds the texture.
    pub drawable_index: usize,
    /// That drawable's name hash, from the dictionary's hash list.
    pub drawable_hash: u32,
    /// That drawable's own name (often the resource's name rather than the entry's).
    pub drawable_name: String,
    /// The [`Texture`] block.
    pub texture: BlockId,
    /// The stride in the file's texture header; it differs from [`texture_of`]'s when the stored one was wrong for
    /// the format and the pixels were read by the right one ([`crate::ytd::layout_stride`]).
    pub stored_stride: u16,
}

/// Every texture embedded in the dictionary's drawables, in drawable order then dictionary order. A texture
/// reached through a shader group or texture dictionary that several drawables share is listed once.
pub fn embedded_textures(g: &Graph, root: BlockId) -> Vec<EmbeddedTexture> {
    let dict = g.get::<DrawableDictionary>(root);
    let hashes = dict.hash_list(g);
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for (i, d) in dict.drawable_list(g).into_iter().enumerate() {
        let Some(d) = d else { continue };
        push_drawable_textures(g, d, i, hashes.get(i).copied().unwrap_or(0), &mut seen, &mut out);
    }
    out
}

/// The textures embedded in one [`Drawable`] (a `.ydr`'s root), as [`embedded_textures`] lists them (index 0, hash 0).
pub fn drawable_embedded_textures(g: &Graph, drawable: BlockId) -> Vec<EmbeddedTexture> {
    let mut out = Vec::new();
    push_drawable_textures(g, drawable, 0, 0, &mut HashSet::new(), &mut out);
    out
}

fn push_drawable_textures(g: &Graph, d: BlockId, index: usize, hash: u32, seen: &mut HashSet<BlockId>, out: &mut Vec<EmbeddedTexture>) {
    let drawable = g.get::<Drawable>(d);
    let Some(td) = drawable.shader_group.and_then(|sg| g.get::<ShaderGroup>(sg).dictionary) else { return };
    let Some(textures) = g.get::<TextureDictionary>(td).textures else { return };
    let name = drawable.name.map_or_else(String::new, |n| g.get::<StringBlock>(n).0.clone());
    for t in g.get::<PointerArray64>(textures).items.iter().flatten() {
        if !seen.insert(*t) { continue; }
        out.push(EmbeddedTexture { drawable_index: index, drawable_hash: hash, drawable_name: name.clone(), texture: *t, stored_stride: g.get::<Texture>(*t).stride });
    }
}

/// The embedded texture `id` as a decoded-texture value (name, size, format, levels, stride, pixels).
pub fn texture_of(g: &Graph, id: BlockId) -> YtdTexture { g.get::<Texture>(id).to_ytd_texture(g) }

/// Puts `t`'s size, format, mip chain and pixels (already in the game's layout) into the embedded texture `id`.
/// Its name, name hash, usage, usage flags and extra flags stay, so the dictionary's order and every shader
/// parameter that points at it are unchanged. The pixels go into a new data block, never one another texture
/// might share.
pub fn replace_texture(g: &mut Graph, id: BlockId, t: &YtdTexture) -> Result<()> {
    if t.width == 0 || t.height == 0 { bail!("a {}x{} texture cannot replace an embedded one", t.width, t.height); }
    let data = if t.pixel_data.is_empty() { None } else { Some(g.add(TextureData { bytes: t.pixel_data.clone() })) };
    let tex = g.get_mut::<Texture>(id);
    tex.width = t.width;
    tex.height = t.height;
    tex.depth = t.depth.max(1);
    tex.stride = t.stride;
    tex.format = t.format;
    tex.levels = t.levels;
    tex.data = data;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blocks::base::PagesInfo;
    use crate::resource::{build_rsc7_with_flags, prepare_rsc7};
    use crate::texture_encode::{encode_texture, EncodeFormat};
    use crate::ytd::TextureFormat;

    fn normal_map(w: u32, h: u32) -> Vec<u8> {
        (0..w * h).flat_map(|i| [((i % w) * 255 / w) as u8, ((i / w) * 255 / h) as u8, 255, 255]).collect()
    }

    /// The triangle fixture's drawable with a 64x64 ATI2 texture embedded, as the one entry of a dictionary.
    fn dictionary_with_ati2() -> Vec<u8> {
        let ydr = crate::build_ydr_from_xml(include_str!("../../tests/fixtures/one_triangle.ydr.xml"), None).unwrap();
        let (mut g, d) = crate::read_ydr(&ydr).unwrap();
        let t = encode_texture("tri_prop_n", &normal_map(64, 64), 64, 64, EncodeFormat::Bc5, None).unwrap();
        let tex = Texture::from_ytd_texture(&mut g, &t, 22, 0, 0, 0);
        let td = TextureDictionary::from_textures(&mut g, vec![tex]);
        let sg = g.get::<Drawable>(d).shader_group.unwrap();
        g.get_mut::<ShaderGroup>(sg).dictionary = Some(td);
        g.get_mut::<Drawable>(d).pages = None;
        let pages = g.add(PagesInfo::default());
        let hashes = g.add(StructArray { items: vec![0x1234_5678u32] });
        let drawables = g.add(PointerArray64 { items: vec![Some(d)] });
        let root = g.add(DrawableDictionary { vft: 0, pages: Some(pages), unknown_10h: 0, unknown_18h: 1, hashes: Some(hashes), drawables: Some(drawables) });
        write_ydd(&mut g, root).unwrap()
    }

    #[test]
    fn a_written_dictionary_reads_back_with_both_readers() {
        let file = dictionary_with_ati2();
        let (g, root) = read_ydd(&file).unwrap();
        assert_eq!(g.get::<DrawableDictionary>(root).hash_list(&g), &[0x1234_5678]);
        let list = embedded_textures(&g, root);
        assert_eq!(list.len(), 1);
        let t = texture_of(&g, list[0].texture);
        assert_eq!((t.name.as_str(), t.width, t.height, t.format), ("tri_prop_n", 64, 64, TextureFormat::ATI2));
        assert_eq!(list[0].drawable_hash, 0x1234_5678);

        let old = crate::parse_ydd(&file).unwrap();
        assert_eq!(old.len(), 1);
        assert_eq!(old[0].hash, 0x1234_5678);
        let textures = &old[0].drawable.shader_group.as_ref().unwrap().textures;
        assert_eq!((textures[0].width, textures[0].format), (64, TextureFormat::ATI2));
    }

    #[test]
    fn a_replaced_texture_is_written_and_everything_else_stays() {
        let file = dictionary_with_ati2();
        let (mut g, root) = read_ydd(&file).unwrap();
        let id = embedded_textures(&g, root)[0].texture;
        let small = encode_texture("tri_prop_n", &normal_map(32, 32), 32, 32, EncodeFormat::Bc5, None).unwrap();
        replace_texture(&mut g, id, &small).unwrap();
        let out = write_ydd(&mut g, root).unwrap();
        assert!(out.len() <= file.len(), "a smaller texture does not grow the file");

        let (g2, root2) = read_ydd(&out).unwrap();
        let t = texture_of(&g2, embedded_textures(&g2, root2)[0].texture);
        assert_eq!((t.width, t.height, t.levels, t.format), (32, 32, small.levels, TextureFormat::ATI2));
        assert_eq!(t.pixel_data, small.pixel_data);
        assert_eq!(t.name_hash, crate::rage_joaat("tri_prop_n"));

        let (a, b) = (crate::parse_ydd(&file).unwrap(), crate::parse_ydd(&out).unwrap());
        let geo = |e: &crate::DrawableEntry| e.drawable.lods.iter().flat_map(|l| &l.models).flat_map(|m| &m.geometries)
            .map(|g| (g.vertices_count, g.indices_count, g.vertex_buffer.as_ref().map(|v| v.data.clone()))).collect::<Vec<_>>();
        assert_eq!(geo(&a[0]), geo(&b[0]));
        assert_eq!(a[0].drawable.name, b[0].drawable.name);
    }

    #[test]
    fn an_ati2_texture_with_the_half_stride_some_tools_write_is_read_whole() {
        // as found in real clothing packs: a 64x64 ATI2 whose header says stride 32 (BC4's), full BC5 data after it
        let file = dictionary_with_ati2();
        let (mut g, root) = read_ydd(&file).unwrap();
        let id = embedded_textures(&g, root)[0].texture;
        let whole = texture_of(&g, id);
        g.get_mut::<Texture>(id).stride = whole.stride / 2;
        let odd = write_ydd(&mut g, root).unwrap();

        for read in [|f: &[u8]| { let (g, r) = read_ydd(f).unwrap(); let e = &embedded_textures(&g, r)[0]; (texture_of(&g, e.texture), e.stored_stride) }] {
            let (t, stored) = read(&odd);
            assert_eq!(stored, whole.stride / 2, "the header keeps what the file says");
            assert_eq!(t.stride, whole.stride, "the pixels are laid out by the format's stride");
            assert_eq!(t.pixel_data, whole.pixel_data, "the whole chain is read, not half of it");
            assert!(crate::decompress_texture(&t).is_ok());
        }
        let old = crate::parse_ydd(&odd).unwrap();
        let t = &old[0].drawable.shader_group.as_ref().unwrap().textures[0];
        assert_eq!((t.stride, t.pixel_data.len()), (whole.stride, whole.pixel_data.len()), "the older parser reads it whole too");

        // written back untouched, it stays as it was
        let (mut g2, root2) = read_ydd(&odd).unwrap();
        let again = write_ydd(&mut g2, root2).unwrap();
        let (g3, root3) = read_ydd(&again).unwrap();
        assert_eq!(embedded_textures(&g3, root3)[0].stored_stride, whole.stride / 2);
    }

    #[test]
    fn a_gen9_dictionary_is_refused() {
        let file = dictionary_with_ati2();
        let (sys, gfx) = prepare_rsc7(&file).unwrap();
        let flags = |at: usize| u32::from_le_bytes(file[at..at + 4].try_into().unwrap());
        let gen9 = build_rsc7_with_flags(159, flags(8), &sys, flags(12), &gfx);
        let err = read_ydd(&gen9).err().expect("refused").to_string();
        assert!(err.contains("Gen9"), "{err}");
    }
}
