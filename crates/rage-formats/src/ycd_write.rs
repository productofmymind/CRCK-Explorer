//! Writing a clip dictionary back to a binary `.ycd`.
//!
//! Every structure is built as its own small block and referred to by block number; once the
//! blocks are packed into pages their addresses are known and the pointers are filled in.
//! Virtual-table words are written as zero, as the tools that write `.ycd` files do: the game
//! sets its own when it places the resource.

use std::collections::HashMap;

use anyhow::{bail, ensure, Result};

use crate::hash::rage_joaat;
use crate::resource::{build_rsc7_with_flags, pack_pages, rsc7_page_count, SYSTEM_BASE};
use crate::ycd::*;
use crate::ycd::{bit_count, BitWriter};
use crate::ycd_xml::attr_type;

struct Blocks {
    data: Vec<Vec<u8>>,
    /// (block, byte offset in it, block it points at)
    fixups: Vec<(usize, usize, usize)>,
}

impl Blocks {
    fn alloc(&mut self, size: usize) -> usize {
        self.data.push(vec![0; size]);
        self.data.len() - 1
    }
    fn put(&mut self, b: usize, at: usize, bytes: &[u8]) {
        self.data[b][at..at + bytes.len()].copy_from_slice(bytes);
    }
    fn u8(&mut self, b: usize, at: usize, v: u8) {
        self.data[b][at] = v;
    }
    fn u16(&mut self, b: usize, at: usize, v: u16) {
        self.put(b, at, &v.to_le_bytes());
    }
    fn u32(&mut self, b: usize, at: usize, v: u32) {
        self.put(b, at, &v.to_le_bytes());
    }
    fn f32(&mut self, b: usize, at: usize, v: f32) {
        self.put(b, at, &v.to_le_bytes());
    }
    fn ptr(&mut self, b: usize, at: usize, target: usize) {
        self.fixups.push((b, at, target));
    }
    /// A NUL-terminated string in its own block.
    fn string(&mut self, s: &str) -> usize {
        let b = self.alloc(s.len() + 1);
        self.put(b, 0, s.as_bytes());
        b
    }
}

/// The number of buckets the game's hash tables use for `n` entries.
fn buckets(n: usize) -> usize {
    const SIZES: [usize; 16] = [11, 29, 59, 107, 191, 331, 563, 953, 1609, 2729, 4621, 7841, 13297, 22571, 38351, 65167];
    SIZES.iter().find(|&&s| n < s).copied().unwrap_or(65521)
}

/// A hash table: an array of bucket-head pointers and, for every entry, a 32-byte block holding
/// its hash, its payload pointer (at 8) and the next entry in its bucket (at 0x10). Returns the
/// array block, the bucket count and the entry blocks.
fn hash_table(w: &mut Blocks, entries: &[(u32, usize)]) -> (usize, usize) {
    let n = buckets(entries.len());
    let array = w.alloc(n * 8);
    let mut tails: Vec<Option<usize>> = vec![None; n];
    for &(hash, payload) in entries {
        let e = w.alloc(0x20);
        w.u32(e, 0, hash);
        w.ptr(e, 8, payload);
        let bucket = hash as usize % n;
        match tails[bucket] {
            None => w.ptr(array, bucket * 8, e),
            Some(prev) => w.ptr(prev, 0x10, e),
        }
        tails[bucket] = Some(e);
    }
    (array, n)
}

pub fn serialize_ycd(d: &ClipDictionary) -> Result<Vec<u8>> {
    let mut w = Blocks { data: Vec::new(), fixups: Vec::new() };
    let root = w.alloc(0x40);
    let pagemap = w.alloc(16);

    // how many places refer to each animation: its map entry and every clip that plays it
    let mut usage: HashMap<u32, u32> = d.animations.iter().map(|a| (a.hash, 1)).collect();
    for c in &d.clips {
        let refs: Vec<u32> = match &c.kind {
            ClipKind::Animation(a) => vec![a.animation],
            ClipKind::List { animations, .. } => animations.iter().map(|a| a.animation).collect(),
        };
        for r in refs {
            if let Some(u) = usage.get_mut(&r) {
                *u += 1;
            }
        }
    }

    // animations
    let mut anim_block: HashMap<u32, usize> = HashMap::new();
    let mut anim_entries = Vec::new();
    for a in &d.animations {
        ensure!(!anim_block.contains_key(&a.hash), "two animations share the hash hash_{:08X}", a.hash);
        let b = write_animation(&mut w, a, usage[&a.hash])?;
        anim_block.insert(a.hash, b);
        anim_entries.push((a.hash, b));
    }
    let anim_map = w.alloc(0x30);
    let (anim_array, anim_buckets) = hash_table(&mut w, &anim_entries);
    w.u32(anim_map, 4, 1);
    w.ptr(anim_map, 0x18, anim_array);
    w.u16(anim_map, 0x20, anim_buckets as u16);
    w.u16(anim_map, 0x22, d.animations.len() as u16);
    w.u32(anim_map, 0x24, 0x0100_0000);
    w.u32(anim_map, 0x28, 1);

    // clips
    let mut clip_entries = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for c in &d.clips {
        ensure!(seen.insert(c.hash), "two clips share the hash hash_{:08X}", c.hash);
        clip_entries.push((c.hash, write_clip(&mut w, c, &anim_block)?));
    }
    let (clip_array, clip_buckets) = hash_table(&mut w, &clip_entries);

    w.u32(root, 4, 1);
    w.ptr(root, 8, pagemap);
    w.ptr(root, 0x18, anim_map);
    w.u32(root, 0x20, 0x101);
    w.ptr(root, 0x28, clip_array);
    w.u16(root, 0x30, clip_buckets as u16);
    w.u16(root, 0x32, d.clips.len() as u16);
    w.u32(root, 0x34, 0x0100_0000);

    // pack into pages; the page map's own size depends on how many pages there turn out to be
    let mut pages = 1;
    let layout = loop {
        w.data[pagemap] = vec![0; 16 + 8 * pages];
        let sizes: Vec<usize> = w.data.iter().map(Vec::len).collect();
        let layout = pack_pages(&sizes, true, 128)?;
        let n = rsc7_page_count(layout.flags);
        if n == pages {
            break layout;
        }
        pages = n;
    };
    let mut system = vec![0u8; layout.size];
    for (i, block) in w.data.iter().enumerate() {
        system[layout.offsets[i]..layout.offsets[i] + block.len()].copy_from_slice(block);
    }
    for &(b, at, target) in &w.fixups {
        let va = SYSTEM_BASE + layout.offsets[target] as u64;
        let dst = layout.offsets[b] + at;
        system[dst..dst + 8].copy_from_slice(&va.to_le_bytes());
    }
    system[layout.offsets[pagemap] + 8] = pages as u8;
    Ok(build_rsc7_with_flags(YCD_VERSION, layout.flags, &system, 0, &[]))
}

// ─── animations ────────────────────────────────────────────────────────────────────────────

fn write_animation(w: &mut Blocks, a: &Animation, usage: u32) -> Result<usize> {
    ensure!(
        a.sequences.iter().all(|s| s.data.len() == a.bone_ids.len()),
        "animation hash_{:08X}: every sequence needs one entry for each of its {} bone-tracks",
        a.hash,
        a.bone_ids.len()
    );
    let b = w.alloc(0x60);
    let mut longest = 0usize;
    let seq_array = (!a.sequences.is_empty()).then(|| w.alloc(a.sequences.len() * 8));
    for (i, s) in a.sequences.iter().enumerate() {
        let blk = write_sequence(w, s, &a.bone_ids).map_err(|e| e.context(format!("sequence {i}")))?;
        longest = longest.max(w.data[blk].len());
        w.ptr(seq_array.unwrap(), i * 8, blk);
    }
    let bone_array = (!a.bone_ids.is_empty()).then(|| w.alloc(a.bone_ids.len() * 4));
    for (i, id) in a.bone_ids.iter().enumerate() {
        w.u16(bone_array.unwrap(), i * 4, id.id);
        w.u8(bone_array.unwrap(), i * 4 + 2, id.unk0);
        w.u8(bone_array.unwrap(), i * 4 + 3, id.track);
    }
    w.u32(b, 4, 1);
    w.u8(b, 0x10, a.unknown10);
    w.u8(b, 0x11, 1);
    w.u16(b, 0x14, a.frames);
    w.u16(b, 0x16, a.sequence_frame_limit);
    w.f32(b, 0x18, a.duration);
    w.u32(b, 0x1C, a.unknown1c);
    w.u32(b, 0x38, longest as u32);
    w.u32(b, 0x3C, usage);
    if let Some(arr) = seq_array {
        w.ptr(b, 0x40, arr);
    }
    w.u16(b, 0x48, a.sequences.len() as u16);
    w.u16(b, 0x4A, a.sequences.len() as u16);
    if let Some(arr) = bone_array {
        w.ptr(b, 0x50, arr);
    }
    w.u16(b, 0x58, a.bone_ids.len() as u16);
    w.u16(b, 0x5A, a.bone_ids.len() as u16);
    Ok(b)
}

/// One channel's place in a sequence, as the data block lists it.
struct Placed<'a> {
    sequence: usize,
    index: usize,
    channel: &'a Channel,
    /// Where its per-channel data starts, in 32-bit words, and where its frame bits start.
    data_words: usize,
    frame_bit: usize,
}

fn quanta(v: f32, quantum: f32, offset: f32) -> u32 {
    if quantum == 0.0 { 0 } else { ((v - offset) / quantum + 0.5) as u32 }
}

fn write_sequence(w: &mut Blocks, s: &Sequence, bone_ids: &[BoneId]) -> Result<usize> {
    let frames = s.frame_count as usize;
    let mut by_kind: Vec<Vec<Placed>> = (0..9).map(|_| Vec::new()).collect();
    for (si, channels) in s.data.iter().enumerate() {
        for (ci, ch) in channels.iter().enumerate() {
            let index = if let Channel::CachedQuaternion { quat_index, .. } = ch { *quat_index as usize } else { ci };
            ensure!(index < 4, "a channel's index {index} does not fit in two bits");
            by_kind[ch.type_id()].push(Placed { sequence: si, index, channel: ch, data_words: 0, frame_bit: 0 });
        }
    }

    // per-channel data, with the list of counts and the channel-to-sequence words alongside
    let (mut main, mut counts, mut items) = (Vec::<u8>::new(), Vec::<u8>::new(), Vec::<u8>::new());
    let (mut quant_bits, mut indirect_ints) = (0usize, 0usize);
    let mut frame_bit = 0usize;
    let mut frame_writers: Vec<Box<dyn Fn(&mut BitWriter, usize)>> = Vec::new();
    for kind in 0..9 {
        counts.extend((by_kind[kind].len() as u16).to_le_bytes());
        for p in by_kind[kind].iter_mut() {
            items.extend(((p.index + (p.sequence << 2)) as u16).to_le_bytes());
            p.data_words = main.len() / 4;
            p.frame_bit = frame_bit;
            match p.channel {
                Channel::StaticQuaternion(q) => {
                    for v in &q[..3] {
                        main.extend(v.to_le_bytes());
                    }
                }
                Channel::StaticVector3(v) => {
                    for x in v {
                        main.extend(x.to_le_bytes());
                    }
                }
                Channel::StaticFloat(v) => main.extend(v.to_le_bytes()),
                Channel::RawFloat(values) => {
                    ensure!(values.len() == frames, "a RawFloat channel has {} values for {frames} frames", values.len());
                    let values = values.clone();
                    frame_writers.push(Box::new(move |bw, f| bw.write(values[f].to_bits(), 32)));
                    frame_bit += 32;
                }
                Channel::QuantizeFloat { quantum, offset, values } => {
                    ensure!(values.len() == frames, "a QuantizeFloat channel has {} values for {frames} frames", values.len());
                    let q: Vec<u32> = values.iter().map(|v| quanta(*v, *quantum, *offset)).collect();
                    let bits = bit_count(q.iter().copied().max().unwrap_or(0)).max(1);
                    main.extend((bits as i32).to_le_bytes());
                    main.extend(quantum.to_le_bytes());
                    main.extend(offset.to_le_bytes());
                    frame_writers.push(Box::new(move |bw, f| bw.write(q[f], bits)));
                    frame_bit += bits;
                    quant_bits += bits;
                }
                Channel::IndirectQuantizeFloat { quantum, offset, values, frames: indices } => {
                    ensure!(indices.len() == frames, "an IndirectQuantizeFloat channel has {} frame indices for {frames} frames", indices.len());
                    ensure!(indices.iter().all(|&i| (i as usize) < values.len()), "an IndirectQuantizeFloat frame index is past its values");
                    let frame_bits = bit_count(values.len() as u32).max(2);
                    let q: Vec<u32> = values.iter().map(|v| quanta(*v, *quantum, *offset)).collect();
                    let value_bits = bit_count(q.iter().copied().max().unwrap_or(0)).max(3);
                    let mut bw = BitWriter::default();
                    for v in &q {
                        bw.write(*v, value_bits);
                    }
                    main.extend((frame_bits as i32).to_le_bytes());
                    main.extend((value_bits as i32).to_le_bytes());
                    main.extend((bw.words.len() as i32).to_le_bytes());
                    main.extend(quantum.to_le_bytes());
                    main.extend(offset.to_le_bytes());
                    for word in &bw.words {
                        main.extend(word.to_le_bytes());
                    }
                    indirect_ints += bw.words.len() + 5;
                    let indices = indices.clone();
                    frame_writers.push(Box::new(move |bw, f| bw.write(indices[f], frame_bits)));
                    frame_bit += frame_bits;
                }
                Channel::CachedQuaternion { .. } => {}
            }
        }
        // the table holds four words to a group: pad a short group with the sequence count
        let n = by_kind[kind].len();
        if n % 4 != 0 {
            for _ in 0..4 - n % 4 {
                items.extend(((s.data.len() << 2) as u16).to_le_bytes());
            }
        }
    }

    // per-frame bits
    let mut frame_words: Vec<Vec<u32>> = Vec::with_capacity(frames);
    for f in 0..frames {
        let mut bw = BitWriter::default();
        for write in &frame_writers {
            write(&mut bw, f);
        }
        frame_words.push(bw.words);
    }
    let frame_length = frame_words.iter().map(|f| f.len() * 4).max().unwrap_or(0);
    let mut frame_data = Vec::with_capacity(frame_length * frames);
    for words in &frame_words {
        for i in 0..frame_length / 4 {
            frame_data.extend(words.get(i).copied().unwrap_or(0).to_le_bytes());
        }
    }

    // root motion channels (bone-tracks 5 and 6) are listed again at the end, six bytes each
    let mut refs: [Vec<[u8; 6]>; 2] = [Vec::new(), Vec::new()];
    for (which, track) in [5u8, 6].into_iter().enumerate() {
        let mut list: Vec<(u8, u8, [u8; 6])> = Vec::new();
        for p in by_kind.iter().flatten() {
            if bone_ids.get(p.sequence).map(|b| b.track) != Some(track) {
                continue;
            }
            let kind = p.channel.type_id() as u8;
            let mut r = [kind, p.index as u8, 0, 0, 0, 0];
            r[2..4].copy_from_slice(&(p.data_words as u16).to_le_bytes());
            r[4..6].copy_from_slice(&(p.frame_bit as u16).to_le_bytes());
            list.push((kind, p.index as u8, r));
        }
        list.sort_by_key(|(k, i, _)| (*k, *i));
        refs[which] = list.into_iter().map(|(_, _, r)| r).collect();
    }
    ensure!(refs[0].len() < 16 && refs[1].len() < 16, "too many root motion channels");
    for r in refs.iter().flatten() {
        items.extend(r);
    }

    let frame_offset = main.len();
    let mut data = main;
    data.extend(&frame_data);
    data.extend(&counts);
    data.extend(&items);
    let n_refs = refs[0].len() + refs[1].len();
    let b = w.alloc(0x20 + data.len());
    w.u32(b, 0, s.hash);
    w.u32(b, 4, data.len() as u32);
    w.u32(b, 0x0C, frame_offset as u32);
    w.u32(b, 0x10, (0x20 + data.len() - n_refs * 6) as u32);
    w.u16(b, 0x16, frames as u16);
    w.u16(b, 0x18, frame_length as u16);
    w.u16(b, 0x1A, indirect_ints as u16);
    w.u16(b, 0x1C, quant_bits as u16);
    w.u8(b, 0x1E, 255);
    w.u8(b, 0x1F, ((refs[0].len() << 4) | refs[1].len()) as u8);
    w.put(b, 0x20, &data);
    Ok(b)
}

// ─── clips ─────────────────────────────────────────────────────────────────────────────────

fn write_clip(w: &mut Blocks, c: &Clip, anim_block: &HashMap<u32, usize>) -> Result<usize> {
    let b = w.alloc(0x70);
    w.u32(b, 4, 1);
    w.u32(b, 0x10, if matches!(c.kind, ClipKind::Animation(_)) { 1 } else { 2 });
    if !c.name.is_empty() {
        let name = w.string(&c.name);
        w.ptr(b, 0x18, name);
    }
    w.u16(b, 0x20, c.name.len() as u16);
    w.u16(b, 0x22, if c.name.is_empty() { 0 } else { c.name.len() as u16 + 1 });
    w.put(b, 0x28, &0x5000_0000u64.to_le_bytes());
    w.u32(b, 0x30, c.unknown30);
    w.u32(b, 0x48, 1);

    // tags: a list block that every tag points back at
    let list = w.alloc(0x20);
    let block_hash = rage_joaat("block");
    if !c.tags.is_empty() {
        let array = w.alloc(c.tags.len() * 8);
        for (i, t) in c.tags.iter().enumerate() {
            let tb = w.alloc(0x50);
            write_property_body(w, tb, &t.property)?;
            w.f32(tb, 0x40, t.start_phase);
            w.f32(tb, 0x44, t.end_phase);
            w.ptr(tb, 0x48, list);
            w.ptr(array, i * 8, tb);
        }
        w.ptr(list, 0, array);
    }
    w.u16(list, 8, c.tags.len() as u16);
    w.u16(list, 0xA, c.tags.len() as u16);
    w.u32(list, 0x10, c.tags.iter().any(|t| t.property.name == block_hash) as u32);
    w.ptr(b, 0x38, list);

    // properties: a bucketed table of entries pointing at the properties
    let map = w.alloc(0x10);
    let n = buckets(c.properties.len());
    let array = w.alloc(n * 8);
    let mut tails: Vec<Option<usize>> = vec![None; n];
    for p in &c.properties {
        let pb = w.alloc(0x40);
        write_property_body(w, pb, p)?;
        let e = w.alloc(0x20);
        w.u32(e, 0, p.name);
        w.ptr(e, 8, pb);
        let bucket = p.name as usize % n;
        match tails[bucket] {
            None => w.ptr(array, bucket * 8, e),
            Some(prev) => w.ptr(prev, 0x10, e),
        }
        tails[bucket] = Some(e);
    }
    w.ptr(map, 0, array);
    w.u16(map, 8, n as u16);
    w.u16(map, 0xA, c.properties.len() as u16);
    w.u32(map, 0xC, 0x0100_0000);
    w.ptr(b, 0x40, map);

    let anim = |hash: u32| -> Result<usize> {
        anim_block.get(&hash).copied().ok_or_else(|| anyhow::anyhow!("clip '{}' plays hash_{hash:08X}, which is not one of the animations", c.name))
    };
    match &c.kind {
        ClipKind::Animation(a) => {
            w.ptr(b, 0x50, anim(a.animation)?);
            w.f32(b, 0x58, a.start);
            w.f32(b, 0x5C, a.end);
            w.f32(b, 0x60, a.rate);
        }
        ClipKind::List { duration, animations } => {
            if !animations.is_empty() {
                let array = w.alloc(animations.len() * 24);
                for (i, a) in animations.iter().enumerate() {
                    w.f32(array, i * 24, a.start);
                    w.f32(array, i * 24 + 4, a.end);
                    w.f32(array, i * 24 + 8, a.rate);
                    w.ptr(array, i * 24 + 0x10, anim(a.animation)?);
                }
                w.ptr(b, 0x50, array);
            }
            w.u16(b, 0x58, animations.len() as u16);
            w.u16(b, 0x5A, animations.len() as u16);
            w.f32(b, 0x60, *duration);
            w.u32(b, 0x64, 1);
        }
    }
    Ok(b)
}

/// The part a property and a tag share: names, and the attributes it points at.
fn write_property_body(w: &mut Blocks, b: usize, p: &Property) -> Result<()> {
    w.u32(b, 4, 1);
    w.u32(b, 0x18, p.name);
    w.u32(b, 0x38, p.unk);
    if !p.attributes.is_empty() {
        let array = w.alloc(p.attributes.len() * 8);
        for (i, a) in p.attributes.iter().enumerate() {
            let ab = w.alloc(0x30);
            w.u32(ab, 4, 1);
            w.u8(ab, 8, attr_type(&a.value).1);
            w.u32(ab, 0x18, a.name);
            match &a.value {
                AttrValue::Float(v) => w.f32(ab, 0x20, *v),
                AttrValue::Int(v) => w.u32(ab, 0x20, *v as u32),
                AttrValue::Bool(v) => w.u32(ab, 0x20, *v),
                AttrValue::String(s) => {
                    let sb = w.string(s);
                    w.ptr(ab, 0x20, sb);
                    w.u16(ab, 0x28, s.len() as u16);
                    w.u16(ab, 0x2A, s.len() as u16);
                }
                AttrValue::Vector3 { value, unknown2c } => {
                    for (k, v) in value.iter().enumerate() {
                        w.f32(ab, 0x20 + k * 4, *v);
                    }
                    w.f32(ab, 0x2C, *unknown2c);
                }
                AttrValue::Vector4(v) => {
                    for (k, x) in v.iter().enumerate() {
                        w.f32(ab, 0x20 + k * 4, *x);
                    }
                }
                AttrValue::HashString(v) => w.u32(ab, 0x20, *v),
            }
            w.ptr(array, i * 8, ab);
        }
        w.ptr(b, 0x20, array);
    }
    w.u16(b, 0x28, p.attributes.len() as u16);
    w.u16(b, 0x2A, p.attributes.len() as u16);
    if p.attributes.len() > u16::MAX as usize {
        bail!("too many attributes");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ycd::{from_xml, parse_ycd, to_xml};

    fn prop(name: u32, attrs: Vec<Attribute>) -> Property {
        Property { name, unk: 0xABCD, attributes: attrs }
    }

    /// Every channel type, both clip kinds, tags and properties with every attribute type, and hash
    /// collisions (11 buckets: 1 and 12 share one, 5 and 16 share one).
    fn sample() -> ClipDictionary {
        let q = |vals: &[u32]| Channel::QuantizeFloat { quantum: 0.5, offset: -1.0, values: vals.iter().map(|v| *v as f32 * 0.5 - 1.0).collect() };
        let rotation = vec![q(&[0, 1, 2, 3]), q(&[3, 3, 1, 0]), q(&[1, 1, 1, 1]), Channel::CachedQuaternion { kind: 1, quat_index: 3 }];
        let rotation4 = vec![q(&[0, 1, 2, 3]), q(&[3, 3, 1, 0]), q(&[1, 1, 1, 1]), q(&[2, 2, 2, 2]), Channel::CachedQuaternion { kind: 2, quat_index: 1 }];
        let position = vec![
            Channel::IndirectQuantizeFloat { quantum: 0.25, offset: 1.0, values: vec![1.0, 1.25, 2.5], frames: vec![0, 2, 2, 1] },
            Channel::StaticFloat(4.5),
            Channel::RawFloat(vec![0.1, -3.7, 8.0, 1.0e-7]),
        ];
        let frames = 4u16;
        let anim = |hash: u32| Animation {
            hash,
            unknown10: 1,
            frames,
            sequence_frame_limit: frames + 30,
            duration: 0.125,
            unknown1c: 0xDEAD_BEEF,
            bone_ids: vec![
                BoneId { id: 0, track: 0, unk0: 0 },
                BoneId { id: 1356, track: 1, unk0: 0 },
                BoneId { id: 2108, track: 1, unk0: 0 },
                BoneId { id: 0, track: 5, unk0: 0 },
                BoneId { id: 0, track: 6, unk0: 0 },
                BoneId { id: 4000, track: 0, unk0: 0 },
            ],
            sequences: vec![Sequence {
                hash: 77,
                frame_count: frames,
                data: vec![
                    position.clone(),
                    rotation.clone(),
                    vec![Channel::StaticQuaternion([0.5, 0.5, 0.5, 0.5])],
                    vec![Channel::StaticVector3([1.0, -2.0, 3.5])],
                    rotation4.clone(),
                    vec![Channel::StaticFloat(0.0)],
                ],
            }],
        };
        let tag = Tag {
            property: prop(rage_joaat("block"), vec![
                Attribute { name: 1, value: AttrValue::HashString(0x1234) },
                Attribute { name: 2, value: AttrValue::Float(0.75) },
            ]),
            start_phase: 0.0,
            end_phase: 0.5,
        };
        let props = vec![
            prop(2, vec![Attribute { name: 3, value: AttrValue::Int(-7) }, Attribute { name: 4, value: AttrValue::Bool(1) }]),
            prop(16, vec![Attribute { name: 5, value: AttrValue::String("hello clip".into()) }]),
            prop(5, vec![
                Attribute { name: 6, value: AttrValue::Vector3 { value: [1.0, 2.0, 3.0], unknown2c: 0.5 } },
                Attribute { name: 7, value: AttrValue::Vector4([0.0, 0.5, 1.0, 1.5]) },
            ]),
        ];
        ClipDictionary {
            clips: vec![
                Clip {
                    hash: 3,
                    name: "pack:/idle.clip".into(),
                    unknown30: 1,
                    tags: vec![tag],
                    properties: props,
                    kind: ClipKind::List {
                        duration: 0.125,
                        animations: vec![
                            ClipAnimation { animation: 1, start: 0.0, end: 0.125, rate: 1.0 },
                            ClipAnimation { animation: 12, start: 0.0, end: 0.125, rate: 1.0 },
                        ],
                    },
                },
                Clip {
                    hash: 7,
                    name: String::new(),
                    unknown30: 0,
                    tags: vec![],
                    properties: vec![],
                    kind: ClipKind::Animation(ClipAnimation { animation: 12, start: 0.0, end: 0.1, rate: 0.5 }),
                },
            ],
            animations: vec![anim(1), anim(12)],
        }
    }

    #[test]
    fn a_dictionary_survives_binary_and_back() {
        let d = sample();
        let bytes = serialize_ycd(&d).unwrap();
        assert_eq!(&bytes[..4], b"RSC7");
        let back = parse_ycd(&bytes).unwrap();
        assert_eq!(back, d);
    }

    #[test]
    fn a_dictionary_survives_xml_and_back() {
        let d = sample();
        let xml = to_xml(&d);
        assert_eq!(from_xml(&xml).unwrap(), d);
        // and through the binary as well, which is what the CLI does
        assert_eq!(parse_ycd(&serialize_ycd(&from_xml(&xml).unwrap()).unwrap()).unwrap(), d);
    }

    #[test]
    fn an_empty_dictionary_is_valid() {
        let d = ClipDictionary::default();
        let back = parse_ycd(&serialize_ycd(&d).unwrap()).unwrap();
        assert_eq!(back, d);
    }

    #[test]
    fn a_clip_playing_a_missing_animation_is_refused() {
        let mut d = sample();
        d.animations.pop();
        let err = serialize_ycd(&d).unwrap_err().to_string();
        assert!(err.contains("not one of the animations"), "{err}");
    }

    #[test]
    fn a_channel_with_the_wrong_number_of_values_is_refused() {
        let mut d = sample();
        d.animations[0].sequences[0].data[0][2] = Channel::RawFloat(vec![1.0, 2.0]);
        let err = format!("{:#}", serialize_ycd(&d).unwrap_err());
        assert!(err.contains("RawFloat") && err.contains("4 frames"), "{err}");
    }

    #[test]
    fn xml_that_is_not_a_clip_dictionary_is_refused() {
        assert!(from_xml("<Other/>").is_err());
        assert!(from_xml("not xml").is_err());
    }
}
