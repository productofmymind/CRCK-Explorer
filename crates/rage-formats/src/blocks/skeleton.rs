//! Skeletons and joints, ported from the reference tool's `Drawable.cs` (`Skeleton`,
//! `SkeletonBonesBlock`, `Bone`, `SkeletonBoneTag`, `Joints`, `JointRotationLimit_s`,
//! `JointTranslationLimit_s`). Only the legacy PC layout is handled.
//!
//! A skeleton's bone-tag hash table, child/parent index arrays and the local and
//! inverse-global transform arrays are derived data: [`Skeleton::from_bones`] computes
//! them the way `Skeleton.ReadXml` does (`BuildIndices`, `BuildBoneTags`,
//! `BuildTransformations`), and a skeleton read from bytes keeps what the file holds.

use anyhow::Result;

use super::base::{read_pointer_array64, read_string_block, read_struct_array, PointerArray64, StringBlock, StructArray};
use super::xml::{child, child_attr_i32, child_attr_u32, child_text, child_vec3, child_vec4, format_flags, items, parse_flags, write_items, Node, XmlOut};
use super::*;

/// `EBoneFlags` (`Drawable.cs:2190`).
pub const BONE_FLAG_NAMES: [(&str, u16); 17] = [
    ("None", 0), ("RotX", 0x1), ("RotY", 0x2), ("RotZ", 0x4), ("LimitRotation", 0x8), ("TransX", 0x10),
    ("TransY", 0x20), ("TransZ", 0x40), ("LimitTranslation", 0x80), ("ScaleX", 0x100), ("ScaleY", 0x200),
    ("ScaleZ", 0x400), ("LimitScale", 0x800), ("Unk0", 0x1000), ("Unk1", 0x2000), ("Unk2", 0x4000), ("Unk3", 0x8000),
];

/// `EBoneFlags.ToString()`: the set names joined by `, `, `None` for 0.
fn bone_flags_name(f: u16) -> String { format_flags(f, &BONE_FLAG_NAMES, |v, b| v & b == b, 0, "None") }

/// `Bone` (80 bytes), a part of its [`SkeletonBonesBlock`]. `Index2` is always written equal to `index`.
#[derive(Clone, Debug)]
pub struct Bone {
    pub rotation: Vec4,
    pub translation: Vec3,
    pub scale: Vec3,
    pub next_sibling: i16,
    pub parent: i16,
    pub name: Option<BlockId>,
    pub flags: u16,
    pub index: i16,
    pub tag: u16,
    /// Column 4 of the skeleton's local transform, kept for round trips (`Bone.TransformUnk`).
    pub transform_unk: Vec4,
}

impl Bone {
    /// `Bone.WriteXml`.
    pub fn write_xml(&self, x: &mut XmlOut, g: &Graph) {
        x.string("Name", self.name.map_or("", |n| g.get::<StringBlock>(n).0.as_str()));
        x.value("Tag", self.tag);
        x.value("Index", self.index);
        x.value("ParentIndex", self.parent);
        x.value("SiblingIndex", self.next_sibling);
        x.string("Flags", &bone_flags_name(self.flags));
        x.vec3("Translation", self.translation);
        x.vec4("Rotation", self.rotation);
        x.vec3("Scale", self.scale);
        x.vec4("TransformUnk", self.transform_unk);
    }

    /// `Bone.ReadXml`: the bone value, its name (when there is a `Name` element) added to `g`.
    pub fn read_xml(n: Node, g: &mut Graph) -> Bone {
        let name = child(n, "Name").map(|c| g.add(StringBlock(c.text().unwrap_or("").to_owned())));
        Bone {
            rotation: child_vec4(n, "Rotation"),
            translation: child_vec3(n, "Translation"),
            scale: child_vec3(n, "Scale"),
            next_sibling: child_attr_i32(n, "SiblingIndex", "value") as i16,
            parent: child_attr_i32(n, "ParentIndex", "value") as i16,
            name,
            flags: parse_flags(&child_text(n, "Flags"), &BONE_FLAG_NAMES, |a, b| a | b, 0),
            index: child_attr_i32(n, "Index", "value") as i16,
            tag: child_attr_u32(n, "Tag", "value") as u16,
            transform_unk: child_vec4(n, "TransformUnk"),
        }
    }

    pub fn read(r: &mut Reader, g: &mut Graph, va: u64) -> Result<Option<BlockId>> {
        if va == 0 { return Ok(None); }
        if let Some(id) = r.cached_as::<Self>(va)? { return Ok(Some(id)); }
        let mut c = r.cursor(va)?;
        let rotation = c.vec4(); let translation = c.vec3(); c.skip(4);
        let scale = c.vec3(); c.skip(4);
        let (next_sibling, parent) = (c.i16(), c.i16()); c.skip(4);
        let name_ptr = c.u64();
        let (flags, index, tag) = (c.u16(), c.i16(), c.u16());
        c.skip(2 + 8);
        c.check()?;
        let name = read_string_block(r, g, name_ptr)?;
        let id = g.add(Bone { rotation, translation, scale, next_sibling, parent, name, flags, index, tag, transform_unk: Vec4::new(0.0, 0.0, 0.0, 0.0) });
        r.cache::<Self>(va, id); Ok(Some(id))
    }
}
impl Block for Bone {
    fn length(&self) -> usize { 80 }
    fn references(&self, _g: &Graph) -> Vec<BlockId> { self.name.into_iter().collect() }
    fn write(&self, w: &mut Writer, g: &Graph) -> Result<()> {
        w.vec4(self.rotation); w.vec3(self.translation); w.u32(0);
        w.vec3(self.scale); w.f32(1.0);
        w.i16(self.next_sibling); w.i16(self.parent); w.u32(0);
        w.u64(g.ptr(self.name));
        w.u16(self.flags); w.i16(self.index); w.u16(self.tag); w.i16(self.index);
        w.u64(0);
        Ok(())
    }
    fn as_any(&self) -> &dyn Any { self } fn as_any_mut(&mut self) -> &mut dyn Any { self }
}

/// `SkeletonBonesBlock`: a 16-byte header, then the bones inline (each one a part).
pub struct SkeletonBonesBlock { pub bones: Vec<BlockId> }

impl SkeletonBonesBlock {
    /// Reads `count` bones after the 16-byte header at `va`.
    pub fn read(r: &mut Reader, g: &mut Graph, va: u64, count: usize) -> Result<Option<BlockId>> {
        if va == 0 { return Ok(None); }
        if let Some(id) = r.cached_as::<Self>(va)? { return Ok(Some(id)); }
        let mut c = r.cursor(va)?; c.skip(16); c.check()?;
        let mut bones = Vec::new();
        for i in 0..count {
            if let Some(b) = Bone::read(r, g, va + 16 + 80 * i as u64)? { bones.push(b); }
        }
        let id = g.add(SkeletonBonesBlock { bones });
        r.cache::<Self>(va, id); Ok(Some(id))
    }
}
impl Block for SkeletonBonesBlock {
    fn length(&self) -> usize { 16 + 80 * self.bones.len() }
    fn parts(&self) -> Vec<(usize, BlockId)> { self.bones.iter().enumerate().map(|(i, &b)| (16 + 80 * i, b)).collect() }
    fn write(&self, w: &mut Writer, g: &Graph) -> Result<()> {
        w.u32(self.bones.len() as u32); w.u32(0); w.u32(0); w.u32(0);
        for &b in &self.bones { g.get::<Bone>(b).write(w, g)?; }
        Ok(())
    }
    fn as_any(&self) -> &dyn Any { self } fn as_any_mut(&mut self) -> &mut dyn Any { self }
}

/// `SkeletonBoneTag` (16 bytes): a bone-tag hash table entry, chained through `next`.
pub struct SkeletonBoneTag { pub tag: u32, pub index: u32, pub next: Option<BlockId> }

impl SkeletonBoneTag {
    /// Reads the entry at `va` and the chain after it (iteratively, sharing entries already read).
    pub fn read(r: &mut Reader, g: &mut Graph, va: u64) -> Result<Option<BlockId>> {
        if va == 0 { return Ok(None); }
        if let Some(id) = r.cached_as::<Self>(va)? { return Ok(Some(id)); }
        let (mut first, mut prev, mut cur) = (None, None::<BlockId>, va);
        loop {
            let mut c = r.cursor(cur)?;
            let (tag, index, next_ptr) = (c.u32(), c.u32(), c.u64());
            c.check()?;
            let id = g.add(SkeletonBoneTag { tag, index, next: None });
            r.cache::<Self>(cur, id);
            if let Some(p) = prev { g.get_mut::<SkeletonBoneTag>(p).next = Some(id); }
            first.get_or_insert(id);
            prev = Some(id);
            if next_ptr == 0 { break; }
            if let Some(n) = r.cached_as::<Self>(next_ptr)? { g.get_mut::<SkeletonBoneTag>(id).next = Some(n); break; }
            cur = next_ptr;
        }
        Ok(first)
    }
}
impl Block for SkeletonBoneTag {
    fn length(&self) -> usize { 16 }
    fn references(&self, _g: &Graph) -> Vec<BlockId> { self.next.into_iter().collect() }
    fn write(&self, w: &mut Writer, g: &Graph) -> Result<()> { w.u32(self.tag); w.u32(self.index); w.u64(g.ptr(self.next)); Ok(()) }
    fn as_any(&self) -> &dyn Any { self } fn as_any_mut(&mut self) -> &mut dyn Any { self }
}

/// `Skeleton.GetNumHashBuckets`.
pub fn num_hash_buckets(n: usize) -> usize {
    const LADDER: [usize; 16] = [11, 29, 59, 107, 191, 331, 563, 953, 1609, 2729, 4621, 7841, 13297, 22571, 38351, 65167];
    LADDER.iter().copied().find(|&b| n < b).unwrap_or(65521)
}

/// `Skeleton.BuildIndices`: the parent of every bone, and the child array: `(bone index, parent index)`
/// pairs in layers of at most four bones, breadth first, each layer padded to a multiple of eight entries
/// with its last pair.
pub fn build_indices(bones: &[Bone]) -> (Vec<i16>, Vec<i16>) {
    let n = bones.len();
    let parents: Vec<i16> = bones.iter().map(|b| b.parent).collect();
    let children = |of: &[usize]| -> Vec<usize> {
        of.iter().flat_map(|&b| (0..n).filter(move |&i| bones[i].parent == bones[b].index)).collect()
    };
    let roots: Vec<usize> = (0..n).filter(|&i| bones[i].parent < 0).collect();
    let mut layer = children(&roots);
    let mut layers: Vec<Vec<usize>> = Vec::new();
    // A well-formed hierarchy takes each bone once, so at most `n` layers; duplicate
    // indices could otherwise make a cycle that never ends.
    while !layer.is_empty() && layers.len() <= n {
        let take = layer.len().min(4);
        let ins: Vec<usize> = layer.drain(..take).collect();
        let ext = children(&ins);
        layer.splice(0..0, ext);
        layers.push(ins);
    }
    let mut childs = Vec::new();
    for l in &layers {
        for &b in l { childs.push(bones[b].index); childs.push(bones[b].parent); }
        if let Some(&last) = l.last() {
            let npad = 8 - childs.len() % 8;
            if npad < 8 {
                for _ in (0..npad).step_by(2) { childs.push(bones[last].index); childs.push(bones[last].parent); }
            }
        }
    }
    (parents, childs)
}

/// `Skeleton.BuildBoneTags`: the hash table of `(tag, bone index)` entries; `None` for fewer than two bones.
pub fn build_bone_tags(g: &mut Graph, bones: &[Bone]) -> Option<BlockId> {
    if bones.len() < 2 { return None; }
    let buckets = num_hash_buckets(bones.len());
    let mut by_bucket: Vec<Vec<usize>> = vec![Vec::new(); buckets];
    for (i, b) in bones.iter().enumerate() { by_bucket[b.tag as usize % buckets].push(i); }
    let mut slots = Vec::with_capacity(buckets);
    for mut b in by_bucket {
        if b.is_empty() { slots.push(None); continue; }
        b.reverse();
        let ids: Vec<BlockId> = b.iter().map(|&i| g.add(SkeletonBoneTag { tag: bones[i].tag as u32, index: i as u32, next: None })).collect();
        for w in ids.windows(2) { g.get_mut::<SkeletonBoneTag>(w[0]).next = Some(w[1]); }
        slots.push(Some(ids[0]));
    }
    Some(g.add(PointerArray64 { items: slots }))
}

/// `Skeleton.BuildTransformations`: every bone's local matrix (scaled diagonal, `Column4 = TransformUnk`)
/// and the inverse of its global matrix (`Column4 = 0`), the global one composed up the parent chain.
pub fn build_transformations(bones: &[Bone]) -> (Vec<Mat4>, Vec<Mat4>) {
    let n = bones.len();
    let (mut transforms, mut inverses) = (Vec::with_capacity(n), Vec::with_capacity(n));
    for bone in bones {
        let (mut pos, mut ori) = (bone.translation, bone.rotation);
        let mut m = Mat4::from_quat_pos(ori, pos);
        let s = m.scale_vector();
        m.set_scale_vector(Vec3::new(s.x * bone.scale.x, s.y * bone.scale.y, s.z * bone.scale.z));
        m.set_column4(bone.transform_unk);
        let (mut p, mut steps) = (bone.parent, 0);
        while p >= 0 && (p as usize) < n && steps < n {
            let pb = &bones[p as usize];
            pos = pb.rotation.quat_rotate(pos) + pb.translation;
            ori = pb.rotation.quat_mul(ori);
            p = pb.parent;
            steps += 1;
        }
        let mut mi = Mat4::from_quat_pos(ori, pos).inverse();
        mi.set_column4(Vec4::new(0.0, 0.0, 0.0, 0.0));
        transforms.push(m);
        inverses.push(mi);
    }
    (transforms, inverses)
}

/// `Skeleton` (112 bytes).
pub struct Skeleton {
    pub vft: u32,
    /// A `PointerArray64` of [`SkeletonBoneTag`].
    pub bone_tags: Option<BlockId>,
    pub bone_tags_capacity: u16,
    pub unknown_1ch: u32,
    pub bones: Option<BlockId>,
    pub transforms_inv: Option<BlockId>,
    pub transforms: Option<BlockId>,
    pub parent_indices: Option<BlockId>,
    pub child_indices: Option<BlockId>,
    pub unknown_50h: u32,
    pub unknown_54h: u32,
    pub unknown_58h: u32,
    pub bones_count: u16,
    pub child_indices_count: u16,
}

impl Skeleton {
    /// A skeleton over `bones` with the derived tables `Skeleton.ReadXml` builds.
    pub fn from_bones(g: &mut Graph, bones: Vec<Bone>, unknown_1ch: u32, u50: u32, u54: u32, u58: u32) -> BlockId {
        let (parents, childs) = build_indices(&bones);
        let bone_tags = build_bone_tags(g, &bones);
        let (transforms, inverses) = build_transformations(&bones);
        let bone_tags_capacity = bone_tags.map_or(0, |t| g.get::<PointerArray64>(t).items.len() as u16);
        let bones_count = bones.len() as u16;
        let child_indices_count = childs.len() as u16;
        let bones = if bones.is_empty() { None } else {
            let ids = bones.into_iter().map(|b| g.add(b)).collect();
            Some(g.add(SkeletonBonesBlock { bones: ids }))
        };
        fn array<T: Pod + 'static>(g: &mut Graph, items: Vec<T>) -> Option<BlockId> {
            if items.is_empty() { None } else { Some(g.add(StructArray { items })) }
        }
        let (transforms_inv, transforms) = (array(g, inverses), array(g, transforms));
        let (parent_indices, child_indices) = (array(g, parents), array(g, childs));
        g.add(Skeleton {
            vft: 1080114336, bone_tags, bone_tags_capacity, unknown_1ch, bones, transforms_inv, transforms, parent_indices, child_indices,
            unknown_50h: u50, unknown_54h: u54, unknown_58h: u58, bones_count, child_indices_count,
        })
    }

    fn bone_ids(&self, g: &Graph) -> Vec<BlockId> {
        self.bones.map_or_else(Vec::new, |b| g.get::<SkeletonBonesBlock>(b).bones.clone())
    }

    /// `Skeleton.WriteXml`.
    pub fn write_xml(&self, x: &mut XmlOut, g: &Graph) {
        x.value("Unknown1C", self.unknown_1ch);
        x.value("Unknown50", self.unknown_50h);
        x.value("Unknown54", self.unknown_54h);
        x.value("Unknown58", self.unknown_58h);
        if self.bones.is_some() {
            write_items(x, "Bones", &self.bone_ids(g), |x, &b| g.get::<Bone>(b).write_xml(x, g));
        }
    }

    /// `Skeleton.ReadXml`: the bones of `n`'s `Bones` items, then the derived tables. A bone with no
    /// (or an empty) `Name` adds a line to `warnings` (the reference tool accepts it silently).
    pub fn read_xml(n: Node, g: &mut Graph, warnings: &mut Vec<String>) -> Result<BlockId> {
        let mut bones = Vec::new();
        for (i, item) in items(n, "Bones").into_iter().enumerate() {
            let bone = Bone::read_xml(item, g);
            if bone.name.is_none_or(|s| g.get::<StringBlock>(s).0.is_empty()) { warnings.push(format!("bone {i}: no name")); }
            bones.push(bone);
        }
        Ok(Self::from_bones(
            g, bones,
            child_attr_u32(n, "Unknown1C", "value"), child_attr_u32(n, "Unknown50", "value"),
            child_attr_u32(n, "Unknown54", "value"), child_attr_u32(n, "Unknown58", "value"),
        ))
    }

    /// `Skeleton.Read`; each bone's `TransformUnk` is column 4 of its stored local transform.
    pub fn read(r: &mut Reader, g: &mut Graph, va: u64) -> Result<Option<BlockId>> {
        if va == 0 { return Ok(None); }
        if let Some(id) = r.cached_as::<Self>(va)? { return Ok(Some(id)); }
        let mut c = r.cursor(va)?;
        let vft = c.u32(); c.skip(4 + 8);
        let tags_ptr = c.u64();
        let (bone_tags_capacity, _tags_count) = (c.u16(), c.u16());
        let unknown_1ch = c.u32();
        let bones_ptr = c.u64();
        let (inv_ptr, trans_ptr, parents_ptr, childs_ptr) = (c.u64(), c.u64(), c.u64(), c.u64());
        c.skip(8);
        let (unknown_50h, unknown_54h, unknown_58h) = (c.u32(), c.u32(), c.u32());
        c.skip(2);
        let (bones_count, child_indices_count) = (c.u16(), c.u16());
        c.skip(2 + 4 + 8);
        c.check()?;

        let bone_tags = if tags_ptr == 0 {
            None
        } else if let Some(id) = r.cached_as::<PointerArray64>(tags_ptr)? {
            Some(id)
        } else {
            let ptrs = read_pointer_array64(r, g, tags_ptr, bone_tags_capacity as usize)?;
            let items = ptrs.into_iter().map(|p| SkeletonBoneTag::read(r, g, p)).collect::<Result<Vec<_>>>()?;
            let id = g.add(PointerArray64 { items });
            r.cache::<PointerArray64>(tags_ptr, id);
            Some(id)
        };
        // the pointer is to the first bone, 16 bytes into the bones block
        let bones = if bones_ptr != 0 {
            let block = bones_ptr.checked_sub(16).with_context(|| format!("the skeleton at {va:#x} has a bones pointer {bones_ptr:#x} below 16"))?;
            SkeletonBonesBlock::read(r, g, block, bones_count as usize)?
        } else { None };
        let transforms_inv = read_struct_array::<Mat4>(r, g, inv_ptr, bones_count as usize)?;
        let transforms = read_struct_array::<Mat4>(r, g, trans_ptr, bones_count as usize)?;
        let parent_indices = read_struct_array::<i16>(r, g, parents_ptr, bones_count as usize)?;
        let child_indices = read_struct_array::<i16>(r, g, childs_ptr, child_indices_count as usize)?;
        if let (Some(b), Some(t)) = (bones, transforms) {
            let unk: Vec<Vec4> = g.get::<StructArray<Mat4>>(t).items.iter().map(Mat4::column4).collect();
            let ids = g.get::<SkeletonBonesBlock>(b).bones.clone();
            for (id, u) in ids.into_iter().zip(unk) { g.get_mut::<Bone>(id).transform_unk = u; }
        }
        let id = g.add(Skeleton {
            vft, bone_tags, bone_tags_capacity, unknown_1ch, bones, transforms_inv, transforms, parent_indices, child_indices,
            unknown_50h, unknown_54h, unknown_58h, bones_count, child_indices_count,
        });
        r.cache::<Self>(va, id); Ok(Some(id))
    }
}
impl Block for Skeleton {
    fn length(&self) -> usize { 112 }
    fn references(&self, _g: &Graph) -> Vec<BlockId> {
        [self.bone_tags, self.bones, self.transforms_inv, self.transforms, self.parent_indices, self.child_indices].into_iter().flatten().collect()
    }
    fn write(&self, w: &mut Writer, g: &Graph) -> Result<()> {
        let capacity = count_u16(self.bone_tags.map_or(0, |t| g.get::<PointerArray64>(t).items.len()), "bone tag buckets")?;
        let count = count_u16(self.bones.map_or(0, |b| g.get::<SkeletonBonesBlock>(b).bones.len()), "bones")?;
        let child_count = count_u16(self.child_indices.map_or(0, |c| g.get::<StructArray<i16>>(c).items.len()), "child index entries")?;
        w.u32(self.vft); w.u32(1); w.u64(0);
        w.u64(g.ptr(self.bone_tags)); w.u16(capacity); w.u16(count.min(capacity));
        w.u32(self.unknown_1ch);
        w.u64(self.bones.map_or(0, |b| g.position(b) + 16));
        w.u64(g.ptr(self.transforms_inv)); w.u64(g.ptr(self.transforms));
        w.u64(g.ptr(self.parent_indices)); w.u64(g.ptr(self.child_indices));
        w.u64(0);
        w.u32(self.unknown_50h); w.u32(self.unknown_54h); w.u32(self.unknown_58h);
        w.u16(1); w.u16(count); w.u16(child_count); w.u16(0);
        w.u32(0); w.u64(0);
        Ok(())
    }
    fn as_any(&self) -> &dyn Any { self } fn as_any_mut(&mut self) -> &mut dyn Any { self }
}

fn le32(b: &[u8], slot: usize) -> u32 { u32::from_le_bytes(b[slot * 4..slot * 4 + 4].try_into().unwrap()) }

/// `JointRotationLimit_s` (192 bytes, `Drawable.cs:2913`). Only the bone id, `UnknownA`, `min` and `max`
/// are named; every other 4-byte slot (offset / 4, the constants the reference tool lists in its comments) is in `other`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct JointRotationLimit { pub bone_id: u16, pub unknown_a: u16, pub min: Vec3, pub max: Vec3, pub other: [u32; 48] }

impl Default for JointRotationLimit {
    /// `JointRotationLimit_s.Init`.
    fn default() -> Self {
        let pi = std::f32::consts::PI;
        let mut o = [0u32; 48];
        o[3] = 1; o[4] = 3;
        o[11] = 1.0f32.to_bits(); o[16] = 1.0f32.to_bits();
        o[20] = (-pi).to_bits(); o[21] = pi.to_bits(); o[22] = 1.0f32.to_bits();
        for (k, slot) in o[29..47].iter_mut().enumerate() { *slot = if k % 3 == 1 { (-pi).to_bits() } else { pi.to_bits() }; }
        o[47] = 0x100;
        JointRotationLimit { bone_id: 0, unknown_a: 0, min: Vec3::ZERO, max: Vec3::ZERO, other: o }
    }
}
impl Pod for JointRotationLimit {
    const SIZE: usize = 192;
    fn write(&self, w: &mut Writer) {
        for (slot, &v) in self.other.iter().enumerate() {
            match slot {
                2 => { w.u16(self.bone_id); w.u16(self.unknown_a); }
                23 => w.vec3(self.min),
                26 => w.vec3(self.max),
                24 | 25 | 27 | 28 => {}
                _ => w.u32(v),
            }
        }
    }
    fn read(b: &[u8]) -> Self {
        let mut other = [0u32; 48];
        for (slot, o) in other.iter_mut().enumerate() { *o = le32(b, slot); }
        let f = |slot| f32::from_bits(le32(b, slot));
        JointRotationLimit {
            bone_id: le32(b, 2) as u16, unknown_a: (le32(b, 2) >> 16) as u16,
            min: Vec3::new(f(23), f(24), f(25)), max: Vec3::new(f(26), f(27), f(28)), other,
        }
    }
}
impl JointRotationLimit {
    /// `WriteXml`: `BoneId`, `UnknownA`, `Min`, `Max`.
    pub fn write_xml(&self, x: &mut XmlOut) {
        x.value("BoneId", self.bone_id);
        x.value("UnknownA", self.unknown_a);
        x.vec3("Min", self.min);
        x.vec3("Max", self.max);
    }
    /// `ReadXml`: the defaults, then the XML's values.
    pub fn read_xml(n: Node) -> Self {
        JointRotationLimit {
            bone_id: child_attr_u32(n, "BoneId", "value") as u16, unknown_a: child_attr_u32(n, "UnknownA", "value") as u16,
            min: child_vec3(n, "Min"), max: child_vec3(n, "Max"), ..Default::default()
        }
    }
}

/// `JointTranslationLimit_s` (64 bytes, `Drawable.cs:3029`); `other` holds the 4-byte slots
/// (offset / 4) that are not the bone id, `min` or `max`, all 0 in the game's files.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct JointTranslationLimit { pub bone_id: u32, pub min: Vec3, pub max: Vec3, pub other: [u32; 16] }

impl Pod for JointTranslationLimit {
    const SIZE: usize = 64;
    fn write(&self, w: &mut Writer) {
        for (slot, &v) in self.other.iter().enumerate() {
            match slot {
                2 => w.u32(self.bone_id),
                8 => w.vec3(self.min),
                12 => w.vec3(self.max),
                9 | 10 | 13 | 14 => {}
                _ => w.u32(v),
            }
        }
    }
    fn read(b: &[u8]) -> Self {
        let mut other = [0u32; 16];
        for (slot, o) in other.iter_mut().enumerate() { *o = le32(b, slot); }
        let f = |slot| f32::from_bits(le32(b, slot));
        JointTranslationLimit { bone_id: le32(b, 2), min: Vec3::new(f(8), f(9), f(10)), max: Vec3::new(f(12), f(13), f(14)), other }
    }
}
impl JointTranslationLimit {
    /// `WriteXml`: `BoneId`, `Min`, `Max`.
    pub fn write_xml(&self, x: &mut XmlOut) {
        x.value("BoneId", self.bone_id);
        x.vec3("Min", self.min);
        x.vec3("Max", self.max);
    }
    /// `ReadXml` (the bone id is cast to `ushort`).
    pub fn read_xml(n: Node) -> Self {
        JointTranslationLimit { bone_id: child_attr_u32(n, "BoneId", "value") as u16 as u32, min: child_vec3(n, "Min"), max: child_vec3(n, "Max"), other: [0; 16] }
    }
}

/// `Joints` (64 bytes): the rotation and translation limit arrays.
pub struct Joints {
    pub vft: u32,
    pub rotation_limits: Option<BlockId>,
    pub translation_limits: Option<BlockId>,
    pub rot_count: u16,
    pub trans_count: u16,
}

impl Joints {
    /// A `Joints` over the limits; an empty list is no array.
    pub fn from_limits(g: &mut Graph, rot: Vec<JointRotationLimit>, trans: Vec<JointTranslationLimit>) -> BlockId {
        let (rot_count, trans_count) = (rot.len() as u16, trans.len() as u16);
        let rotation_limits = if rot.is_empty() { None } else { Some(g.add(StructArray { items: rot })) };
        let translation_limits = if trans.is_empty() { None } else { Some(g.add(StructArray { items: trans })) };
        g.add(Joints { vft: 1080130656, rotation_limits, translation_limits, rot_count, trans_count })
    }

    /// `Joints.WriteXml`.
    pub fn write_xml(&self, x: &mut XmlOut, g: &Graph) {
        if let Some(r) = self.rotation_limits {
            write_items(x, "RotationLimits", &g.get::<StructArray<JointRotationLimit>>(r).items, |x, l| l.write_xml(x));
        }
        if let Some(t) = self.translation_limits {
            write_items(x, "TranslationLimits", &g.get::<StructArray<JointTranslationLimit>>(t).items, |x, l| l.write_xml(x));
        }
    }

    /// `Joints.ReadXml`.
    pub fn read_xml(n: Node, g: &mut Graph) -> Result<BlockId> {
        let rot = items(n, "RotationLimits").into_iter().map(JointRotationLimit::read_xml).collect();
        let trans = items(n, "TranslationLimits").into_iter().map(JointTranslationLimit::read_xml).collect();
        Ok(Self::from_limits(g, rot, trans))
    }

    pub fn read(r: &mut Reader, g: &mut Graph, va: u64) -> Result<Option<BlockId>> {
        if va == 0 { return Ok(None); }
        if let Some(id) = r.cached_as::<Self>(va)? { return Ok(Some(id)); }
        let mut c = r.cursor(va)?;
        let vft = c.u32(); c.skip(4 + 8);
        let (rot_ptr, trans_ptr) = (c.u64(), c.u64());
        c.skip(16);
        let (rot_count, trans_count) = (c.u16(), c.u16());
        c.skip(2 + 2 + 8);
        c.check()?;
        let rotation_limits = read_struct_array::<JointRotationLimit>(r, g, rot_ptr, rot_count as usize)?;
        let translation_limits = read_struct_array::<JointTranslationLimit>(r, g, trans_ptr, trans_count as usize)?;
        let id = g.add(Joints { vft, rotation_limits, translation_limits, rot_count, trans_count });
        r.cache::<Self>(va, id); Ok(Some(id))
    }
}
impl Block for Joints {
    fn length(&self) -> usize { 64 }
    fn references(&self, _g: &Graph) -> Vec<BlockId> { self.rotation_limits.into_iter().chain(self.translation_limits).collect() }
    fn write(&self, w: &mut Writer, g: &Graph) -> Result<()> {
        let rot = count_u16(self.rotation_limits.map_or(0, |r| g.get::<StructArray<JointRotationLimit>>(r).items.len()), "joint rotation limits")?;
        let trans = count_u16(self.translation_limits.map_or(0, |t| g.get::<StructArray<JointTranslationLimit>>(t).items.len()), "joint translation limits")?;
        w.u32(self.vft); w.u32(1); w.u64(0);
        w.u64(g.ptr(self.rotation_limits)); w.u64(g.ptr(self.translation_limits));
        w.u64(0); w.u64(0);
        w.u16(rot); w.u16(trans); w.u16(0); w.u16(1);
        w.u64(0);
        Ok(())
    }
    fn as_any(&self) -> &dyn Any { self } fn as_any_mut(&mut self) -> &mut dyn Any { self }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blocks::base::{write_file_base, PagesInfo, PointerArray64};
    use crate::resource::SYSTEM_BASE;

    fn bone(_name: &str, index: i16, parent: i16, tag: u16) -> Bone {
        Bone { rotation: Vec4::new(0.0, 0.0, 0.0, 1.0), translation: Vec3::new(index as f32, 0.0, 0.0), scale: Vec3::new(1.0, 1.0, 1.0), next_sibling: -1, parent, name: None, flags: 0, index, tag, transform_unk: Vec4::new(0.0, 0.0, 0.0, 0.0) }
    }

    #[test]
    fn hash_buckets_follow_the_ladder() {
        assert_eq!((num_hash_buckets(1), num_hash_buckets(11), num_hash_buckets(28), num_hash_buckets(29)), (11, 29, 29, 59));
    }
    #[test]
    fn child_indices_are_layered_by_four_and_padded_to_eight() {
        let bones = vec![bone("root", 0, -1, 0), bone("a", 1, 0, 10), bone("b", 2, 0, 11), bone("c", 3, 1, 12)];
        let (parents, childs) = build_indices(&bones);
        assert_eq!(parents, vec![-1, 0, 0, 1]);
        assert_eq!(childs, vec![1, 0, 2, 0, 2, 0, 2, 0, 3, 1, 3, 1, 3, 1, 3, 1]);
    }
    #[test]
    fn bone_tags_hash_into_buckets_with_chains() {
        let mut g = Graph::new();
        let bones = vec![bone("root", 0, -1, 0), bone("a", 1, 0, 11), bone("b", 2, 0, 22)];
        let tags = build_bone_tags(&mut g, &bones).unwrap();
        let arr = g.get::<PointerArray64>(tags);
        assert_eq!(arr.items.len(), 11);
        let first = g.get::<SkeletonBoneTag>(arr.items[0].unwrap());
        assert_eq!(first.index, 2, "bucket is reversed: last inserted first");
        let second = g.get::<SkeletonBoneTag>(first.next.unwrap());
        assert_eq!(second.index, 1);
        assert!(arr.items[1..].iter().all(Option::is_none));
        assert!(build_bone_tags(&mut g, &bones[..1]).is_none(), "a single bone gets no tag table");
    }
    #[test]
    fn transformations_compose_parents_and_invert() {
        let mut bones = vec![bone("root", 0, -1, 0), bone("a", 1, 0, 1)];
        bones[0].rotation = Vec4::new(0.0, 0.0, (0.5f32).sqrt(), (0.5f32).sqrt());
        let (t, ti) = build_transformations(&bones);
        assert_eq!(t.len(), 2);
        let p = ti[1].transform_point(Vec3::new(0.0, 1.0, 0.0));
        assert!(p.xyz().length() < 1e-5, "inverse maps the bone's global position to the origin: {p:?}");
    }
    #[test]
    fn skeleton_round_trips_through_bytes_and_xml() {
        let mut g = Graph::new();
        let pages = g.add(PagesInfo::default());
        let mut bones = vec![bone("root", 0, -1, 0), bone("a", 1, 0, 5)];
        for b in &mut bones { b.name = Some(g.add(StringBlock(format!("bone{}", b.index)))); }
        bones[1].flags = 0x11;
        bones[1].transform_unk = Vec4::new(0.0, 4.0, -3.0, 0.0);
        let sk = Skeleton::from_bones(&mut g, bones, 7, 1, 2, 3);
        assert_eq!(g.length(sk), 112);
        struct Root(BlockId, BlockId);
        impl Block for Root {
            fn length(&self) -> usize { 32 }
            fn references(&self, _: &Graph) -> Vec<BlockId> { vec![self.0, self.1] }
            fn write(&self, w: &mut Writer, g: &Graph) -> Result<()> { write_file_base(w, g, 1, Some(self.0)); w.u64(g.position(self.1)); w.u64(0); Ok(()) }
            fn as_any(&self) -> &dyn Any { self } fn as_any_mut(&mut self) -> &mut dyn Any { self }
        }
        let root = g.add(Root(pages, sk));
        let file = g.build(root, pages, 1).unwrap();
        let mut r = Reader::open(&file).unwrap(); let mut g2 = Graph::new();
        let mut c = r.cursor(SYSTEM_BASE).unwrap(); c.skip(16); let sk_va = c.u64();
        let sk2 = Skeleton::read(&mut r, &mut g2, sk_va).unwrap().unwrap();
        let s1 = g.get::<Skeleton>(sk); let s2 = g2.get::<Skeleton>(sk2);
        assert_eq!((s2.bones_count, s2.child_indices_count), (2, 8));
        assert_eq!(s2.bones_count, s1.bones_count);
        let ci = |g: &Graph, s: &Skeleton| g.get::<StructArray<i16>>(s.child_indices.unwrap()).items.clone();
        assert_eq!(ci(&g2, s2), ci(&g, s1));
        assert_eq!(ci(&g2, s2), vec![1, 0, 1, 0, 1, 0, 1, 0]);
        let names = |g: &Graph, s: &Skeleton| -> Vec<String> {
            g.get::<SkeletonBonesBlock>(s.bones.unwrap()).bones.iter().map(|&b| g.get::<StringBlock>(g.get::<Bone>(b).name.unwrap()).0.clone()).collect()
        };
        assert_eq!(names(&g2, s2), vec!["bone0", "bone1"]);
        assert_eq!(names(&g2, s2), names(&g, s1));

        let xml = |g: &Graph, id: BlockId| { let mut x = XmlOut::new(); g.get::<Skeleton>(id).write_xml(&mut x, g); x.out };
        let a = xml(&g2, sk2);
        assert_eq!(a, xml(&g, sk));
        assert!(a.contains("<Flags>RotX, TransX</Flags>"), "{a}");
        let body = a.replacen("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n", "", 1);
        let text = format!("<Skeleton>{body}</Skeleton>"); let doc = roxmltree::Document::parse(&text).unwrap();
        let mut g3 = Graph::new();
        let sk3 = Skeleton::read_xml(doc.root_element(), &mut g3, &mut Vec::new()).unwrap();
        assert_eq!(g3.length(sk3), 112);
        assert_eq!(xml(&g3, sk3), a);
    }
    #[test]
    fn joint_limits_have_reference_sizes_and_defaults() {
        assert_eq!((JointRotationLimit::SIZE, JointTranslationLimit::SIZE), (192, 64));
        let mut g = Graph::new();
        let rot = JointRotationLimit { bone_id: 5, min: Vec3::new(-1.0, 0.0, 0.0), max: Vec3::new(1.0, 0.0, 0.0), ..Default::default() };
        let tr = JointTranslationLimit { bone_id: 6, min: Vec3::new(0.0, -2.0, 0.0), max: Vec3::new(0.0, 2.0, 0.0), ..Default::default() };
        let j = Joints::from_limits(&mut g, vec![rot], vec![tr]);
        assert_eq!(g.length(j), 64);
        let mut x = XmlOut::new(); g.get::<Joints>(j).write_xml(&mut x, &g);
        let body = x.out.replacen("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n", "", 1);
        let text = format!("<Joints>{body}</Joints>"); let doc = roxmltree::Document::parse(&text).unwrap();
        let mut g2 = Graph::new();
        let j2 = Joints::read_xml(doc.root_element(), &mut g2).unwrap();
        let mut x2 = XmlOut::new(); g2.get::<Joints>(j2).write_xml(&mut x2, &g2);
        assert_eq!(x.out, x2.out);
        let mut w = Writer::new(SYSTEM_BASE, 192); rot.write(&mut w);
        let bytes = w.into_inner();
        assert_eq!(JointRotationLimit::read(&bytes).bone_id, 5);
        assert_eq!(f32::from_le_bytes(bytes[0x50..0x54].try_into().unwrap()), -std::f32::consts::PI);
        assert_eq!(u32::from_le_bytes(bytes[0xBC..0xC0].try_into().unwrap()), 0x100);
    }
}
