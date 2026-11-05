//! What the World view needs from the game's models and texture dictionaries, as plain data: the triangles of a drawable by name,
//! the textures of a `.ytd` decoded on demand, and the texture dictionary parent chain (`gtxd.meta`).
use crate::{game::Game, index::{FileIndex, Filter}, model};
use anyhow::{bail, Result};
use rage_formats::{
    parse_txd_relationships,
    ydd::{DrawableKind, LodLevel},
    YtdTexture,
};
use std::collections::HashMap;

/// One drawable part: triangles and the name of its diffuse texture.
pub struct CpuPart {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub uvs: Vec<[f32; 2]>,
    pub uvs1: Vec<[f32; 2]>,
    /// layer 1, 2, 3 and the blend mask of a terrain shader
    pub terrain: Option<[String; 4]>,
    pub colors: Vec<[u8; 4]>,
    pub tangents: Vec<[f32; 4]>,
    /// names of the normal map and the specular map the shader names
    pub bump: Option<String>,
    pub spec: Option<String>,
    pub indices: Vec<u32>,
    pub diffuse: Option<String>,
    /// the shader's name (`hash_XXXXXXXX` when the table does not know it)
    pub shader: String,
    /// a proxy (reflections, shadows) rather than a model to be seen: its render mask's low bit is clear
    pub proxy: bool,
    pub mask: u16,
}

/// The triangles of the drawable called `want` in the bytes of a `.ydr`, `.yft` or `.ydd` (`ext` says which), at the best detail
/// level it has. A dictionary holds many drawables: only the one named `want` (any, when none has that name and there is one).
pub fn parts(bytes: &[u8], ext: &str, want: &str) -> Result<Vec<CpuPart>> {
    Ok(parts_with_embedded(bytes, ext, want)?.0)
}

/// Like [`parts`], and the textures the model carries inside itself (a drawable can hold its own, which no dictionary has).
pub fn parts_with_embedded(bytes: &[u8], ext: &str, want: &str) -> Result<(Vec<CpuPart>, Ytd)> {
    parts_with_embedded_hash(bytes, ext, want, 0)
}

/// The same, picking a drawable of a dictionary by the hash of its name as well (a dictionary stores only hashes, and the archetype may name a
/// drawable the name table does not know): `want_hash` 0 means none.
pub fn parts_with_embedded_hash(bytes: &[u8], ext: &str, want: &str, want_hash: u32) -> Result<(Vec<CpuPart>, Ytd)> {
    let Some(kind) = DrawableKind::from_extension(ext) else { bail!("not a model file: .{ext}") };
    let mut entries = rage_formats::parse_drawables(bytes, kind)?;
    if entries.len() > 1 {
        let named: Vec<_> = entries.iter().filter(|e| e.name.eq_ignore_ascii_case(want) || e.drawable.name.eq_ignore_ascii_case(want) || (want_hash != 0 && (e.hash == want_hash || e.drawable.name_hash == want_hash))).cloned().collect();
        if !named.is_empty() {
            entries = named;
        }
    }
    // a fragment's other drawables (doors, glass, pieces) use the shaders of its main one
    let shared = entries.iter().find_map(|e| e.drawable.shader_group.clone());
    for e in entries.iter_mut() {
        if e.drawable.shader_group.is_none() {
            e.drawable.shader_group = shared.clone();
        }
    }
    let embedded = Ytd(entries.iter().filter_map(|e| e.drawable.shader_group.as_ref()).flat_map(|g| g.textures.iter().cloned()).collect());
    for level in [LodLevel::High, LodLevel::Medium, LodLevel::Low, LodLevel::VeryLow] {
        let found = model::meshes(&entries, level);
        if !found.is_empty() {
            return Ok((found.into_iter().map(|p| CpuPart { positions: p.positions, normals: p.normals, uvs: p.uvs, uvs1: p.uvs1, terrain: p.terrain, colors: p.colors, tangents: p.tangents, bump: p.bump, spec: p.spec, indices: p.indices, diffuse: p.diffuse, shader: p.shader, proxy: p.render_mask & 1 == 0, mask: p.render_mask }).collect(), embedded));
        }
    }
    Ok((vec![], embedded))
}

/// The block-compression formats the GPU can read directly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bc {
    Bc1,
    Bc2,
    Bc3,
    Bc7,
}

/// A texture's mip levels as stored (largest first, whole 4x4 blocks), ready to upload without decoding.
pub struct Compressed {
    pub kind: Bc,
    pub width: u32,
    pub height: u32,
    pub levels: Vec<Vec<u8>>,
    /// a good part of its alpha is neither clear nor solid (smoke, glass, leaves): it is drawn blended
    pub soft: bool,
    /// levels were dropped because `skip` asked for it (memory was short): the picture can be uploaded again at full size later
    pub reduced: bool,
}

impl Bc {
    pub fn block_bytes(self) -> u32 {
        if self == Bc::Bc1 { 8 } else { 16 }
    }
}

/// A parsed `.ytd`: its textures are decoded only when asked for.
pub struct Ytd(Vec<YtdTexture>);

impl Ytd {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        Ok(Ytd(rage_formats::parse_ytd(bytes)?))
    }

    /// width, height and RGBA8 pixels of the texture called `name`
    pub fn decode(&self, name: &str) -> Option<(u32, u32, Vec<u8>)> {
        let t = self.0.iter().find(|t| t.name.eq_ignore_ascii_case(name))?;
        model::texture_rgba(t).ok()
    }

    pub fn has(&self, name: &str) -> bool {
        self.0.iter().any(|t| t.name.eq_ignore_ascii_case(name))
    }

    /// The texture as the game stores it, for a GPU that reads block-compressed pictures: its mip levels as they are (no decoding, a quarter to
    /// an eighth of the memory of RGBA8). `skip` drops that many of the largest levels (a smaller picture when memory is short), and a level
    /// is dropped while it is wider than `max_side`. `None` for formats not handled (the caller decodes those) or sizes that are not whole blocks.
    pub fn compressed(&self, name: &str, skip: u32, max_side: u32) -> Option<Compressed> {
        use rage_formats::TextureFormat as F;
        let t = self.0.iter().find(|t| t.name.eq_ignore_ascii_case(name))?;
        let (kind, block) = match t.format {
            F::DXT1 => (Bc::Bc1, 8usize),
            F::DXT3 => (Bc::Bc2, 16),
            F::DXT5 => (Bc::Bc3, 16),
            F::BC7 => (Bc::Bc7, 16),
            _ => return None,
        };
        let (w, h) = (t.width as u32, t.height as u32);
        if w < 4 || h < 4 || w % 4 != 0 || h % 4 != 0 {
            return None;
        }
        // the levels stored in the file whose sides are still whole blocks
        let mut levels = vec![];
        let mut at = 0usize;
        for k in 0..t.levels.max(1) as u32 {
            let (lw, lh) = (w >> k, h >> k);
            if lw < 4 || lh < 4 {
                break;
            }
            let size = (lw / 4) as usize * (lh / 4) as usize * block;
            let Some(data) = t.pixel_data.get(at..at + size) else { break };
            levels.push((lw, lh, data));
            at += size;
        }
        if levels.is_empty() {
            return None;
        }
        // the soft-alpha test looks at a small level (about 32 pixels wide) rather than decoding the big one
        let soft = match kind {
            Bc::Bc1 => false,
            _ => {
                let &(lw, lh, data) = levels.iter().find(|l| l.0.min(l.1) <= 32).unwrap_or_else(|| levels.last().unwrap());
                let small = YtdTexture { name: String::new(), name_hash: 0, width: lw as u16, height: lh as u16, depth: 1, format: t.format, levels: 1, stride: (lw / 4 * block as u32) as u16, pixel_data: data.to_vec() };
                model::texture_rgba(&small).map(|(_, _, p)| {
                    let mid = p.chunks_exact(4).filter(|q| q[3] > 24 && q[3] < 232).count();
                    mid * 100 > (p.len() / 4) * 12
                }).unwrap_or(false)
            }
        };
        let mut drop = skip as usize;
        let reduced = skip > 0 && levels.len() > 1;
        while levels.len() > 1 && (drop > 0 || levels[0].0.max(levels[0].1) > max_side) {
            levels.remove(0);
            drop = drop.saturating_sub(1);
        }
        Some(Compressed { kind, width: levels[0].0, height: levels[0].1, levels: levels.into_iter().map(|l| l.2.to_vec()).collect(), soft, reduced })
    }

    /// the names of the textures in it
    pub fn names(&self) -> Vec<String> {
        self.0.iter().map(|t| t.name.clone()).collect()
    }
}

/// `child -> parent` of every texture dictionary the game's `gtxd.meta` files declare (lowercase; later files win, as in the game).
pub fn txd_parents(game: &Game, ix: &FileIndex) -> HashMap<String, String> {
    let mut out = HashMap::new();
    // the base game keeps them in `gtxd.ymt` (an RBF file); the DLC packs add `gtxd.meta` files, applied after it
    for (name, ext) in [("gtxd.ymt", "ymt"), ("gtxd.meta", "meta")] {
        for r in ix.find(name, &Filter { exts: vec![ext.into()], ..Default::default() }, false, 100) {
            if !r.name().eq_ignore_ascii_case(name) {
                continue;
            }
            let Ok(data) = game.read(&r.path) else { continue };
            if let Ok(list) = parse_txd_relationships(&data) {
                for t in list {
                    // the first relationship recorded for a child wins
                    out.entry(t.child.to_lowercase()).or_insert_with(|| t.parent.to_lowercase());
                }
            }
        }
    }
    out
}

/// The bones of a model as line segments in the model's own space (each bone joined to its parent), for the World's skeleton view. Empty when
/// the drawable has no skeleton. The drawable is picked as `parts` picks it.
pub fn skeleton_segments(bytes: &[u8], ext: &str, want: &str) -> Vec<([f32; 3], [f32; 3])> {
    let Some(kind) = DrawableKind::from_extension(ext) else { return vec![] };
    let Ok(entries) = rage_formats::parse_drawables(bytes, kind) else { return vec![] };
    let pick = entries.iter().find(|e| e.name.eq_ignore_ascii_case(want) || e.drawable.name.eq_ignore_ascii_case(want)).or_else(|| entries.first());
    let Some(bones) = pick.and_then(|e| e.drawable.skeleton.as_ref()) else { return vec![] };
    bone_segments(bones)
}

/// Each bone's place in the model (its local transform composed with its parents'), and a segment from its parent's place to its own.
pub fn bone_segments(bones: &[rage_formats::ydd::Bone]) -> Vec<([f32; 3], [f32; 3])> {
    use glam::{Mat4, Quat, Vec3};
    let local = |b: &rage_formats::ydd::Bone| Mat4::from_scale_rotation_translation(Vec3::from(b.scale), Quat::from_xyzw(b.rotation[0], b.rotation[1], b.rotation[2], b.rotation[3]).normalize(), Vec3::from(b.translation));
    let mut world: Vec<Option<Mat4>> = vec![None; bones.len()];
    fn place(i: usize, bones: &[rage_formats::ydd::Bone], world: &mut Vec<Option<Mat4>>, local: &dyn Fn(&rage_formats::ydd::Bone) -> Mat4, depth: usize) -> Mat4 {
        if let Some(m) = world[i] {
            return m;
        }
        let p = bones[i].parent;
        let parent = if p >= 0 && (p as usize) < bones.len() && p as usize != i && depth < 64 { place(p as usize, bones, world, local, depth + 1) } else { Mat4::IDENTITY };
        let m = parent * local(&bones[i]);
        world[i] = Some(m);
        m
    }
    for i in 0..bones.len() {
        place(i, bones, &mut world, &local, 0);
    }
    let pos = |i: usize| world[i].map_or(Vec3::ZERO, |m| m.w_axis.truncate());
    (0..bones.len())
        .filter_map(|i| {
            let p = bones[i].parent;
            (p >= 0 && (p as usize) < bones.len() && p as usize != i).then(|| (pos(p as usize).to_array(), pos(i).to_array()))
        })
        .collect()
}

#[cfg(test)]
mod skeleton_tests {
    use super::*;
    use rage_formats::ydd::Bone;

    #[test]
    fn bones_are_placed_through_their_parents() {
        let b = |parent: i16, t: [f32; 3]| Bone { name: String::new(), parent, rotation: [0.0, 0.0, 0.0, 1.0], translation: t, scale: [1.0; 3] };
        // the child is listed before its parent: it still lands at 1 + 2 along x
        let bones = vec![b(1, [2.0, 0.0, 0.0]), b(2, [1.0, 0.0, 0.0]), b(-1, [0.0, 0.0, 0.0])];
        let s = bone_segments(&bones);
        assert_eq!(s, vec![([1.0, 0.0, 0.0], [3.0, 0.0, 0.0]), ([0.0, 0.0, 0.0], [1.0, 0.0, 0.0])]);
    }

    /// With a vehicle's fragment at `target/scratch/skel.yft` (the user's own game file): its skeleton has bones.
    #[test]
    fn a_real_fragment_has_a_skeleton() {
        let Ok(d) = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../../target/scratch/skel.yft")) else { return };
        let s = skeleton_segments(&d, "yft", "");
        eprintln!("skeleton segments: {}", s.len());
        assert!(s.len() > 10);
    }

    fn dxt1(w: u16, h: u16, levels: u8) -> Ytd {
        let mut size = 0;
        for k in 0..levels as u32 {
            size += ((w as usize >> k) / 4) * ((h as usize >> k) / 4) * 8;
        }
        Ytd(vec![YtdTexture { name: "T".into(), name_hash: 0, width: w, height: h, depth: 1, format: rage_formats::TextureFormat::DXT1, levels, stride: w / 4 * 8, pixel_data: vec![0; size] }])
    }

    #[test]
    fn compressed_textures_keep_their_stored_levels() {
        let y = dxt1(16, 8, 3);
        let c = y.compressed("t", 0, 4096).unwrap();
        assert_eq!((c.width, c.height, c.levels.len()), (16, 8, 2), "a level below 4 pixels is not kept: 16x8, 8x4");
        assert_eq!(c.levels[0].len(), 4 * 2 * 8);
        // the largest level is dropped on request, or when it is wider than the limit
        assert_eq!(y.compressed("T", 1, 4096).unwrap().width, 8);
        assert_eq!(y.compressed("T", 0, 8).unwrap().width, 8);
        // the last level is never dropped
        assert_eq!(y.compressed("T", 9, 4096).unwrap().levels.len(), 1);
        // sizes that are not whole blocks, and other formats, are left to the caller
        assert!(dxt1(6, 8, 1).compressed("T", 0, 4096).is_none());
        assert!(y.compressed("missing", 0, 4096).is_none());
    }
}
